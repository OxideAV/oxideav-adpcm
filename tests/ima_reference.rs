//! IMA reference-compressor conformance suite.
//!
//! Double-entry verification of the crate's IMA quantizer/expansion pair
//! against **independent transcriptions** of the two staged listings:
//!
//! - the IMA "Recommended Practices for Enhancing Digital Audio
//!   Compatibility" Rev 3.00, Appendix D §6.1 (compression) and §6.2
//!   (decompression) — the 4-bit reference algorithm, including its
//!   worked examples;
//! - the *DVI ADPCM Wave Type* specification's 3-bit and 4-bit
//!   encoding/decoding procedures and its block layout.
//!
//! The oracles below are transcribed here directly from the listings,
//! deliberately NOT calling into the crate, structured the way the
//! listings are (repeated-subtraction quantization loop, conditional-add
//! expansion, explicit overflow/index clamps). Every sweep then pins the
//! crate's implementation against the oracle state-for-state and — at
//! block level — byte-for-byte.
//!
//! The listing-shaped if/else clamps are the point of the transcription,
//! so the idiomatic-`clamp` lint is silenced file-wide.
#![allow(clippy::manual_clamp)]

use oxideav_adpcm::encoder::{
    ima_encode_block_3bit_reference, ima_encode_block_reference, ima_qt_encode_block_reference,
};
use oxideav_adpcm::ima_wav::{
    ima_expand_code3, ima_expand_nibble, ima_quantize_code3, ima_quantize_nibble, ImaCodecState,
};
use oxideav_adpcm::tables::IMA_STEP_SIZE;

// ---------------------------------------------------------------------------
// Oracle: Appendix D §6.1 / §6.2 transcribed as listed
// ---------------------------------------------------------------------------

/// §6.1 "Preinitialized variables": indexTable[16].
const ORACLE_INDEX_TABLE: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];

/// DVI 3-bit IndexTab[8].
const ORACLE_INDEX_TABLE_3: [i32; 8] = [-1, -1, 1, 2, -1, -1, 1, 2];

/// §6.1 "Calculation for each sample" — compression, transcribed with
/// the listing's own structure (three-iteration repeated-subtraction
/// quantization, repetitive-addition expansion, overflow checks, index
/// clamp). State is (predictedSample, index); returns newSample.
fn oracle_compress_4bit(predicted_sample: &mut i32, index: &mut i32, original_sample: i16) -> u8 {
    let stepsize = IMA_STEP_SIZE[*index as usize] as i32;

    // find difference from predicted sample; set sign bit
    let mut difference = original_sample as i32 - *predicted_sample;
    let mut new_sample: u8;
    if difference >= 0 {
        new_sample = 0;
    } else {
        new_sample = 8;
        difference = -difference;
    }
    // quantize difference down to four bits through repeated subtraction
    let mut mask: u8 = 4;
    let mut temp_stepsize = stepsize;
    for _ in 0..3 {
        if difference >= temp_stepsize {
            new_sample |= mask;
            difference -= temp_stepsize;
        }
        temp_stepsize >>= 1;
        mask >>= 1;
    }

    // compute new sample estimate predictedSample:
    // calculate difference = (newSample + 1/2) * stepsize/4 through
    // repetitive addition
    let mut difference = 0i32;
    if new_sample & 4 != 0 {
        difference += stepsize;
    }
    if new_sample & 2 != 0 {
        difference += stepsize >> 1;
    }
    if new_sample & 1 != 0 {
        difference += stepsize >> 2;
    }
    difference += stepsize >> 3;
    if new_sample & 8 != 0 {
        difference = -difference;
    }
    *predicted_sample += difference;
    // check for overflow
    if *predicted_sample > 32767 {
        *predicted_sample = 32767;
    } else if *predicted_sample < -32768 {
        *predicted_sample = -32768;
    }

    // compute new stepsize: adjust index into stepsize lookup table
    *index += ORACLE_INDEX_TABLE[new_sample as usize];
    if *index < 0 {
        *index = 0;
    } else if *index > 88 {
        *index = 88;
    }
    new_sample
}

