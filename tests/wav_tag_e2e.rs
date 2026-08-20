//! Container-tag end-to-end proof: resolve a real WAV file's
//! `wFormatTag` through the codec registry **exactly the way a WAV
//! demuxer does** — a [`ProbeContext`] carrying the tag, the raw `fmt `
//! chunk bytes and the geometry hints — then build the decoder from
//! demuxer-shaped [`CodecParameters`] (container tag + channels + rate +
//! the `fmt ` extension as `extradata`; **no hand-set codec options**)
//! and decode the `data` chunk in large multi-block packets, the
//! granularity real demuxers emit. The output is cross-correlated
//! against the opaque validator's own PCM dump of the same file.
//!
//! This pins the whole tag-claiming chain a WAV demuxer relies on:
//!
//! 1. the registry claim (`register_codecs` `.tag(...)` wiring) resolves
//!    the on-wire `wFormatTag` to this crate's codec id;
//! 2. the decoder factory reconstructs the block framing from what the
//!    container actually provides (the `wSamplesPerBlock` word of the
//!    documented `fmt ` trailers via `extradata`, or the tag-derived
//!    G.726 framing default) — no out-of-band options;
//! 3. the decoded PCM matches the validator's reference.
//!
//! The `WAVE_FORMAT_EXTENSIBLE` leg covers the escape-hatch form: per
//! the staged conversion note
//! (`docs/container/riff/waveformatextensible/ms-converting-format-tags-and-subformat-guids.md`)
//! a SubFormat GUID built from the `DEFINE_WAVEFORMATEX_GUID(x)`
//! template is exactly equivalent to the legacy tag `x`, so a demuxer
//! folds it back to the embedded 16-bit tag and resolves the same
//! `CodecTag::WaveFormat` claim — the core registry needs no GUID form.
//!
//! Fixtures are generated on demand with the validator binary (used
//! strictly as a black box) and the tests skip when it is absent.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use oxideav_adpcm::{
    dialogic, register_codecs, CODEC_ID_DIALOGIC, CODEC_ID_G726, CODEC_ID_IMA_WAV, CODEC_ID_MS,
    CODEC_ID_YAMAHA,
};
use oxideav_core::{
    CodecId, CodecParameters, CodecRegistry, CodecTag, Error, Frame, Packet, ProbeContext, TimeBase,
};

// ---------------------------------------------------------------------------
// Fixture plumbing (same on-demand pattern as tests/wav_decode.rs).
// ---------------------------------------------------------------------------

fn fixtures_dir() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests");
    p.push("fixtures");
    p
}

fn have_validator() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok()
}

fn ensure_fixture(name: &str, build: impl FnOnce() -> bool) -> Option<PathBuf> {
    let path = fixtures_dir().join(name);
    if path.exists() {
        return Some(path);
    }
    if !have_validator() {
        eprintln!("validator binary not installed — skipping test that needs {name}");
        return None;
    }
    fs::create_dir_all(fixtures_dir()).ok();
    if !build() {
        eprintln!("fixture generation failed for {name}; skipping");
        return None;
    }
    path.exists().then_some(path)
}

