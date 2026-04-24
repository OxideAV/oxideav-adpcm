//! Microsoft ADPCM decoder (`adpcm_ms`, WAVEFORMATEX tag `0x0002`).
//!
//! Algorithm (transcribed from the public Microsoft ADPCM spec):
//!
//! 1. Each block starts with a header: per-channel predictor index byte,
//!    then per-channel 16-bit signed initial delta, then two 16-bit signed
//!    initial samples `sample1` and `sample2` (sample1 is decoded first).
//! 2. The rest of the block is a stream of 4-bit nibbles, **hi nibble
//!    first** within each byte, interleaved by channel in the stereo case.
//! 3. For each nibble:
//!    - `signed_nibble = (nibble ^ 8) - 8`  (4-bit two's complement)
//!    - `predicted = (sample1 * coef1 + sample2 * coef2) / 256`
//!    - `new_sample = predicted + signed_nibble * delta`
//!    - clamp to i16, append as output
//!    - shift history: `sample2 = sample1; sample1 = new_sample`
//!    - `delta = max(16, (delta * adaptation[nibble]) / 256)`
//!
//! The block lays down `sample2` then `sample1` as the first two PCM
//! outputs before any nibble is processed, so each block produces
//! `2 + 2 * (body_bytes / channels)` samples per channel (2 prelude + one
//! sample per nibble, each body byte carrying two nibbles).

use crate::tables::{MS_ADAPTATION, MS_ADAPT_COEFF1, MS_ADAPT_COEFF2};
use oxideav_core::{Error, Result};

/// Per-channel running state carried across the nibbles in a block.
#[derive(Clone, Copy, Debug)]
struct ChannelState {
    coef1: i32,
    coef2: i32,
    delta: i32,
    sample1: i32,
    sample2: i32,
}

