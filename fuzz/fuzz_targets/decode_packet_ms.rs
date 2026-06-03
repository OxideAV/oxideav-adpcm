#![no_main]

//! Coverage-guided fuzz harness for `oxideav_adpcm::ms::decode_block`.
//!
//! The in-tree `tests/decoder_fuzz.rs` already drives the MS-ADPCM
//! decoder through a deterministic in-process LCG + a hand-enumerated
//! set of truncated-prefix / out-of-range-predictor / body-misalignment
//! cases. This libfuzzer target adds **coverage-guided** exploration
//! of the same surface so the corpus minimiser can find branches the
//! hand-built tests don't reach.
//!
//! Contract under test: every byte slice produces either `Ok(Vec<i16>)`
//! or `Err(Error::Invalid | Error::Unsupported)`. Panics, debug-mode
//! integer overflows, allocator-overflowing length arithmetic, and
//! index-out-of-bounds are all bugs.

use libfuzzer_sys::fuzz_target;
use oxideav_adpcm::ms;

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    // First fuzz byte picks the channel count (1 or 2 — the only ones
    // the decoder accepts); rest of the slice feeds the block body.
    let channels = ((data[0] & 1) as usize) + 1;
    let block = &data[1..];
    let _ = ms::decode_block(block, channels);
});
