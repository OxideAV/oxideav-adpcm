#![no_main]

//! Coverage-guided fuzz harness for `oxideav_adpcm::encoder::encode_block`
//! (MS-ADPCM block encoder).
//!
//! Symmetric counterpart to the existing decode fuzz target
//! ([`decode_packet_ms`]): drives the encoder's PCM → ADPCM-block path
//! with arbitrary input. The first fuzz byte chooses 1- or 2-channel
//! output; the next two bytes pick a body-byte budget in `[0, 65535]`
//! which we add to the per-channel header length to get a `block_size`;
//! the remainder of the slice is interpreted as little-endian `i16` PCM
//! (padded out to the encoder's exact-sample-count expectation with the
//! last seen sample replicated, mirroring the trait encoder's tail-pad
//! behaviour).
//!
//! Contract under test: every fuzz slice must produce `Ok(Vec<u8>)` or
//! `Err(Error::Invalid | Error::Unsupported)`. Panics, debug-mode
//! integer overflow, allocator-overflowing length arithmetic, and
//! index-out-of-bounds are all bugs in the encoder.

use libfuzzer_sys::fuzz_target;
use oxideav_adpcm::encoder;

fuzz_target!(|data: &[u8]| {
    if data.len() < 3 {
        return;
    }
    let channels = ((data[0] & 1) as usize) + 1;
    // Bound the block size: header is 7 * channels, body up to ~16 KiB so a
    // pathological allocation request can't OOM the fuzz worker.
    let body_budget = u16::from_le_bytes([data[1], data[2]]) as usize % 16_384;
    let header_len = 7 * channels;
    let block_size = header_len + body_budget;
    // body_len * 2 / channels samples after the 2 prelude samples.
    let body_len = block_size - header_len;
    let samples_per_channel = 2 + (body_len * 2) / channels;
    let total_samples = samples_per_channel * channels;
    // Build PCM: each consecutive pair of fuzz bytes is one i16. Pad with
    // zero if the slice is short.
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
    let _ = encoder::encode_block(&pcm, channels, block_size);
});