/// §6.2 "Calculation for each sample" — decompression.
fn oracle_decompress_4bit(new_sample: &mut i32, index: &mut i32, original_sample: u8) -> i16 {
    let stepsize = IMA_STEP_SIZE[*index as usize] as i32;
    // calculate difference = (originalSample + 1/2) * stepsize/4
    let mut difference = 0i32;
    if original_sample & 4 != 0 {
        difference += stepsize;
    }
    if original_sample & 2 != 0 {
        difference += stepsize >> 1;
    }
    if original_sample & 1 != 0 {
        difference += stepsize >> 2;
    }
    difference += stepsize >> 3;
    if original_sample & 8 != 0 {
        difference = -difference;
    }
    *new_sample += difference;
    if *new_sample > 32767 {
        *new_sample = 32767;
    } else if *new_sample < -32768 {
        *new_sample = -32768;
    }
    *index += ORACLE_INDEX_TABLE[original_sample as usize];
    if *index < 0 {
        *index = 0;
    } else if *index > 88 {
        *index = 88;
    }
    *new_sample as i16
}

/// DVI 3-bit encoding procedure, transcribed as listed (sign split,
/// two threshold comparisons, expansion by conditional additions).
fn oracle_compress_3bit(pred_samp: &mut i32, index: &mut i32, samp_x: i16) -> u8 {
    let step = IMA_STEP_SIZE[*index as usize] as i32;
    let mut diff = samp_x as i32 - *pred_samp;
    let mut code: u8;
    if diff < 0 {
        code = 4;
        diff = -diff;
    } else {
        code = 0;
    }
    if diff >= step {
        code |= 2;
        diff -= step;
    }
    if diff >= step >> 1 {
        code |= 1;
    }

    // predict the current sample based on the sample code
    let mut diff = 0i32;
    if code & 2 != 0 {
        diff += step;
    }
    if code & 1 != 0 {
        diff += step >> 1;
    }
    diff += step >> 2;
    if code & 4 != 0 {
        diff = -diff;
    }
    *pred_samp += diff;
    if *pred_samp > 32767 {
        *pred_samp = 32767;
    } else if *pred_samp < -32768 {
        *pred_samp = -32768;
    }
    *index += ORACLE_INDEX_TABLE_3[code as usize];
    if *index < 0 {
        *index = 0;
    } else if *index > 88 {
        *index = 88;
    }
    code
}

/// DVI 3-bit decoding procedure, transcribed as listed.
fn oracle_decompress_3bit(samp: &mut i32, index: &mut i32, code: u8) -> i16 {
    let step = IMA_STEP_SIZE[*index as usize] as i32;
    let mut diff = 0i32;
    if code & 2 != 0 {
        diff += step;
    }
    if code & 1 != 0 {
        diff += step >> 1;
    }
    diff += step >> 2;
    if code & 4 != 0 {
        diff = -diff;
    }
    *samp += diff;
    if *samp > 32767 {
        *samp = 32767;
    } else if *samp < -32768 {
        *samp = -32768;
    }
    *index += ORACLE_INDEX_TABLE_3[code as usize];
    if *index < 0 {
        *index = 0;
    } else if *index > 88 {
        *index = 88;
    }
    *samp as i16
}

// ---------------------------------------------------------------------------
// Step-table cross-check
// ---------------------------------------------------------------------------

#[test]
fn step_table_matches_the_recommendation_listing() {
    // §6.1 stepsizeTable[89] first row, boundary rows and length —
    // transcribed from the listing rather than the crate table module.
    assert_eq!(IMA_STEP_SIZE.len(), 89);
    assert_eq!(
        &IMA_STEP_SIZE[..16],
        &[7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31]
    );
    assert_eq!(IMA_STEP_SIZE[24], 73);
    assert_eq!(IMA_STEP_SIZE[88], 32767);
    assert_eq!(
        &IMA_STEP_SIZE[80..],
        &[15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767]
    );
}

// ---------------------------------------------------------------------------
// Worked examples (the Recommendation's own numbers)
// ---------------------------------------------------------------------------

#[test]
fn oracle_reproduces_the_compression_worked_example() {
    // §6.1: 0x873F vs predicted 0x8700 at stepsize 73 / index 24 →
    // newSample 3, predictedSample 0x873F, index 23, stepsize 66.
    let mut p = 0x8700u16 as i16 as i32;
    let mut idx = 24i32;
    let code = oracle_compress_4bit(&mut p, &mut idx, 0x873Fu16 as i16);
    assert_eq!(code, 3);
    assert_eq!(p, 0x873Fu16 as i16 as i32);
    assert_eq!(idx, 23);
    assert_eq!(IMA_STEP_SIZE[23], 66);
}

