//! I2S input clocked for the SPH0645LM4H.
//!
//! The SPH0645 needs BCLK between 2.048 and 4.096 MHz. Embassy's generic
//! `PioI2sInProgram` is hardcoded to 16-bit half-frames (`set x, 14`), which on
//! a 48 kHz stereo bus gives BCLK = 1.536 MHz — below spec, where the mic's
//! decimator does not run and the data line reads a stuck MSB: every sample
//! comes back 0x8000. This driver reads 32-bit half-frames instead, so
//! BCLK = 48 kHz × 32 × 2 = 3.072 MHz, comfortably in range.
//!
//! Each stereo frame therefore yields *two* FIFO words, not one. With
//! SELECT tied to GND the microphone is the left channel, so even indices
//! carry the sample, MSB-first from bit 31 — see [`sample_from_frame`].
//!
//! Wiring:
//!   bclk : GPIO 18 (physical pin 24)
//!   lrc  : GPIO 19 (physical pin 25)
//!   din  : GPIO 20 (physical pin 26)
//!   SELECT → GND
//!
//! The program and driver here come from the duplex audio work on
//! `claude/pico-udp-audio-duplex-l472po` (PR #9), which is where the clocking
//! problem was diagnosed. Only the driver is kept: the capture task that goes
//! with it depends on that branch's block format.

use embassy_rp::dma;
use embassy_rp::gpio::Pull;
use embassy_rp::pio::{
    Common, Config, Direction as PioDirection, FifoJoin, Instance, LoadedProgram, PioPin,
    ShiftConfig, ShiftDirection, StateMachine,
};
use embassy_rp::pio_programs::clock_divider::calculate_pio_clock_divider;
use embassy_rp::{interrupt, Peri};

/// Bits per half-frame on the wire. The reason this driver exists.
pub const FRAME_BITS: u32 = 32;

/// FIFO words per stereo frame: one left, one right.
pub const WORDS_PER_FRAME: usize = 2;

/// The RP2040's internal pull-downs are adequate; the Pico 2 is the board with
/// the known problem here. Set true only if your microphone needs it and you
/// have no external resistor.
pub const USE_ONBOARD_PULLDOWN: bool = false;

/// Extract the mono sample from one stereo frame's pair of FIFO words.
///
/// The state machine starts at the beginning of a left half-frame and FIFO
/// stalls pause the bus rather than dropping words, so the left word is always
/// the first of the pair.
pub fn sample_from_frame(frame: &[u32]) -> i16 {
    ((frame[0] as i32) >> 16) as i16
}

/// I2S input program with 32-bit half-frames. Same structure as embassy's
/// `PioI2sInProgram`, counting 31 bits per channel instead of 15.
pub struct Sph0645InProgram<'d, PIO: Instance> {
    prg: LoadedProgram<'d, PIO>,
}

impl<'d, PIO: Instance> Sph0645InProgram<'d, PIO> {
    pub fn new(common: &mut Common<'d, PIO>) -> Self {
        let prg = pio::pio_asm!(
            ".side_set 2",
            "    set x, 30               side 0b01",
            "left_data:",
            "    in pins, 1              side 0b00",
            "    jmp x-- left_data       side 0b01",
            "    in pins, 1              side 0b10", // ws changes 1 clock before MSB
            "    set x, 30               side 0b11",
            "right_data:",
            "    in pins, 1              side 0b10",
            "    jmp x-- right_data      side 0b11",
            "    in pins, 1              side 0b00"
        );
        Self {
            prg: common.load_program(&prg.program),
        }
    }
}

/// PIO-backed I2S input clocked for the SPH0645.
pub struct Sph0645I2sIn<'d, P: Instance, const S: usize> {
    dma: dma::Channel<'d>,
    sm: StateMachine<'d, P, S>,
}

impl<'d, P: Instance, const S: usize> Sph0645I2sIn<'d, P, S> {
    #[allow(clippy::too_many_arguments)]
    pub fn new<D: dma::ChannelInstance>(
        common: &mut Common<'d, P>,
        mut sm: StateMachine<'d, P, S>,
        dma: Peri<'d, D>,
        irq: impl interrupt::typelevel::Binding<D::Interrupt, dma::InterruptHandler<D>> + 'd,
        data_pulldown: bool,
        data_pin: Peri<'d, impl PioPin>,
        bit_clock_pin: Peri<'d, impl PioPin>,
        lr_clock_pin: Peri<'d, impl PioPin>,
        sample_rate: u32,
        program: &Sph0645InProgram<'d, P>,
    ) -> Self {
        let mut data_pin = common.make_pio_pin(data_pin);
        if data_pulldown {
            data_pin.set_pull(Pull::Down);
        }
        let bit_clock_pin = common.make_pio_pin(bit_clock_pin);
        let lr_clock_pin = common.make_pio_pin(lr_clock_pin);

        let cfg = {
            let mut cfg = Config::default();
            cfg.use_program(&program.prg, &[&bit_clock_pin, &lr_clock_pin]);
            cfg.set_in_pins(&[&data_pin]);
            // 2 PIO cycles per bit clock, FRAME_BITS bits per channel, stereo.
            let bit_clock_hz = sample_rate * FRAME_BITS * 2;
            cfg.clock_divider = calculate_pio_clock_divider(bit_clock_hz * 2);
            cfg.shift_in = ShiftConfig {
                threshold: 32,
                direction: ShiftDirection::Left,
                auto_fill: true,
            };
            // Join the FIFOs to have twice the time to start the next transfer.
            cfg.fifo_join = FifoJoin::RxOnly;
            cfg
        };
        sm.set_config(&cfg);
        sm.set_pin_dirs(PioDirection::In, &[&data_pin]);
        sm.set_pin_dirs(PioDirection::Out, &[&lr_clock_pin, &bit_clock_pin]);

        Self {
            dma: dma::Channel::new(dma, irq),
            sm,
        }
    }

    pub fn start(&mut self) {
        self.sm.set_enable(true);
    }

    /// An in-progress DMA transfer. Awaiting it guarantees a complete transfer.
    pub fn read<'b>(&'b mut self, buff: &'b mut [u32]) -> dma::Transfer<'b> {
        self.sm.rx().dma_pull(&mut self.dma, buff, false)
    }
}
