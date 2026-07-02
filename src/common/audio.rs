//! Shared audio types and the UDP wire format.
//!
//! # Wire format
//!
//! Every UDP packet, in either direction, is a 12-byte header optionally
//! followed by one block of samples:
//!
//! | bytes  | field                                                    |
//! |--------|----------------------------------------------------------|
//! | 0..4   | magic `b"PVAU"`                                          |
//! | 4      | protocol version (currently 1)                           |
//! | 5      | direction: 0 = client → pico, 1 = pico → client          |
//! | 6..8   | reserved, zero                                           |
//! | 8..12  | sequence number, `u32` little-endian                     |
//! | 12..   | payload: `BUFFER_SIZE` × `i16` LE samples, or empty      |
//!
//! A header-only packet is a keep-alive: it carries no audio but tells the
//! pico where to send its microphone stream. The client always initiates —
//! the pico learns the return endpoint from the source address of whatever
//! it receives.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel as SyncChannel;
use {defmt_rtt as _, panic_probe as _};

/// Samples per audio block (mono, 16-bit).
/// 720 samples @ 48 kHz = 15 ms per block, and 12 + 1440 bytes per packet —
/// comfortably under a 1500-byte MTU.
pub const BUFFER_SIZE: usize = 720;
pub const SAMPLE_RATE: u32 = 48_000;
pub const BLOCK_DURATION_MICROS: u64 = (BUFFER_SIZE as u64 * 1_000_000) / SAMPLE_RATE as u64;

pub const MAGIC: [u8; 4] = *b"PVAU";
pub const PROTOCOL_VERSION: u8 = 1;
pub const HEADER_BYTES: usize = 12;
pub const PAYLOAD_BYTES: usize = BUFFER_SIZE * 2;
pub const PACKET_BYTES: usize = HEADER_BYTES + PAYLOAD_BYTES;

/// Queue depths for the two on-device audio paths. The mic side stays
/// shallow (fresh audio beats complete audio); the speaker side is a small
/// jitter buffer for network delivery variance.
pub const MIC_QUEUE_LEN: usize = 4;
pub const SPEAKER_QUEUE_LEN: usize = 8;

pub type MicChannel = SyncChannel<CriticalSectionRawMutex, AudioBlock, MIC_QUEUE_LEN>;
pub type SpeakerChannel = SyncChannel<CriticalSectionRawMutex, AudioBlock, SPEAKER_QUEUE_LEN>;

/// Which way a packet is travelling. Carried in the header so a client
/// can't mistake its own transmit stream for the pico's (both use the
/// same port).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    ToPico = 0,
    FromPico = 1,
}

/// Serialise one block of samples into `buf` as a wire packet.
pub fn write_packet(
    buf: &mut [u8; PACKET_BYTES],
    direction: Direction,
    seq: u32,
    samples: &[i16; BUFFER_SIZE],
) {
    buf[0..4].copy_from_slice(&MAGIC);
    buf[4] = PROTOCOL_VERSION;
    buf[5] = direction as u8;
    buf[6] = 0;
    buf[7] = 0;
    buf[8..12].copy_from_slice(&seq.to_le_bytes());
    for (chunk, &s) in buf[HEADER_BYTES..].chunks_exact_mut(2).zip(samples.iter()) {
        chunk.copy_from_slice(&s.to_le_bytes());
    }
}

/// Validate a received packet travelling in `expected_direction`.
///
/// Returns the sequence number and the payload bytes; an empty payload is a
/// keep-alive. Anything malformed (bad magic/version/direction/length)
/// returns `None`.
pub fn parse_packet(buf: &[u8], expected_direction: Direction) -> Option<(u32, &[u8])> {
    if buf.len() != HEADER_BYTES && buf.len() != PACKET_BYTES {
        return None;
    }
    if buf[0..4] != MAGIC || buf[4] != PROTOCOL_VERSION || buf[5] != expected_direction as u8 {
        return None;
    }
    let seq = u32::from_le_bytes(buf[8..12].try_into().unwrap());
    Some((seq, &buf[HEADER_BYTES..]))
}

#[derive(Clone, Copy)]
pub struct AudioBlock {
    pub samples: [i16; BUFFER_SIZE],
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
            samples: [0; BUFFER_SIZE],
            block_id: 0,
            timestamp: 0,
        }
    }

    pub fn update_samples_from_u16(&mut self, samples: [u16; BUFFER_SIZE]) {
        let samples = samples.map(|x| ((x as i16).wrapping_sub(2048)) << 4);
        self.samples = samples;
    }

    pub fn update_samples_from_u32(&mut self, samples: &[u32; BUFFER_SIZE]) {
        for (dst, &src) in self.samples.iter_mut().zip(samples.iter()) {
            // The PIO ISR shifts LEFT, so the first 16 bits clocked in (the
            // LEFT channel, WS low) end up in the upper half-word. With the
            // mic's SELECT pin tied to GND it outputs on the left channel,
            // which is what we extract here.
            *dst = ((src as i32) >> 16) as i16;
        }
    }

    /// Fill `samples` from a wire-format payload of little-endian `i16`s.
    pub fn update_samples_from_le_bytes(&mut self, payload: &[u8]) {
        for (dst, chunk) in self.samples.iter_mut().zip(payload.chunks_exact(2)) {
            *dst = i16::from_le_bytes([chunk[0], chunk[1]]);
        }
    }

    pub fn centre_samples(&mut self) {
        self.samples = self.samples.map(|x| {
            let centered = (x as i32) - 2048; // widen first, -2048..+2047
            (centered * 16) as i16 // scale into ~-32768..+32752
        });
    }
}
