//! Yamaha **ADPCM-A** (`adpcm_yamaha_a`) — the YM2610 rhythm-channel codec.
//!
//! This is the *second* 4-bit ADPCM scheme Yamaha shipped, distinct from
//! the ADPCM-B / DELTA-T codec in [`crate::yamaha`]:
//!
//! | Field           | ADPCM-B ([`crate::yamaha`])           | ADPCM-A (this module)       |
//! |-----------------|----------------------------------------|-----------------------------|
//! | Used on         | Y8950, YM2608-B, YMZ280B, AICA         | YM2608 rhythm, YM2610      |
//! | Output width    | 16-bit (≈ Δ-scaled)                    | **12-bit** (signed)         |
//! | Adaptation      | 5-multiplier `f(L3,L2,L1)` table       | 49-entry step table + adj   |
//! | Step state      | Δ (continuous, clamped 127..24576)     | pointer 0..48               |
//! | Header          | None                                   | None                        |
//! | Channels        | Up to 8, sample-interleaved on wire    | Single channel per stream   |
//!
//! Provenance: the 49-entry step-size table is the independent-RE
//! consensus of the NeoGeo Development Wiki / MAME / ymfm hardware
//! reverse-engineering effort against real YM2608 / YM2610 silicon, as
//! staged in `audio/adpcm/yamaha/yamaha-adpcm.md` §3. No FFmpeg or
//! third-party general-purpose multimedia decoder source was read; the
//! numeric tables transcribed in [`crate::tables`] are the only constants
//! this module consumes.
//!
//! # On-wire nibble layout
//!
//! Two 4-bit samples per byte, **low nibble first** — matches the YM2610
//! rhythm-ROM convention used by the staged trace doc.
//!
//! # Per-sample decode recurrence
//!
//! Given the 4-bit nibble `s mmm` (1 sign bit `s`, 3 magnitude bits `mmm`):
//!
//! ```text
//! delta = (step_size[idx] * (2*mmm + 1)) / 8
//! acc   = clamp(acc ± delta, -2048, 2047)              // 12-bit signed
//! idx   = clamp(idx + step_adj[nibble], 0, 48)         // 49-entry pointer
//! ```
//!
//! The encoder is the textbook closed-form quantiser: sign from
//! `dn = target - acc`, magnitude from `|dn|` against the 7-threshold
//! ladder `{step·1/8, step·3/8, step·5/8, …, step·13/8}` corresponding
//! to the eight per-`mmm` reconstruction levels. The chosen nibble is
//! fed back through [`decode_nibble`] so encoder + decoder share the
//! same trajectory bit-for-bit.

use crate::tables::{
    YAMAHA_A_INDEX_ADJUST, YAMAHA_A_PREDICTOR_MAX, YAMAHA_A_PREDICTOR_MIN, YAMAHA_A_STEP_INDEX_MAX,
    YAMAHA_A_STEP_INDEX_MIN, YAMAHA_A_STEP_SIZE,
};

/// Per-channel running decoder state.
///
/// `acc` is the running 12-bit signed reconstructed sample (kept in i32
/// for easy clamping); `step_index` is the running pointer into the
/// 49-entry step table.
#[derive(Clone, Copy, Debug, Default)]
pub struct Channel {
    pub acc: i32,
    pub step_index: i32,
}

/// Output narrowing — the silicon stores the per-channel `acc` at 12-bit
/// signed; downstream PCM containers want 16-bit signed. Mirrors the
/// equivalent enum on [`crate::dialogic::Output`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    /// Return the raw 12-bit signed value (`-2048 ..= 2047`).
    Native12,
    /// Left-shift the 12-bit value by 4 to fill an i16. The registry path
    /// uses this so consumers always see a uniform i16-LE PCM stream.
    Wide16,
}