fn clamp_i16(x: i32) -> i16 {
    x.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

fn decode_nibble(st: &mut ChannelState, nibble: u8) -> i16 {
    // Sign-extend 4-bit → i32 via (n ^ 8) - 8.
    let signed = ((nibble as i32) ^ 8) - 8;

    // Linear predictor (scaled by 256).
    let predicted = (st.sample1 * st.coef1 + st.sample2 * st.coef2) >> 8;

    // Add error term.
    let new = predicted + signed * st.delta;
    let out = clamp_i16(new);

    // Shift history.
    st.sample2 = st.sample1;
    st.sample1 = out as i32;

    // Update delta (adapt step).
    let mut d = (MS_ADAPTATION[nibble as usize] * st.delta) >> 8;
    if d < 16 {
        d = 16;
    }
    st.delta = d;

    out
}

/// Decode a single Microsoft-ADPCM block with `channels` channels.
///
/// Returns a flat interleaved i16 vector (L, R, L, R, …) of `samples *
/// channels` i16 values. The sample count is derived from the block size
/// per the standard formula:
///
/// `samples_per_channel = 2 + (body_bytes * 2) / channels`
///
/// where body_bytes is the block size minus the header (7 bytes mono /
/// 14 bytes stereo).
pub fn decode_block(block: &[u8], channels: usize) -> Result<Vec<i16>> {
    if channels == 0 || channels > 2 {
        return Err(Error::unsupported(format!(
            "adpcm_ms: channel count {channels} not supported (1 or 2)"
        )));
    }
    let header_len = 7 * channels;
    if block.len() < header_len {
        return Err(Error::invalid(format!(
            "adpcm_ms: block too short ({} < header {header_len})",
            block.len()
        )));
    }

    // Parse header.
    let mut states: [ChannelState; 2] = [
        ChannelState {
            coef1: 0,
            coef2: 0,
            delta: 0,
            sample1: 0,
            sample2: 0,
        },
        ChannelState {
            coef1: 0,
            coef2: 0,
            delta: 0,
            sample1: 0,
            sample2: 0,
        },
    ];

    // Predictor indices (one byte per channel).
    for (ch, st) in states.iter_mut().take(channels).enumerate() {
        let pi = block[ch] as usize;
        if pi > 6 {
            return Err(Error::invalid(format!(
                "adpcm_ms: predictor index {pi} out of range 0..=6"
            )));
        }
        st.coef1 = MS_ADAPT_COEFF1[pi];
        st.coef2 = MS_ADAPT_COEFF2[pi];
    }

    // Initial delta (i16 LE, one per channel).
    for (ch, st) in states.iter_mut().take(channels).enumerate() {
        let off = channels + ch * 2;
        let d = i16::from_le_bytes([block[off], block[off + 1]]) as i32;
        st.delta = d;
    }

    // Initial sample1 (i16 LE, one per channel).
    for (ch, st) in states.iter_mut().take(channels).enumerate() {
        let off = channels + 2 * channels + ch * 2;
        let s = i16::from_le_bytes([block[off], block[off + 1]]) as i32;
        st.sample1 = s;
    }

    // Initial sample2 (i16 LE, one per channel).
    for (ch, st) in states.iter_mut().take(channels).enumerate() {
        let off = channels + 4 * channels + ch * 2;
        let s = i16::from_le_bytes([block[off], block[off + 1]]) as i32;
        st.sample2 = s;
    }

    let body = &block[header_len..];
    // Prelude: `sample2` first, then `sample1` per channel (this matches
    // the ordering Microsoft's spec puts into the decoded stream — they
    // are the oldest two samples of the block).
    let mut out = Vec::with_capacity((body.len() * 2 + 2 * channels) * 2);
    for ch in 0..channels {
        out.push(states[ch].sample2 as i16);
    }
    for ch in 0..channels {
        out.push(states[ch].sample1 as i16);
    }

    // Nibble stream. For each byte emit hi-nibble then lo-nibble; nibbles
    // alternate channels (L, R, L, R, ...) in stereo.
    let mut ch_cursor: usize = 0;
    for &byte in body {
        let hi = (byte >> 4) & 0x0F;
        let lo = byte & 0x0F;

        let s = decode_nibble(&mut states[ch_cursor], hi);
        out.push(s);
        ch_cursor = (ch_cursor + 1) % channels;

        let s = decode_nibble(&mut states[ch_cursor], lo);
        out.push(s);
        ch_cursor = (ch_cursor + 1) % channels;
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand-built minimal mono block that decodes to a known prefix.
    /// Predictor index 0 → coef1=256, coef2=0, so `predicted = sample1`.
    #[test]
    fn mono_minimal_block_header_only() {
        let mut block = Vec::new();
        block.push(0); // predictor index 0
        block.extend_from_slice(&100i16.to_le_bytes()); // initial delta
        block.extend_from_slice(&500i16.to_le_bytes()); // sample1
        block.extend_from_slice(&300i16.to_le_bytes()); // sample2
                                                        // No body bytes.
        let pcm = decode_block(&block, 1).unwrap();
        // Prelude emits sample2 then sample1.
        assert_eq!(pcm, vec![300, 500]);
    }

    #[test]
    fn mono_block_decodes_zero_nibbles_as_pure_prediction() {
        // With coef1=256 coef2=0 and nibble=0, new_sample = sample1.
        // So the predictor output is constant after the prelude.
        let mut block = Vec::new();
        block.push(0);
        block.extend_from_slice(&16i16.to_le_bytes());
        block.extend_from_slice(&1000i16.to_le_bytes()); // sample1
        block.extend_from_slice(&2000i16.to_le_bytes()); // sample2
        block.push(0x00); // two zero nibbles
        let pcm = decode_block(&block, 1).unwrap();
        // prelude: [sample2, sample1] = [2000, 1000]; then two nibble=0
        // decodes → both predict = sample1 = 1000 each iteration.
        assert_eq!(pcm, vec![2000, 1000, 1000, 1000]);
    }

    #[test]
    fn rejects_bad_predictor_index() {
        let mut block = Vec::new();
        block.push(7); // out of range
        block.extend_from_slice(&16i16.to_le_bytes());
        block.extend_from_slice(&0i16.to_le_bytes());
        block.extend_from_slice(&0i16.to_le_bytes());
        assert!(decode_block(&block, 1).is_err());
    }

    #[test]
    fn rejects_truncated_block() {
        assert!(decode_block(&[0u8; 3], 1).is_err());
        assert!(decode_block(&[0u8; 13], 2).is_err());
    }

    #[test]
    fn stereo_prelude_interleaves_channels() {
        // 14-byte stereo header, no body bytes.
        let mut block = Vec::new();
        block.extend_from_slice(&[0, 0]); // predictor L, R
        block.extend_from_slice(&50i16.to_le_bytes()); // delta L
        block.extend_from_slice(&60i16.to_le_bytes()); // delta R
        block.extend_from_slice(&100i16.to_le_bytes()); // s1 L
        block.extend_from_slice(&200i16.to_le_bytes()); // s1 R
        block.extend_from_slice(&300i16.to_le_bytes()); // s2 L
        block.extend_from_slice(&400i16.to_le_bytes()); // s2 R
        let pcm = decode_block(&block, 2).unwrap();
        // Interleaved [L_s2, R_s2, L_s1, R_s1].
        assert_eq!(pcm, vec![300, 400, 100, 200]);
    }
}
