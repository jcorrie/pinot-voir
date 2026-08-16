//! Client side of the SPQR audio room.
//!
//! The room is one always-on channel: everyone who sends into it receives the
//! sum of everyone *else*. The wire format is deliberately bare — raw PCM16
//! little endian, mono, 16 kHz, 20 ms frames, no header and no handshake. One
//! datagram is one frame, and sending is joining: the mix comes back to
//! whatever address and port the datagrams came from, so a single socket serves
//! both directions.
//!
//! `SPQR/docs/audio-protocol.md` is the reference; this module mirrors it.
//!
//! Two consequences shape the code here:
//!
//! * **Silence is not transmitted.** The server sends nothing when nobody is
//!   talking, so "no packet this tick" is the normal state, not a fault. The
//!   playback clock has to free-run and fill with silence.
//! * **A listener still has to be seen.** A device with nothing to say sends a
//!   zero-length datagram every [`KEEPALIVE_INTERVAL`] to stay registered; the
//!   server forgets a UDP participant 5 s after its last datagram of any kind.

use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_time::Duration;

pub const SAMPLE_RATE: u32 = 16_000;
/// Samples in one 20 ms frame.
pub const FRAME_SAMPLES: usize = 320;
pub const FRAME_BYTES: usize = FRAME_SAMPLES * 2;
pub const UDP_PORT: u16 = 1234;

/// Well inside the server's 5 s timeout, so a couple of lost datagrams do not
/// drop us out of the room.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(2);

pub type Frame = [i16; FRAME_SAMPLES];

/// Frames of room audio held before playback. Beyond this the oldest is
/// dropped: late audio is worth nothing in a conversation.
const JITTER_DEPTH: usize = 6;
/// Frames to bank before playback starts, and again after every gap. Costs
/// 40 ms of latency and stops imprecise arrival timing from punching holes in
/// the audio — the same trade the server makes on its own inputs.
const PREFILL_FRAMES: usize = 2;

/// Elastic buffer between the network (bursty, and silent for long stretches)
/// and the I2S output (a fixed 50 Hz clock that cannot be paused).
pub struct Playback {
    frames: [Frame; JITTER_DEPTH],
    head: usize,
    len: usize,
    /// False until [`PREFILL_FRAMES`] have banked up. Cleared whenever the
    /// buffer runs dry, so playback re-buffers after each silence.
    primed: bool,
    /// Frames discarded because the buffer was full — network ahead of the
    /// playback clock, or a burst that outran it.
    pub overruns: u32,
    /// Times the buffer ran dry mid-stream. Silence between talkers is not
    /// counted; only a transition from playing to empty is.
    pub underruns: u32,
}

impl Default for Playback {
    fn default() -> Self {
        Self::new()
    }
}

impl Playback {
    pub const fn new() -> Self {
        Self {
            frames: [[0; FRAME_SAMPLES]; JITTER_DEPTH],
            head: 0,
            len: 0,
            primed: false,
            overruns: 0,
            underruns: 0,
        }
    }

    /// Decode one datagram of room audio into the buffer. Anything shorter than
    /// a whole frame is ignored, and anything longer is truncated to one.
    pub fn push(&mut self, bytes: &[u8]) {
        if bytes.len() < FRAME_BYTES {
            return;
        }
        if self.len == JITTER_DEPTH {
            self.head = (self.head + 1) % JITTER_DEPTH;
            self.len -= 1;
            self.overruns = self.overruns.wrapping_add(1);
        }
        let slot = (self.head + self.len) % JITTER_DEPTH;
        for (sample, pair) in self.frames[slot].iter_mut().zip(bytes.chunks_exact(2)) {
            *sample = i16::from_le_bytes([pair[0], pair[1]]);
        }
        self.len += 1;
    }

    /// Take the next frame to play. `false` means "play silence": either the
    /// room is quiet or the buffer is still priming.
    pub fn pop(&mut self, out: &mut Frame) -> bool {
        if !self.primed {
            if self.len < PREFILL_FRAMES {
                return false;
            }
            self.primed = true;
        }
        if self.len == 0 {
            self.primed = false;
            self.underruns = self.underruns.wrapping_add(1);
            return false;
        }
        out.copy_from_slice(&self.frames[self.head]);
        self.head = (self.head + 1) % JITTER_DEPTH;
        self.len -= 1;
        true
    }

    /// Drop everything queued and re-arm the prefill.
    pub fn flush(&mut self) {
        self.head = 0;
        self.len = 0;
        self.primed = false;
    }
}

/// Shared between the network task, which fills it, and the speaker task, which
/// drains it on the I2S clock. A blocking critical-section mutex rather than an
/// async one: every access is a short memcpy, and the speaker task must never
/// be made to wait while a DMA transfer is in flight.
pub static PLAYBACK: Mutex<CriticalSectionRawMutex, RefCell<Playback>> =
    Mutex::new(RefCell::new(Playback::new()));

static PTT_HELD: AtomicBool = AtomicBool::new(false);

/// True while the push-to-talk button is down.
///
/// This is the half-duplex gate. While it is held the microphone is live and
/// **nothing is played out of the speaker** — incoming room audio is discarded
/// rather than buffered, so releasing the button does not dump a backlog. The
/// device has no acoustic echo canceller, and on an M0+ it is not going to get
/// one, so the mic and the speaker are never live at the same time.
pub fn ptt_held() -> bool {
    PTT_HELD.load(Ordering::Relaxed)
}

pub fn set_ptt(held: bool) {
    PTT_HELD.store(held, Ordering::Relaxed);
}
