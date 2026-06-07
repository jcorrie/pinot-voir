//! I2S microphone task for SPH0645LM4H
//!
//! Connect the i2s microphone as follows:
//!   bclk : GPIO 18
//!   lrc  : GPIO 19
//!   din  : GPIO 20

use core::mem;

use crate::common::audio::AudioBlock;
use defmt::*;
use embassy_rp::peripherals::PIO1;
use embassy_rp::pio_programs::i2s::PioI2sIn;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel as SyncChannel;
use embassy_time::Instant;
use static_cell::StaticCell;

pub const BUFFER_SIZE: usize = 720;
pub const SAMPLE_RATE: u32 = 48_000;
pub const BIT_DEPTH: u32 = 16;
pub const CHANNELS: u32 = 2;
pub const USE_ONBOARD_PULLDOWN: bool = false;

#[embassy_executor::task]
pub async fn i2s_mic_task(
    audio_channel: &'static SyncChannel<CriticalSectionRawMutex, AudioBlock, 4>,
    mut i2s: PioI2sIn<'static, PIO1, 0>,
) {
    i2s.start();

    info!("Started i2s");
    static DMA_BUFFER: StaticCell<[u32; BUFFER_SIZE * 2]> = StaticCell::new();
    let dma_buffer = DMA_BUFFER.init_with(|| [0u32; BUFFER_SIZE * 2]);
    let (mut back_buffer, mut front_buffer) = dma_buffer.split_at_mut(BUFFER_SIZE);

    let mut block_counter = 0u32;
    loop {
        let mut audio_block = AudioBlock::new();

        i2s.read(front_buffer).await;
        mem::swap(&mut back_buffer, &mut front_buffer);

        // Queue next DMA immediately before processing
        let next_dma = i2s.read(front_buffer);

        block_counter += 1;
        audio_block.block_id = block_counter;
        audio_block.timestamp = Instant::now().as_micros();
        let back: &[u32; BUFFER_SIZE] = (&*back_buffer).try_into().expect("Buffer size mismatch");

        // Log raw DMA values every 100 blocks to verify mic is outputting data
        if block_counter % 100 == 0 {
            let max_raw = back.iter().copied().max().unwrap_or(0);
            info!("block {} raw[0..4]: {:08x} {:08x} {:08x} {:08x} max={:08x}",
                block_counter, back[0], back[1], back[2], back[3], max_raw);
        }

        audio_block.update_samples_from_u32(back);

        match audio_channel.try_send(audio_block) {
            Ok(_) => {}
            Err(_) => info!("Audio channel full, dropping block"),
        }

        next_dma.await;
    }
}
