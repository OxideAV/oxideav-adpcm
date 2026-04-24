//! Yamaha ADPCM decoder (`adpcm_yamaha`, WAVEFORMATEX tag `0x0020`).
//!
//! The codec is Yamaha's Y8950 / YM2608 / AICA ADPCM, with the decoder
//! recurrence published in the *Y8950 Application Manual* (MSX-AUDIO),
//! section I-4 "Outline of ADPCM Voice Analysis/Synthesis":
//!
//! ```text
//! X(n+1) = X(n) + (1 - 2·L4) · (L3 + L2/2 + L1/4 + 1/8) · Δ(n)
//! Δ(n+1) = f(L3, L2, L1) · Δ(n)
//! ```
//!
//! where `L4` is the sign bit of the 4-bit nibble and `L3 L2 L1` are the
//! magnitude bits. `f(·)` is Table I-2 of the manual (values 0.9/1.2/1.6/
//! 2.0/2.4, transcribed as int-over-256 multipliers in [`crate::tables`]).
//!
//! There is **no block header**: the decoder starts with predictor 0 and
//! step [`YAMAHA_STEP_MIN`](crate::tables::YAMAHA_STEP_MIN), and decodes
//! nibbles sequentially from the packet. Each byte yields two samples —
//! per the manual and the WAV convention, the **low nibble is decoded
//! first**, then the high nibble. For stereo, channels are
//! sample-interleaved (nibble 0 → L, nibble 1 → R, nibble 2 → L, …).
//!
//! State persists across packets for a stream, unlike MS / IMA-WAV which
//! carry an explicit initial predictor per block.

use crate::tables::{YAMAHA_DIFF_LOOKUP, YAMAHA_INDEX_SCALE, YAMAHA_STEP_MAX, YAMAHA_STEP_MIN};

/// Per-channel running decoder state.
#[derive(Clone, Copy, Debug)]
pub struct Channel {
    pub predictor: i32,
    pub step: i32,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            predictor: 0,
            step: YAMAHA_STEP_MIN,
        }
    }
}

/// Decode a single 4-bit nibble, advancing `state`. Returns the new PCM
/// sample (clamped to i16 range).
#[inline]
pub fn decode_nibble(state: &mut Channel, nibble: u8) -> i16 {
    let mag = (nibble & 7) as usize;
    let sign = (nibble & 8) != 0;

    // Contribution = (diff_lookup[mag] * step) / 8, signed by L4.
    let diff = (YAMAHA_DIFF_LOOKUP[mag] * state.step) >> 3;
    if sign {
        state.predictor -= diff;
    } else {
        state.predictor += diff;
    }
    state.predictor = state.predictor.clamp(i16::MIN as i32, i16::MAX as i32);

    // Step update.
    state.step = (state.step * YAMAHA_INDEX_SCALE[mag]) >> 8;
    state.step = state.step.clamp(YAMAHA_STEP_MIN, YAMAHA_STEP_MAX);

    state.predictor as i16
}

/// Decode a Yamaha-ADPCM packet. `state` is a mutable slice of
/// per-channel state; one entry per channel (so `state.len() == channels`).
/// Samples within a byte are **lo nibble first**, channels interleave at
/// the sample level.
pub fn decode_packet(packet: &[u8], state: &mut [Channel]) -> Vec<i16> {
    let channels = state.len();
    if channels == 0 {
        return Vec::new();
    }
    // Each byte = 2 samples total (one per nibble), distributed over
    // channels round-robin. For the decoded-sample count to come out
    // integer-per-channel we need the packet to carry an even number of
    // nibbles per channel.
    let mut out = Vec::with_capacity(packet.len() * 2);
    let mut ch_cursor = 0usize;
    for &byte in packet {
        let lo = byte & 0x0F;
        let hi = (byte >> 4) & 0x0F;

        let s = decode_nibble(&mut state[ch_cursor], lo);
        out.push(s);
        ch_cursor = (ch_cursor + 1) % channels;

        let s = decode_nibble(&mut state[ch_cursor], hi);
        out.push(s);
        ch_cursor = (ch_cursor + 1) % channels;
    }
    // Reorder round-robin nibble output into strict L-R-L-R interleave.
    // In the loop above, with channels=2 we emit L R L R ... already
    // because the cursor advances one per nibble. For channels=1 the
    // ordering is trivially correct.
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_nibble_positive_moves_predictor_up_by_step_over_8() {
        let mut st = Channel::default();
        // Nibble 0: mag=0 → diff = (1 * step)/8 = step/8, sign positive.
        let _ = decode_nibble(&mut st, 0);
        assert!(st.predictor > 0);
        // Step should shrink slightly (0.9).
        assert!(st.step < YAMAHA_STEP_MIN * 2);
        assert!(st.step >= YAMAHA_STEP_MIN);
    }

    #[test]
    fn sign_bit_flips_direction() {
        let mut a = Channel::default();
        let mut b = Channel::default();
        // Same magnitude nibble, opposite signs.
        let sa = decode_nibble(&mut a, 5);
        let sb = decode_nibble(&mut b, 0x5 | 0x8);
        assert_eq!(sa.saturating_add(sb), 0);
    }

    #[test]
    fn step_size_stays_in_spec_range() {
        let mut st = Channel::default();
        for _ in 0..1000 {
            // Max magnitude nibble = 7, which has index_scale = 614.
            let _ = decode_nibble(&mut st, 7);
            assert!(st.step >= YAMAHA_STEP_MIN);
            assert!(st.step <= YAMAHA_STEP_MAX);
        }
    }

    #[test]
    fn predictor_clamps_to_i16() {
        let mut st = Channel::default();
        for _ in 0..1000 {
            let _ = decode_nibble(&mut st, 7);
        }
        assert!(st.predictor <= i16::MAX as i32);
        assert!(st.predictor >= i16::MIN as i32);
    }

    #[test]
    fn packet_decode_mono_emits_two_samples_per_byte() {
        let mut st = [Channel::default()];
        let pcm = decode_packet(&[0x00, 0x00, 0x00], &mut st);
        assert_eq!(pcm.len(), 6);
    }
}
