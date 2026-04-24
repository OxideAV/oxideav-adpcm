//! IMA ADPCM QuickTime variant decoder (`adpcm_ima_qt`, QuickTime fourcc
//! `ima4`).
//!
//! Block structure (per Apple QuickTime IMA ADPCM spec):
//!
//! - Each IMA block is **34 bytes** per channel.
//! - First 2 bytes are a big-endian 16-bit preamble:
//!     * bits 15..7 → the top 9 bits of a signed 16-bit initial predictor
//!       (low 7 bits are always 0);
//!     * bits 6..0  → the initial step index, clamped to 0..=88.
//! - Remaining 32 bytes are ADPCM nibbles; in each byte the **bottom
//!   nibble is decoded first**, then the top nibble. 32 bytes × 2 nibbles
//!   = 64 samples per block per channel.
//!
//! Stereo files use **block-level** interleaving: the first 34-byte block
//! is channel 0, the next 34-byte block is channel 1, and so on. Decoding
//! a packet of `channels * 34` bytes therefore produces 64 samples per
//! channel, interleaved on output.

use crate::ima_wav::ima_expand_nibble;
use oxideav_core::{Error, Result};

/// Size of one QuickTime IMA ADPCM block (per channel).
pub const QT_BLOCK_SIZE: usize = 34;

/// Samples per block per channel (32 body bytes × 2 nibbles).
pub const QT_SAMPLES_PER_BLOCK: usize = 64;

/// Decode `channels` contiguous 34-byte QT-IMA blocks into interleaved i16.
///
/// Returns `64 * channels` i16 samples (one packet of QT-IMA = 64 samples
/// per channel, block-level-interleaved for stereo).
pub fn decode_block(data: &[u8], channels: usize) -> Result<Vec<i16>> {
    if channels == 0 || channels > 2 {
        return Err(Error::unsupported(format!(
            "adpcm_ima_qt: channel count {channels} not supported (1 or 2)"
        )));
    }
    let needed = QT_BLOCK_SIZE * channels;
    if data.len() < needed {
        return Err(Error::invalid(format!(
            "adpcm_ima_qt: need {needed} bytes for {channels}ch, got {}",
            data.len()
        )));
    }

    let mut out = vec![0i16; QT_SAMPLES_PER_BLOCK * channels];

    for ch in 0..channels {
        let block = &data[ch * QT_BLOCK_SIZE..(ch + 1) * QT_BLOCK_SIZE];

        // Preamble: big-endian u16.
        let preamble = u16::from_be_bytes([block[0], block[1]]);

        // Top 9 bits = signed predictor (low 7 bits always zero).  The
        // published range of the raw 16-bit word is treated as a signed
        // i16 by the spec (so 0x8000 is most-negative), consistent with
        // how QuickTime stores it.
        let predictor_seed = preamble as i16 as i32 & !0x7F;

        let mut predictor = predictor_seed;
        let mut step_index = (preamble & 0x7F) as i32;
        // The spec requires clamping 0..127 to 0..=88.
        if step_index > 88 {
            step_index = 88;
        }

        // Decode 32 body bytes → 64 samples.
        for (i, &byte) in block[2..].iter().enumerate() {
            let n_lo = byte & 0x0F;
            let n_hi = (byte >> 4) & 0x0F;
            let s_lo = ima_expand_nibble(&mut predictor, &mut step_index, n_lo);
            let s_hi = ima_expand_nibble(&mut predictor, &mut step_index, n_hi);
            out[(i * 2) * channels + ch] = s_lo;
            out[(i * 2 + 1) * channels + ch] = s_hi;
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_zero_block_emits_64_samples() {
        let block = [0u8; QT_BLOCK_SIZE];
        let pcm = decode_block(&block, 1).unwrap();
        assert_eq!(pcm.len(), 64);
        // Body is zero-nibbles; predictor_seed is 0, step_index 0 → the
        // first output should be (step>>3) = 0; driftless.
        assert!(pcm.iter().all(|&s| s.abs() < 16));
    }

    #[test]
    fn rejects_short_input() {
        assert!(decode_block(&[0u8; 33], 1).is_err());
        assert!(decode_block(&[0u8; 34], 2).is_err());
    }

    #[test]
    fn stereo_reads_two_blocks() {
        let mut data = vec![0u8; QT_BLOCK_SIZE * 2];
        // Give left and right different initial predictors so the output
        // reflects the block-level interleave.
        //
        // Left: preamble 0x0800 → top 9 bits = 0x0800 & 0xFF80 = 0x0800
        //                                     = 2048 (signed), step_index=0.
        data[0] = 0x08;
        data[1] = 0x00;
        // Right: preamble 0xF880 → as i16 = -1920, step_index = 0.
        data[34] = 0xF8;
        data[35] = 0x00;
        let pcm = decode_block(&data, 2).unwrap();
        assert_eq!(pcm.len(), 128);
        // First interleaved pair = (left predictor update from first
        // nibble, right predictor update from first nibble). With body
        // zeroed the magnitudes are tiny; main signal is the sign of the
        // seed predictor.
        assert!(pcm[0] >= 2048 - 64);
        assert!(pcm[1] <= -1920 + 64);
    }
}
