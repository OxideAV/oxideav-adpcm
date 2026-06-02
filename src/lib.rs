//! ADPCM audio decoder family for oxideav.
//!
//! Covers the six common WAV / AVI / QuickTime / VOX / FM-synth ADPCM
//! flavours:
//!
//! - **`adpcm_ms`** — Microsoft ADPCM (WAVEFORMATEX tag `0x0002`).
//! - **`adpcm_ima_wav`** — IMA/DVI ADPCM, WAV packaging (tag `0x0011`).
//! - **`adpcm_ima_qt`** — IMA ADPCM, Apple QuickTime packaging (fourcc
//!   `ima4`).
//! - **`adpcm_yamaha`** — Yamaha ADPCM-B / DELTA-T as found on the
//!   Y8950, YM2608-B, YMZ280B, AICA (WAV tag `0x0020`).
//! - **`adpcm_yamaha_a`** — Yamaha ADPCM-A — the YM2608 / YM2610
//!   rhythm-channel codec; 12-bit silicon, 49-entry step table, no
//!   canonical WAV tag (chip-internal format).
//! - **`adpcm_dialogic`** — OKI / Dialogic ADPCM (`.vox`); headerless,
//!   no canonical WAV tag (rate supplied out of band).
//!
//! G.722 / G.726 / G.729 are *not* handled here — they live in their own
//! crates.
//!
//! # Registration
//!
//! Call [`register`] from an aggregator crate (or from application code):
//!
//! ```no_run
//! # use oxideav_core::RuntimeContext;
//! let mut ctx = RuntimeContext::new();
//! oxideav_adpcm::register(&mut ctx);
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
pub mod dialogic;
pub mod encoder;
pub mod ima_qt;
pub mod ima_wav;
pub mod ms;
pub mod tables;
pub mod yamaha;
pub mod yamaha_a;

use oxideav_core::{CodecCapabilities, CodecId, CodecTag};
use oxideav_core::{CodecInfo, CodecRegistry};

/// Canonical codec id for Microsoft ADPCM.
pub const CODEC_ID_MS: &str = "adpcm_ms";
/// Canonical codec id for Microsoft IMA ADPCM (WAV variant).
pub const CODEC_ID_IMA_WAV: &str = "adpcm_ima_wav";
/// Canonical codec id for Apple QuickTime IMA ADPCM.
pub const CODEC_ID_IMA_QT: &str = "adpcm_ima_qt";
/// Canonical codec id for Yamaha ADPCM-B / DELTA-T (Y8950, YM2608-B,
/// YMZ280B, AICA — WAV tag `0x0020`).
pub const CODEC_ID_YAMAHA: &str = "adpcm_yamaha";
/// Canonical codec id for Yamaha **ADPCM-A** — the YM2608 / YM2610
/// rhythm-channel codec.
///
/// Distinct from [`CODEC_ID_YAMAHA`] (ADPCM-B / DELTA-T): ADPCM-A uses a
/// 49-entry step-size table with `step_adj = {-1,-1,-1,-1, 2, 5, 7, 9}`
/// and a 12-bit signed reconstructed signal. No canonical WAV tag — the
/// format is chip-internal to the YM2608 rhythm ROM and YM2610 ADPCM-A
/// channels.
pub const CODEC_ID_YAMAHA_A: &str = "adpcm_yamaha_a";
/// Canonical codec id for OKI / Dialogic ADPCM (`.vox`).
///
/// MSB-first nibble unpack (Dialogic VOX / MSM6295 ordering); 16-bit
/// PCM output ([`dialogic::Output::Wide16`]); 12-bit silicon predictor
/// internally. LSB-first MSM6258 streams should be decoded with
/// [`dialogic::decode_packet`] directly so the nibble order can be
/// specified explicitly — the registry-resolved decoder commits to the
/// canonical VOX layout.
pub const CODEC_ID_DIALOGIC: &str = "adpcm_dialogic";