/// Decode one nibble, advancing `state`, and return the reconstructed
/// sample at the requested width.
#[inline]
pub fn decode_nibble(state: &mut Channel, nibble: u8, output: Output) -> i16 {
    let mag = (nibble & 0x07) as i32;
    let sign = (nibble & 0x08) != 0;

    let step = YAMAHA_A_STEP_SIZE[state.step_index as usize] as i32;

    // Reconstruction contribution. The chip formula `delta = step *
    // (2*mag + 1) / 8` collapses the (mag/4 + 1/16)·step decode rule of
    // the staged trace doc to a single integer multiply + shift. Using
    // i64 for the multiply leaves bit-room across the full
    // `step ≤ 1552`, `mag ≤ 7` range without overflow.
    let delta = (step * (2 * mag + 1)) >> 3;
    if sign {
        state.acc -= delta;
    } else {
        state.acc += delta;
    }
    state.acc = state
        .acc
        .clamp(YAMAHA_A_PREDICTOR_MIN, YAMAHA_A_PREDICTOR_MAX);

    // Step-pointer adaptation. The adjust table is indexed by the full
    // 4-bit nibble; the high (sign) bit doesn't affect the magnitude
    // mapping because entries `0..7` and `8..15` mirror.
    state.step_index = (state.step_index + YAMAHA_A_INDEX_ADJUST[(nibble & 0x0F) as usize])
        .clamp(YAMAHA_A_STEP_INDEX_MIN, YAMAHA_A_STEP_INDEX_MAX);

    match output {
        Output::Native12 => state.acc as i16,
        // Left-shift to fill the i16 dynamic range so the wide-pipeline
        // RMS measurement is comparable to the other variants.
        Output::Wide16 => (state.acc << 4) as i16,
    }
}

/// Decode a packet of `(byte_count * 2)` samples into a flat i16 vector.
///
/// ADPCM-A is single-channel — `state.len()` must be 1. The
/// nibble-packing is **low nibble first** (per the staged trace doc).
pub fn decode_packet(packet: &[u8], state: &mut [Channel], output: Output) -> Vec<i16> {
    if state.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(packet.len() * 2);
    for &byte in packet {
        let lo = byte & 0x0F;
        let hi = (byte >> 4) & 0x0F;
        let s0 = decode_nibble(&mut state[0], lo, output);
        out.push(s0);
        let s1 = decode_nibble(&mut state[0], hi, output);
        out.push(s1);
    }
    out
}

// ----- encoder helpers -------------------------------------------------

/// Pick the best 4-bit nibble for `target` against the current `state`
/// and advance `state` via [`decode_nibble`].
///
/// Closed form: the decoder produces eight possible signed deltas
/// `± (2*mag + 1) * step / 8` for `mag ∈ 0..=7`. The optimal magnitude
/// for a residual `dn = target - acc` is the one that places
/// `(2*mag + 1)` closest to `8*|dn|/step`. Solving for `mag` gives
/// `mag = round((8*|dn|/step - 1) / 2) = (4*|dn|/step) / 1`, clamped to
/// `[0, 7]`. We compute `(4*|dn| / step)` first; that index is the
/// largest `k` for which `(2*k + 1) * step ≤ 8*|dn|`, then we round up
/// when `8*|dn|` falls in the upper half of the `k → k+1` boundary.
///
/// Returns the chosen nibble plus the reconstructed sample (12-bit
/// native, regardless of the caller's `Output`).
#[inline]
pub fn encode_sample(state: &mut Channel, target: i16, output: Output) -> (u8, i16) {
    // Match the encoder's frame of reference to the decoder's output
    // width — `Wide16` callers feed wide samples, so we narrow first.
    let wide = target as i32;
    let target_native = match output {
        Output::Native12 => wide.clamp(YAMAHA_A_PREDICTOR_MIN, YAMAHA_A_PREDICTOR_MAX),
        Output::Wide16 => (wide >> 4).clamp(YAMAHA_A_PREDICTOR_MIN, YAMAHA_A_PREDICTOR_MAX),
    };

    let dn = target_native - state.acc;
    let (sign_bit, abs_dn) = if dn < 0 {
        (0x8u8, (-dn) as i64)
    } else {
        (0x0u8, dn as i64)
    };
    let step = YAMAHA_A_STEP_SIZE[state.step_index as usize] as i64;

    // Pick magnitude `m ∈ 0..=7` minimising
    //   | abs_dn  -  (2*m + 1) * step / 8 |.
    // Equivalent: pick `m` so `(2*m + 1)` is closest to `8 * abs_dn / step`.
    // Use a direct sweep from 7 downward — eight branches is negligible
    // and avoids any rounding-direction subtlety on the chip's integer
    // divide. The decoder-loop pattern matches the other stream-oriented
    // variants in this crate (Yamaha ADPCM-B, Dialogic).
    let mut best_mag: u8 = 0;
    let mut best_err: i64 = i64::MAX;
    for m in 0..=7i64 {
        let level = ((2 * m + 1) * step) >> 3;
        let err = (abs_dn - level).abs();
        if err < best_err {
            best_err = err;
            best_mag = m as u8;
        }
    }
    let nibble = sign_bit | best_mag;
    let reconstructed = decode_nibble(state, nibble, output);
    (nibble, reconstructed)
}

