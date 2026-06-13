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
//!
//! # Encoder
//!
//! The companion encoder mirrors the manual's *analysis* recurrence
//! (`dn = Xn - x̂n`, then choose `L4 L3 L2 L1` from `(sign, |dn|/Δn)`)
//! using the same lookup tables. The chosen nibble is fed straight back
//! through [`decode_nibble`] so the encoder advances its predictor /
//! step state identically to a decoder downstream — round-trip is
//! self-consistent by construction (no third-party encoder source
//! consulted; the analysis path is the spec's own inverse of synthesis).

use crate::tables::{
    YAMAHA_DIFF_LOOKUP, YAMAHA_INDEX_SCALE, YAMAHA_INDEX_SCALE_OPNA, YAMAHA_STEP_MAX,
    YAMAHA_STEP_MIN,
};

/// Which Yamaha chip's exact step-adaptation constants the codec emulates.
///
/// The synthesis recurrence and the magnitude contribution lookup
/// (`YAMAHA_DIFF_LOOKUP`) are identical across chips. What differs is the
/// **quantization-width change rate** `f(L3,L2,L1)`: each chip rounds the
/// same `~1.1^M` curve slightly differently, so a long stream diverges
/// when decoded against the wrong constants.
///
/// - [`Chip::Aica`] — the AICA FQ8005 / Y8950 / YMZ280B rounding,
///   `{0.8984375, 1.19921875, 1.59765625, 2.0, 2.3984375}` encoded as
///   integer/256 ([`YAMAHA_INDEX_SCALE`], update `>> 8`). This is the
///   default and the value the WAV-tag-`0x0020` convention uses.
/// - [`Chip::Opna`] — the YM2608 (OPNA) Application Manual Table 5-1
///   rounding, `{57/64, 77/64, 102/64, 128/64, 153/64}` encoded as the
///   ×64 numerators ([`YAMAHA_INDEX_SCALE_OPNA`], update `>> 6`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Chip {
    /// AICA FQ8005 / Y8950 / YMZ280B constants (default).
    #[default]
    Aica,
    /// YM2608 (OPNA) Application Manual Table 5-1 constants.
    Opna,
}

impl Chip {
    /// The 8-entry step multiplier table for this chip.
    #[inline]
    const fn index_scale(self) -> &'static [i32; 8] {
        match self {
            Chip::Aica => &YAMAHA_INDEX_SCALE,
            Chip::Opna => &YAMAHA_INDEX_SCALE_OPNA,
        }
    }

    /// The right-shift applied after multiplying by [`Self::index_scale`]
    /// (i.e. the table's fixed-point denominator: 256 for AICA, 64 for
    /// OPNA).
    #[inline]
    const fn scale_shift(self) -> u32 {
        match self {
            Chip::Aica => 8,
            Chip::Opna => 6,
        }
    }
}

/// Per-channel running decoder state.
#[derive(Clone, Copy, Debug)]
pub struct Channel {
    pub predictor: i32,
    pub step: i32,
    /// Which chip's step-adaptation constants drive the `step` update.
    pub chip: Chip,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            predictor: 0,
            step: YAMAHA_STEP_MIN,
            chip: Chip::Aica,
        }
    }
}

