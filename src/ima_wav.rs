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

use crate::tables::{IMA_INDEX_ADJUST, IMA_STEP_SIZE};
use oxideav_core::{Error, Result};

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