/// Register every ADPCM variant with `reg`. Decoders **and** encoders
/// for all six variants (MS-ADPCM, IMA-ADPCM-WAV, IMA-ADPCM-QT,
/// Yamaha-ADPCM-B, Yamaha-ADPCM-A, OKI/Dialogic VOX).
pub fn register_codecs(reg: &mut CodecRegistry) {
    // adpcm_ms — WAVE_FORMAT_ADPCM = 0x0002.
    reg.register(
        CodecInfo::new(CodecId::new(CODEC_ID_MS))
            .capabilities(
                CodecCapabilities::audio("adpcm_ms_sw")
                    .with_lossy(true)
                    .with_intra_only(true),
            )
            .decoder(decoder::make_decoder)
            .encoder(encoder::make_encoder)
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
            .encoder(encoder::make_encoder)
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
            .encoder(encoder::make_encoder)
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
            .encoder(encoder::make_encoder)
            .tag(CodecTag::wave_format(0x0020)),
    );
    // adpcm_yamaha_a — YM2608/YM2610 rhythm channel ADPCM (no WAV tag).
    reg.register(
        CodecInfo::new(CodecId::new(CODEC_ID_YAMAHA_A))
            .capabilities(
                CodecCapabilities::audio("adpcm_yamaha_a_sw")
                    .with_lossy(true)
                    .with_intra_only(false),
            )
            .decoder(decoder::make_decoder)
            .encoder(encoder::make_encoder),
    );
    // adpcm_dialogic — VOX (no canonical WAV tag; rate is out-of-band).
    reg.register(
        CodecInfo::new(CodecId::new(CODEC_ID_DIALOGIC))
            .capabilities(
                CodecCapabilities::audio("adpcm_dialogic_sw")
                    .with_lossy(true)
                    .with_intra_only(false),
            )
            .decoder(decoder::make_decoder)
            .encoder(encoder::make_encoder),
    );
}

/// Unified registration entry point — installs every ADPCM variant
/// into the codec sub-registry of the supplied
/// [`oxideav_core::RuntimeContext`].
pub fn register(ctx: &mut oxideav_core::RuntimeContext) {
    register_codecs(&mut ctx.codecs);
}

oxideav_core::register!("adpcm", register);

#[cfg(test)]
mod tests {
    use super::*;
    use oxideav_core::CodecParameters;

    #[test]
    fn registers_all_decoders() {
        let mut reg = CodecRegistry::new();
        register_codecs(&mut reg);
        for id in [
            CODEC_ID_MS,
            CODEC_ID_IMA_WAV,
            CODEC_ID_IMA_QT,
            CODEC_ID_YAMAHA,
            CODEC_ID_YAMAHA_A,
            CODEC_ID_DIALOGIC,
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
        register_codecs(&mut reg);
        for id in [
            CODEC_ID_MS,
            CODEC_ID_IMA_WAV,
            CODEC_ID_IMA_QT,
            CODEC_ID_YAMAHA,
            CODEC_ID_YAMAHA_A,
            CODEC_ID_DIALOGIC,
        ] {
            let mut p = CodecParameters::audio(CodecId::new(id));
            p.sample_rate = Some(22_050);
            p.channels = Some(1);
            reg.first_decoder(&p)
                .unwrap_or_else(|e| panic!("make_decoder for {id}: {e:?}"));
        }
    }

    #[test]
    fn register_via_runtime_context_installs_codec_factory() {
        let mut ctx = oxideav_core::RuntimeContext::new();
        register(&mut ctx);
        for id in [
            CODEC_ID_MS,
            CODEC_ID_IMA_WAV,
            CODEC_ID_IMA_QT,
            CODEC_ID_YAMAHA,
            CODEC_ID_YAMAHA_A,
            CODEC_ID_DIALOGIC,
        ] {
            assert!(
                ctx.codecs.has_decoder(&CodecId::new(id)),
                "decoder factory not installed via RuntimeContext for {id}"
            );
        }
    }
}