/// Encode a 0.5 s sine with the validator into a WAV at `rate` Hz /
/// `channels` ch using its `codec` encoder.
fn ensure_wav(codec: &str, rate: u32, channels: u16) -> Option<PathBuf> {
    let name = format!("tag_e2e_{codec}_{rate}_{channels}ch.wav");
    ensure_fixture(&name, || {
        let out = fixtures_dir().join(&name);
        Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                &format!("sine=frequency=440:duration=0.5:sample_rate={rate}"),
                "-ac",
                &channels.to_string(),
                "-c:a",
                codec,
                out.to_str().unwrap(),
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

/// The validator's own s16le PCM decode of `src` (the comparison oracle).
fn ensure_pcm_of(src: &Path, rate: u32, channels: u16) -> Option<PathBuf> {
    let name = format!(
        "{}.pcm",
        src.file_stem().and_then(|s| s.to_str()).unwrap_or("ref")
    );
    ensure_fixture(&name, || {
        let out = fixtures_dir().join(&name);
        Command::new("ffmpeg")
            .args([
                "-y",
                "-i",
                src.to_str().unwrap(),
                "-f",
                "s16le",
                "-ar",
                &rate.to_string(),
                "-ac",
                &channels.to_string(),
                out.to_str().unwrap(),
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

fn load_pcm(path: &Path) -> Vec<i16> {
    fs::read(path)
        .unwrap()
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

fn xcorr(a: &[i16], b: &[i16]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let (mut num, mut da, mut db) = (0f64, 0f64, 0f64);
    for i in 0..n {
        let (ai, bi) = (a[i] as f64, b[i] as f64);
        num += ai * bi;
        da += ai * ai;
        db += bi * bi;
    }
    let denom = (da * db).sqrt();
    if denom == 0.0 {
        0.0
    } else {
        num / denom
    }
}

// ---------------------------------------------------------------------------
// Demuxer-shaped WAV view.
// ---------------------------------------------------------------------------

/// What a WAV demuxer has in hand after parsing the headers: the `fmt `
/// chunk both raw (the `ProbeContext::header` blob) and decomposed, plus
/// the `data` payload.
struct WavView {
    format_tag: u16,
    channels: u16,
    sample_rate: u32,
    block_align: u16,
    bits_per_sample: u16,
    /// Raw `fmt ` chunk bytes (base + `cbSize` + extension).
    fmt_raw: Vec<u8>,
    /// The `fmt ` extension body without the leading `cbSize` word —
    /// this crate's `CodecParameters::extradata` convention.
    extension: Vec<u8>,
    data: Vec<u8>,
}

fn parse_wav(bytes: &[u8]) -> WavView {
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    let mut off = 12usize;
    let mut fmt_raw: Option<Vec<u8>> = None;
    let mut data: Option<Vec<u8>> = None;
    while off + 8 <= bytes.len() {
        let id = &bytes[off..off + 4];
        let size = u32::from_le_bytes([
            bytes[off + 4],
            bytes[off + 5],
            bytes[off + 6],
            bytes[off + 7],
        ]) as usize;
        let body = &bytes[off + 8..off + 8 + size];
        match id {
            b"fmt " => fmt_raw = Some(body.to_vec()),
            b"data" => data = Some(body.to_vec()),
            _ => {}
        }
        off += 8 + size + (size & 1);
    }
    let fmt_raw = fmt_raw.expect("fmt chunk");
    let data = data.expect("data chunk");
    let format_tag = u16::from_le_bytes([fmt_raw[0], fmt_raw[1]]);
    let channels = u16::from_le_bytes([fmt_raw[2], fmt_raw[3]]);
    let sample_rate = u32::from_le_bytes([fmt_raw[4], fmt_raw[5], fmt_raw[6], fmt_raw[7]]);
    let block_align = u16::from_le_bytes([fmt_raw[12], fmt_raw[13]]);
    let bits_per_sample = u16::from_le_bytes([fmt_raw[14], fmt_raw[15]]);
    let extension = if fmt_raw.len() >= 18 {
        let cb = u16::from_le_bytes([fmt_raw[16], fmt_raw[17]]) as usize;
        fmt_raw[18..(18 + cb).min(fmt_raw.len())].to_vec()
    } else {
        Vec::new()
    };
    WavView {
        format_tag,
        channels,
        sample_rate,
        block_align,
        bits_per_sample,
        fmt_raw,
        extension,
        data,
    }
}

/// Resolve the view's `wFormatTag` through the registry the way a WAV
/// demuxer does: tag + raw `fmt ` header blob + geometry hints.
fn resolve_by_tag(reg: &CodecRegistry, wav: &WavView, tag: u16) -> CodecId {
    let probe_tag = CodecTag::wave_format(tag);
    let ctx = ProbeContext::new(&probe_tag)
        .header(&wav.fmt_raw)
        .bits(wav.bits_per_sample)
        .channels(wav.channels)
        .sample_rate(wav.sample_rate);
    reg.resolve_tag_ref(&ctx)
        .unwrap_or_else(|| panic!("wFormatTag {tag:#06x} did not resolve through the registry"))
        .clone()
}

/// Build demuxer-shaped parameters: what a WAV demuxer can actually
/// provide — codec id, on-wire tag, channels, rate, and the `fmt `
/// extension as extradata. No codec options.
fn demuxer_params(codec_id: &CodecId, wav: &WavView, tag: u16) -> CodecParameters {
    let mut params = CodecParameters::audio(codec_id.clone());
    params.tag = Some(CodecTag::wave_format(tag));
    params.channels = Some(wav.channels);
    params.sample_rate = Some(wav.sample_rate);
    params.extradata = wav.extension.clone();
    params
}

/// Decode the `data` chunk through the registry decoder in multi-block
/// packets of up to 1024 × `nBlockAlign` bytes — the packet granularity
/// a real WAV demuxer emits.
fn decode_demuxer_style(reg: &CodecRegistry, params: &CodecParameters, wav: &WavView) -> Vec<i16> {
    let mut dec = reg
        .first_decoder(params)
        .expect("registry-resolved decoder must construct from demuxer-shaped params");
    let tb = TimeBase::new(1, wav.sample_rate as i64);
    let step = (wav.block_align.max(1) as usize) * 1024;
    let mut pcm = Vec::<i16>::new();
    for chunk in wav.data.chunks(step) {
        dec.send_packet(&Packet::new(0, tb, chunk.to_vec()))
            .unwrap();
        match dec.receive_frame() {
            Ok(Frame::Audio(af)) => {
                for c in af.data[0].chunks_exact(2) {
                    pcm.push(i16::from_le_bytes([c[0], c[1]]));
                }
            }
            Ok(_) => panic!("expected audio frame"),
            Err(Error::NeedMore) => {}
            Err(e) => panic!("decode failed: {e:?}"),
        }
    }
    pcm
}

/// Full chain for one validator-generated WAV: resolve by the file's own
/// tag, decode demuxer-style, cross-correlate against the validator PCM.
fn check_wav_by_tag(codec: &str, expect_id: &str, rate: u32, channels: u16) {
    let Some(wav_path) = ensure_wav(codec, rate, channels) else {
        return;
    };
    let Some(pcm_path) = ensure_pcm_of(&wav_path, rate, channels) else {
        return;
    };
    let wav = parse_wav(&fs::read(&wav_path).unwrap());
    assert_eq!(wav.channels, channels);

    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);
    let id = resolve_by_tag(&reg, &wav, wav.format_tag);
    assert_eq!(
        id.as_str(),
        expect_id,
        "{codec}: tag {:#06x} resolved to the wrong codec",
        wav.format_tag
    );

    let params = demuxer_params(&id, &wav, wav.format_tag);
    let ours = decode_demuxer_style(&reg, &params, &wav);
    let reference = load_pcm(&pcm_path);
    assert!(
        (ours.len() as i64 - reference.len() as i64).abs() <= (reference.len() as i64 / 100 + 128),
        "{codec}: sample count drift — ours {} vs ref {}",
        ours.len(),
        reference.len()
    );
    let score = xcorr(&ours, &reference);
    assert!(
        score > 0.97,
        "{codec}: low waveform similarity with the validator reference: {score:.4}"
    );
}

// ---------------------------------------------------------------------------
// Straight legacy-tag legs — the file's own wFormatTag resolves and the
// registry decoder reconstructs the framing from the fmt trailer alone.
// ---------------------------------------------------------------------------

#[test]
fn ms_wav_tag_resolves_and_decodes_mono() {
    check_wav_by_tag("adpcm_ms", CODEC_ID_MS, 22050, 1);
}

#[test]
fn ms_wav_tag_resolves_and_decodes_stereo() {
    check_wav_by_tag("adpcm_ms", CODEC_ID_MS, 22050, 2);
}

#[test]
fn ima_wav_tag_resolves_and_decodes_mono() {
    check_wav_by_tag("adpcm_ima_wav", CODEC_ID_IMA_WAV, 22050, 1);
}

#[test]
fn ima_wav_tag_resolves_and_decodes_stereo() {
    check_wav_by_tag("adpcm_ima_wav", CODEC_ID_IMA_WAV, 22050, 2);
}

#[test]
fn yamaha_wav_tag_resolves_and_decodes() {
    check_wav_by_tag("adpcm_yamaha", CODEC_ID_YAMAHA, 22050, 1);
}

#[test]
fn g726_wav_tag_0x0045_resolves_and_decodes() {
    // The validator's G.726-in-WAV output carries wFormatTag 0x0045
    // (raw bit-continuous MSB-first stream, nBlockAlign = 1) — the
    // empirical basis for the crate's 0x0045 claim.
    let Some(wav_path) = ensure_wav("g726", 8000, 1) else {
        return;
    };
    let wav = parse_wav(&fs::read(&wav_path).unwrap());
    assert_eq!(
        wav.format_tag, 0x0045,
        "validator G.726 WAV no longer carries tag 0x0045 — re-examine the claim"
    );
    assert_eq!(wav.bits_per_sample, 4, "expected the 32 kbit/s rate");
    check_wav_by_tag("g726", CODEC_ID_G726, 8000, 1);
}

#[test]
fn g726_wav_tag_0x0064_decodes_identically_to_0x0045() {
    // RFC 2361 §A.54 assigns WAVE_FORMAT_G726_ADPCM = 0x0064; the
    // validator decodes a 0x0064-tagged file byte-identically to the
    // 0x0045 form, so both resolve here and must produce the same PCM.
    let Some(wav_path) = ensure_wav("g726", 8000, 1) else {
        return;
    };
    let wav = parse_wav(&fs::read(&wav_path).unwrap());
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);

    let mut outputs = Vec::new();
    for tag in [0x0045u16, 0x0064] {
        let id = resolve_by_tag(&reg, &wav, tag);
        assert_eq!(id.as_str(), CODEC_ID_G726, "{tag:#06x}");
        let params = demuxer_params(&id, &wav, tag);
        outputs.push(decode_demuxer_style(&reg, &params, &wav));
    }
    assert_eq!(
        outputs[0], outputs[1],
        "0x0045 and 0x0064 must decode byte-identically (same raw framing)"
    );
}

// ---------------------------------------------------------------------------
// WAVE_FORMAT_EXTENSIBLE leg — the SubFormat template GUID folds back to
// the embedded legacy tag, which is this crate's registry claim.
// ---------------------------------------------------------------------------

/// The 14 trailing bytes shared by every `DEFINE_WAVEFORMATEX_GUID(x)`
/// SubFormat: `0x0000, 0x0010, 0x80,0x00,0x00,0xaa,0x00,0x38,0x9b,0x71`
/// after the 16-bit tag (staged conversion note, `Ksmedia.h` template).
const WAVEFORMATEX_GUID_TAIL: [u8; 14] = [
    0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
];

/// Build the template SubFormat GUID for a legacy tag (little-endian
/// on-wire field order: Data1 LE, Data2 LE, Data3 LE, Data4 bytes).
fn template_guid(tag: u16) -> [u8; 16] {
    let mut g = [0u8; 16];
    g[0] = (tag & 0xFF) as u8;
    g[1] = (tag >> 8) as u8;
    g[2..16].copy_from_slice(&WAVEFORMATEX_GUID_TAIL);
    g
}

/// The demuxer-side fold: if the GUID matches the template outside its
/// leading 16 bits, return the embedded legacy tag (the staged
/// `IS_VALID_WAVEFORMATEX_GUID` / `EXTRACT_WAVEFORMATEX_ID` pair).
fn extract_waveformatex_tag(guid: &[u8; 16]) -> Option<u16> {
    if guid[2..16] == WAVEFORMATEX_GUID_TAIL {
        Some(u16::from_le_bytes([guid[0], guid[1]]))
    } else {
        None
    }
}

/// Rewrite a legacy WAV's `fmt ` chunk into the `WAVE_FORMAT_EXTENSIBLE`
/// form: `wFormatTag = 0xFFFE`, `cbSize = 22`, Samples union =
/// `wSamplesPerBlock` (block-compressed subformat), `dwChannelMask = 0`
/// (no assigned positions), SubFormat = template GUID of the original
/// tag. The `data` chunk is byte-identical, so the validator's PCM dump
/// of the *legacy* file stays the oracle.
fn to_extensible(wav: &WavView, samples_per_block: u16) -> WavView {
    let mut fmt = Vec::with_capacity(18 + 22);
    fmt.extend_from_slice(&0xFFFEu16.to_le_bytes());
    fmt.extend_from_slice(&wav.fmt_raw[2..16]); // channels..bits unchanged
    fmt.extend_from_slice(&22u16.to_le_bytes()); // cbSize
    let mut ext = Vec::with_capacity(22);
    ext.extend_from_slice(&samples_per_block.to_le_bytes());
    ext.extend_from_slice(&0u32.to_le_bytes()); // dwChannelMask
    ext.extend_from_slice(&template_guid(wav.format_tag));
    fmt.extend_from_slice(&ext);
    WavView {
        format_tag: 0xFFFE,
        channels: wav.channels,
        sample_rate: wav.sample_rate,
        block_align: wav.block_align,
        bits_per_sample: wav.bits_per_sample,
        fmt_raw: fmt,
        extension: ext,
        data: wav.data.clone(),
    }
}

#[test]
fn extensible_template_guid_folds_to_claimed_tag_and_decodes() {
    // MS-ADPCM (0x0002) and IMA-WAV (0x0011) wrapped in the EXTENSIBLE
    // escape hatch. The demuxer folds the template SubFormat back to
    // the embedded tag and resolves the same registry claim; the
    // Samples union carries wSamplesPerBlock, from which the decoder
    // could re-derive the framing — here the demuxer-shaped parameters
    // carry the union word as the leading extradata word, matching the
    // documented trailer layouts, so the factory's wSamplesPerBlock
    // path applies unchanged.
    for (codec, expect_id) in [
        ("adpcm_ms", CODEC_ID_MS),
        ("adpcm_ima_wav", CODEC_ID_IMA_WAV),
    ] {
        let Some(wav_path) = ensure_wav(codec, 22050, 2) else {
            return;
        };
        let Some(pcm_path) = ensure_pcm_of(&wav_path, 22050, 2) else {
            return;
        };
        let legacy = parse_wav(&fs::read(&wav_path).unwrap());
        // Both documented trailers open with wSamplesPerBlock.
        let spb = u16::from_le_bytes([legacy.extension[0], legacy.extension[1]]);
        let ext = to_extensible(&legacy, spb);
        assert_eq!(ext.format_tag, 0xFFFE);

        // Demuxer-side fold: SubFormat → embedded legacy tag.
        let guid: [u8; 16] = ext.extension[6..22].try_into().unwrap();
        let folded = extract_waveformatex_tag(&guid)
            .expect("template SubFormat GUID must fold to a legacy tag");
        assert_eq!(folded, legacy.format_tag);

        // A non-template GUID must NOT fold (the guard the demuxer
        // relies on before extracting the tag).
        let mut foreign = guid;
        foreign[8] ^= 0xFF;
        assert_eq!(extract_waveformatex_tag(&foreign), None);

        let mut reg = CodecRegistry::new();
        register_codecs(&mut reg);
        let id = resolve_by_tag(&reg, &ext, folded);
        assert_eq!(id.as_str(), expect_id, "{codec}: folded tag misrouted");

        // Demuxer-shaped parameters for the EXTENSIBLE form: the tag is
        // the folded legacy tag; extradata is the EXTENSIBLE extension,
        // whose leading word is the Samples union (= wSamplesPerBlock
        // for block-compressed subformats) — exactly where the legacy
        // trailers put it, so block framing derives identically.
        // MS custom-coefficient parsing does not apply (the EXTENSIBLE
        // extension carries no aCoeff table), so extradata is trimmed
        // to the union word alone — the honest content for framing.
        let mut params = CodecParameters::audio(id.clone());
        params.tag = Some(CodecTag::wave_format(folded));
        params.channels = Some(ext.channels);
        params.sample_rate = Some(ext.sample_rate);
        params.extradata = ext.extension[0..2].to_vec();
        let ours = decode_demuxer_style(&reg, &params, &ext);
        let reference = load_pcm(&pcm_path);
        let score = xcorr(&ours, &reference);
        assert!(
            score > 0.97,
            "{codec} (EXTENSIBLE): low similarity with the validator reference: {score:.4}"
        );
    }
}

// ---------------------------------------------------------------------------
// OKI / Dialogic 0x0017 leg — the RFC 2361 alias claim, validated
// black-box: the validator decodes the same file through its own OKI
// path and both PCM streams must agree.
// ---------------------------------------------------------------------------

/// Assemble a minimal WAV around a raw body (no `fmt ` extension).
fn wrap_wav(
    tag: u16,
    channels: u16,
    rate: u32,
    block_align: u16,
    bits: u16,
    body: &[u8],
) -> Vec<u8> {
    let mut fmt = Vec::with_capacity(16);
    fmt.extend_from_slice(&tag.to_le_bytes());
    fmt.extend_from_slice(&channels.to_le_bytes());
    fmt.extend_from_slice(&rate.to_le_bytes());
    let avg = rate * block_align as u32;
    fmt.extend_from_slice(&avg.to_le_bytes());
    fmt.extend_from_slice(&block_align.to_le_bytes());
    fmt.extend_from_slice(&bits.to_le_bytes());
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    let riff_size = 4 + (8 + fmt.len()) + (8 + body.len()) + (body.len() & 1);
    out.extend_from_slice(&(riff_size as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
    out.extend_from_slice(&fmt);
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
    out
}

#[test]
fn oki_wav_tag_0x0017_resolves_and_matches_validator() {
    if !have_validator() {
        eprintln!("validator binary not installed — skipping 0x0017 OKI leg");
        return;
    }
    // A 0.5 s 440 Hz sine at 8 kHz, encoded to the canonical 4-bit OKI
    // VOX body by this crate's own encoder.
    let n = 4000usize;
    let pcm: Vec<i16> = (0..n)
        .map(|i| {
            let t = i as f64 / 8000.0;
            ((t * 440.0 * std::f64::consts::TAU).sin() * 6000.0) as i16
        })
        .collect();
    let mut st = dialogic::Channel::default();
    let body = dialogic::encode_packet(&pcm, &mut st, dialogic::NibbleOrder::HiFirst);
    let file = wrap_wav(0x0017, 1, 8000, 1, 4, &body);

    let dir = fixtures_dir();
    fs::create_dir_all(&dir).ok();
    let wav_path = dir.join("tag_e2e_oki_0x0017.wav");
    fs::write(&wav_path, &file).unwrap();
    let pcm_path = dir.join("tag_e2e_oki_0x0017.pcm");
    let ok = Command::new("ffmpeg")
        .args([
            "-y",
            "-i",
            wav_path.to_str().unwrap(),
            "-f",
            "s16le",
            "-ar",
            "8000",
            "-ac",
            "1",
            pcm_path.to_str().unwrap(),
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(
        ok,
        "validator rejected the 0x0017-tagged OKI WAV — re-examine the claim"
    );

    // Registry resolution + demuxer-style decode of the same file.
    let wav = parse_wav(&file);
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);
    let id = resolve_by_tag(&reg, &wav, 0x0017);
    assert_eq!(id.as_str(), CODEC_ID_DIALOGIC, "0x0017 must route to VOX");
    let params = demuxer_params(&id, &wav, 0x0017);
    let ours = decode_demuxer_style(&reg, &params, &wav);

    let reference = load_pcm(&pcm_path);
    let score = xcorr(&ours, &reference);
    assert!(
        score > 0.97,
        "0x0017 OKI: our decode disagrees with the validator's ({score:.4})"
    );
    // And the original input survives the lossy round trip through two
    // independent implementations.
    let score_in = xcorr(&ours, &pcm);
    assert!(
        score_in > 0.95,
        "0x0017 OKI: decode diverged from the encoder input ({score_in:.4})"
    );
}
