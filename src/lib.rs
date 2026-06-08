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

pub use decoder::{Shape, Variant};

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
    fn variant_codec_id_round_trip() {
        // Every variant's codec_id() string parses back to the same
        // variant via from_codec_id() — the typed enum and the id-string
        // table never drift.
        for &v in Variant::all() {
            let id = CodecId::new(v.codec_id());
            let parsed = Variant::from_codec_id(&id)
                .unwrap_or_else(|| panic!("from_codec_id({:?}) returned None", id));
            assert_eq!(parsed, v, "round-trip mismatch for {:?}", v);
        }
    }

    #[test]
    fn variant_from_codec_id_rejects_unknown_ids() {
        // A non-ADPCM codec id must not be misclassified.
        for id in ["pcm_s16le", "mp3", "opus", "", "adpcm_unknown"] {
            assert!(
                Variant::from_codec_id(&CodecId::new(id)).is_none(),
                "unknown id {id:?} mis-parsed as a Variant"
            );
        }
    }

    #[test]
    fn variant_all_covers_every_known_codec_id() {
        // Cross-check: the codec-id constants and `Variant::all()` are
        // exhaustive in parallel.
        let from_all: Vec<&'static str> = Variant::all().iter().map(|v| v.codec_id()).collect();
        for id in [
            CODEC_ID_MS,
            CODEC_ID_IMA_WAV,
            CODEC_ID_IMA_QT,
            CODEC_ID_YAMAHA,
            CODEC_ID_YAMAHA_A,
            CODEC_ID_DIALOGIC,
        ] {
            assert!(
                from_all.contains(&id),
                "codec id {id} missing from Variant::all()"
            );
        }
        assert_eq!(from_all.len(), 6, "Variant::all() drifted from 6 entries");
    }

    #[test]
    fn variant_wave_format_tag_matches_registered_tag() {
        // The variant's typed tag accessor must agree with what
        // `register_codecs` actually wires into the registry.
        for &v in Variant::all() {
            match v.wave_format_tag() {
                Some(0x0002) => assert_eq!(v, Variant::Ms),
                Some(0x0011) => assert_eq!(v, Variant::ImaWav),
                Some(0x0020) => assert_eq!(v, Variant::Yamaha),
                Some(other) => panic!("unexpected wave_format_tag {other:#06x} on {v:?}"),
                None => assert!(
                    matches!(v, Variant::ImaQt | Variant::YamahaA | Variant::Dialogic),
                    "{v:?} returned None from wave_format_tag but is not a tagless variant"
                ),
            }
        }
    }

    #[test]
    fn variant_fourcc_only_set_for_quicktime_ima4() {
        for &v in Variant::all() {
            match v {
                Variant::ImaQt => assert_eq!(v.fourcc(), Some(*b"ima4")),
                other => assert_eq!(other.fourcc(), None, "{other:?} should have no fourcc"),
            }
        }
    }

    #[test]
    fn variant_shape_partitions_block_vs_stream() {
        // Three block-oriented (WAV/AVI/QT — per-block header re-seed)
        // and three stream-oriented (Yamaha-A/B + Dialogic VOX —
        // headerless, predictor + step carry across packets) variants.
        for &v in Variant::all() {
            let shape = v.shape();
            match v {
                Variant::Ms | Variant::ImaWav | Variant::ImaQt => {
                    assert_eq!(
                        shape,
                        Shape::BlockOriented,
                        "{:?} is a block-oriented WAV/AVI/QT variant",
                        v
                    );
                }
                Variant::Yamaha | Variant::YamahaA | Variant::Dialogic => {
                    assert_eq!(
                        shape,
                        Shape::StreamOriented,
                        "{:?} is a stream-oriented chip variant",
                        v
                    );
                }
            }
        }
        // Exactly 3 in each bucket — pins the partition against silent
        // future drift.
        let block = Variant::all()
            .iter()
            .filter(|v| v.shape() == Shape::BlockOriented)
            .count();
        let stream = Variant::all()
            .iter()
            .filter(|v| v.shape() == Shape::StreamOriented)
            .count();
        assert_eq!(block, 3, "expected 3 block-oriented variants, got {block}");
        assert_eq!(
            stream, 3,
            "expected 3 stream-oriented variants, got {stream}"
        );
    }

    #[test]
    fn variant_max_channels_matches_factory_accept_reject() {
        // For every variant, ask the decoder factory to build at the
        // claimed maximum (and reject above it, where there is an upper
        // bound) — keeps the typed accessor and the scattered factory
        // checks in lockstep.
        let mut reg = CodecRegistry::new();
        register_codecs(&mut reg);
        for &v in Variant::all() {
            let id = CodecId::new(v.codec_id());
            match v.max_channels() {
                Some(max) => {
                    // Factory must accept exactly `max` channels.
                    let mut p = CodecParameters::audio(id.clone());
                    p.sample_rate = Some(22_050);
                    p.channels = Some(max);
                    reg.first_decoder(&p).unwrap_or_else(|e| {
                        panic!("{:?}: factory rejected claimed max {max}ch: {e:?}", v)
                    });
                    // And must reject `max + 1` channels.
                    let mut p_over = CodecParameters::audio(id.clone());
                    p_over.sample_rate = Some(22_050);
                    p_over.channels = Some(max + 1);
                    assert!(
                        reg.first_decoder(&p_over).is_err(),
                        "{:?}: factory accepted {}ch but max_channels() says {max}",
                        v,
                        max + 1
                    );
                }
                None => {
                    // Unbounded — confirm with a generously high count
                    // (Yamaha-B carries sample-level round-robin).
                    let mut p = CodecParameters::audio(id);
                    p.sample_rate = Some(22_050);
                    p.channels = Some(16);
                    reg.first_decoder(&p).unwrap_or_else(|e| {
                        panic!(
                            "{:?}: max_channels()=None implies unbounded but factory rejected 16ch: {e:?}",
                            v
                        )
                    });
                }
            }
        }
    }

    #[test]
    fn variant_max_channels_rejects_zero_for_every_variant() {
        // Zero channels is nonsensical for every ADPCM variant — the
        // factory rejects 0 across the board regardless of upper bound.
        let mut reg = CodecRegistry::new();
        register_codecs(&mut reg);
        for &v in Variant::all() {
            let mut p = CodecParameters::audio(CodecId::new(v.codec_id()));
            p.sample_rate = Some(22_050);
            p.channels = Some(0);
            assert!(
                reg.first_decoder(&p).is_err(),
                "{:?}: factory accepted 0 channels",
                v
            );
        }
    }

    #[test]
    fn variant_header_bytes_block_oriented_only() {
        // Block-oriented variants return Some(n) for every accepted
        // channel count; stream-oriented variants always return None.
        for &v in Variant::all() {
            let max = v.max_channels().unwrap_or(16);
            for ch in 1..=max {
                let h = v.header_bytes(ch);
                match v.shape() {
                    Shape::BlockOriented => {
                        assert!(h.is_some(), "{:?} ch={ch}: header_bytes returned None", v);
                    }
                    Shape::StreamOriented => {
                        assert!(h.is_none(), "{:?} ch={ch}: header_bytes returned Some", v);
                    }
                }
            }
            // Zero channels is never a valid layout.
            assert_eq!(v.header_bytes(0), None, "{:?}: ch=0 must be None", v);
        }
        // Pin the exact spec-derived constants for each block-oriented
        // variant at the canonical mono + stereo widths.
        assert_eq!(Variant::Ms.header_bytes(1), Some(7));
        assert_eq!(Variant::Ms.header_bytes(2), Some(14));
        assert_eq!(Variant::ImaWav.header_bytes(1), Some(4));
        assert_eq!(Variant::ImaWav.header_bytes(2), Some(8));
        assert_eq!(Variant::ImaQt.header_bytes(1), Some(2));
        assert_eq!(Variant::ImaQt.header_bytes(2), Some(4));
    }

    #[test]
    fn variant_samples_per_block_matches_actual_decode() {
        use crate::{ima_qt, ima_wav, ms};
        // For each block-oriented variant, build a minimum-valid block,
        // a single-body-unit block, and a multi-body-unit block; pin
        // samples_per_block() against the slice the decoder actually
        // produces, per channel-count.
        //
        // Microsoft: header = 7*ch, body = 2*ch bytes per "row" of
        // output (one byte per channel → 2 nibbles → 2 samples). Cases:
        // ch=1, block 7B (no body) → 2 samples; block 9B → 6 samples.
        // ch=2, block 14B → 2 samples; block 18B → 6 samples.
        for (ch, block_bytes) in &[(1u16, 7usize), (1, 9), (2, 14), (2, 18)] {
            let expected = Variant::Ms.samples_per_block(*ch, *block_bytes).unwrap();
            // Build a zero-content block at the chosen size — every byte
            // is a valid header/body byte for the decoder (predictor
            // index 0 is in range; sample / delta seeds are zero).
            // Per-channel MS header layout (parsed in this order, NOT as
            // contiguous 7-byte groups): [pi_ch0..pi_chN],
            // [delta_ch0_lo, delta_ch0_hi, delta_ch1_lo, ...],
            // [s1_ch0_lo, s1_ch0_hi, ...], [s2_ch0_lo, s2_ch0_hi, ...].
            // Initial delta = 16 (not zero — `delta < 16` clamps to 16
            // inside the decoder; setting it explicitly keeps the test's
            // intent clear). Predictor indices stay 0 (in-range).
            let ch_u = *ch as usize;
            let mut block = vec![0u8; *block_bytes];
            for c in 0..ch_u {
                let off = ch_u + c * 2;
                block[off] = 16; // delta_lo
                block[off + 1] = 0; // delta_hi
            }
            let pcm = ms::decode_block(&block, ch_u).unwrap();
            let actual = pcm.len() / ch_u;
            assert_eq!(
                actual, expected,
                "MS ch={ch} block={block_bytes}: decoder produced {actual}, accessor predicted {expected}"
            );
        }
        // IMA-WAV: header = 4*ch, body groups = 4*ch bytes each → 8
        // samples per channel per group. Add a sample with the header
        // alone (1 sample) and with N groups.
        for (ch, block_bytes) in &[(1u16, 4usize), (1, 8), (1, 12), (2, 8), (2, 16)] {
            let expected = Variant::ImaWav
                .samples_per_block(*ch, *block_bytes)
                .unwrap();
            let block = vec![0u8; *block_bytes];
            let pcm = ima_wav::decode_block(&block, *ch as usize).unwrap();
            let actual = pcm.len() / *ch as usize;
            assert_eq!(
                actual, expected,
                "IMA-WAV ch={ch} block={block_bytes}: decoder produced {actual}, accessor predicted {expected}"
            );
        }
        // IMA-QT: fixed 34 B per channel → 64 samples per channel.
        for ch in &[1u16, 2u16] {
            let block_bytes = ima_qt::QT_BLOCK_SIZE * *ch as usize;
            let expected = Variant::ImaQt.samples_per_block(*ch, block_bytes).unwrap();
            assert_eq!(expected, ima_qt::QT_SAMPLES_PER_BLOCK);
            let block = vec![0u8; block_bytes];
            let pcm = ima_qt::decode_block(&block, *ch as usize).unwrap();
            assert_eq!(pcm.len() / *ch as usize, expected);
        }
    }

    #[test]
    fn variant_samples_per_block_rejects_bad_inputs() {
        // Stream-oriented variants always return None — no block framing
        // exists for Yamaha-A / Yamaha-B / Dialogic.
        for &v in &[Variant::Yamaha, Variant::YamahaA, Variant::Dialogic] {
            assert_eq!(v.samples_per_block(1, 0), None, "{:?}: must be None", v);
            assert_eq!(v.samples_per_block(1, 256), None, "{:?}: must be None", v);
        }
        // Zero channels: None for every variant.
        for &v in Variant::all() {
            assert_eq!(v.samples_per_block(0, 256), None, "{:?}: ch=0", v);
        }
        // Over-cap channels: None.
        assert_eq!(Variant::Ms.samples_per_block(3, 21), None);
        assert_eq!(Variant::ImaWav.samples_per_block(9, 36), None);
        assert_eq!(Variant::ImaQt.samples_per_block(3, 102), None);
        // Block shorter than the per-channel header: None.
        assert_eq!(Variant::Ms.samples_per_block(1, 6), None);
        assert_eq!(Variant::ImaWav.samples_per_block(1, 3), None);
        assert_eq!(Variant::ImaQt.samples_per_block(1, 1), None);
        // MS body not a whole number of per-channel bytes.
        // ch=2, block=15: body=1 byte, 1%2 != 0 → None.
        assert_eq!(Variant::Ms.samples_per_block(2, 15), None);
        // IMA-WAV body not a whole number of (4*ch)-byte groups.
        // ch=1, block=5: body=1 byte, 1%4 != 0 → None.
        assert_eq!(Variant::ImaWav.samples_per_block(1, 5), None);
        // ch=2, block=12: body=4 bytes, 4%8 != 0 → None.
        assert_eq!(Variant::ImaWav.samples_per_block(2, 12), None);
        // IMA-QT block size that isn't 34*channels: None.
        assert_eq!(Variant::ImaQt.samples_per_block(1, 33), None);
        assert_eq!(Variant::ImaQt.samples_per_block(1, 35), None);
        assert_eq!(Variant::ImaQt.samples_per_block(2, 34), None);
        assert_eq!(Variant::ImaQt.samples_per_block(2, 67), None);
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