#[test]
fn oracle_reproduces_the_decompression_worked_example() {
    // §6.2: code 0x3 from newSample[previous] 0x8700 at stepsize 73 /
    // index 24 → newSample 0x873F, index 23, stepsize 66.
    let mut s = 0x8700u16 as i16 as i32;
    let mut idx = 24i32;
    let out = oracle_decompress_4bit(&mut s, &mut idx, 0x3);
    assert_eq!(out, 0x873Fu16 as i16);
    assert_eq!(idx, 23);
}

// ---------------------------------------------------------------------------
// Sweeps: crate implementation == oracle, state for state
// ---------------------------------------------------------------------------

/// Tiny deterministic xorshift for the random sweeps.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn quantizer_matches_oracle_on_a_dense_grid() {
    // Every step index × a dense predictor/sample grid (edges included).
    let grid: Vec<i32> = {
        let mut g: Vec<i32> = (-32768..=32767).step_by(1543).collect();
        g.extend_from_slice(&[-32768, -32767, -1, 0, 1, 32766, 32767]);
        g
    };
    for idx0 in 0..=88i32 {
        for &p0 in &grid {
            for &s in &grid {
                let (mut po, mut io) = (p0, idx0);
                let co = oracle_compress_4bit(&mut po, &mut io, s as i16);
                let (mut pc, mut ic) = (p0, idx0);
                let cc = ima_quantize_nibble(&mut pc, &mut ic, s as i16);
                assert_eq!(
                    (co, po, io),
                    (cc, pc, ic),
                    "4-bit quantize diverged at idx={idx0} p={p0} s={s}"
                );
            }
        }
    }
}

#[test]
fn expansion_matches_oracle_exhaustively_over_codes() {
    // Every step index × every code × a dense predictor grid.
    let grid: Vec<i32> = {
        let mut g: Vec<i32> = (-32768..=32767).step_by(509).collect();
        g.extend_from_slice(&[-32768, -32767, -1, 0, 1, 32766, 32767]);
        g
    };
    for idx0 in 0..=88i32 {
        for code in 0u8..16 {
            for &p0 in &grid {
                let (mut po, mut io) = (p0, idx0);
                let so = oracle_decompress_4bit(&mut po, &mut io, code);
                let (mut pc, mut ic) = (p0, idx0);
                let sc = ima_expand_nibble(&mut pc, &mut ic, code);
                assert_eq!(
                    (so, po, io),
                    (sc, pc, ic),
                    "4-bit expand diverged at idx={idx0} p={p0} code={code}"
                );
            }
        }
    }
}

#[test]
fn quantizer_3bit_matches_oracle_on_a_dense_grid() {
    let grid: Vec<i32> = {
        let mut g: Vec<i32> = (-32768..=32767).step_by(1543).collect();
        g.extend_from_slice(&[-32768, -32767, -1, 0, 1, 32766, 32767]);
        g
    };
    for idx0 in 0..=88i32 {
        for &p0 in &grid {
            for &s in &grid {
                let (mut po, mut io) = (p0, idx0);
                let co = oracle_compress_3bit(&mut po, &mut io, s as i16);
                let (mut pc, mut ic) = (p0, idx0);
                let cc = ima_quantize_code3(&mut pc, &mut ic, s as i16);
                assert_eq!(
                    (co, po, io),
                    (cc, pc, ic),
                    "3-bit quantize diverged at idx={idx0} p={p0} s={s}"
                );
            }
        }
    }
}

#[test]
fn expansion_3bit_matches_oracle_exhaustively_over_codes() {
    let grid: Vec<i32> = {
        let mut g: Vec<i32> = (-32768..=32767).step_by(509).collect();
        g.extend_from_slice(&[-32768, -32767, -1, 0, 1, 32766, 32767]);
        g
    };
    for idx0 in 0..=88i32 {
        for code in 0u8..8 {
            for &p0 in &grid {
                let (mut po, mut io) = (p0, idx0);
                let so = oracle_decompress_3bit(&mut po, &mut io, code);
                let (mut pc, mut ic) = (p0, idx0);
                let sc = ima_expand_code3(&mut pc, &mut ic, code);
                assert_eq!(
                    (so, po, io),
                    (sc, pc, ic),
                    "3-bit expand diverged at idx={idx0} p={p0} code={code}"
                );
            }
        }
    }
}

