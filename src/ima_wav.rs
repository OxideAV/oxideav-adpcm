//! IMA ADPCM WAV variant decoder (`adpcm_ima_wav`, WAVEFORMATEX tag
//! `0x0011`).
//!
//! Algorithm follows the IMA / DVI "Recommended Practices" spec for the
//! nibble recurrence (see [`crate::tables`] for the step + index-adjust
//! tables), and the Microsoft IMA-ADPCM WAV packaging for block layout:
//!
//! - Block header: per channel, 4 bytes:
//!     * `predictor` — signed 16-bit little-endian.
//!     * `step_index` — u8, clamped to 0..=88.
//!     * reserved byte (typically 0).
//! - Body: 4-byte interleave groups. The first 4 bytes belong to channel
//!   0, the next 4 to channel 1, etc., and the pattern repeats until the
//!   block ends. Within each 4-byte group nibbles are unpacked
//!   **bottom-nibble first** (n1 n0 / n3 n2 / n5 n4 / n7 n6 in
//!   little-endian-nibble order).
//!
//! Each block therefore produces exactly
//! `1 + (body_bytes_per_channel * 2)` samples per channel, where the
//! header predictor counts as sample 0.
//!
//! # 3-bit mode
//!
//! WAV tag `0x0011` supports `wBitsPerSample` of **3 or 4**. The 3-bit
//! mode ([`decode_block_3bit`]) shares the block header and the 89-entry
//! step table with the 4-bit mode but differs in three ways:
//!
//! - The code is 1 sign + 2 magnitude bits; the reconstruction is
//!   `diff = step/4 + (code&1 ? step/2 : 0) + (code&2 ? step : 0)`.
//! - The index adjustment uses the 8-entry
//!   [`crate::tables::IMA3_INDEX_ADJUST`] table (`{-1, -1, 1, 2}`,
//!   sign-mirrored).
//! - The body interleaves channels in **groups of three 32-bit words**
//!   (12 bytes) per channel — the smallest unit holding a whole number
//!   of 3-bit codes (32 codes = 96 bits, zero padding waste). Codes are
//!   extracted 3 bits at a time from the least-significant end of the
//!   little-endian 96-bit group, the same low-bits-first order the
//!   4-bit layout uses for its nibbles (lo nibble before hi nibble in
//!   each byte of the little-endian word).
//!
//! Each 3-bit block therefore produces `1 + groups * 32` samples per
//! channel, where the body must be a whole number of 12-byte groups per
//! channel.

use crate::tables::{IMA3_INDEX_ADJUST, IMA_INDEX_ADJUST, IMA_STEP_SIZE};
use oxideav_core::{Error, Result};

/// The IMA codec state pair shared by every expansion / quantization
/// entry point in this module: the running reconstruction (`predictor`,
/// clamped to the i16 range) and the step-table index (`step_index`,
/// clamped to `0..=88`).
///
/// [`crate::encoder::ima_encode_block_reference`] and friends carry one
/// of these per channel **across blocks** — the reference compressor
/// clears the index only once, before the first block, and each block
/// header then records the index the previous block ended on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImaCodecState {
    /// Running reconstructed sample (the decoder's predictor).
    pub predictor: i32,
    /// Step-size table index, `0..=88`.
    pub step_index: i32,
}

/// Expand one 4-bit nibble into the next PCM sample for a given channel.
///
/// Tracks and updates `predictor` (i32, clamped to i16 range on output)
/// and `step_index` (clamped to 0..=88).
#[inline]
pub fn ima_expand_nibble(predictor: &mut i32, step_index: &mut i32, nibble: u8) -> i16 {
    let n = nibble as i32;
    // Current step comes from the **pre-update** index, per the IMA spec.
    let step = IMA_STEP_SIZE[(*step_index).clamp(0, 88) as usize] as i32;

    // diff = step/8 + step/4 * L0 + step/2 * L1 + step * L2  (per spec:
    // "diff = (step >> 3) + conditional additions per magnitude bit").
    let mag = n & 7;
    let mut diff = step >> 3;
    if (mag & 1) != 0 {
        diff += step >> 2;
    }
    if (mag & 2) != 0 {
        diff += step >> 1;
    }
    if (mag & 4) != 0 {
        diff += step;
    }

    if (n & 8) != 0 {
        *predictor -= diff;
    } else {
        *predictor += diff;
    }
    *predictor = (*predictor).clamp(i16::MIN as i32, i16::MAX as i32);

    *step_index += IMA_INDEX_ADJUST[nibble as usize];
    *step_index = (*step_index).clamp(0, 88);

    *predictor as i16
}

