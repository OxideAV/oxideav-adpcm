#![no_main]

//! Coverage-guided fuzz harness for the three **stream-oriented**
//! ADPCM decoders this crate ships: Yamaha ADPCM-B (`yamaha`), Yamaha
//! ADPCM-A (`yamaha_a`), and OKI/Dialogic VOX (`dialogic`).
//!
//! Unlike the block-oriented MS / IMA-WAV / IMA-QT decoders, the
//! stream decoders take a flat nibble stream + a caller-supplied
//! per-channel `Channel` state and emit one sample per nibble. Their
//! validation surface is therefore the **state seed** (predictor +
//! step_index can be out-of-spec on input, and the decoders must clamp
//! rather than panic) plus the nibble stream itself.
//!
//! One fuzz byte picks the variant; the rest is the nibble payload.
//! State is seeded from a deterministic xorshift32 driven by the next
//! 8 fuzz bytes so the fuzzer can reach cold-start, mid-stream, and
//! near-saturation step indices on demand.

use libfuzzer_sys::fuzz_target;
use oxideav_adpcm::{dialogic, yamaha, yamaha_a};

/// xorshift32 — same minimal PRNG the in-tree fuzz harness uses to
/// seed channel state.
fn xorshift32(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

fn seed_state(seed_bytes: &[u8]) -> (i32, i32) {
    let mut rng = if seed_bytes.is_empty() {
        0x12345678u32
    } else {
        // Mix up to 4 bytes into a u32 seed.
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
    let step_index = (xorshift32(&mut rng) as i32) % 100 - 5; // intentionally
                                                              // includes out-of-range values so the clamp paths fire.
    (predictor, step_index)
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 10 {
        return;
    }
    let variant = data[0] % 6;
    let channels = ((data[1] & 1) as usize) + 1;
    let seed = &data[2..10];
    let payload = &data[10..];

    match variant {
        0 | 1 => {
            // Yamaha ADPCM-B (mono / stereo).
            let mut state = vec![yamaha::Channel::default(); channels];
            let (p, si) = seed_state(seed);
            for ch in state.iter_mut() {
                ch.predictor = p;
                ch.step = si;
            }
            let _ = yamaha::decode_packet(payload, &mut state);
        }
        2 => {
            // Yamaha ADPCM-A (single channel by chip design).
            let mut state = [yamaha_a::Channel::default()];
            let (p, si) = seed_state(seed);
            state[0].acc = p;
            state[0].step_index = si;
            let _ = yamaha_a::decode_packet(payload, &mut state, yamaha_a::Output::Wide16);
        }
        3 => {
            // Yamaha ADPCM-A with Native12 output (different shift path).
            let mut state = [yamaha_a::Channel::default()];
            let (p, si) = seed_state(seed);
            state[0].acc = p;
            state[0].step_index = si;
            let _ = yamaha_a::decode_packet(payload, &mut state, yamaha_a::Output::Native12);
        }
        4 => {
            // Dialogic VOX, HiFirst (canonical .vox / MSM6295).
            let mut state = vec![dialogic::Channel::default(); channels];
            let (p, si) = seed_state(seed);
            for ch in state.iter_mut() {
                ch.predictor = p;
                ch.step_index = si;
            }
            let _ = dialogic::decode_packet(
                payload,
                &mut state,
                dialogic::NibbleOrder::HiFirst,
                dialogic::Output::Wide16,
            );
        }
        _ => {
            // Dialogic VOX, LoFirst (MSM6258).
            let mut state = vec![dialogic::Channel::default(); channels];
            let (p, si) = seed_state(seed);
            for ch in state.iter_mut() {
                ch.predictor = p;
                ch.step_index = si;
            }
            let _ = dialogic::decode_packet(
                payload,
                &mut state,
                dialogic::NibbleOrder::LoFirst,
                dialogic::Output::Native12,
            );
        }
    }
});
