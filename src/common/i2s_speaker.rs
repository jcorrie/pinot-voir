//! I2S speaker task for a DAC/amp such as the MAX98357A.
//!
//! Suggested wiring (any GPIOs work, these match `audio_duplex.rs`):
//!   din  : GPIO 13 (physical pin 17)
//!   bclk : GPIO 14 (physical pin 19)
//!   lrc  : GPIO 15 (physical pin 20)
//!
//! The DMA output must never stall, so this task always has a block queued:
//! the real thing when a packet arrived in time, silence otherwise.

use core::mem;

use crate::common::audio::{AudioBlock, SpeakerChannel, BLOCK_DURATION_MICROS, BUFFER_SIZE};
use defmt::*;
use embassy_rp::peripherals::PIO1;
use embassy_rp::pio_programs::i2s::PioI2sOut;
use embassy_time::{with_timeout, Duration};
use static_cell::StaticCell;

pub const BIT_DEPTH: u32 = 16;

/// Expand mono samples into stereo I2S frames (same sample on both
/// channels; left in the upper half-word as the PIO shifts out MSB-first).
fn fill_frames(dst: &mut [u32], block: Option<&AudioBlock>) {
    match block {
        Some(b) => {
            for (d, &s) in dst.iter_mut().zip(b.samples.iter()) {
                let v = s as u16 as u32;
                *d = (v << 16) | v;
            }
        }
        None => dst.fill(0),
    }
}

#[embassy_executor::task]
pub async fn i2s_speaker_task(
    audio_channel: &'static SpeakerChannel,
    mut i2s: PioI2sOut<'static, PIO1, 1>,
) {
    static DMA_BUFFER: StaticCell<[u32; BUFFER_SIZE * 2]> = StaticCell::new();
    let dma_buffer = DMA_BUFFER.init_with(|| [0u32; BUFFER_SIZE * 2]);
    let (mut front_buffer, mut back_buffer) = dma_buffer.split_at_mut(BUFFER_SIZE);

    i2s.start();
    info!("Started i2s speaker");

    let mut was_playing = false;
    loop {
        let transfer = i2s.write(front_buffer);

        // While the DMA drains the front buffer (one block period), wait for
        // the next block. Give up a little early so the back buffer is
        // always ready before the transfer completes.
        let deadline = Duration::from_micros(BLOCK_DURATION_MICROS.saturating_sub(3_000));
        match with_timeout(deadline, audio_channel.receive()).await {
            Ok(block) => {
                if !was_playing {
                    info!("Speaker: stream started");
                    was_playing = true;
                }
                fill_frames(back_buffer, Some(&block));
            }
            Err(_) => {
                if was_playing {
                    info!("Speaker: stream idle, playing silence");
                    was_playing = false;
                }
                fill_frames(back_buffer, None);
            }
        }

        transfer.await;
        mem::swap(&mut front_buffer, &mut back_buffer);
    }
}
