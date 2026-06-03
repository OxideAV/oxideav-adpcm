#![no_main]

//! Coverage-guided fuzz harness for
//! `oxideav_adpcm::encoder::ima_encode_block` (IMA-ADPCM-WAV block
//! encoder).
//!
//! First fuzz byte picks the channel count in `1..=8`; the next two
//! bytes pick a group budget in `[0, 4095]` from which the block size
//! is derived (header `4*ch` + body `groups * 4 * ch`); the remainder
//! is interpreted as little-endian i16 PCM. The exact-sample-count
//! invariant the encoder demands (`samples.len() == samples_per_block
//! * channels`) is honoured by padding with zero — that lets the fuzzer
//! exercise the body-write path rather than bouncing on the size-mismatch
//! gate.

use libfuzzer_sys::fuzz_target;
use oxideav_adpcm::encoder;

fuzz_target!(|data: &[u8]| {
    if data.len() < 3 {
        return;
    }
    let channels = ((data[0] & 0x07) as usize) + 1;
    let group_budget = (u16::from_le_bytes([data[1], data[2]]) as usize) % 4096;
    let header_len = 4 * channels;
    let group_bytes = 4 * channels;
    let body_len = group_budget * group_bytes;
    let block_size = header_len + body_len;
    let groups = body_len / group_bytes;
    let samples_per_channel = 1 + groups * 8;
    let total_samples = samples_per_channel * channels;
    // Bound total PCM to ~32 KiB so allocator pressure stays sane.
    if total_samples > 16_384 {
        return;
    }
    let pcm_bytes = &data[3..];
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
});