/// Expand one 3-bit code into the next PCM sample for a given channel.
///
/// The 3-bit recurrence (1 sign bit + 2 magnitude bits): the
/// reconstructed difference is `step/4` unconditionally, plus `step/2`
/// if magnitude bit 0 is set, plus `step` if magnitude bit 1 is set.
/// Code bit 2 is the sign. The index adjustment comes from the 8-entry
/// [`IMA3_INDEX_ADJUST`] table, indexed by the full 3-bit code.
///
/// Tracks and updates `predictor` (i32, clamped to i16 range on output)
/// and `step_index` (clamped to 0..=88) — the same state pair as the
/// 4-bit [`ima_expand_nibble`].
#[inline]
pub fn ima_expand_code3(predictor: &mut i32, step_index: &mut i32, code: u8) -> i16 {
    let c = (code & 7) as i32;
    // Current step comes from the **pre-update** index, as in 4-bit mode.
    let step = IMA_STEP_SIZE[(*step_index).clamp(0, 88) as usize] as i32;

    let mut diff = step >> 2;
    if (c & 1) != 0 {
        diff += step >> 1;
    }
    if (c & 2) != 0 {
        diff += step;
    }

    if (c & 4) != 0 {
        *predictor -= diff;
    } else {
        *predictor += diff;
    }
    *predictor = (*predictor).clamp(i16::MIN as i32, i16::MAX as i32);

    *step_index += IMA3_INDEX_ADJUST[c as usize];
    *step_index = (*step_index).clamp(0, 88);

    *predictor as i16
}

/// Quantize one PCM sample into a 4-bit IMA code with the
/// **reference ladder compressor** — the compression algorithm the IMA
/// "Recommended Practices" Rev 3.00 publishes in Appendix D §6.1
/// (identical to the *DVI ADPCM Wave Type* specification's 4-bit
/// encoding procedure).
///
/// The ladder quantizes `sample - predictor` against the current step
/// by three successive threshold comparisons (`step`, `step/2`,
/// `step/4`, each subtracted when met), producing a sign-magnitude
/// code, then advances the shared `(predictor, step_index)` state
/// through [`ima_expand_nibble`] — the §6.2 expansion — exactly as the
/// listing prescribes ("uncompressed using the same quantization
/// stepsize to obtain a linear difference identical to that calculated
/// by the decompressor"). Encode is therefore bit-exactly the inverse
/// of decode *by construction*: feeding the returned code to
/// [`ima_expand_nibble`] from the same start state reproduces this
/// function's post-state.
///
/// This is the deterministic interchange compressor: unlike the
/// error-minimising search the frame encoders default to, the ladder's
/// output for a given input stream is exactly the byte stream every
/// other Appendix-D-conforming compressor produces.
#[inline]
pub fn ima_quantize_nibble(predictor: &mut i32, step_index: &mut i32, sample: i16) -> u8 {
    let step = IMA_STEP_SIZE[(*step_index).clamp(0, 88) as usize] as i32;
    let mut diff = sample as i32 - *predictor;
    let mut code: u8 = 0;
    if diff < 0 {
        code = 8;
        diff = -diff;
    }
    // §6.1 ladder: three iterations against step, step/2, step/4 —
    // "quantize difference down to four bits ... through repeated
    // subtraction".
    let mut temp = step;
    let mut mask: u8 = 4;
    for _ in 0..3 {
        if diff >= temp {
            code |= mask;
            diff -= temp;
        }
        temp >>= 1;
        mask >>= 1;
    }
    // Advance state through the published expansion (§6.2) so the
    // encoder-side reconstruction is identical to the decoder's.
    let _ = ima_expand_nibble(predictor, step_index, code);
    code
}