#[test]
fn long_random_walk_stays_in_lockstep_with_the_oracle() {
    // 200k random samples through one continuously-carried state pair —
    // any single-step divergence would compound and trip immediately.
    let mut rng = Rng(0xADCF_00D5_1234_9876);
    let (mut po, mut io) = (0i32, 0i32);
    let (mut pc, mut ic) = (0i32, 0i32);
    let (mut po3, mut io3) = (0i32, 0i32);
    let (mut pc3, mut ic3) = (0i32, 0i32);
    for i in 0..200_000 {
        let s = rng.next() as i16;
        let co = oracle_compress_4bit(&mut po, &mut io, s);
        let cc = ima_quantize_nibble(&mut pc, &mut ic, s);
        assert_eq!((co, po, io), (cc, pc, ic), "4-bit walk diverged at {i}");
        let co3 = oracle_compress_3bit(&mut po3, &mut io3, s);
        let cc3 = ima_quantize_code3(&mut pc3, &mut ic3, s);
        assert_eq!(
            (co3, po3, io3),
            (cc3, pc3, ic3),
            "3-bit walk diverged at {i}"
        );
    }
}

// ---------------------------------------------------------------------------
// Block level: oracle assembler == crate block encoder, byte for byte
// ---------------------------------------------------------------------------

/// Assemble a sequence of 4-bit IMA-WAV blocks with the oracle
/// compressor and the DVI block layout (Samp0 + index header per
/// channel, 4-byte channel groups, low nibble first), carrying the
/// index across blocks as the listing prescribes.
fn oracle_encode_stream_4bit(pcm: &[i16], channels: usize, block_size: usize) -> Vec<u8> {
    let header_len = 4 * channels;
    let group_bytes = 4 * channels;
    let groups = (block_size - header_len) / group_bytes;
    let samples_per_channel = 1 + groups * 8;
    let per_block = samples_per_channel * channels;

    let mut pred = vec![0i32; channels];
    let mut index = vec![0i32; channels];
    let mut out = Vec::new();
    for block in pcm.chunks(per_block) {
        // Header: Samp0 verbatim + carried index.
        for ch in 0..channels {
            let samp0 = block[ch];
            pred[ch] = samp0 as i32;
            out.extend_from_slice(&samp0.to_le_bytes());
            out.push(index[ch] as u8);
            out.push(0);
        }
        // Body: per group, per channel, 4 bytes of low-nibble-first codes.
        for g in 0..groups {
            for ch in 0..channels {
                for i in 0..4 {
                    let lo_idx = 1 + g * 8 + i * 2;
                    let lo = oracle_compress_4bit(
                        &mut pred[ch],
                        &mut index[ch],
                        block[lo_idx * channels + ch],
                    );
                    let hi = oracle_compress_4bit(
                        &mut pred[ch],
                        &mut index[ch],
                        block[(lo_idx + 1) * channels + ch],
                    );
                    out.push((hi << 4) | lo);
                }
            }
        }
    }
    out
}

fn test_signal(n: usize, channels: usize) -> Vec<i16> {
    // A moving mix of tones + a step transient per channel, exercising
    // both the fine rungs and ladder saturation.
    let mut pcm = Vec::with_capacity(n * channels);
    for i in 0..n {
        for ch in 0..channels {
            let f = 220.0 * (ch as f64 + 1.0);
            let t = i as f64 / 22050.0;
            let mut s = (2.0 * std::f64::consts::PI * f * t).sin() * 9000.0
                + (2.0 * std::f64::consts::PI * 3.1 * f * t).sin() * 3000.0;
            if (i / 400) % 2 == 1 {
                s += 8000.0; // block-scale DC step transient
            }
            pcm.push(s.round().clamp(-32768.0, 32767.0) as i16);
        }
    }
    pcm
}

#[test]
fn block_encoder_matches_oracle_stream_mono_and_stereo() {
    for channels in [1usize, 2] {
        let header_len = 4 * channels;
        let groups = (256 - header_len) / (4 * channels);
        let samples_per_channel = 1 + groups * 8;
        let n_blocks = 5;
        let pcm = test_signal(samples_per_channel * n_blocks, channels);

        let oracle = oracle_encode_stream_4bit(&pcm, channels, 256);

        let mut states = vec![ImaCodecState::default(); channels];
        let mut ours = Vec::new();
        for chunk in pcm.chunks(samples_per_channel * channels) {
            ours.extend_from_slice(
                &ima_encode_block_reference(chunk, channels, 256, &mut states).unwrap(),
            );
        }
        assert_eq!(
            ours, oracle,
            "{channels}ch: crate block stream != oracle stream"
        );
    }
}

