//! I2S microphone task for SPH0645LM4H
//!
//! The SPH0645 needs BCLK between 2.048 and 4.096 MHz. The generic
//! embassy 16-bit I2S input program clocks a 48 kHz stereo bus at
//! 1.536 MHz — below spec, where the mic's decimator doesn't run and the
//! data line just reads a stuck MSB (every sample 0x8000). This driver
//! uses 32-bit half-frames instead: BCLK = 48 kHz × 64 = 3.072 MHz, in
//! spec. Each stereo frame yields two FIFO words; the left word carries
//! the mic's 18-bit sample MSB-first from bit 31 (SELECT → GND).
//!
//! Connect the i2s microphone as follows (SELECT → GND, i.e. left channel):
//!   bclk : GPIO 18  (physical pin 24)
//!   lrc  : GPIO 19  (physical pin 25)
//!   din  : GPIO 20  (physical pin 26)

use core::mem;

use crate::common::audio::{AudioBlock, MicChannel};
use defmt::*;
use embassy_rp::dma;
use embassy_rp::gpio::Pull;
use embassy_rp::peripherals::PIO1;
use embassy_rp::pio::{
    Common, Config, Direction as PioDirection, FifoJoin, Instance, LoadedProgram, PioPin,
    ShiftConfig, ShiftDirection, StateMachine,
};
use embassy_rp::pio_programs::clock_divider::calculate_pio_clock_divider;
use embassy_rp::{interrupt, Peri};
use embassy_time::Instant;
use static_cell::StaticCell;

pub use crate::common::audio::{BUFFER_SIZE, SAMPLE_RATE};

/// Bits per half-frame on the wire.
pub const FRAME_BITS: u32 = 32;
pub const USE_ONBOARD_PULLDOWN: bool = false;

/// I2S input program with 32-bit half-frames. Same structure as embassy's
/// `PioI2sInProgram` (which is hardcoded to 16 bits per half via
/// `set x, 14`) but reading 32 bits per channel so the bit clock meets the
/// SPH0645's minimum.
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

/// PIO-backed I2S input clocked for the SPH0645 (32-bit half-frames).
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
            // join fifos to have twice the time to start the next dma transfer
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

    /// Return an in-progress dma transfer future. Awaiting it will guarantee a complete transfer.
    pub fn read<'b>(&'b mut self, buff: &'b mut [u32]) -> dma::Transfer<'b> {
        self.sm.rx().dma_pull(&mut self.dma, buff, false)
    }
}

/// FIFO words per audio block: one left + one right word per sample.
const WORDS_PER_BLOCK: usize = BUFFER_SIZE * 2;

#[embassy_executor::task]
pub async fn i2s_mic_task(
    audio_channel: &'static MicChannel,
    mut i2s: Sph0645I2sIn<'static, PIO1, 0>,
) {
    i2s.start();

    info!("Started i2s");
    static DMA_BUFFER: StaticCell<[u32; WORDS_PER_BLOCK * 2]> = StaticCell::new();
    let dma_buffer = DMA_BUFFER.init_with(|| [0u32; WORDS_PER_BLOCK * 2]);
    let (mut back_buffer, mut front_buffer) = dma_buffer.split_at_mut(WORDS_PER_BLOCK);

    let mut block_counter = 0u32;
    let mut dropped = 0u32;
    loop {
        // One DMA read per loop iteration: capture into the front buffer
        // while we process the back buffer the previous iteration filled.
        // (An earlier version awaited a second read at the bottom of the
        // loop, so every other buffer was overwritten unprocessed and the
        // stream ran at half rate.)
        let transfer = i2s.read(front_buffer);

        let mut audio_block = AudioBlock::new();
        block_counter += 1;
        audio_block.block_id = block_counter;
        audio_block.timestamp = Instant::now().as_micros();
        audio_block.update_samples_from_i2s_frames(back_buffer);

        // Skip block 1: the back buffer holds nothing on the first pass.
        if block_counter > 1 {
            match audio_channel.try_send(audio_block) {
                Ok(_) => {}
                Err(_) => {
                    dropped += 1;
                    if dropped % 128 == 1 {
                        info!("Audio channel full, {} blocks dropped so far", dropped);
                    }
                }
            }
        }

        transfer.await;
        mem::swap(&mut back_buffer, &mut front_buffer);
    }
}
