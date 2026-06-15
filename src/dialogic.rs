//! OKI / Dialogic ADPCM (VOX) decoder + encoder.
//!
//! VOX is the file format of the OKI MSM6258 / MSM6295 family used by
//! Dialogic voice-processing telephony hardware and a substantial body of
//! retro-computing / arcade audio. The codec is **headerless**: a `.vox`
//! file is a flat stream of 4-bit ADPCM samples, two samples per byte,
//! with the sampling rate (commonly 6 kHz or 8 kHz) supplied out of band.
//!
//! # Algorithm (Dialogic app note 00-1366-001, §2)
//!
//! Per sample, given the current step size `ss` and 4 sample bits
//! `B3 B2 B1 B0`:
//!
//! ```text
//! d = ss * B2 + (ss/2) * B1 + (ss/4) * B0 + ss/8
//! if B3 == 1 { d = -d }
//! X(n) = X(n-1) + d
//! ```
//!
//! Then the step-size pointer is advanced by `M(magnitude)` from the
//! 8-entry adjustment table ([`crate::tables::OKI_INDEX_ADJUST`]) and
//! clamped to `0..=48`. The next step size is
//! [`crate::tables::OKI_STEP_SIZE`] at the new pointer.
//!
//! Reconstructed `X` is **12-bit signed** (clamped to `-2048..=2047`);
//! initial conditions are `X = 0`, step pointer = 0 (entry 1 in the app
//! note's 1-indexed table — value 16).
//!
//! # Nibble order
//!
//! - Dialogic `.vox` and the OKI MSM6295 read each byte **MSB nibble
//!   first** (`hi` then `lo`).
//! - The OKI MSM6258 reads **LSB nibble first**; the arithmetic is
//!   identical. We expose both orders via [`NibbleOrder`] so callers
//!   targeting MSM6258 streams can flip the unpack convention.
//!
//! # 12-bit → 16-bit output
//!
//! The reconstructed predictor is signed 12-bit. Most consumer pipelines
//! ingest 16-bit PCM, so the decoder optionally shifts the reconstructed
//! sample left by 4 bits before returning. The shift is requested via the
//! [`Output`] enum; bare 12-bit values are also available for callers
//! that want the silicon's literal output.

use oxideav_core::{Error, Result};

use crate::tables::{
    OKI_INDEX_ADJUST, OKI_PREDICTOR_MAX, OKI_PREDICTOR_MIN, OKI_STEP_INDEX_MAX, OKI_STEP_INDEX_MIN,
    OKI_STEP_SIZE,
};

/// Order in which two samples are packed into one byte.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum NibbleOrder {
    /// MSB nibble first (Dialogic `.vox`, OKI MSM6295).
    HiFirst,
    /// LSB nibble first (OKI MSM6258).
    LoFirst,
}

/// PCM-sample width the decoder returns.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Output {
    /// Native silicon output — signed 12-bit value in an i16 carrier
    /// (range `-2048..=2047`).
    Native12,
    /// Shifted to fill the i16 dynamic range (`<< 4`) so the result drops
    /// straight into a standard PCM pipeline. Range `-32768..=32752`.
    Wide16,
}

/// Running per-channel decoder state.
///
/// The Dialogic app note specifies the initial conditions as `X = 0`
/// and step pointer = 0 (the table's first entry); both default to zero
/// here, so `Default` matches the spec-mandated reset state.
#[derive(Clone, Copy, Debug, Default)]
pub struct Channel {
    /// Reconstructed sample estimate `X`, signed 12-bit.
    pub predictor: i32,
    /// Running pointer into [`OKI_STEP_SIZE`] (0..=48). Dialogic app note
    /// numbers the entries 1..49; we use 0-indexed.
    pub step_index: i32,
}

