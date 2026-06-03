#![no_main]

//! Coverage-guided fuzz harness for `oxideav_adpcm::ima_wav::decode_block`.
//!
//! Mirrors the [`decode_packet_ms`] target's contract for the IMA-ADPCM
//! WAV variant: every byte slice must produce `Ok` or `Err`, never
//! panic / overflow / OOM.
//!
//! The IMA-WAV decoder accepts 1..=8 channels and validates the
//! step-index header byte against the 0..=88 spec range up front; the
//! body length is required to be a whole number of 4-byte groups per
//! channel. Both validation paths are covered here.

use libfuzzer_sys::fuzz_target;
use oxideav_adpcm::ima_wav;

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    // First fuzz byte picks the channel count in 1..=8.
    let channels = ((data[0] & 0x07) as usize) + 1;
    let block = &data[1..];
    let _ = ima_wav::decode_block(block, channels);
});
