use crate::common::audio::{AudioBlock, BUFFER_SIZE};
use defmt::*;
use embassy_rp::adc::{Adc, Channel, Config, InterruptHandler as ADCInterruptHandler};
use embassy_rp::bind_interrupts;
use embassy_rp::dma;
use embassy_rp::gpio::Pull;
use embassy_rp::peripherals::{ADC, PIN_26};
use embassy_rp::Peri;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel as SyncChannel;
use embassy_time::{Instant, Timer};
use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct IrqsADC {
    ADC_IRQ_FIFO => ADCInterruptHandler;
});

// ---------- Core1: ADC sampling ----------
#[embassy_executor::task]
pub async fn adc_task(
    audio_channel: &'static SyncChannel<CriticalSectionRawMutex, AudioBlock, 4>,
    adc_peripheral: Peri<'static, ADC>,
    dma: dma::Channel<'static>,
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
        let mut samples: [u16; BUFFER_SIZE] = [0u16; BUFFER_SIZE];
        match adc
            .read_many(&mut p26, &mut samples, ADC_DIV, &mut dma)
            .await
        {
            Ok(_) => {
                let mut audio_block = AudioBlock::new();
                block_counter += 1;
                audio_block.block_id = block_counter;
                audio_block.timestamp = Instant::now().as_micros();
                audio_block.update_samples_from_u16(samples);
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