/// Decode one 4-bit nibble. Advances `state` and returns the
/// reconstructed PCM sample widened to i16 per `output`.
#[inline]
pub fn decode_nibble(state: &mut Channel, nibble: u8, output: Output) -> i16 {
    let ss = OKI_STEP_SIZE[state
        .step_index
        .clamp(OKI_STEP_INDEX_MIN, OKI_STEP_INDEX_MAX) as usize] as i32;
    let mag = (nibble & 0x07) as i32;

    // d = ss/8 + (B0 ? ss/4 : 0) + (B1 ? ss/2 : 0) + (B2 ? ss : 0).
    //
    // The app note writes the additions in MSB→LSB order
    // (`ss*B2 + ss/2*B1 + ss/4*B0 + ss/8`); we compute in any order
    // because integer addition is associative and the divisions are
    // exact shifts that don't re-order with the addition.
    let mut d = ss >> 3;
    if (mag & 0x01) != 0 {
        d += ss >> 2;
    }
    if (mag & 0x02) != 0 {
        d += ss >> 1;
    }
    if (mag & 0x04) != 0 {
        d += ss;
    }

    if (nibble & 0x08) != 0 {
        state.predictor -= d;
    } else {
        state.predictor += d;
    }
    state.predictor = state.predictor.clamp(OKI_PREDICTOR_MIN, OKI_PREDICTOR_MAX);

    // Update the step pointer (magnitude-indexed; sign B3 is ignored
    // for the lookup per Table 1's row collapse).
    state.step_index += OKI_INDEX_ADJUST[mag as usize];
    state.step_index = state
        .step_index
        .clamp(OKI_STEP_INDEX_MIN, OKI_STEP_INDEX_MAX);

    match output {
        Output::Native12 => state.predictor as i16,
        // The clamp above caps the predictor at +2047 / -2048, so the
        // shift can't overflow i16 (the maximum produced word is
        // 2047 << 4 = 32752; the minimum is -2048 << 4 = -32768).
        Output::Wide16 => (state.predictor << 4) as i16,
    }
}

/// Decode a `.vox` (or generic OKI/Dialogic) byte stream into i16 PCM.
///
/// State persists across calls when callers reuse the same `state` value
/// (the codec is stream-oriented; there are no per-block resets).
///
/// `state` carries one entry per channel. For multi-channel inputs the
/// nibble stream is assumed sample-interleaved at the nibble level
/// (nibble 0 → ch 0, nibble 1 → ch 1, nibble 2 → ch 0, …), matching the
/// Yamaha convention for sample-level interleave; in practice Dialogic
/// VOX is mono and most callers should pass a single-channel state.
pub fn decode_packet(
    packet: &[u8],
    state: &mut [Channel],
    order: NibbleOrder,
    output: Output,
) -> Vec<i16> {
    let channels = state.len();
    if channels == 0 || packet.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(packet.len() * 2);
    let mut cursor = 0usize;
    for &byte in packet {
        let (first, second) = match order {
            NibbleOrder::HiFirst => ((byte >> 4) & 0x0F, byte & 0x0F),
            NibbleOrder::LoFirst => (byte & 0x0F, (byte >> 4) & 0x0F),
        };
        out.push(decode_nibble(&mut state[cursor], first, output));
        cursor = (cursor + 1) % channels;
        out.push(decode_nibble(&mut state[cursor], second, output));
        cursor = (cursor + 1) % channels;
    }
    out
}