#[test]
fn qt_block_encoder_nibbles_match_oracle() {
    // The QT framing wraps the same §6.1 quantizer: verify each block's
    // 32 body bytes against the oracle run from the preamble state, and
    // the preamble against the top-9-bit seed + carried index.
    let n_blocks = 4;
    let pcm = test_signal(64 * n_blocks, 1);
    let mut states = vec![ImaCodecState::default(); 1];
    let mut carried_index = 0i32;
    for chunk in pcm.chunks(64) {
        let blk = ima_qt_encode_block_reference(chunk, 1, &mut states).unwrap();
        let preamble = u16::from_be_bytes([blk[0], blk[1]]);
        let seed = (preamble & 0xFF80) as i16 as i32;
        assert_eq!(seed, (chunk[0] as i32) & !0x7F, "preamble predictor seed");
        assert_eq!((preamble & 0x7F) as i32, carried_index, "preamble index");

        let mut p = seed;
        let mut idx = carried_index;
        for (i, &byte) in blk[2..34].iter().enumerate() {
            let lo = oracle_compress_4bit(&mut p, &mut idx, chunk[i * 2]);
            let hi = oracle_compress_4bit(&mut p, &mut idx, chunk[i * 2 + 1]);
            assert_eq!(byte, (hi << 4) | lo, "body byte {i}");
        }
        carried_index = idx;
        assert_eq!(states[0].step_index, idx, "carried state index");
    }
}

// ---------------------------------------------------------------------------
// Quality: the reference stream decodes within spec-expected error
// ---------------------------------------------------------------------------

fn rms(a: &[i16], b: &[i16]) -> f64 {
    let mut sse = 0f64;
    for (x, y) in a.iter().zip(b) {
        let d = *x as f64 - *y as f64;
        sse += d * d;
    }
    (sse / a.len() as f64).sqrt()
}

#[test]
fn reference_round_trip_quality_and_search_comparison() {
    // The reference ladder round-trips a broadband signal within a
    // bounded RMS through the shipped decoder; the default search
    // encoder must not be worse than the reference on the same signal
    // (that superiority is the reason Search stays the default).
    let samples_per_block = 1 + 63 * 8;
    let n_blocks = 8;
    let pcm = test_signal(samples_per_block * n_blocks, 1);

    let mut states = vec![ImaCodecState::default(); 1];
    let mut ref_decoded = Vec::new();
    let mut search_decoded = Vec::new();
    for chunk in pcm.chunks(samples_per_block) {
        let blk = ima_encode_block_reference(chunk, 1, 256, &mut states).unwrap();
        ref_decoded.extend_from_slice(&oxideav_adpcm::ima_wav::decode_block(&blk, 1).unwrap());
        let blk_s = oxideav_adpcm::encoder::ima_encode_block(chunk, 1, 256).unwrap();
        search_decoded.extend_from_slice(&oxideav_adpcm::ima_wav::decode_block(&blk_s, 1).unwrap());
    }
    let rms_ref = rms(&ref_decoded, &pcm);
    let rms_search = rms(&search_decoded, &pcm);
    assert!(rms_ref < 2500.0, "reference round-trip RMS {rms_ref}");
    assert!(
        rms_search <= rms_ref * 1.05,
        "search RMS {rms_search} materially worse than reference RMS {rms_ref}"
    );
}

#[test]
fn reference_3bit_stream_decodes_through_crate_decoder() {
    // End-to-end 3-bit: oracle-compressed codes packed per the crate's
    // 3-bit group layout decode bit-exactly through decode_block_3bit —
    // and the crate's own reference 3-bit blocks equal the oracle's
    // reconstruction trajectory.
    let block_size = 4 + 20 * 12; // mono: 20 groups
    let samples_per_block = 1 + 20 * 32;
    let pcm = test_signal(samples_per_block * 3, 1);
    let mut states = vec![ImaCodecState::default(); 1];
    let mut i_o = 0i32;
    for chunk in pcm.chunks(samples_per_block) {
        let blk = ima_encode_block_3bit_reference(chunk, 1, block_size, &mut states).unwrap();
        let decoded = oxideav_adpcm::ima_wav::decode_block_3bit(&blk, 1).unwrap();
        // Oracle trajectory for the same input, carried across blocks.
        // The listing's compressor tracks its own reconstruction
        // (PredSamp), which the §-matching decompressor reproduces — so
        // the decoded block must equal [Samp0, PredSamp after each
        // sample].
        let mut p_o = chunk[0] as i32; // header re-seeds the predictor
        let mut expect = vec![chunk[0]];
        for &s in &chunk[1..] {
            let _code = oracle_compress_3bit(&mut p_o, &mut i_o, s);
            expect.push(p_o as i16);
        }
        assert_eq!(decoded, expect, "3-bit decode != oracle trajectory");
    }
}
