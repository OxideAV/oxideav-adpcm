#![no_main]

//! Coverage-guided fuzz harness for
//! `oxideav_adpcm::encoder::ima_qt_encode_block` (Apple `ima4` block
//! encoder) and its reference-quantizer variant.
//!
//! The QT block geometry is spec-fixed: 64 samples per channel produce
//! one 34-byte block per channel. First fuzz byte picks 1 or 2 channels;
//! four more seed a hostile carried codec state (arbitrary predictor +
//! step index) for the reference leg; the rest is read as little-endian
//! i16 PCM (zero-padded when short). Adversarial bit patterns in the
//! per-block predictor seed + step-index heuristic (search leg) and in
//! the carried state (reference leg) are the interesting fuzz surface.
//! The reference output must always parse back through the block
//! decoder.

use libfuzzer_sys::fuzz_target;
use oxideav_adpcm::encoder;
use oxideav_adpcm::ima_wav::ImaCodecState;

const QT_SAMPLES_PER_BLOCK: usize = 64;

fuzz_target!(|data: &[u8]| {
    if data.len() < 5 {
        return;
    }
    let channels = ((data[0] & 1) as usize) + 1;
    let seed_pred = i16::from_le_bytes([data[1], data[2]]) as i32;
    let seed_index = i16::from_le_bytes([data[3], data[4]]) as i32;
    let total_samples = QT_SAMPLES_PER_BLOCK * channels;
    let pcm_bytes = &data[5..];
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

    let mut states = vec![
        ImaCodecState {
            predictor: seed_pred,
            step_index: seed_index,
        };
        channels
    ];
    if let Ok(blk) = encoder::ima_qt_encode_block_reference(&pcm, channels, &mut states) {
        let decoded =
            oxideav_adpcm::ima_qt::decode_block(&blk, channels).expect("reference QT decode");
        assert_eq!(decoded.len(), total_samples);
    }
});