/// Encode an `i16` PCM sample into a 4-bit nibble. Advances `state` and
/// returns the chosen nibble together with the reconstructed sample
/// (12-bit signed, available pre-widening so callers can compute exact
/// quantisation error against the encoder's own decode path).
///
/// The encoder is the closed-form quantiser from the app note (§3):
/// pick the sign bit, then greedily set the three magnitude bits from
/// the residual `|target - X(n-1)|` against `ss`, `ss/2`, `ss/4`. After
/// emitting the nibble the state is advanced through `decode_nibble`
/// against the just-chosen code; this guarantees encode/decode share the
/// same trajectory.
///
/// `target` should be a 12-bit signed sample. Callers feeding 16-bit
/// PCM should pre-shift `target >> 4` before each call. We treat
/// out-of-range targets as if clipped — the closed-form quantiser
/// naturally saturates because the maximum representable step is small.
#[inline]
pub fn encode_sample(state: &mut Channel, target: i32) -> (u8, i16) {
    let ss = OKI_STEP_SIZE[state
        .step_index
        .clamp(OKI_STEP_INDEX_MIN, OKI_STEP_INDEX_MAX) as usize] as i32;

    // Sign + |residual|.
    let mut d = target - state.predictor;
    let mut nibble = 0u8;
    if d < 0 {
        nibble |= 0x08;
        d = -d;
    }
    // Greedy magnitude bits, matching the app-note pseudocode order
    // exactly (B2, then B1, then B0).
    if d >= ss {
        nibble |= 0x04;
        d -= ss;
    }
    if d >= ss >> 1 {
        nibble |= 0x02;
        d -= ss >> 1;
    }
    if d >= ss >> 2 {
        nibble |= 0x01;
        // d -= ss >> 2;  // No further use of d after this point.
    }

    // Advance state through the canonical decode path so encode is a
    // self-consistent inverse of decode.
    let reconstructed = decode_nibble(state, nibble, Output::Native12);
    (nibble, reconstructed)
}

/// Encode an interleaved i16-PCM mono stream into a `.vox` byte stream.
///
/// `samples` is the 12-bit-signed PCM stream — callers ingesting 16-bit
/// PCM should pre-shift `s >> 4` before passing them in (a wrapper
/// [`encode_packet_wide16`] does this automatically). `state` advances
/// across calls so a long stream can be encoded incrementally.
///
/// The output packs two consecutive samples into one byte per `order`.
/// If `samples.len()` is odd, the trailing nibble is paired with a
/// zero-magnitude pad so the byte count stays consistent; callers that
/// require strict pair alignment should pad on the input side.
pub fn encode_packet(samples: &[i16], state: &mut Channel, order: NibbleOrder) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len().div_ceil(2));
    let mut buf: Option<u8> = None;
    for &s in samples {
        let (n, _) = encode_sample(state, s as i32);
        match buf.take() {
            None => buf = Some(n),
            Some(first) => {
                let packed = match order {
                    NibbleOrder::HiFirst => (first << 4) | (n & 0x0F),
                    NibbleOrder::LoFirst => (n << 4) | (first & 0x0F),
                };
                out.push(packed);
            }
        }
    }
    if let Some(n) = buf {
        let packed = match order {
            NibbleOrder::HiFirst => n << 4,
            NibbleOrder::LoFirst => n & 0x0F,
        };
        out.push(packed);
    }
    out
}

/// Convenience wrapper around [`encode_packet`] that takes 16-bit-wide
/// PCM and right-shifts to the codec's native 12-bit range before
/// quantising. Mirrors [`Output::Wide16`] on the decode side.
pub fn encode_packet_wide16(samples: &[i16], state: &mut Channel, order: NibbleOrder) -> Vec<u8> {
    let narrowed: Vec<i16> = samples.iter().map(|&s| s >> 4).collect();
    encode_packet(&narrowed, state, order)
}

/// Length in bytes of the reset preamble (§5: 24 bytes = 48 samples).
pub const RESET_PREAMBLE_BYTES: usize = 24;

/// Number of decoded samples the reset preamble produces (§5: 48 samples).
pub const RESET_PREAMBLE_SAMPLES: usize = 48;