/// Quantize one PCM sample into a 3-bit IMA/DVI code with the
/// **reference ladder compressor** — the 3-bit encoding procedure of
/// the *DVI ADPCM Wave Type* specification (the `wBitsPerSample = 3`
/// mode of WAV tag `0x0011`).
///
/// The 3-bit ladder is two threshold comparisons (`step`, `step/2`)
/// after the sign split; state advances through [`ima_expand_code3`]
/// (the specification's own expansion) so encode is bit-exactly the
/// inverse of decode by construction, as in the 4-bit
/// [`ima_quantize_nibble`].
#[inline]
pub fn ima_quantize_code3(predictor: &mut i32, step_index: &mut i32, sample: i16) -> u8 {
    let step = IMA_STEP_SIZE[(*step_index).clamp(0, 88) as usize] as i32;
    let mut diff = sample as i32 - *predictor;
    let mut code: u8 = 0;
    if diff < 0 {
        code = 4;
        diff = -diff;
    }
    if diff >= step {
        code |= 2;
        diff -= step;
    }
    if diff >= step >> 1 {
        code |= 1;
    }
    let _ = ima_expand_code3(predictor, step_index, code);
    code
}

/// Per-channel body bytes in one 3-bit interleave group: three 32-bit
/// words = 12 bytes = 32 three-bit codes with zero padding waste.
pub const GROUP_BYTES_3BIT: usize = 12;

/// Samples produced per channel by one 12-byte 3-bit group.
pub const GROUP_SAMPLES_3BIT: usize = 32;

/// Decode a single **3-bit** IMA-ADPCM-WAV block (`wBitsPerSample = 3`)
/// into interleaved i16 PCM.
///
/// Block layout: the same 4-byte-per-channel header as the 4-bit mode
/// (i16-LE predictor seed, u8 step index, reserved byte), then a body
/// that interleaves channels in 12-byte groups (three 32-bit words per
/// channel — the smallest unit holding a whole number of 3-bit codes).
/// Each group carries 32 codes per channel, extracted low-bits-first
/// from the little-endian 96-bit group value.
pub fn decode_block_3bit(block: &[u8], channels: usize) -> Result<Vec<i16>> {
    if channels == 0 || channels > 8 {
        return Err(Error::unsupported(format!(
            "adpcm_ima_wav(3-bit): channel count {channels} not supported (1..=8)"
        )));
    }
    let header_len = 4 * channels;
    if block.len() < header_len {
        return Err(Error::invalid(format!(
            "adpcm_ima_wav(3-bit): block too short ({} < header {header_len})",
            block.len()
        )));
    }

    // Parse header — identical layout to the 4-bit mode.
    let mut predictor: Vec<i32> = Vec::with_capacity(channels);
    let mut step_index: Vec<i32> = Vec::with_capacity(channels);
    for ch in 0..channels {
        let base = ch * 4;
        let p = i16::from_le_bytes([block[base], block[base + 1]]) as i32;
        let idx = block[base + 2] as i32;
        if !(0..=88).contains(&idx) {
            return Err(Error::invalid(format!(
                "adpcm_ima_wav(3-bit): step index {idx} out of range 0..=88 (ch {ch})"
            )));
        }
        predictor.push(p);
        step_index.push(idx);
    }

    let body = &block[header_len..];
    // Body must be a whole number of 12-byte groups per channel.
    let group_bytes = GROUP_BYTES_3BIT * channels;
    if body.len() % group_bytes != 0 {
        return Err(Error::invalid(format!(
            "adpcm_ima_wav(3-bit): body length {} not a multiple of {} ({}ch × 12B group)",
            body.len(),
            group_bytes,
            channels
        )));
    }
    let groups = body.len() / group_bytes;
    let samples_per_channel = 1 + groups * GROUP_SAMPLES_3BIT;

    let mut out = vec![0i16; samples_per_channel * channels];
    // Seed sample 0 per channel = header predictor.
    for ch in 0..channels {
        out[ch] = predictor[ch] as i16;
    }

    // For each group, channel c's code stream is the 12 bytes starting at
    // (group_start + 12*c). Assemble them into a little-endian 96-bit
    // value and pull 3 bits at a time from the least-significant end.
    for g in 0..groups {
        let group_start = g * group_bytes;
        for ch in 0..channels {
            let ch_bytes = &body
                [group_start + GROUP_BYTES_3BIT * ch..group_start + GROUP_BYTES_3BIT * (ch + 1)];
            let mut bits: u128 = 0;
            for (i, &b) in ch_bytes.iter().enumerate() {
                bits |= (b as u128) << (8 * i);
            }
            for k in 0..GROUP_SAMPLES_3BIT {
                let code = ((bits >> (3 * k)) & 7) as u8;
                let s = ima_expand_code3(&mut predictor[ch], &mut step_index[ch], code);
                let sample_idx = 1 + g * GROUP_SAMPLES_3BIT + k;
                out[sample_idx * channels + ch] = s;
            }
        }
    }

    Ok(out)
}