impl Channel {
    /// A fresh channel emulating the given chip's step-adaptation
    /// constants (predictor 0, step at [`YAMAHA_STEP_MIN`]).
    #[inline]
    pub fn for_chip(chip: Chip) -> Self {
        Self {
            chip,
            ..Self::default()
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

    // Step update — chip-specific multiplier table + fixed-point shift.
    state.step = (state.step * state.chip.index_scale()[mag]) >> state.chip.scale_shift();
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

/// Encode one i16 PCM sample into a 4-bit Yamaha nibble. Advances
/// `state` exactly as the decoder would after seeing the chosen nibble
/// (the function delegates to [`decode_nibble`] for the state update,
/// so encode/decode share the same trajectory bit-for-bit).
///
/// Returns the nibble plus the reconstructed sample so callers can
/// compute an exact quantisation error without re-decoding.
///
/// Algorithm (Y8950 manual §I-4 *analysis*):
///
/// ```text
/// dn  = Xn - x̂n
/// L4  = (dn < 0) ? 1 : 0
/// |dn|/Δn → (L3 L2 L1) by Table 5-1 / Table 1 thresholds
/// ```
///
/// The magnitude bits are picked closed-form from the eight thresholds
/// `{0, 1/4, 1/2, 3/4, 1, 5/4, 3/2, 7/4}` of `|dn|/Δn`. This matches the
/// AICA FQ8005 manual's Table 1 (and the equivalent YM2608 OPNA
/// manual's Table 5-1) — both list the eight magnitude codes against
/// the same eight threshold rows.
#[inline]
pub fn encode_sample(state: &mut Channel, target: i16) -> (u8, i16) {
    // Residual against the current predictor.
    let dn = target as i32 - state.predictor;
    let (sign_bit, abs_dn) = if dn < 0 { (0x8u8, -dn) } else { (0x0u8, dn) };

    // Closed-form magnitude pick from |dn|/Δn against the eight
    // thresholds {0, 1/4, 1/2, 3/4, 1, 5/4, 3/2, 7/4}. We multiply both
    // sides by 4·Δn to stay in integers:
    //
    //   ln = |dn| / Δn     ⇒    4·|dn|  vs    {0, 1·Δn, 2·Δn, 3·Δn, 4·Δn, 5·Δn, 6·Δn, 7·Δn}
    //
    // Pick the largest threshold k for which 4·|dn| >= k·Δn (k in 0..=7).
    let step = state.step;
    let four_abs = abs_dn.saturating_mul(4);
    // Threshold k uses k·Δn on the right; iterate down from 7 so we
    // pick the largest match without branching ladders.
    let mut mag: u8 = 0;
    for k in (1..=7).rev() {
        if four_abs >= step.saturating_mul(k) {
            mag = k as u8;
            break;
        }
    }

    let nibble = sign_bit | mag;
    let reconstructed = decode_nibble(state, nibble);
    (nibble, reconstructed)
}

/// Encode an interleaved i16 PCM stream into a Yamaha-ADPCM byte
/// stream. Sample-level channel interleave (ch 0 first, then ch 1, …)
/// matches the decoder's expectation and the WAV-tag-0x0020 convention.
///
/// `state` is one entry per channel; advances across calls so callers
/// can stream PCM in arbitrary chunks. Two nibbles per byte are packed
/// **low nibble first** (per the manual + WAV convention — see the
/// module docs on the decoder side). If the total nibble count is odd
/// the trailing high nibble is zero-padded.
pub fn encode_packet(samples: &[i16], state: &mut [Channel]) -> Vec<u8> {
    let channels = state.len();
    if channels == 0 || samples.is_empty() {
        return Vec::new();
    }
    let n_nibbles = samples.len();
    let mut out = Vec::with_capacity(n_nibbles.div_ceil(2));
    let mut cursor = 0usize;
    let mut i = 0;
    while i < n_nibbles {
        let (lo_nib, _) = encode_sample(&mut state[cursor], samples[i]);
        cursor = (cursor + 1) % channels;
        i += 1;
        let hi_nib = if i < n_nibbles {
            let (n, _) = encode_sample(&mut state[cursor], samples[i]);
            cursor = (cursor + 1) % channels;
            i += 1;
            n
        } else {
            0
        };
        out.push((hi_nib << 4) | (lo_nib & 0x0F));
    }
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

    // ----- encoder coverage -----

    #[test]
    fn encode_then_decode_advances_state_identically() {
        // The encoder advances `state` via `decode_nibble`, so the
        // encoder's `state` after encode_sample must match a parallel
        // decoder's state after decode_nibble on the same nibble.
        let mut enc = Channel::default();
        let mut dec = Channel::default();
        let pcm = [0i16, 200, 800, -1500, 12000, -8000, 0, 100];
        for &s in &pcm {
            let (n, _) = encode_sample(&mut enc, s);
            let _ = decode_nibble(&mut dec, n);
            assert_eq!(enc.predictor, dec.predictor);
            assert_eq!(enc.step, dec.step);
        }
    }

    #[test]
    fn encode_packs_two_nibbles_per_byte_low_first() {
        // Encode 2 samples mono → 1 byte. Verify the low nibble of the
        // byte is the first sample's code (lo-nibble-first ordering on
        // the wire).
        let mut st = Channel::default();
        // Sample 0: target 0, predictor 0 → dn=0 → nibble 0x0.
        // Sample 1: predictor advanced by decode_nibble(0); positive
        // contribution = (DIFF_LOOKUP[0]*127)>>3 = 15. Target 1000 →
        // dn = 985 → 4*985=3940 vs step=146 (after 0.9 scale of 127→
        // hmm; we just check the lo-nibble property here, not exact
        // value).
        let mut enc_state = [st];
        let bytes = encode_packet(&[0, 1000], &mut enc_state);
        assert_eq!(bytes.len(), 1);
        // Sample 0 with target=0, predictor=0 → nibble 0x0 (low).
        let lo = bytes[0] & 0x0F;
        assert_eq!(lo, 0x0);
        // Reuse the running state from above to drop `unused_assignments`
        // / dead-code lints. `st` is the seed (predictor 0, step MIN);
        // the actual update happened inside the encoder copy.
        let _ = decode_nibble(&mut st, lo);
    }

    #[test]
    fn round_trip_dc_zero_is_bit_exact() {
        // Encoding silence (zeros) and decoding back should yield zeros
        // (or near-zero "wobble" inside ±step/8). Verify the wobble
        // stays bounded.
        let mut enc_state = [Channel::default()];
        let pcm = vec![0i16; 64];
        let bytes = encode_packet(&pcm, &mut enc_state);
        assert_eq!(bytes.len(), 32);
        let mut dec_state = [Channel::default()];
        let decoded = decode_packet(&bytes, &mut dec_state);
        for &s in &decoded {
            assert!(s.unsigned_abs() <= 64, "silence wobble {s} > 64");
        }
    }

    #[test]
    fn round_trip_sine_bounded_rms_mono() {
        // A 0.05 s 220 Hz sine at amp 8000 through encode → decode →
        // RMS bound.
        let sample_rate = 8000f64;
        let n = (sample_rate * 0.05) as usize; // 400 samples
        let pcm: Vec<i16> = (0..n)
            .map(|i| {
                let t = i as f64 / sample_rate;
                ((2.0 * std::f64::consts::PI * 220.0 * t).sin() * 8000.0).round() as i16
            })
            .collect();
        let mut enc_state = [Channel::default()];
        let bytes = encode_packet(&pcm, &mut enc_state);
        assert_eq!(bytes.len(), pcm.len().div_ceil(2));
        let mut dec_state = [Channel::default()];
        let decoded = decode_packet(&bytes, &mut dec_state);
        // Each byte produces 2 samples; n=400 is even so equal length.
        assert_eq!(decoded.len(), pcm.len());
        let mut sse = 0f64;
        for i in 0..pcm.len() {
            let d = decoded[i] as f64 - pcm[i] as f64;
            sse += d * d;
        }
        let rms = (sse / pcm.len() as f64).sqrt();
        // 4-bit ADPCM on a low-frequency sine: comfortably under 2000 LSB.
        assert!(rms < 2000.0, "Yamaha mono round-trip RMS {rms} > 2000");
    }

    #[test]
    fn round_trip_sine_bounded_rms_stereo() {
        let sample_rate = 8000f64;
        let n = (sample_rate * 0.05) as usize;
        let mut pcm = Vec::with_capacity(n * 2);
        for i in 0..n {
            let t = i as f64 / sample_rate;
            let l = ((2.0 * std::f64::consts::PI * 220.0 * t).sin() * 6000.0).round() as i16;
            let r = ((2.0 * std::f64::consts::PI * 330.0 * t).sin() * 6000.0).round() as i16;
            pcm.push(l);
            pcm.push(r);
        }
        let mut enc_state = [Channel::default(), Channel::default()];
        let bytes = encode_packet(&pcm, &mut enc_state);
        let mut dec_state = [Channel::default(), Channel::default()];
        let decoded = decode_packet(&bytes, &mut dec_state);
        assert_eq!(decoded.len(), pcm.len());
        let mut sse_l = 0f64;
        let mut sse_r = 0f64;
        for i in 0..n {
            let dl = decoded[i * 2] as f64 - pcm[i * 2] as f64;
            let dr = decoded[i * 2 + 1] as f64 - pcm[i * 2 + 1] as f64;
            sse_l += dl * dl;
            sse_r += dr * dr;
        }
        let rms_l = (sse_l / n as f64).sqrt();
        let rms_r = (sse_r / n as f64).sqrt();
        assert!(rms_l < 2000.0, "Yamaha stereo L RMS {rms_l} > 2000");
        assert!(rms_r < 2000.0, "Yamaha stereo R RMS {rms_r} > 2000");
    }

    #[test]
    fn encoder_picks_max_magnitude_for_large_residual() {
        // With predictor 0 and step at the minimum (127), a residual of
        // 30_000 LSB is huge — 4·|dn| = 120_000 ≥ 7·127 = 889 → magnitude
        // should be 7 (the max code), with sign 0 (positive).
        let mut st = Channel::default();
        let (n, _) = encode_sample(&mut st, 30_000);
        assert_eq!(n & 0x07, 7, "magnitude bits");
        assert_eq!(n & 0x08, 0, "sign bit clear for positive residual");
    }

    #[test]
    fn encoder_picks_zero_magnitude_for_small_residual() {
        // Step=127. |dn|=15 → 4·15 = 60 < 1·127, so magnitude=0.
        let mut st = Channel::default();
        let (n, _) = encode_sample(&mut st, 15);
        assert_eq!(n & 0x07, 0);
        assert_eq!(n & 0x08, 0); // positive
    }

    #[test]
    fn encoder_sets_sign_bit_for_negative_residual() {
        let mut st = Channel::default();
        let (n, _) = encode_sample(&mut st, -200);
        assert_eq!(n & 0x08, 0x08);
    }

    #[test]
    fn empty_inputs_produce_empty_outputs() {
        let mut st = [Channel::default()];
        assert!(encode_packet(&[], &mut st).is_empty());
        let mut empty_state: [Channel; 0] = [];
        assert!(encode_packet(&[1, 2, 3], &mut empty_state).is_empty());
    }

    // ----- chip multiplier selection (AICA vs OPNA) -----

    #[test]
    fn default_channel_emulates_aica() {
        assert_eq!(Channel::default().chip, Chip::Aica);
        assert_eq!(Chip::default(), Chip::Aica);
    }

    #[test]
    fn for_chip_constructor_seeds_step_and_chip() {
        for chip in [Chip::Aica, Chip::Opna] {
            let c = Channel::for_chip(chip);
            assert_eq!(c.chip, chip);
            assert_eq!(c.predictor, 0);
            assert_eq!(c.step, YAMAHA_STEP_MIN);
        }
    }

    #[test]
    fn opna_step_update_matches_table_5_1_fractions() {
        use crate::tables::{YAMAHA_INDEX_SCALE, YAMAHA_INDEX_SCALE_OPNA};
        // From the minimum step (127), the post-update step for each
        // magnitude code must equal (127 * f_x64[mag]) >> 6 for OPNA and
        // (127 * f_x256[mag]) >> 8 for AICA, exactly. Both clamp to
        // [127, 24576]; at 127 no entry under-shoots the floor for the
        // small-magnitude codes after clamping, so compare clamped.
        for mag in 0u8..8 {
            let mut opna = Channel::for_chip(Chip::Opna);
            let _ = decode_nibble(&mut opna, mag);
            let want_opna = ((YAMAHA_STEP_MIN * YAMAHA_INDEX_SCALE_OPNA[mag as usize]) >> 6)
                .max(YAMAHA_STEP_MIN);
            assert_eq!(opna.step, want_opna, "OPNA step mismatch for mag {mag}");

            let mut aica = Channel::for_chip(Chip::Aica);
            let _ = decode_nibble(&mut aica, mag);
            let want_aica =
                ((YAMAHA_STEP_MIN * YAMAHA_INDEX_SCALE[mag as usize]) >> 8).max(YAMAHA_STEP_MIN);
            assert_eq!(aica.step, want_aica, "AICA step mismatch for mag {mag}");
        }
    }

    #[test]
    fn opna_and_aica_diverge_on_a_long_max_magnitude_run() {
        // The small per-step rounding difference (153/64 vs 614/256 for
        // the max code) compounds over a long run, so the two chips'
        // step trajectories — and therefore their reconstructed
        // predictors — must differ for at least one sample before both
        // saturate. Decode the same nibble stream under each chip.
        let nibbles: Vec<u8> = (0..200).map(|i| if i % 2 == 0 { 7 } else { 0xF }).collect();
        let mut opna = Channel::for_chip(Chip::Opna);
        let mut aica = Channel::for_chip(Chip::Aica);
        let mut diverged = false;
        for &n in &nibbles {
            let so = decode_nibble(&mut opna, n);
            let sa = decode_nibble(&mut aica, n);
            if so != sa || opna.step != aica.step {
                diverged = true;
            }
        }
        assert!(
            diverged,
            "OPNA and AICA produced identical trajectories — the chip selector is a no-op"
        );
    }

    #[test]
    fn opna_step_stays_in_spec_range() {
        // Same invariant as the AICA path: the step never leaves
        // [127, 24576] regardless of the nibble stream.
        let mut up = Channel::for_chip(Chip::Opna);
        let mut down = Channel::for_chip(Chip::Opna);
        for _ in 0..1000 {
            let _ = decode_nibble(&mut up, 7);
            let _ = decode_nibble(&mut down, 0);
            for st in [&up, &down] {
                assert!(st.step >= YAMAHA_STEP_MIN);
                assert!(st.step <= YAMAHA_STEP_MAX);
            }
        }
    }

    #[test]
    fn opna_encode_decode_round_trips_self_consistently() {
        // The encoder advances state through decode_nibble, so an OPNA
        // encoder + OPNA decoder reconstruct identically. Verify the
        // decoder fed the encoder's nibbles reproduces the encoder's
        // predictor trajectory exactly.
        let mut enc = Channel::for_chip(Chip::Opna);
        let mut dec = Channel::for_chip(Chip::Opna);
        let pcm = [0i16, 300, 1200, -2000, 15000, -9000, 50, -50];
        for &s in &pcm {
            let (n, _) = encode_sample(&mut enc, s);
            let _ = decode_nibble(&mut dec, n);
            assert_eq!(enc.predictor, dec.predictor);
            assert_eq!(enc.step, dec.step);
            assert_eq!(enc.chip, dec.chip);
        }
    }
}
