#![no_main]

//! Coverage-guided fuzz harness for the IMA-ADPCM-WAV block encoders
//! (`oxideav_adpcm::encoder::ima_encode_block` + the 3-bit and
//! reference-quantizer variants).
//!
//! First fuzz byte picks the channel count in `1..=8`; the next two
//! bytes pick a group budget in `[0, 4095]` from which the block sizes
//! are derived (4-bit: header `4*ch` + body `groups * 4 * ch`; 3-bit:
//! body `groups * 12 * ch`); four more bytes seed a hostile carried
//! codec state (arbitrary predictor + step index) for the reference
//! legs; the remainder is interpreted as little-endian i16 PCM. The
//! exact-sample-count invariant the encoders demand is honoured by
//! padding with zero — that lets the fuzzer exercise the body-write
//! paths rather than bouncing on the size-mismatch gate. Reference-leg
//! outputs must always parse back through the matching block decoder.

use libfuzzer_sys::fuzz_target;
use oxideav_adpcm::encoder;
use oxideav_adpcm::ima_wav::{self, ImaCodecState};

fuzz_target!(|data: &[u8]| {
    if data.len() < 7 {
        return;
    }
    let channels = ((data[0] & 0x07) as usize) + 1;
    let group_budget = (u16::from_le_bytes([data[1], data[2]]) as usize) % 4096;
    // Hostile carried state for the reference legs: arbitrary predictor,
    // arbitrary (possibly out-of-range) step index.
    let seed_pred = i16::from_le_bytes([data[3], data[4]]) as i32;
    let seed_index = i16::from_le_bytes([data[5], data[6]]) as i32;
    let pcm_bytes = &data[7..];

    let header_len = 4 * channels;

    // ---- 4-bit legs ----
    let group_bytes = 4 * channels;
    let body_len = group_budget * group_bytes;
    let block_size = header_len + body_len;
    let groups = body_len / group_bytes;
    let samples_per_channel = 1 + groups * 8;
    let total_samples = samples_per_channel * channels;
    // Bound total PCM to ~32 KiB so allocator pressure stays sane.
    if total_samples <= 16_384 {
        let mut pcm: Vec<i16> = Vec::with_capacity(total_samples);
        for c in pcm_bytes.chunks(2).take(total_samples) {
            let lo = c[0];
            let hi = if c.len() > 1 { c[1] } else { 0 };
            pcm.push(i16::from_le_bytes([lo, hi]));
        }
        while pcm.len() < total_samples {
            pcm.push(0);
        }
        let _ = encoder::ima_encode_block(&pcm, channels, block_size);

        let mut states = vec![
            ImaCodecState {
                predictor: seed_pred,
                step_index: seed_index,
            };
            channels
        ];
        if let Ok(blk) = encoder::ima_encode_block_reference(&pcm, channels, block_size, &mut states)
        {
            // The reference stream must always parse back.
            let decoded = ima_wav::decode_block(&blk, channels).expect("reference 4-bit decode");
            assert_eq!(decoded.len(), total_samples);
        }
    }

    // ---- 3-bit legs (12-byte groups; reuse the same budget + PCM) ----
    let group_bytes3 = 12 * channels;
    let body_len3 = (group_budget % 1366) * group_bytes3;
    let block_size3 = header_len + body_len3;
    let groups3 = body_len3 / group_bytes3;
    let samples_per_channel3 = 1 + groups3 * 32;
    let total_samples3 = samples_per_channel3 * channels;
    if total_samples3 <= 16_384 {
        let mut pcm: Vec<i16> = Vec::with_capacity(total_samples3);
        for c in pcm_bytes.chunks(2).take(total_samples3) {
            let lo = c[0];
            let hi = if c.len() > 1 { c[1] } else { 0 };
            pcm.push(i16::from_le_bytes([lo, hi]));
        }
        while pcm.len() < total_samples3 {
            pcm.push(0);
        }
        let _ = encoder::ima_encode_block_3bit(&pcm, channels, block_size3);

        let mut states = vec![
            ImaCodecState {
                predictor: seed_pred,
                step_index: seed_index,
            };
            channels
        ];
        if let Ok(blk) =
            encoder::ima_encode_block_3bit_reference(&pcm, channels, block_size3, &mut states)
        {
            let decoded =
                ima_wav::decode_block_3bit(&blk, channels).expect("reference 3-bit decode");
            assert_eq!(decoded.len(), total_samples3);
        }
    }
});