/// Decode a single Microsoft-IMA-ADPCM-WAV block into interleaved i16 PCM.
pub fn decode_block(block: &[u8], channels: usize) -> Result<Vec<i16>> {
    if channels == 0 || channels > 8 {
        return Err(Error::unsupported(format!(
            "adpcm_ima_wav: channel count {channels} not supported (1..=8)"
        )));
    }
    let header_len = 4 * channels;
    if block.len() < header_len {
        return Err(Error::invalid(format!(
            "adpcm_ima_wav: block too short ({} < header {header_len})",
            block.len()
        )));
    }

    // Parse header.
    let mut predictor: Vec<i32> = Vec::with_capacity(channels);
    let mut step_index: Vec<i32> = Vec::with_capacity(channels);
    for ch in 0..channels {
        let base = ch * 4;
        let p = i16::from_le_bytes([block[base], block[base + 1]]) as i32;
        let idx = block[base + 2] as i32;
        if !(0..=88).contains(&idx) {
            return Err(Error::invalid(format!(
                "adpcm_ima_wav: step index {idx} out of range 0..=88 (ch {ch})"
            )));
        }
        predictor.push(p);
        step_index.push(idx);
    }

    let body = &block[header_len..];
    // Body must be a whole number of 4-byte groups per channel.
    let group_bytes = 4 * channels;
    if body.len() % group_bytes != 0 {
        return Err(Error::invalid(format!(
            "adpcm_ima_wav: body length {} not a multiple of {} ({}ch × 4B group)",
            body.len(),
            group_bytes,
            channels
        )));
    }
    let groups = body.len() / group_bytes;
    // Each 4-byte group carries 8 nibbles → 8 samples per channel.
    let samples_per_channel = 1 + groups * 8;

    let mut out = vec![0i16; samples_per_channel * channels];
    // Seed sample 0 per channel = header predictor.
    for ch in 0..channels {
        out[ch] = predictor[ch] as i16;
    }

    // For each group of 4*channels bytes, channel c's nibble stream is the
    // 4 bytes starting at (group_start + 4*c), decoded lo-nibble first.
    for g in 0..groups {
        let group_start = g * group_bytes;
        for ch in 0..channels {
            let ch_bytes = &body[group_start + 4 * ch..group_start + 4 * ch + 4];
            for (i, &byte) in ch_bytes.iter().enumerate() {
                let n_lo = byte & 0x0F;
                let n_hi = (byte >> 4) & 0x0F;
                let sample_lo_idx = 1 + g * 8 + i * 2;
                let sample_hi_idx = sample_lo_idx + 1;

                let s_lo = ima_expand_nibble(&mut predictor[ch], &mut step_index[ch], n_lo);
                let s_hi = ima_expand_nibble(&mut predictor[ch], &mut step_index[ch], n_hi);
                out[sample_lo_idx * channels + ch] = s_lo;
                out[sample_hi_idx * channels + ch] = s_hi;
            }
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_zero_nibble_grows_toward_zero_index() {
        let mut p = 0i32;
        let mut si = 10i32;
        let _ = ima_expand_nibble(&mut p, &mut si, 0);
        // nibble=0: step_index delta is -1 → 9.
        assert_eq!(si, 9);
    }

    #[test]
    fn expand_high_nibble_grows_index() {
        let mut p = 0i32;
        let mut si = 10i32;
        let _ = ima_expand_nibble(&mut p, &mut si, 7);
        // nibble=7: step_index delta is +8 → 18.
        assert_eq!(si, 18);
    }

    #[test]
    fn step_index_saturates() {
        let mut p = 0i32;
        let mut si = 88i32;
        // Repeated +8 adjusts keep step_index clamped.
        for _ in 0..10 {
            let _ = ima_expand_nibble(&mut p, &mut si, 7);
        }
        assert_eq!(si, 88);
        let mut si = 0i32;
        for _ in 0..10 {
            let _ = ima_expand_nibble(&mut p, &mut si, 0);
        }
        assert_eq!(si, 0);
    }

    #[test]
    fn predictor_clamps_to_i16_range() {
        let mut p: i32 = 30000;
        let mut si: i32 = 80;
        for _ in 0..10 {
            // Positive max-magnitude nibble (7) pushes predictor up.
            let _ = ima_expand_nibble(&mut p, &mut si, 7);
        }
        assert!(p <= i16::MAX as i32);

        let mut p: i32 = -30000;
        let mut si: i32 = 80;
        for _ in 0..10 {
            // Negative max-magnitude nibble (0xF) pushes predictor down.
            let _ = ima_expand_nibble(&mut p, &mut si, 0xF);
        }
        assert!(p >= i16::MIN as i32);
    }

    #[test]
    fn mono_block_header_only_emits_seed_sample() {
        let mut block = Vec::new();
        block.extend_from_slice(&1234i16.to_le_bytes());
        block.push(7); // step index
        block.push(0); // reserved
        let pcm = decode_block(&block, 1).unwrap();
        assert_eq!(pcm, vec![1234]);
    }

    #[test]
    fn mono_block_with_one_group_decodes_9_samples() {
        let mut block = Vec::new();
        block.extend_from_slice(&0i16.to_le_bytes());
        block.push(10);
        block.push(0);
        block.extend_from_slice(&[0u8; 4]); // one 4-byte group = 8 nibbles
        let pcm = decode_block(&block, 1).unwrap();
        assert_eq!(pcm.len(), 9);
        assert_eq!(pcm[0], 0);
    }

    #[test]
    fn rejects_bad_step_index() {
        let mut block = Vec::new();
        block.extend_from_slice(&0i16.to_le_bytes());
        block.push(200); // > 88
        block.push(0);
        assert!(decode_block(&block, 1).is_err());
    }

    #[test]
    fn expand_code3_zero_code_is_quarter_step_and_shrinks_index() {
        let mut p = 0i32;
        let mut si = 10i32;
        // step at index 10 is 19; code 0 → diff = 19>>2 = 4, sign +.
        let s = ima_expand_code3(&mut p, &mut si, 0);
        assert_eq!(s, 4);
        assert_eq!(si, 9); // adjust -1
    }

    #[test]
    fn expand_code3_full_magnitude_grows_index() {
        let mut p = 0i32;
        let mut si = 10i32;
        // code 3 → diff = step + step/2 + step/4 = 19 + 9 + 4 = 32.
        let s = ima_expand_code3(&mut p, &mut si, 3);
        assert_eq!(s, 32);
        assert_eq!(si, 12); // adjust +2
    }

    #[test]
    fn expand_code3_sign_bit_mirrors_diff() {
        // For each magnitude, codes m and m|4 must move the predictor by
        // ±the same amount and adjust the index identically.
        for m in 0u8..4 {
            let mut p_pos = 0i32;
            let mut si_pos = 40i32;
            let s_pos = ima_expand_code3(&mut p_pos, &mut si_pos, m);
            let mut p_neg = 0i32;
            let mut si_neg = 40i32;
            let s_neg = ima_expand_code3(&mut p_neg, &mut si_neg, m | 4);
            assert_eq!(s_pos as i32, -(s_neg as i32), "magnitude {m}");
            assert_eq!(si_pos, si_neg, "magnitude {m}");
        }
    }

    #[test]
    fn expand_code3_index_saturates() {
        let mut p = 0i32;
        let mut si = 88i32;
        for _ in 0..10 {
            let _ = ima_expand_code3(&mut p, &mut si, 3);
        }
        assert_eq!(si, 88);
        let mut si = 0i32;
        for _ in 0..10 {
            let _ = ima_expand_code3(&mut p, &mut si, 0);
        }
        assert_eq!(si, 0);
    }

    #[test]
    fn expand_code3_predictor_clamps_to_i16_range() {
        let mut p: i32 = 32000;
        let mut si: i32 = 88;
        for _ in 0..10 {
            let _ = ima_expand_code3(&mut p, &mut si, 3);
        }
        assert!(p <= i16::MAX as i32);
        let mut p: i32 = -32000;
        let mut si: i32 = 88;
        for _ in 0..10 {
            let _ = ima_expand_code3(&mut p, &mut si, 7);
        }
        assert!(p >= i16::MIN as i32);
    }

    #[test]
    fn mono_3bit_header_only_emits_seed_sample() {
        let mut block = Vec::new();
        block.extend_from_slice(&1234i16.to_le_bytes());
        block.push(7);
        block.push(0);
        let pcm = decode_block_3bit(&block, 1).unwrap();
        assert_eq!(pcm, vec![1234]);
    }

    #[test]
    fn mono_3bit_one_group_decodes_33_samples_as_quarter_step_ramp() {
        // Header: predictor 0, step index 0 (step = 7). One 12-byte
        // all-zero group = 32 code-0 samples. Code 0 keeps the index at
        // 0 (adjust -1, clamped) so every step stays 7 → diff = 1 each.
        let mut block = Vec::new();
        block.extend_from_slice(&0i16.to_le_bytes());
        block.push(0);
        block.push(0);
        block.extend_from_slice(&[0u8; GROUP_BYTES_3BIT]);
        let pcm = decode_block_3bit(&block, 1).unwrap();
        assert_eq!(pcm.len(), 1 + GROUP_SAMPLES_3BIT);
        for (i, &s) in pcm.iter().enumerate() {
            assert_eq!(s as usize, i, "sample {i}");
        }
    }

    #[test]
    fn decode_3bit_extracts_codes_low_bits_first() {
        // Pack a single non-zero code in the low 3 bits of the group and
        // confirm it lands on sample 1 (not sample 32): byte 0 = 0b100
        // (code 4 = negative quarter-step) followed by all-zero codes.
        let mut block = Vec::new();
        block.extend_from_slice(&1000i16.to_le_bytes());
        block.push(20); // step index 20 → step 50
        block.push(0);
        let mut group = [0u8; GROUP_BYTES_3BIT];
        group[0] = 0b100; // code #0 = 4, codes #1.. = 0
        block.extend_from_slice(&group);
        let pcm = decode_block_3bit(&block, 1).unwrap();
        // Sample 1 = 1000 - 50/4 = 1000 - 12 = 988 (negative quarter step).
        assert_eq!(pcm[1], 988);
    }

    #[test]
    fn rejects_3bit_bad_step_index_and_off_size_body() {
        let mut block = Vec::new();
        block.extend_from_slice(&0i16.to_le_bytes());
        block.push(89); // out of range
        block.push(0);
        block.extend_from_slice(&[0u8; GROUP_BYTES_3BIT]);
        assert!(decode_block_3bit(&block, 1).is_err());

        // Body not a multiple of 12 bytes per channel.
        let mut block = Vec::new();
        block.extend_from_slice(&0i16.to_le_bytes());
        block.push(0);
        block.push(0);
        block.extend_from_slice(&[0u8; 8]);
        assert!(decode_block_3bit(&block, 1).is_err());

        // Truncated header.
        assert!(decode_block_3bit(&[0u8; 3], 1).is_err());
        // Channel bounds.
        assert!(decode_block_3bit(&[0u8; 48], 0).is_err());
        assert!(decode_block_3bit(&[0u8; 48], 9).is_err());
    }

    #[test]
    fn stereo_3bit_block_layout() {
        // 8-byte header + one group (12B × 2ch = 24 body bytes) →
        // 1 + 32 = 33 samples per channel, interleaved.
        let mut block = Vec::new();
        block.extend_from_slice(&100i16.to_le_bytes());
        block.push(0);
        block.push(0);
        block.extend_from_slice(&200i16.to_le_bytes());
        block.push(0);
        block.push(0);
        block.extend_from_slice(&[0u8; 24]);
        let pcm = decode_block_3bit(&block, 2).unwrap();
        assert_eq!(pcm.len(), 33 * 2);
        assert_eq!(pcm[0], 100);
        assert_eq!(pcm[1], 200);
        // Both channels ramp by step/4 = 1 per code-0 sample from their
        // own seeds — confirms per-channel state isolation.
        assert_eq!(pcm[2], 101);
        assert_eq!(pcm[3], 201);
    }

    #[test]
    fn quantize_nibble_reproduces_the_recommendation_worked_example() {
        // Appendix D §6.1 worked example: originalSample = 0x873F,
        // predictedSample = 0x8700, stepsize = 73 (index 24) →
        // newSample = 3, predictedSample' = 0x873F, index' = 23,
        // stepsize' = 66.
        assert_eq!(IMA_STEP_SIZE[24], 73, "step table row 24");
        let mut predictor = 0x8700u16 as i16 as i32; // -30976
        let mut step_index = 24i32;
        let sample = 0x873Fu16 as i16; // -30913
        let code = ima_quantize_nibble(&mut predictor, &mut step_index, sample);
        assert_eq!(code, 3, "worked-example code");
        assert_eq!(
            predictor, 0x873Fu16 as i16 as i32,
            "worked-example predictor"
        );
        assert_eq!(step_index, 23, "worked-example index");
        assert_eq!(IMA_STEP_SIZE[23], 66, "worked-example next stepsize");
    }

    #[test]
    fn expand_nibble_reproduces_the_recommendation_worked_example() {
        // Appendix D §6.2 worked example: originalSample (code) = 0x3,
        // newSample[previous] = 0x8700, stepsize = 73 (index 24) →
        // newSample = 0x873F, index' = 23, stepsize' = 66.
        let mut predictor = 0x8700u16 as i16 as i32;
        let mut step_index = 24i32;
        let s = ima_expand_nibble(&mut predictor, &mut step_index, 0x3);
        assert_eq!(s, 0x873Fu16 as i16, "worked-example expansion");
        assert_eq!(step_index, 23);
        assert_eq!(IMA_STEP_SIZE[23], 66);
    }

    #[test]
    fn quantize_nibble_state_advance_is_exactly_the_expansion() {
        // By construction the quantizer advances through the §6.2
        // expansion; pin it against an explicit expand call from the
        // same start state, across a state/sample sweep.
        let mut x = 0x2545F1u64;
        let mut rng = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..20_000 {
            let p0 = (rng() as i16) as i32;
            let si0 = (rng() % 89) as i32;
            let sample = rng() as i16;

            let mut pq = p0;
            let mut sq = si0;
            let code = ima_quantize_nibble(&mut pq, &mut sq, sample);
            assert!(code < 16);

            let mut pe = p0;
            let mut se = si0;
            let _ = ima_expand_nibble(&mut pe, &mut se, code);
            assert_eq!((pq, sq), (pe, se), "quantizer state != expansion state");
        }
    }

    #[test]
    fn quantize_code3_state_advance_is_exactly_the_expansion() {
        let mut x = 0x9E3779B9u64;
        let mut rng = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..20_000 {
            let p0 = (rng() as i16) as i32;
            let si0 = (rng() % 89) as i32;
            let sample = rng() as i16;

            let mut pq = p0;
            let mut sq = si0;
            let code = ima_quantize_code3(&mut pq, &mut sq, sample);
            assert!(code < 8);

            let mut pe = p0;
            let mut se = si0;
            let _ = ima_expand_code3(&mut pe, &mut se, code);
            assert_eq!(
                (pq, sq),
                (pe, se),
                "3-bit quantizer state != expansion state"
            );
        }
    }

    #[test]
    fn quantize_nibble_sign_and_magnitude_selection() {
        // Zero difference → code 0 (positive sign, zero magnitude).
        let mut p = 1000i32;
        let mut si = 30i32;
        assert_eq!(ima_quantize_nibble(&mut p, &mut si, 1000), 0);
        // Large positive difference at a small step saturates to 7.
        let mut p = 0i32;
        let mut si = 0i32; // step 7
        assert_eq!(ima_quantize_nibble(&mut p, &mut si, 20_000), 7);
        // Large negative difference saturates to 0xF.
        let mut p = 0i32;
        let mut si = 0i32;
        assert_eq!(ima_quantize_nibble(&mut p, &mut si, -20_000), 0xF);
        // Exactly one step below → sign bit + magnitude-4 (code 0xC):
        // step at index 20 is 50; diff = -50 meets the first threshold
        // and leaves nothing for the finer rungs.
        let mut p = 0i32;
        let mut si = 20i32;
        assert_eq!(IMA_STEP_SIZE[20], 50);
        assert_eq!(ima_quantize_nibble(&mut p, &mut si, -50), 0xC);
    }

    #[test]
    fn quantize_code3_sign_and_magnitude_selection() {
        let mut p = 500i32;
        let mut si = 30i32;
        assert_eq!(ima_quantize_code3(&mut p, &mut si, 500), 0);
        let mut p = 0i32;
        let mut si = 0i32;
        assert_eq!(ima_quantize_code3(&mut p, &mut si, 20_000), 3);
        let mut p = 0i32;
        let mut si = 0i32;
        assert_eq!(ima_quantize_code3(&mut p, &mut si, -20_000), 7);
        // step at index 20 is 50; diff = +50 → magnitude bit 1 only.
        let mut p = 0i32;
        let mut si = 20i32;
        assert_eq!(ima_quantize_code3(&mut p, &mut si, 50), 2);
    }

    #[test]
    fn quantize_nibble_converges_on_a_sine() {
        // The ladder + adaptation pair converges: after the cold-start
        // attack (index seeds at 0 → step 7, so the first few samples
        // lag a fast-moving target) the reconstruction tracks a
        // 6000-amplitude sine with a bounded RMS error.
        let mut p = 0i32;
        let mut si = 0i32;
        let mut sse = 0f64;
        let mut n = 0f64;
        for i in 0..2_000 {
            let target = ((i as f64 / 40.0).sin() * 6000.0) as i16;
            let _ = ima_quantize_nibble(&mut p, &mut si, target);
            if i >= 100 {
                let e = p as f64 - target as f64;
                sse += e * e;
                n += 1.0;
            }
        }
        let rms = (sse / n).sqrt();
        assert!(rms < 200.0, "post-warm-up RMS {rms} exceeds 200");
    }

    #[test]
    fn stereo_block_layout() {
        // 8-byte header + two 8-byte groups = 24 bytes → 1 + 2*8 = 17
        // samples per channel.
        let mut block = Vec::new();
        // Channel 0 header.
        block.extend_from_slice(&100i16.to_le_bytes());
        block.push(0);
        block.push(0);
        // Channel 1 header.
        block.extend_from_slice(&200i16.to_le_bytes());
        block.push(0);
        block.push(0);
        // Two groups (4 bytes × 2 channels × 2 groups = 16 body bytes).
        block.extend_from_slice(&[0u8; 16]);
        let pcm = decode_block(&block, 2).unwrap();
        assert_eq!(pcm.len(), 17 * 2);
        // Interleaved seed samples.
        assert_eq!(pcm[0], 100);
        assert_eq!(pcm[1], 200);
    }
}