/// Build the Dialogic §5 *reset preamble* — the 24-byte (48-sample)
/// sequence of alternating plus/minus zero codes a `.vox` stream
/// prepends to drive a fresh decoder back to its initial conditions.
///
/// Per the app note (§5): on reset the step size is the minimum (entry
/// 1, value 16) and the waveform estimate `X = 0`. Because the decoder
/// *always* adds the `ss/8` bias term (§2), a constant-sign zero stream
/// would accumulate a DC reference; the spec therefore alternates the
/// sign — the byte values `0x08` / `0x80` carry one `0000`b (`+0`) code
/// and one `1000`b (`-0`) code each. Every zero code has magnitude 0, so
/// the step-pointer adjustment is `−1` per sample (Table 1), walking the
/// pointer down to its minimum; the alternating sign keeps the running
/// `X` net-zero so no DC offset is introduced.
///
/// The returned bytes are laid out so the **decoded** sample order is
/// `−0, +0, −0, +0, …` regardless of `order`: for [`NibbleOrder::HiFirst`]
/// each byte is `0x80` (hi nibble `1000` decoded first), and for
/// [`NibbleOrder::LoFirst`] each byte is `0x08` (lo nibble `1000`
/// decoded first). Feeding the result through [`decode_packet`] from the
/// default [`Channel`] therefore leaves the predictor at 0 and the step
/// pointer at its minimum entry.
pub fn reset_preamble(order: NibbleOrder) -> [u8; RESET_PREAMBLE_BYTES] {
    // Pick the byte whose first-decoded nibble is the `-0` code (`1000`b)
    // and whose second-decoded nibble is the `+0` code (`0000`b), so the
    // decoded stream alternates -,+ starting on the minus side.
    let byte = match order {
        // HiFirst decodes the high nibble first: 0x80 = hi 1000, lo 0000.
        NibbleOrder::HiFirst => 0x80u8,
        // LoFirst decodes the low nibble first: 0x08 = lo 1000, hi 0000.
        NibbleOrder::LoFirst => 0x08u8,
    };
    [byte; RESET_PREAMBLE_BYTES]
}

