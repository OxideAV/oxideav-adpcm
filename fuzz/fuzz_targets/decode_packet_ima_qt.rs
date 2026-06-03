#![no_main]

//! Coverage-guided fuzz harness for `oxideav_adpcm::ima_qt::decode_block`.
//!
//! IMA-QT differs from IMA-WAV in that the per-block frame is the
//! spec-mandated 34 bytes per channel — the decoder has a hard length
//! gate up front, so most short / mis-aligned slices return `Err`
//! quickly. The interesting fuzz surface is the 9-bit predictor +
//! 7-bit step-index preamble interleaved with the body nibble stream.

use libfuzzer_sys::fuzz_target;
use oxideav_adpcm::ima_qt;

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    // First fuzz byte picks the channel count (1 or 2 — the only ones
    // the decoder accepts).
    let channels = ((data[0] & 1) as usize) + 1;
    let block = &data[1..];
    let _ = ima_qt::decode_block(block, channels);
});
