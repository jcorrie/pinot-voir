use defmt::*;
use embassy_rp::Peri;
use embassy_rp::adc::{Adc, Channel, Config, InterruptHandler as ADCInterruptHandler};
use embassy_rp::bind_interrupts;
use embassy_rp::gpio::Pull;
use embassy_rp::peripherals::{ADC, DMA_CH1, PIN_26};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel as SyncChannel;
use embassy_time::{Instant, Timer};
use {defmt_rtt as _, panic_probe as _};

pub const AUDIO_BUFFER_SIZE: usize = 512;

#[derive(Clone, Copy)]
pub struct AudioBlock {
    pub samples: [u16; AUDIO_BUFFER_SIZE],
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
            samples: [0; AUDIO_BUFFER_SIZE],
            block_id: 0,
            timestamp: 0,
        }
    }

    pub fn centre_samples(&self) -> [i16; AUDIO_BUFFER_SIZE] {
        self.samples.map(|x| {
            let centered = (x as i32) - 2048; // widen first, -2048..+2047
            (centered * 16) as i16 // scale into ~-32768..+32752
        })
    }
}

bind_interrupts!(struct IrqsADC {
    ADC_IRQ_FIFO => ADCInterruptHandler;
});

// ---------- Core1: ADC sampling ----------
#[embassy_executor::task]
pub async fn adc_task(
    audio_channel: &'static SyncChannel<CriticalSectionRawMutex, AudioBlock, 4>,
    adc_peripheral: Peri<'static, ADC>,
    dma: Peri<'static, DMA_CH1>,
    pin: Peri<'static, PIN_26>,
) {
    info!("ADC task starting on Core 1");

    let mut adc = Adc::new(adc_peripheral, IrqsADC, Config::default());
    let mut p26 = Channel::new_pin(pin, Pull::None);

    const SAMPLE_RATE_HZ: u32 = 44100;
    const ADC_DIV: u16 = (48_000_000 / SAMPLE_RATE_HZ - 1) as u16;

    let mut dma = dma;
    let mut block_counter = 0u32;

    loop {
        let mut audio_block = AudioBlock::new();

        match adc
            .read_many(&mut p26, &mut audio_block.samples, ADC_DIV, dma.reborrow())
            .await
        {
            Ok(_) => {
                block_counter += 1;
                audio_block.block_id = block_counter;
                audio_block.timestamp = Instant::now().as_micros();
                audio_channel.send(audio_block).await;

                if block_counter.is_multiple_of(100) {
                    info!("ADC: Captured block {}", block_counter);
                }
            }
            Err(_) => {
                error!("ADC read error");
                Timer::after_millis(1).await;
            }
        }
    }
}