/// Reject obviously-invalid channel counts; used by the registry
/// factories. Dialogic VOX is mono in practice; we permit up to 2
/// channels for synthetic stereo material via nibble interleave.
pub(crate) fn validate_channels(channels: u16) -> Result<()> {
    if channels == 0 {
        return Err(Error::unsupported("adpcm_dialogic: channels must be >= 1"));
    }
    if channels > 2 {
        return Err(Error::unsupported(format!(
            "adpcm_dialogic: only 1 or 2 channels supported, got {channels}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_nibble_positive_grows_predictor_by_step_over_8() {
        let mut st = Channel::default();
        // Initial step = 16. Nibble 0 → d = 16/8 = 2; B3=0 → +.
        let s = decode_nibble(&mut st, 0, Output::Native12);
        assert_eq!(s, 2);
        assert_eq!(st.predictor, 2);
        // Magnitude 0 → step_index goes 0 → max(0, 0 + (-1)) = 0 (clamp).
        assert_eq!(st.step_index, 0);
    }

    #[test]
    fn sign_bit_flips_direction() {
        let mut a = Channel::default();
        let mut b = Channel::default();
        let sa = decode_nibble(&mut a, 5, Output::Native12);
        let sb = decode_nibble(&mut b, 0x5 | 0x08, Output::Native12);
        assert_eq!(sa + sb, 0);
    }

    #[test]
    fn predictor_clamps_to_12bit_signed() {
        let mut st = Channel::default();
        for _ in 0..1000 {
            let _ = decode_nibble(&mut st, 7, Output::Native12);
        }
        assert!(st.predictor <= OKI_PREDICTOR_MAX);
        assert!(st.predictor >= OKI_PREDICTOR_MIN);
    }

    #[test]
    fn step_index_clamps_to_table_bounds() {
        let mut st = Channel::default();
        // 1000 max-magnitude positive nibbles → step index should pin to 48.
        for _ in 0..1000 {
            let _ = decode_nibble(&mut st, 7, Output::Native12);
        }
        assert_eq!(st.step_index, OKI_STEP_INDEX_MAX);
        // Now 1000 min-magnitude → should walk back down to 0.
        for _ in 0..1000 {
            let _ = decode_nibble(&mut st, 0, Output::Native12);
        }
        assert_eq!(st.step_index, OKI_STEP_INDEX_MIN);
    }

    #[test]
    fn wide16_output_is_native12_shifted_left_by_4() {
        let mut a = Channel::default();
        let mut b = Channel::default();
        for &n in &[0u8, 1, 2, 5, 7, 0x9, 0xD, 0xF] {
            let s12 = decode_nibble(&mut a, n, Output::Native12) as i32;
            let s16 = decode_nibble(&mut b, n, Output::Wide16) as i32;
            assert_eq!(s16, s12 << 4, "nibble {:#x}", n);
        }
    }

    #[test]
    fn packet_round_trip_mono_native_12bit() {
        // Build a small synthetic 12-bit input, encode, decode, compare.
        let input: Vec<i16> = (0..256)
            .map(|i| ((i as f32 * 0.1).sin() * 1024.0) as i16)
            .collect();
        let mut enc = Channel::default();
        let bytes = encode_packet(&input, &mut enc, NibbleOrder::HiFirst);
        let mut dec = Channel::default();
        let out = decode_packet(&bytes, &mut [dec], NibbleOrder::HiFirst, Output::Native12);
        assert_eq!(out.len(), bytes.len() * 2);
        // Encoder is decoder-equivalent: every encoded byte's reconstructed
        // sample should match the decoder's reconstruction byte-for-byte
        // (both decode through `decode_nibble`).
        //
        // Round-trip cumulative error against the 12-bit input should be
        // bounded — sine inputs at this magnitude stay within ~256 LSB
        // (4-bit ADPCM quantisation floor at step=16..1552).
        dec = Channel::default();
        let mut rec = Vec::with_capacity(input.len());
        for &b in &bytes {
            let hi = (b >> 4) & 0x0F;
            let lo = b & 0x0F;
            rec.push(decode_nibble(&mut dec, hi, Output::Native12));
            rec.push(decode_nibble(&mut dec, lo, Output::Native12));
        }
        let rms = {
            let mut acc: f64 = 0.0;
            for i in 0..input.len() {
                let d = input[i] as f64 - rec[i] as f64;
                acc += d * d;
            }
            (acc / input.len() as f64).sqrt()
        };
        assert!(rms < 256.0, "round-trip RMS {} too high", rms);
    }

    #[test]
    fn lo_first_byte_order_pairs_with_hi_first_decode() {
        // Take an arbitrary nibble pair, pack each way, then decode.
        let a = Channel::default();
        let b = Channel::default();
        // Pack lo-first: byte = (n1 << 4) | n0  → MSM6258 packing.
        let byte_lo_first = (0x3 << 4) | 0x5;
        let pcm_lo = decode_packet(
            &[byte_lo_first],
            &mut [a],
            NibbleOrder::LoFirst,
            Output::Native12,
        );
        // Pack hi-first manually so the SAME first-/second-nibble order
        // comes out: byte = (n0 << 4) | n1.
        let byte_hi_first = (0x5 << 4) | 0x3;
        let pcm_hi = decode_packet(
            &[byte_hi_first],
            &mut [b],
            NibbleOrder::HiFirst,
            Output::Native12,
        );
        assert_eq!(pcm_lo, pcm_hi);
    }

    #[test]
    fn empty_inputs_produce_empty_outputs() {
        let mut st = [Channel::default()];
        assert!(decode_packet(&[], &mut st, NibbleOrder::HiFirst, Output::Native12).is_empty());
        let mut enc = Channel::default();
        assert!(encode_packet(&[], &mut enc, NibbleOrder::HiFirst).is_empty());
    }

    #[test]
    fn odd_sample_count_pads_a_trailing_nibble() {
        let mut enc = Channel::default();
        // Three samples → 2 bytes (one full pair + one byte with a trailing zero nibble).
        let bytes = encode_packet(&[100, -100, 50], &mut enc, NibbleOrder::HiFirst);
        assert_eq!(bytes.len(), 2);
        // The trailing nibble (low 4 bits of byte 1) should be the
        // pad — but the high nibble is the third sample's code, and
        // the low nibble is zero (pad).
        assert_eq!(bytes[1] & 0x0F, 0x00);
    }

    #[test]
    fn reset_preamble_has_spec_length() {
        assert_eq!(reset_preamble(NibbleOrder::HiFirst).len(), 24);
        assert_eq!(RESET_PREAMBLE_BYTES, 24);
        assert_eq!(RESET_PREAMBLE_SAMPLES, 48);
        // §5 byte values: 0x80 for hi-first, 0x08 for lo-first.
        assert!(reset_preamble(NibbleOrder::HiFirst)
            .iter()
            .all(|&b| b == 0x80));
        assert!(reset_preamble(NibbleOrder::LoFirst)
            .iter()
            .all(|&b| b == 0x08));
    }

    #[test]
    fn reset_preamble_decodes_to_alternating_minus_plus_zero() {
        // From the reset state the preamble's first sample is the -0 code
        // (-ss/8) and the second is the +0 code (+ss/8); with ss starting
        // at 16 the magnitudes are ±2 and the running predictor never
        // drifts away from 0 by more than one ss/8.
        for order in [NibbleOrder::HiFirst, NibbleOrder::LoFirst] {
            let bytes = reset_preamble(order);
            let pcm = decode_packet(&bytes, &mut [Channel::default()], order, Output::Native12);
            assert_eq!(pcm.len(), RESET_PREAMBLE_SAMPLES);
            // Decoded order is -,+,-,+ ...: even indices negative-or-zero,
            // odd indices positive-or-zero, and each |sample| is small.
            for (i, &s) in pcm.iter().enumerate() {
                if i % 2 == 0 {
                    assert!(s <= 0, "sample {i} ({s}) should be the -0 code");
                } else {
                    assert!(s >= 0, "sample {i} ({s}) should be the +0 code");
                }
            }
        }
    }

    #[test]
    fn reset_preamble_walks_step_pointer_to_floor() {
        // §5: the preamble's magnitude-0 codes each adjust the step pointer
        // by -1 (Table 1), so from ANY starting pointer 48 samples drive it
        // to OKI_STEP_INDEX_MIN. (The predictor is NOT mass-corrected — the
        // OKI recurrence is purely additive, so a real reset sets X=0 first;
        // the preamble's job is to floor the step and avoid building DC, not
        // to cancel a pre-existing offset.)
        for order in [NibbleOrder::HiFirst, NibbleOrder::LoFirst] {
            let mut st = Channel {
                predictor: 500,
                step_index: 30,
            };
            let bytes = reset_preamble(order);
            let _ = decode_packet(
                &bytes,
                std::slice::from_mut(&mut st),
                order,
                Output::Native12,
            );
            assert_eq!(st.step_index, OKI_STEP_INDEX_MIN);
        }
    }

    #[test]
    fn reset_preamble_introduces_no_dc_from_fresh_state() {
        // §5's DC-neutrality guarantee: from the spec reset state (X=0)
        // the preamble keeps the predictor net-zero — the alternating
        // ±ss/8 pairs cancel, leaving at most one trailing ss/8 residue
        // (ss=16 at the floor → ±2).
        for order in [NibbleOrder::HiFirst, NibbleOrder::LoFirst] {
            let mut st = Channel::default();
            let bytes = reset_preamble(order);
            let _ = decode_packet(
                &bytes,
                std::slice::from_mut(&mut st),
                order,
                Output::Native12,
            );
            assert_eq!(st.step_index, OKI_STEP_INDEX_MIN);
            assert!(
                st.predictor.abs() <= 2,
                "predictor {} drifted (DC introduced)",
                st.predictor
            );
        }
    }

    #[test]
    fn validate_channels_accepts_1_and_2_rejects_0_and_3() {
        assert!(validate_channels(1).is_ok());
        assert!(validate_channels(2).is_ok());
        assert!(validate_channels(0).is_err());
        assert!(validate_channels(3).is_err());
    }
}
