#![no_main]

//! Coverage-guided fuzz harness for
//! `oxideav_adpcm::encoder::ima_qt_encode_block` (Apple `ima4` block
//! encoder).
//!
//! The QT block geometry is spec-fixed: 64 samples per channel produce
//! one 34-byte block per channel. First fuzz byte picks 1 or 2 channels;
//! the rest is read as little-endian i16 PCM (zero-padded when short).
//! Adversarial bit patterns in the per-block predictor seed + step-index
//! heuristic (the mean-|Δ| pump that picks the initial step index) are
//! the most interesting fuzz surface here.

use libfuzzer_sys::fuzz_target;
use oxideav_adpcm::encoder;

const QT_SAMPLES_PER_BLOCK: usize = 64;

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let channels = ((data[0] & 1) as usize) + 1;
    let total_samples = QT_SAMPLES_PER_BLOCK * channels;
    let pcm_bytes = &data[1..];
    let mut pcm: Vec<i16> = Vec::with_capacity(total_samples);
    for c in pcm_bytes.chunks(2).take(total_samples) {
        let lo = c[0];
        let hi = if c.len() > 1 { c[1] } else { 0 };
        pcm.push(i16::from_le_bytes([lo, hi]));
    }
    while pcm.len() < total_samples {
        pcm.push(0);
    }
    let _ = encoder::ima_qt_encode_block(&pcm, channels);
});
