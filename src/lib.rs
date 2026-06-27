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
/// Canonical codec id for OKI / Dialogic ADPCM (`.vox` and the
/// WAV-container `WAVE_FORMAT_OKI_ADPCM` = `0x0010`).
///
/// MSB-first nibble unpack (Dialogic VOX / MSM6295 ordering); 16-bit
/// PCM output ([`dialogic::Output::Wide16`]); 12-bit silicon predictor
/// internally. LSB-first MSM6258 streams should be decoded with
/// [`dialogic::decode_packet`] directly so the nibble order can be
/// specified explicitly — the registry-resolved decoder commits to the
/// canonical VOX layout.
///
/// The registration also carries WAV tag `0x0010` so a WAV demuxer that
/// has parsed `WAVEFORMATEX::wFormatTag = WAVE_FORMAT_OKI_ADPCM` resolves
/// to this codec by tag. That tag's 4-bit body is the canonical VOX
/// layout (two samples per byte, high nibble first), so the existing
/// decode path is byte-identical.
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
    // adpcm_dialogic — OKI / Dialogic VOX. Headerless `.vox` files carry
    // the sample rate out of band, but the same OKI MSM6258/6585/6295
    // chip-set algorithm also has a WAV-container framing,
    // WAVE_FORMAT_OKI_ADPCM = 0x0010 (SDL_sound "OKI ADPCM Wave Types":
    // "created and read by the OKI ADPCM chip set"). The 4-bit WAV-OKI
    // body is the canonical VOX layout — two samples per byte, high
    // nibble first — which the registry decoder already produces, so the
    // tag routes a WAV demuxer straight to this decoder.
    reg.register(
        CodecInfo::new(CodecId::new(CODEC_ID_DIALOGIC))
            .capabilities(
                CodecCapabilities::audio("adpcm_dialogic_sw")
                    .with_lossy(true)
                    .with_intra_only(false),
            )
            .decoder(decoder::make_decoder)
            .encoder(encoder::make_encoder)
            .tag(CodecTag::wave_format(0x0010)),
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
                Some(0x0010) => assert_eq!(v, Variant::Dialogic),
                Some(other) => panic!("unexpected wave_format_tag {other:#06x} on {v:?}"),
                None => assert!(
                    matches!(v, Variant::ImaQt | Variant::YamahaA),
                    "{v:?} returned None from wave_format_tag but is not a tagless variant"
                ),
            }
        }
    }

    #[test]
    fn registry_resolves_each_wave_format_tag_to_its_variant() {
        // Every `Variant::wave_format_tag()` value must resolve through the
        // registry to a codec id whose typed variant matches — i.e. the
        // accessor and the actual `.tag(...)` wiring in `register_codecs`
        // stay in lockstep. Catches a tag added to the accessor but not the
        // registration (or vice versa).
        use oxideav_core::{CodecTag, ProbeContext};
        let mut reg = CodecRegistry::new();
        register_codecs(&mut reg);
        for &v in Variant::all() {
            let Some(tag) = v.wave_format_tag() else {
                continue;
            };
            let wf = CodecTag::wave_format(tag);
            let id = reg
                .resolve_tag_ref(&ProbeContext::new(&wf))
                .unwrap_or_else(|| panic!("no codec registered for wave tag {tag:#06x} ({v:?})"));
            let resolved = Variant::from_codec_id(id)
                .unwrap_or_else(|| panic!("registered tag {tag:#06x} resolved to non-ADPCM id"));
            assert_eq!(
                resolved, v,
                "wave tag {tag:#06x}: accessor says {v:?} but registry resolves {resolved:?}"
            );
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
    fn variant_from_wave_format_tag_inverts_wave_format_tag() {
        // Every variant whose wave_format_tag() is Some must round-trip
        // through from_wave_format_tag(); the two tagless variants stay
        // tagless on the reverse path (nothing resolves to them by tag).
        for &v in Variant::all() {
            match v.wave_format_tag() {
                Some(tag) => assert_eq!(
                    Variant::from_wave_format_tag(tag),
                    Some(v),
                    "wave tag {tag:#06x} did not invert back to {v:?}"
                ),
                None => {
                    // ImaQt / YamahaA carry no WAV tag — confirm they are
                    // unreachable by the reverse lookup at every tag value
                    // that the tagged variants own.
                    for &owner in Variant::all() {
                        if let Some(tag) = owner.wave_format_tag() {
                            assert_ne!(
                                Variant::from_wave_format_tag(tag),
                                Some(v),
                                "{v:?} is tagless but resolved from {tag:#06x}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn variant_from_wave_format_tag_rejects_foreign_and_unknown_tags() {
        // Tags owned by other codec families (or unassigned) resolve to
        // None — the ADPCM crate must not claim them.
        for tag in [
            0x0000u16, // WAVE_FORMAT_UNKNOWN
            0x0001,    // PCM
            0x0003,    // IEEE float
            0x0006,    // A-law
            0x0007,    // mu-law
            0x0028,    // G.722 (its own crate)
            0x0045,    // G.726 (its own crate)
            0xFFFF,    // WAVE_FORMAT_EXTENSIBLE / sentinel
        ] {
            assert_eq!(
                Variant::from_wave_format_tag(tag),
                None,
                "tag {tag:#06x} must not resolve to an ADPCM variant"
            );
        }
    }

    #[test]
    fn variant_from_fourcc_inverts_fourcc() {
        for &v in Variant::all() {
            match v.fourcc() {
                Some(fourcc) => assert_eq!(
                    Variant::from_fourcc(fourcc),
                    Some(v),
                    "fourcc {fourcc:?} did not invert back to {v:?}"
                ),
                None => {
                    // Only ImaQt owns a fourcc; nothing else should resolve.
                    assert_eq!(Variant::from_fourcc(*b"ima4"), Some(Variant::ImaQt));
                }
            }
        }
        // Foreign / unknown FourCCs resolve to None.
        for code in [b"sowt", b"twos", b"ms\x00\x02", b"\0\0\0\0", b"IMA4"] {
            assert_eq!(
                Variant::from_fourcc(*code),
                None,
                "fourcc {code:?} must not resolve (case-sensitive, ima4 only)"
            );
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
        // ImaQt now accepts up to QT_MAX_CHANNELS (8); one above is None.
        assert_eq!(Variant::ImaQt.samples_per_block(9, 34 * 9), None);
        // …and a mid-range multichannel count (6 = 5.1) is accepted.
        assert_eq!(
            Variant::ImaQt.samples_per_block(6, 34 * 6),
            Some(crate::ima_qt::QT_SAMPLES_PER_BLOCK)
        );
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
    fn variant_block_size_bytes_inverts_samples_per_block() {
        use crate::{ima_qt, ima_wav, ms};
        // For each block-oriented variant + channel count, sweep a range
        // of valid per-channel sample counts, derive the block size with
        // block_size_bytes(), and confirm:
        //   (a) samples_per_block() of that size returns the same count
        //       (the two accessors are exact inverses), and
        //   (b) the per-block decoder seeded with a zero-content block of
        //       that size produces exactly that many samples per channel.
        // MS: header emits 2 samples; body adds 2 per byte per channel, so
        // valid n are those where (n-2)*ch is even.
        for ch in &[1u16, 2u16] {
            let ch_u = *ch as usize;
            for n in [2usize, 4, 6, 10, 32] {
                if (n - 2) * ch_u % 2 != 0 {
                    continue;
                }
                let b = Variant::Ms.block_size_bytes(*ch, n).unwrap();
                assert_eq!(
                    Variant::Ms.samples_per_block(*ch, b),
                    Some(n),
                    "MS ch={ch} n={n}: block_size_bytes/samples_per_block not inverse"
                );
                // Build a zero-content block of the derived size with valid
                // per-channel headers (delta = 16, predictor index 0).
                let mut block = vec![0u8; b];
                for c in 0..ch_u {
                    let off = ch_u + c * 2;
                    block[off] = 16;
                }
                let pcm = ms::decode_block(&block, ch_u).unwrap();
                assert_eq!(pcm.len() / ch_u, n, "MS ch={ch} n={n}: decode mismatch");
            }
        }
        // IMA-WAV: header seeds 1 sample; each 4*ch group adds 8 per
        // channel, so valid n are 1 + 8k.
        for ch in &[1u16, 2u16] {
            let ch_u = *ch as usize;
            for groups in 0..=3usize {
                let n = 1 + groups * 8;
                let b = Variant::ImaWav.block_size_bytes(*ch, n).unwrap();
                assert_eq!(
                    Variant::ImaWav.samples_per_block(*ch, b),
                    Some(n),
                    "IMA-WAV ch={ch} n={n}: not inverse"
                );
                let block = vec![0u8; b];
                let pcm = ima_wav::decode_block(&block, ch_u).unwrap();
                assert_eq!(
                    pcm.len() / ch_u,
                    n,
                    "IMA-WAV ch={ch} n={n}: decode mismatch"
                );
            }
        }
        // IMA-QT: only the fixed 64-sample block exists; block size is
        // 34*ch.
        for ch in &[1u16, 2u16] {
            let b = Variant::ImaQt
                .block_size_bytes(*ch, ima_qt::QT_SAMPLES_PER_BLOCK)
                .unwrap();
            assert_eq!(b, ima_qt::QT_BLOCK_SIZE * *ch as usize);
            assert_eq!(
                Variant::ImaQt.samples_per_block(*ch, b),
                Some(ima_qt::QT_SAMPLES_PER_BLOCK)
            );
            let block = vec![0u8; b];
            let pcm = ima_qt::decode_block(&block, *ch as usize).unwrap();
            assert_eq!(pcm.len() / *ch as usize, ima_qt::QT_SAMPLES_PER_BLOCK);
        }
    }

    #[test]
    fn variant_block_size_bytes_rejects_bad_inputs() {
        // Stream-oriented variants have no block framing — always None.
        for &v in &[Variant::Yamaha, Variant::YamahaA, Variant::Dialogic] {
            assert_eq!(v.block_size_bytes(1, 64), None, "{:?}: must be None", v);
            assert_eq!(v.block_size_bytes(1, 0), None, "{:?}: must be None", v);
        }
        // Zero channels: None for every variant.
        for &v in Variant::all() {
            assert_eq!(v.block_size_bytes(0, 64), None, "{:?}: ch=0", v);
        }
        // Over-cap channels: None.
        assert_eq!(Variant::Ms.block_size_bytes(3, 2), None);
        assert_eq!(Variant::ImaWav.block_size_bytes(9, 1), None);
        // ImaQt now accepts up to QT_MAX_CHANNELS (8); one above is None.
        assert_eq!(Variant::ImaQt.block_size_bytes(9, 64), None);
        // …and a mid-range multichannel count (6 = 5.1) inverts cleanly.
        assert_eq!(
            Variant::ImaQt.block_size_bytes(6, crate::ima_qt::QT_SAMPLES_PER_BLOCK),
            Some(34 * 6)
        );
        // Below header-only minimum: None.
        assert_eq!(Variant::Ms.block_size_bytes(1, 1), None);
        assert_eq!(Variant::ImaWav.block_size_bytes(1, 0), None);
        // Off-boundary sample counts.
        // MS ch=1: (n-2) odd → no whole body byte. n=3 → body_nibbles=1.
        assert_eq!(Variant::Ms.block_size_bytes(1, 3), None);
        // IMA-WAV: (n-1) not a multiple of 8.
        assert_eq!(Variant::ImaWav.block_size_bytes(1, 4), None);
        assert_eq!(Variant::ImaWav.block_size_bytes(2, 10), None);
        // IMA-QT: anything other than the fixed 64 samples.
        assert_eq!(Variant::ImaQt.block_size_bytes(1, 63), None);
        assert_eq!(Variant::ImaQt.block_size_bytes(1, 65), None);
        assert_eq!(Variant::ImaQt.block_size_bytes(2, 32), None);
    }

    #[test]
    fn build_wave_format_extra_ms_round_trips_through_decoder_extradata() {
        // For a chosen nBlockAlign, the MS trailer this produces must parse
        // back through the decoder's extradata path to the standard 7-set
        // table, and its wSamplesPerBlock must equal samples_per_block().
        for (ch, block_align) in [(1u16, 256usize), (2, 256), (1, 1024), (2, 512)] {
            let ext = Variant::Ms
                .build_wave_format_extra(ch, block_align)
                .unwrap_or_else(|| panic!("MS extra None for ch={ch} ba={block_align}"));
            // 2 (spb) + 2 (numCoef) + 7*4 = 32 bytes, no cbSize.
            assert_eq!(ext.len(), 32, "MS trailer length");
            let spb = u16::from_le_bytes([ext[0], ext[1]]) as usize;
            assert_eq!(
                Some(spb),
                Variant::Ms.samples_per_block(ch, block_align),
                "MS wSamplesPerBlock disagrees with samples_per_block"
            );
            // Round-trips back through the parser to the standard table.
            let parsed = crate::ms::parse_extradata_coeffs(&ext).unwrap().unwrap();
            assert_eq!(&parsed[..], &crate::ms::STANDARD_COEFFS[..]);
        }
    }

    #[test]
    fn build_wave_format_extra_ima_wav_is_samples_per_block_only() {
        for (ch, block_align) in [(1u16, 256usize), (2, 256), (1, 1024)] {
            let ext = Variant::ImaWav
                .build_wave_format_extra(ch, block_align)
                .unwrap_or_else(|| panic!("IMA-WAV extra None for ch={ch} ba={block_align}"));
            // IMA-WAV fmt extension is exactly wSamplesPerBlock (2 bytes).
            assert_eq!(ext.len(), 2, "IMA-WAV trailer length");
            let spb = u16::from_le_bytes([ext[0], ext[1]]) as usize;
            assert_eq!(
                Some(spb),
                Variant::ImaWav.samples_per_block(ch, block_align)
            );
        }
    }

    #[test]
    fn build_wave_format_extra_none_for_qt_stream_and_bad_geometry() {
        // IMA-QT (ISO-BMFF sample entry, not WAV) + the three stream
        // variants have no WAVEFORMATEX extension here.
        for &v in &[
            Variant::ImaQt,
            Variant::Yamaha,
            Variant::YamahaA,
            Variant::Dialogic,
        ] {
            assert_eq!(
                v.build_wave_format_extra(1, 256),
                None,
                "{v:?} must have no WAV fmt extension"
            );
        }
        // Block geometry that samples_per_block() rejects → None.
        // MS body not a whole number of per-channel bytes (ch=2, ba=15).
        assert_eq!(Variant::Ms.build_wave_format_extra(2, 15), None);
        // Over-cap channel count.
        assert_eq!(Variant::Ms.build_wave_format_extra(3, 256), None);
        // Block smaller than the header.
        assert_eq!(Variant::ImaWav.build_wave_format_extra(1, 3), None);
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
