//! Fixed 3:1 resampling between the 48 kHz I2S bus and the 16 kHz wire rate.
//!
//! The audio room runs at 16 kHz (see [`crate::common::intercom`]) but I2S
//! microphones such as the SPH0645LM4H and INMP441 are only specified down to
//! 32 kHz, so the bus runs at 48 kHz and the rate conversion happens here. 48/16
//! is exactly 3, which keeps both directions to integer arithmetic — the M0+ has
//! no FPU.
//!
//! Both directions share one 27-tap windowed-sinc lowpass (Hamming, 6.8 kHz
//! cutoff, Q15). Going down it is the anti-alias filter; going up it is the
//! anti-imaging filter. Its response, measured against the 48 kHz bus:
//!
//! | 3 kHz | 4 kHz | 6 kHz | 8 kHz | 10 kHz | 16 kHz |
//! |-------|-------|-------|-------|--------|--------|
//! | 0 dB  | -0.1  | -2.8  | -14.7 | -60.3  | -70.3  |
//!
//! 16 kHz is the frequency that folds to DC when decimating by 3, which is why
//! it matters most; -70 dB there is more than voice needs.
//!
//! Cost is about 17k multiply-accumulates per 20 ms in each direction, roughly
//! 6% of one core — the filters are not what limits this design.

use crate::common::intercom::{Frame, FRAME_SAMPLES};

/// 48 kHz I2S : 16 kHz wire.
pub const RATIO: usize = 3;

/// Samples in one 20 ms block on the I2S side.
pub const I2S_FRAME_SAMPLES: usize = FRAME_SAMPLES * RATIO;

const TAPS: usize = 27;

/// Q15 lowpass, unity DC gain (taps sum to 32768).
const FIR: [i16; TAPS] = [
    -54, -77, -45, 102, 320, 366, -38, -840, -1421, -852, 1400, 4847, 8021, 9310, 8021, 4847, 1400,
    -852, -1421, -840, -38, 366, 320, 102, -45, -77, -54,
];

const PHASE_TAPS: usize = TAPS / RATIO;

/// The same filter split into polyphase branches for interpolation: output
/// `RATIO * k + p` is branch `p` applied to input samples `k - 8 ..= k`. Each
/// branch is stored reversed so it can be walked forwards over the history
/// buffer. Each sums to about 32768/3, so the result is scaled by `RATIO` to
/// put the gain back at unity.
const PHASE_FIR: [[i16; PHASE_TAPS]; RATIO] = [
    [-45, 366, -1421, 4847, 8021, -852, -38, 102, -54],
    [-77, 320, -840, 1400, 9310, 1400, -840, 320, -77],
    [-54, 102, -38, -852, 8021, 4847, -1421, 366, -45],
];

fn clamp_q15(acc: i32) -> i16 {
    (acc >> 15).clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

/// 48 kHz in, 16 kHz out.
///
/// Filter history carries across blocks, so a continuous input stream produces a
/// continuous output stream with no seam every 20 ms.
pub struct Decimator {
    /// `[history | this block]`. The leading `TAPS - 1` samples are the tail of
    /// the previous block.
    buf: [i16; TAPS - 1 + I2S_FRAME_SAMPLES],
}

impl Default for Decimator {
    fn default() -> Self {
        Self::new()
    }
}

impl Decimator {
    pub const fn new() -> Self {
        Self {
            buf: [0; TAPS - 1 + I2S_FRAME_SAMPLES],
        }
    }

    pub fn process(&mut self, input: &[i16; I2S_FRAME_SAMPLES], out: &mut Frame) {
        self.buf[TAPS - 1..].copy_from_slice(input);

        // Output k is the filter evaluated over the 27 samples ending at input
        // 3k+2, which sits at buf[TAPS - 1 + 3k + 2].
        for (k, sample) in out.iter_mut().enumerate() {
            let window = &self.buf[3 * k + 2..3 * k + 2 + TAPS];
            let mut acc: i32 = 0;
            for (s, t) in window.iter().zip(FIR.iter()) {
                acc += *s as i32 * *t as i32;
            }
            *sample = clamp_q15(acc);
        }

        self.buf.copy_within(I2S_FRAME_SAMPLES.., 0);
    }
}

/// 16 kHz in, 48 kHz out.
pub struct Interpolator {
    buf: [i16; PHASE_TAPS - 1 + FRAME_SAMPLES],
}

impl Default for Interpolator {
    fn default() -> Self {
        Self::new()
    }
}

impl Interpolator {
    pub const fn new() -> Self {
        Self {
            buf: [0; PHASE_TAPS - 1 + FRAME_SAMPLES],
        }
    }

    pub fn process(&mut self, input: &Frame, out: &mut [i16; I2S_FRAME_SAMPLES]) {
        self.buf[PHASE_TAPS - 1..].copy_from_slice(input);

        for k in 0..FRAME_SAMPLES {
            let window = &self.buf[k..k + PHASE_TAPS];
            for (p, phase) in PHASE_FIR.iter().enumerate() {
                let mut acc: i32 = 0;
                for (s, t) in window.iter().zip(phase.iter()) {
                    acc += *s as i32 * *t as i32;
                }
                out[k * RATIO + p] = clamp_q15(acc * RATIO as i32);
            }
        }

        self.buf.copy_within(FRAME_SAMPLES.., 0);
    }

    /// Drop the filter history. Used when the speaker is muted, so that the
    /// first frame after unmuting is not filtered against audio from before the
    /// gap.
    pub fn reset(&mut self) {
        self.buf.fill(0);
    }
}
