//! I2S microphone task for SPH0645LM4H
//!
//! Connect the i2s microphone as follows (SELECT → GND, i.e. left channel):
//!   bclk : GPIO 18  (physical pin 24)
//!   lrc  : GPIO 19  (physical pin 25)
//!   din  : GPIO 20  (physical pin 26)

use core::mem;

use crate::common::audio::{AudioBlock, MicChannel};
use defmt::*;
use embassy_rp::peripherals::PIO1;
use embassy_rp::pio_programs::i2s::PioI2sIn;
use embassy_time::Instant;
use static_cell::StaticCell;

pub use crate::common::audio::{BUFFER_SIZE, SAMPLE_RATE};

pub const BIT_DEPTH: u32 = 16;
pub const CHANNELS: u32 = 2;
pub const USE_ONBOARD_PULLDOWN: bool = false;

#[embassy_executor::task]
pub async fn i2s_mic_task(audio_channel: &'static MicChannel, mut i2s: PioI2sIn<'static, PIO1, 0>) {
    i2s.start();

    info!("Started i2s");
    static DMA_BUFFER: StaticCell<[u32; BUFFER_SIZE * 2]> = StaticCell::new();
    let dma_buffer = DMA_BUFFER.init_with(|| [0u32; BUFFER_SIZE * 2]);
    let (mut back_buffer, mut front_buffer) = dma_buffer.split_at_mut(BUFFER_SIZE);

    let mut block_counter = 0u32;
    let mut dropped = 0u32;
    loop {
        // One DMA read per loop iteration: capture into the front buffer
        // while we process the back buffer the previous iteration filled.
        // (The old version awaited a second read at the bottom of the loop,
        // so every other buffer was overwritten unprocessed and the stream
        // ran at half rate.)
        let transfer = i2s.read(front_buffer);

        let mut audio_block = AudioBlock::new();
        block_counter += 1;
        audio_block.block_id = block_counter;
        audio_block.timestamp = Instant::now().as_micros();
        let back: &[u32; BUFFER_SIZE] = (&*back_buffer).try_into().expect("Buffer size mismatch");

        audio_block.update_samples_from_u32(back);

        // Dropping is the normal state whenever nothing drains the channel
        // (WiFi still connecting, no peer attached), so rate-limit the log
        // to roughly one line every two seconds.
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
