//! This example shows receiving audio from a connected I2S microphone (or other audio source)
//! using the PIO module of the RP235x.
//!
//!
//! Connect the i2s microphone as follows:
//!   bclk : GPIO 18
//!   lrc  : GPIO 19
//!   din  : GPIO 20
//! Then hold down the boot select button to begin receiving audio. Received I2S words will be written to
//! buffers for the left and right channels for use in your application, whether that's storage or
//! further processing
//!
//!  Note the const USE_ONBOARD_PULLDOWN is by default set to false, meaning an external
//!  pull-down resistor is being used on the data pin if required by the mic being used.

use core::mem;

use crate::common::audio::AudioBlock;
use defmt::*;
use embassy_executor::Spawner;
use embassy_rp::bind_interrupts;
use embassy_rp::dma;
use embassy_rp::peripherals::{DMA_CH1, PIO0, PIO1};
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_rp::pio_programs::i2s::{PioI2sIn, PioI2sInProgram};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel as SyncChannel;

use embassy_time::{Instant, Timer};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    PIO1_IRQ_0 => InterruptHandler<PIO1>;
    DMA_IRQ_0  => dma::InterruptHandler<DMA_CH1>;
});

pub const BUFFER_SIZE: usize = 960;
pub const SAMPLE_RATE: u32 = 48_000;
pub const BIT_DEPTH: u32 = 16;
const CHANNELS: u32 = 1;
const USE_ONBOARD_PULLDOWN: bool = false; // whether or not to use the onboard pull-down resistor,
                                          // which has documented issues on many RP235x boards

#[embassy_executor::task]
pub async fn i2s_mic_task(
    audio_channel: &'static SyncChannel<CriticalSectionRawMutex, AudioBlock, 4>,
) {
    let p = embassy_rp::init(Default::default());

    // Setup pio state machine for i2s input
    let Pio {
        mut common, sm0, ..
    } = Pio::new(p.PIO1, Irqs);

    let bit_clock_pin = p.PIN_18;
    let left_right_clock_pin = p.PIN_19;
    let data_pin = p.PIN_20;

    let program = PioI2sInProgram::new(&mut common);
    let mut i2s = PioI2sIn::new(
        &mut common,
        sm0,
        p.DMA_CH1,
        Irqs,
        USE_ONBOARD_PULLDOWN,
        data_pin,
        bit_clock_pin,
        left_right_clock_pin,
        SAMPLE_RATE,
        BIT_DEPTH,
        CHANNELS,
        &program,
    );
    i2s.start();

    // create two audio buffers (back and front) which will take turns being
    // filled with new audio data from the PIO fifo using DMA
    static DMA_BUFFER: StaticCell<[u32; BUFFER_SIZE * 2]> = StaticCell::new();
    let dma_buffer = DMA_BUFFER.init_with(|| [0u32; BUFFER_SIZE * 2]);
    let (mut back_buffer, mut front_buffer) = dma_buffer.split_at_mut(BUFFER_SIZE);

    let mut block_counter = 0u32;
    loop {
        let mut audio_block = AudioBlock::new();
        // trigger transfer of front buffer data to the pio fifo
        // but don't await the returned future, yet
        let dma_future = i2s.read(front_buffer);
        // now await the dma future. once the dma finishes, the next buffer needs to be queued
        // within DMA_DEPTH / SAMPLE_RATE = 8 / 48000 seconds = 166us
        dma_future.await;

        info!("Received I2S data word: {:?}", &front_buffer);
        block_counter += 1;

        mem::swap(&mut back_buffer, &mut front_buffer);
        audio_block.block_id = block_counter;
        audio_block.timestamp = Instant::now().as_micros();
        audio_block
            .update_samples_from_u32(back_buffer.try_into().expect("Buffer size doesn't fit."));
        audio_channel.send(audio_block).await;
    }
}
