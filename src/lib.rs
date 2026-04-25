//! ADPCM audio decoder family for oxideav.
//!
//! Covers the four common WAV / AVI / QuickTime ADPCM flavours:
//!
//! - **`adpcm_ms`** — Microsoft ADPCM (WAVEFORMATEX tag `0x0002`).
//! - **`adpcm_ima_wav`** — IMA/DVI ADPCM, WAV packaging (tag `0x0011`).
//! - **`adpcm_ima_qt`** — IMA ADPCM, Apple QuickTime packaging (fourcc
//!   `ima4`).
//! - **`adpcm_yamaha`** — Yamaha Y8950/YM2608/AICA ADPCM (tag `0x0020`).
//!
//! G.722 / G.726 / G.729 are *not* handled here — they live in their own
//! crates.
//!
//! # Registration
//!
//! Call [`register`] from an aggregator crate (or from application code):
//!
//! ```no_run
//! # use oxideav_core::CodecRegistry;
//! let mut reg = CodecRegistry::new();
//! oxideav_adpcm::register(&mut reg);
//! ```
//!
//! Each codec id is also wired to its canonical WAVEFORMATEX tag so an
//! AVI or WAV demuxer that calls
//! [`CodecResolver::resolve_tag`](oxideav_core::CodecResolver::resolve_tag)
//! will pick the right decoder automatically.
//!
//! # Specs and citations
//!
//! All tables in [`tables`] are normative constants transcribed from public
//! specifications — see the README for the specific documents. No decoder
//! source was read while writing this crate.

#![deny(unsafe_code)]
#![allow(clippy::needless_range_loop)]

pub mod decoder;
pub mod ima_qt;
pub mod ima_wav;
pub mod ms;
pub mod tables;
pub mod yamaha;

use oxideav_core::{CodecCapabilities, CodecId, CodecTag};
use oxideav_core::{CodecInfo, CodecRegistry};

/// Canonical codec id for Microsoft ADPCM.
pub const CODEC_ID_MS: &str = "adpcm_ms";
/// Canonical codec id for Microsoft IMA ADPCM (WAV variant).
pub const CODEC_ID_IMA_WAV: &str = "adpcm_ima_wav";
/// Canonical codec id for Apple QuickTime IMA ADPCM.
pub const CODEC_ID_IMA_QT: &str = "adpcm_ima_qt";
/// Canonical codec id for Yamaha ADPCM.
pub const CODEC_ID_YAMAHA: &str = "adpcm_yamaha";

/// Register every ADPCM variant with `reg`. Decode-only.
pub fn register(reg: &mut CodecRegistry) {
    // adpcm_ms — WAVE_FORMAT_ADPCM = 0x0002.
    reg.register(
        CodecInfo::new(CodecId::new(CODEC_ID_MS))
            .capabilities(
                CodecCapabilities::audio("adpcm_ms_sw")
                    .with_lossy(true)
                    .with_intra_only(true),
            )
            .decoder(decoder::make_decoder)
            .tag(CodecTag::wave_format(0x0002)),
    );
    // adpcm_ima_wav — WAVE_FORMAT_DVI_ADPCM = 0x0011.
    reg.register(
        CodecInfo::new(CodecId::new(CODEC_ID_IMA_WAV))
            .capabilities(
                CodecCapabilities::audio("adpcm_ima_wav_sw")
                    .with_lossy(true)
                    .with_intra_only(true),
            )
            .decoder(decoder::make_decoder)
            .tag(CodecTag::wave_format(0x0011)),
    );
    // adpcm_ima_qt — QuickTime fourcc `ima4`.
    reg.register(
        CodecInfo::new(CodecId::new(CODEC_ID_IMA_QT))
            .capabilities(
                CodecCapabilities::audio("adpcm_ima_qt_sw")
                    .with_lossy(true)
                    .with_intra_only(true),
            )
            .decoder(decoder::make_decoder)
            .tag(CodecTag::fourcc(b"ima4")),
    );
    // adpcm_yamaha — WAVE_FORMAT_YAMAHA_ADPCM = 0x0020.
    reg.register(
        CodecInfo::new(CodecId::new(CODEC_ID_YAMAHA))
            .capabilities(
                CodecCapabilities::audio("adpcm_yamaha_sw")
                    .with_lossy(true)
                    .with_intra_only(false),
            )
            .decoder(decoder::make_decoder)
            .tag(CodecTag::wave_format(0x0020)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxideav_core::CodecParameters;

    #[test]
    fn registers_all_four_decoders() {
        let mut reg = CodecRegistry::new();
        register(&mut reg);
        for id in [
            CODEC_ID_MS,
            CODEC_ID_IMA_WAV,
            CODEC_ID_IMA_QT,
            CODEC_ID_YAMAHA,
        ] {
            assert!(
                reg.has_decoder(&CodecId::new(id)),
                "no decoder for codec id {id}"
            );
        }
    }

    #[test]
    fn builds_decoder_with_params() {
        let mut reg = CodecRegistry::new();
        register(&mut reg);
        for id in [
            CODEC_ID_MS,
            CODEC_ID_IMA_WAV,
            CODEC_ID_IMA_QT,
            CODEC_ID_YAMAHA,
        ] {
            let mut p = CodecParameters::audio(CodecId::new(id));
            p.sample_rate = Some(22_050);
            p.channels = Some(1);
            reg.make_decoder(&p)
                .unwrap_or_else(|e| panic!("make_decoder for {id}: {e:?}"));
        }
    }
}