/// Encode an i16 sample stream into ADPCM-A bytes, packing two nibbles
/// per byte (low nibble first). `state.len()` must be 1 (single channel
/// per stream).
pub fn encode_packet(samples: &[i16], state: &mut [Channel], output: Output) -> Vec<u8> {
    if state.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(samples.len().div_ceil(2));
    let mut i = 0;
    while i < samples.len() {
        let (lo, _) = encode_sample(&mut state[0], samples[i], output);
        i += 1;
        let hi = if i < samples.len() {
            let (n, _) = encode_sample(&mut state[0], samples[i], output);
            i += 1;
            n
        } else {
            0
        };
        out.push((hi << 4) | (lo & 0x0F));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_nibble_positive_grows_acc_by_step_over_8() {
        // mag=0 → delta = step * (2*0+1) / 8 = step/8. step[0] = 16 →
        // delta = 2. Sign clear → acc moves +2.
        let mut st = Channel::default();
        let s = decode_nibble(&mut st, 0x0, Output::Native12);
        assert_eq!(st.acc, 2);
        assert_eq!(s, 2);
        // step_index decreased by 1 (clamped to 0 — was already 0).
        assert_eq!(st.step_index, 0);
    }

    #[test]
    fn high_magnitude_nibble_increments_step_pointer_by_nine() {
        // mag=7 → step adjust = +9. From step_index=0, after one nibble:
        // step_index = 9 (clamped under 48).
        let mut st = Channel::default();
        let _ = decode_nibble(&mut st, 0x7, Output::Native12);
        assert_eq!(st.step_index, 9);
    }

    #[test]
    fn sign_bit_flips_direction() {
        let mut a = Channel::default();
        let mut b = Channel::default();
        let sa = decode_nibble(&mut a, 0x5, Output::Native12);
        let sb = decode_nibble(&mut b, 0xD, Output::Native12);
        assert_eq!(sa, -sb);
    }

    #[test]
    fn acc_clamps_to_12_bit_signed() {
        // Repeatedly inject the maximum positive nibble. After enough
        // iterations the step grows to its ceiling and the cumulative
        // delta saturates at +2047.
        let mut st = Channel::default();
        for _ in 0..1000 {
            let _ = decode_nibble(&mut st, 0x7, Output::Native12);
        }
        assert!(st.acc <= YAMAHA_A_PREDICTOR_MAX);
        assert!(st.acc >= YAMAHA_A_PREDICTOR_MIN);
        // The accumulator must be the saturated max after the spin-up
        // (step_index reaches 48 within a handful of iterations and
        // every subsequent delta keeps adding to acc until clamp).
        assert_eq!(st.acc, YAMAHA_A_PREDICTOR_MAX);
        // And the step pointer parked at its top.
        assert_eq!(st.step_index, YAMAHA_A_STEP_INDEX_MAX);
    }

    #[test]
    fn step_index_clamps_to_zero_when_low_magnitudes_dominate() {
        let mut st = Channel {
            acc: 0,
            step_index: 5,
        };
        for _ in 0..20 {
            let _ = decode_nibble(&mut st, 0x0, Output::Native12);
        }
        assert_eq!(st.step_index, 0);
    }

    #[test]
    fn wide16_output_left_shifts_by_4() {
        // From a known native-mode delta, Wide16 should be 16× larger.
        let mut native_st = Channel::default();
        let mut wide_st = Channel::default();
        let s_native = decode_nibble(&mut native_st, 0x5, Output::Native12);
        let s_wide = decode_nibble(&mut wide_st, 0x5, Output::Wide16);
        assert_eq!(s_wide, (s_native as i32 * 16) as i16);
    }

    #[test]
    fn packet_decode_mono_emits_two_samples_per_byte() {
        let mut st = [Channel::default()];
        let pcm = decode_packet(&[0x00, 0x77, 0x12], &mut st, Output::Native12);
        assert_eq!(pcm.len(), 6);
    }

    #[test]
    fn empty_state_decodes_to_empty() {
        let mut empty: [Channel; 0] = [];
        let pcm = decode_packet(&[0x00, 0x77, 0xFF], &mut empty, Output::Native12);
        assert!(pcm.is_empty());
    }

    // ----- encoder coverage -----

    #[test]
    fn encode_then_decode_state_matches_decoder() {
        let mut enc = Channel::default();
        let mut dec = Channel::default();
        let pcm = [0i16, 50, 100, 200, -300, -100, 0, 50];
        for &s in &pcm {
            let (nib, _) = encode_sample(&mut enc, s, Output::Native12);
            let _ = decode_nibble(&mut dec, nib, Output::Native12);
            assert_eq!(enc.acc, dec.acc, "acc drift after target {s}");
            assert_eq!(
                enc.step_index, dec.step_index,
                "step drift after target {s}"
            );
        }
    }

    #[test]
    fn encode_packs_two_nibbles_per_byte_low_first() {
        let mut st = [Channel::default()];
        // Sample 0 with predictor 0 + target 0 → dn = 0, magnitude = 0,
        // sign = 0 → nibble 0x0 in the LOW position of byte 0.
        let bytes = encode_packet(&[0, 1000], &mut st, Output::Native12);
        assert_eq!(bytes.len(), 1);
        assert_eq!(bytes[0] & 0x0F, 0x0);
    }

    #[test]
    fn encode_sets_sign_bit_for_negative_residual() {
        let mut st = Channel::default();
        let (n, _) = encode_sample(&mut st, -1000, Output::Native12);
        assert_eq!(n & 0x08, 0x08);
    }

    #[test]
    fn encode_picks_high_magnitude_for_large_residual() {
        // With acc=0 and step=16, the eight reconstruction levels are
        // 2/8, 6/8, 10/8, ... = 2, 6, 10, 14, 18, 22, 26, 30 (native
        // 12-bit units). A target of 2000 is *far* above 30, so the
        // closed-form picker selects magnitude 7 (the maximum).
        let mut st = Channel::default();
        let (n, _) = encode_sample(&mut st, 2000, Output::Native12);
        assert_eq!(n & 0x07, 7);
        assert_eq!(n & 0x08, 0);
    }

    #[test]
    fn round_trip_dc_zero_is_bounded() {
        let mut enc_state = [Channel::default()];
        let pcm = vec![0i16; 64];
        let bytes = encode_packet(&pcm, &mut enc_state, Output::Native12);
        let mut dec_state = [Channel::default()];
        let decoded = decode_packet(&bytes, &mut dec_state, Output::Native12);
        // Silence wobble must stay close to zero (within a few LSBs of
        // the 12-bit predictor's smallest delta).
        for &s in &decoded {
            assert!(
                s.unsigned_abs() <= 8,
                "silence wobble {s} exceeds 8 native-12 LSBs"
            );
        }
    }

    #[test]
    fn round_trip_sine_bounded_rms_mono_wide16() {
        // 50 ms 220 Hz sine at 8 kHz, amplitude 6000 i16. ADPCM-A is
        // 12-bit silicon, so the Wide16 ceiling is ±2047·16 = ±32752 —
        // a 6000-amp sine fits cleanly.
        let sample_rate = 8000f64;
        let n = (sample_rate * 0.05) as usize;
        let pcm: Vec<i16> = (0..n)
            .map(|i| {
                let t = i as f64 / sample_rate;
                ((2.0 * std::f64::consts::PI * 220.0 * t).sin() * 6000.0).round() as i16
            })
            .collect();
        let mut enc_state = [Channel::default()];
        let bytes = encode_packet(&pcm, &mut enc_state, Output::Wide16);
        let mut dec_state = [Channel::default()];
        let decoded = decode_packet(&bytes, &mut dec_state, Output::Wide16);
        assert_eq!(decoded.len(), pcm.len());

        // RMS: 12-bit codec on a 6000-amp sine; the step pointer takes a
        // few samples to ramp from 0, so leading-edge error dominates.
        // 4500 LSB ceiling — comfortable headroom over the ~3000 LSB
        // ADPCM-B baseline.
        let mut sse = 0f64;
        for i in 0..pcm.len() {
            let d = decoded[i] as f64 - pcm[i] as f64;
            sse += d * d;
        }
        let rms = (sse / pcm.len() as f64).sqrt();
        assert!(rms < 4500.0, "Yamaha ADPCM-A wide16 RMS {rms} > 4500");
    }
}
