use defmt::*;
use embassy_rp::adc::{Adc, Channel, Config};
use embassy_rp::gpio::Pull;
use embassy_rp::peripherals::{ADC, DMA_CH1, PIN_26};
use embassy_rp::Peri;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel as SyncChannel;
use embassy_time::Duration;
use embedded_rtp::profile::{CLOCK_RATE, FRAME_SAMPLES};
use {defmt_rtt as _, panic_probe as _};

pub const FRAME: Duration = Duration::from_millis(20);
pub const FRAME_TICKS: u32 = FRAME_SAMPLES as u32;

#[derive(Clone, Copy, Debug)]
pub struct AudioBlock {
    pub samples: [u16; FRAME_SAMPLES],
    pub block_id: u32,
    pub timestamp: u64,
}

impl Default for AudioBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioBlock {
    pub fn new() -> Self {
        Self {
            samples: [0; FRAME_SAMPLES],
            block_id: 0,
            timestamp: 0,
        }
    }

    pub fn centre_samples(&self) -> [i16; FRAME_SAMPLES] {
        self.samples.map(|x| {
            let centered = (x as i32) - 2048; // widen first, -2048..+2047
            (centered * 16) as i16 // scale into ~-32768..+32752
        })
    }
}

// ---------- Core1: ADC sampling ----------
#[embassy_executor::task]
pub async fn adc_task(
    audio_channel: &'static SyncChannel<CriticalSectionRawMutex, AudioBlock, 4>,
    adc_peripheral: Peri<'static, ADC>,
    dma: Peri<'static, DMA_CH1>,
    pin: Peri<'static, PIN_26>,
) {
    info!("ADC task starting on Core 1");

    let mut adc = Adc::new(adc_peripheral, crate::common::irqs::Irqs, Config::default());
    let mut p26 = Channel::new_pin(pin, Pull::None);

    const SAMPLE_RATE_HZ: u32 = CLOCK_RATE;
    const ADC_DIV: u16 = (48_000_000 / SAMPLE_RATE_HZ - 1) as u16;

    let mut dma = embassy_rp::dma::Channel::new(dma, crate::common::irqs::Irqs);

    // No double buffering: `AudioBlock` is `Copy`, so `try_send` copies the
    // frame out and the buffer is free to be refilled immediately. One capture
    // per frame sent, which is what the 50 fps the mixer expects requires.
    let mut audio_block = AudioBlock::new();
    let mut dropped = 0u32;
    loop {
        if adc
            .read_many(&mut p26, &mut audio_block.samples, ADC_DIV, &mut dma)
            .await
            .is_err()
        {
            error!("ADC read error");
            continue;
        }
        if audio_channel.try_send(audio_block).is_err() {
            dropped += 1;
            // One line a second at 50 fps. Logging every drop floods RTT, and
            // defmt takes a critical section, so it stalls core0 as well.
            if dropped.is_multiple_of(50) {
                info!("Audio channel full, dropped {} frames", dropped);
            }
        }
    }
}
