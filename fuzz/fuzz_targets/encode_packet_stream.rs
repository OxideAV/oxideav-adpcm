#![no_main]

//! Coverage-guided fuzz harness for the three **stream-oriented** ADPCM
//! encoders this crate ships: Yamaha ADPCM-B (`yamaha::encode_packet`),
//! Yamaha ADPCM-A (`yamaha_a::encode_packet`), and OKI/Dialogic VOX
//! (`dialogic::encode_packet` + `encode_packet_wide16`).
//!
//! The contract under test mirrors the decode-side stream target: every
//! fuzz slice must produce a `Vec<u8>` of bounded size — no panics, no
//! debug-mode integer overflow, no allocator-overflowing length
//! arithmetic, no index-out-of-bounds. State seeded from an in-target
//! xorshift32 PRNG so the fuzzer can reach cold-start, mid-stream, and
//! near-saturation step indices on demand.

use libfuzzer_sys::fuzz_target;
use oxideav_adpcm::{dialogic, yamaha, yamaha_a};

/// xorshift32 — same minimal PRNG the decode-side stream target uses.
fn xorshift32(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

fn seed_pair(seed_bytes: &[u8]) -> (i32, i32) {
    let mut rng = if seed_bytes.is_empty() {
        0x12345678u32
    } else {
        let mut acc = 0u32;
        for (i, &b) in seed_bytes.iter().take(4).enumerate() {
            acc |= (b as u32) << (i * 8);
        }
        if acc == 0 {
            0x12345678
        } else {
            acc
        }
    };
    let predictor = (xorshift32(&mut rng) as i32) % 4096 - 2048;
    let step_index = (xorshift32(&mut rng) as i32) % 100 - 5;
    (predictor, step_index)
}

fn pcm_from_bytes(bytes: &[u8], max_samples: usize) -> Vec<i16> {
    let mut pcm: Vec<i16> = Vec::with_capacity(bytes.len() / 2);
    for c in bytes.chunks(2).take(max_samples) {
        let lo = c[0];
        let hi = if c.len() > 1 { c[1] } else { 0 };
        pcm.push(i16::from_le_bytes([lo, hi]));
    }
    pcm
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 10 {
        return;
    }
    let variant = data[0] % 6;
    let channels = ((data[1] & 1) as usize) + 1;
    let seed = &data[2..10];
    let payload = &data[10..];
    // Cap input PCM to 8192 samples so worst-case allocation stays bounded.
    let pcm = pcm_from_bytes(payload, 8192);

    match variant {
        0 | 1 => {
            // Yamaha ADPCM-B (mono / stereo). Sample-interleaved PCM in,
            // packed nibble stream out. Truncate PCM to a whole-channel
            // multiple so the encoder doesn't see a partial frame.
            let aligned_len = (pcm.len() / channels) * channels;
            let mut state = vec![yamaha::Channel::default(); channels];
            let (p, si) = seed_pair(seed);
            for ch in state.iter_mut() {
                ch.predictor = p;
                ch.step = si;
            }
            let _ = yamaha::encode_packet(&pcm[..aligned_len], &mut state);
        }
        2 => {
            // Yamaha ADPCM-A (mono — chip-internal codec).
            let mut state = [yamaha_a::Channel::default()];
            let (p, si) = seed_pair(seed);
            state[0].acc = p;
            state[0].step_index = si;
            let _ = yamaha_a::encode_packet(&pcm, &mut state, yamaha_a::Output::Wide16);
        }
        3 => {
            // Yamaha ADPCM-A Native12 — narrows differently before encode.
            let mut state = [yamaha_a::Channel::default()];
            let (p, si) = seed_pair(seed);
            state[0].acc = p;
            state[0].step_index = si;
            let _ = yamaha_a::encode_packet(&pcm, &mut state, yamaha_a::Output::Native12);
        }
        4 => {
            // Dialogic VOX, HiFirst nibble order (canonical .vox / MSM6295).
            let mut state = dialogic::Channel::default();
            let (p, si) = seed_pair(seed);
            state.predictor = p;
            state.step_index = si;
            let _ =
                dialogic::encode_packet_wide16(&pcm, &mut state, dialogic::NibbleOrder::HiFirst);
        }
        _ => {
            // Dialogic VOX, LoFirst nibble order (MSM6258) — 12-bit
            // direct quantisation, no wide-16 narrowing.
            let mut state = dialogic::Channel::default();
            let (p, si) = seed_pair(seed);
            state.predictor = p;
            state.step_index = si;
            let _ = dialogic::encode_packet(&pcm, &mut state, dialogic::NibbleOrder::LoFirst);
        }
    }
});
