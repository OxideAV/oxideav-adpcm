//! End-to-end **encoder** validation: encode a PCM sine with *our*
//! encoder, wrap the bytes in a spec-correct container, and hand the
//! container to an opaque third-party validator to decode back to PCM.
//! Then cross-correlate the validator's reconstruction against the
//! original input.
//!
//! This closes the loop that `wav_decode.rs` leaves open. That harness
//! proves our *decoder* tracks the validator's decode; the round-trip
//! tests in `encode_round_trip.rs` prove our encoder + our decoder agree.
//! Neither proves our *encoder* emits bytes an independent decoder
//! reconstructs faithfully — i.e. that the blocks we write are
//! spec-conformant on the wire, not merely self-consistent. This file
//! supplies that missing direction:
//!
//! ```text
//!   PCM ──(our encoder)──> ADPCM block stream ──(our WAV writer)──> .wav
//!       ──(opaque validator decode)──> PCM' ──(xcorr vs PCM)──> > 0.97
//! ```
//!
//! The validator is invoked only as an opaque CLI oracle: our bytes go
//! in, its PCM dump comes out, and its source is never consulted. When
//! the validator binary is absent the tests skip with a harmless
//! `eprintln!` (matching `wav_decode.rs`).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use oxideav_adpcm::encoder::{
    encode_block as ms_encode_block, ima_encode_block, ima_qt_encode_block,
};
use oxideav_adpcm::{ima_qt, ms, Variant};

fn fixtures_dir() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests");
    p.push("fixtures");
    p
}

fn have_validator() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok()
}

/// Deterministic mono sine, `amp` peak amplitude in i16 LSB.
fn sine_pcm(n: usize, hz: f64, sample_rate: f64, amp: f64) -> Vec<i16> {
    (0..n)
        .map(|i| {
            let t = i as f64 / sample_rate;
            ((2.0 * std::f64::consts::PI * hz * t).sin() * amp).round() as i16
        })
        .collect()
}

/// Deterministic broadband mono signal: a sum of four detuned partials
/// plus a slow envelope. A pure tone barely moves the predictor; a
/// multi-tone waveform forces the MS per-block coefficient search and the
/// IMA step adaptation to track a richer spectrum, so it is a stronger
/// wire-conformance stress than a single sine.
fn broadband_pcm(n: usize, sample_rate: f64, amp: f64) -> Vec<i16> {
    let partials = [220.0, 523.0, 941.0, 1637.0];
    (0..n)
        .map(|i| {
            let t = i as f64 / sample_rate;
            let env = 0.6 + 0.4 * (2.0 * std::f64::consts::PI * 3.0 * t).sin();
            let s: f64 = partials
                .iter()
                .enumerate()
                .map(|(k, &f)| {
                    let w = 1.0 / (k as f64 + 1.0);
                    w * (2.0 * std::f64::consts::PI * f * t).sin()
                })
                .sum::<f64>()
                / 2.0833; // normalise Σ(1/k) so the peak stays near ±amp
            (s * env * amp).round().clamp(-32767.0, 32767.0) as i16
        })
        .collect()
}

/// Interleaved multi-channel sine: each lane is the same `hz` tone with a
/// distinct phase so the per-lane content differs (a real stereo block).
fn interleaved_sine(
    channels: usize,
    frames: usize,
    hz: f64,
    sample_rate: f64,
    amp: f64,
) -> Vec<i16> {
    let mut out = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        let t = i as f64 / sample_rate;
        for ch in 0..channels {
            let phase = ch as f64 * std::f64::consts::FRAC_PI_2;
            out.push(((2.0 * std::f64::consts::PI * hz * t + phase).sin() * amp).round() as i16);
        }
    }
    out
}

/// Deinterleave one lane out of an interleaved buffer.
fn lane(pcm: &[i16], channels: usize, ch: usize) -> Vec<i16> {
    pcm.iter().skip(ch).step_by(channels).copied().collect()
}

/// Normalised cross-correlation at lag 0, truncated to the shorter array.
/// 1.0 = identical waveform shape, 0.0 = uncorrelated.
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

/// Little-endian helpers for the RIFF writer.
fn push_u16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn push_u32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

/// Assemble a RIFF/WAVE file around an already-encoded ADPCM `data`
/// payload. `fmt_ext` is the codec-specific `fmt ` extension that follows
/// the 16-byte common `WAVEFORMATEX` prefix (i.e. it starts at `cbSize`).
fn build_wav(
    format_tag: u16,
    channels: u16,
    sample_rate: u32,
    block_align: u16,
    bits_per_sample: u16,
    fmt_ext: &[u8],
    data: &[u8],
) -> Vec<u8> {
    // nAvgBytesPerSec: bytes the decoder consumes per second. For a
    // block-oriented codec this is blockAlign * (rate / samplesPerBlock),
    // but WAV stores a single byte rate; demuxers treat it as advisory.
    // We compute a faithful value from the block geometry below at the
    // call site and pass it in via fmt_ext-independent fields, but the
    // common header needs *some* value. blockAlign * rate / spb is the
    // canonical figure; here we approximate with a value the validator
    // tolerates (it re-derives timing from blockAlign + data length).
    let spb = samples_per_block_for(format_tag, channels, block_align as usize);
    let avg_bytes_per_sec = ((sample_rate as u64 * block_align as u64) / spb as u64).max(1) as u32;

    let mut fmt = Vec::new();
    push_u16(&mut fmt, format_tag);
    push_u16(&mut fmt, channels);
    push_u32(&mut fmt, sample_rate);
    push_u32(&mut fmt, avg_bytes_per_sec);
    push_u16(&mut fmt, block_align);
    push_u16(&mut fmt, bits_per_sample);
    fmt.extend_from_slice(fmt_ext); // cbSize + codec-specific trailer

    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    let riff_size_pos = wav.len();
    push_u32(&mut wav, 0); // patched below
    wav.extend_from_slice(b"WAVE");

    wav.extend_from_slice(b"fmt ");
    push_u32(&mut wav, fmt.len() as u32);
    wav.extend_from_slice(&fmt);
    if fmt.len() % 2 == 1 {
        wav.push(0);
    }

    wav.extend_from_slice(b"data");
    push_u32(&mut wav, data.len() as u32);
    wav.extend_from_slice(data);
    if data.len() % 2 == 1 {
        wav.push(0);
    }

    let riff_size = (wav.len() - 8) as u32;
    wav[riff_size_pos..riff_size_pos + 4].copy_from_slice(&riff_size.to_le_bytes());
    wav
}

fn samples_per_block_for(format_tag: u16, channels: u16, block_align: usize) -> usize {
    let variant = match format_tag {
        0x0002 => Variant::Ms,
        0x0011 => Variant::ImaWav,
        _ => unreachable!("unsupported tag for spb"),
    };
    variant
        .samples_per_block(channels, block_align)
        .expect("block geometry valid")
}

/// MS-ADPCM `fmt ` extension: `cbSize` (=32), `wSamplesPerBlock`,
/// `wNumCoef` (=7), and 7 `i16` coefficient pairs taken from the spec
/// standard table our decoder also seeds.
fn ms_fmt_ext(samples_per_block: u16) -> Vec<u8> {
    let mut ext = Vec::new();
    // cbSize = 2 (spb) + 2 (numCoef) + 7*4 (coeff pairs) = 32.
    push_u16(&mut ext, 32);
    push_u16(&mut ext, samples_per_block);
    push_u16(&mut ext, ms::STANDARD_COEFFS.len() as u16);
    for &(c1, c2) in ms::STANDARD_COEFFS.iter() {
        push_u16(&mut ext, c1 as u16);
        push_u16(&mut ext, c2 as u16);
    }
    ext
}

/// IMA-WAV `fmt ` extension: `cbSize` (=2) + `wSamplesPerBlock`.
fn ima_wav_fmt_ext(samples_per_block: u16) -> Vec<u8> {
    let mut ext = Vec::new();
    push_u16(&mut ext, 2);
    push_u16(&mut ext, samples_per_block);
    ext
}

/// Decode a container file at `in_path` to raw interleaved s16le PCM with
/// the opaque validator, preserving the source channel count.
fn validator_decode_file(in_path: &std::path::Path, tag_label: &str) -> Option<Vec<i16>> {
    let out_path = fixtures_dir().join(format!("enc_validate_{tag_label}.pcm"));
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-i",
            in_path.to_str().unwrap(),
            "-f",
            "s16le",
            out_path.to_str().unwrap(),
        ])
        .output()
        .ok()?;
    if !status.status.success() {
        eprintln!(
            "validator failed to decode our {tag_label} container:\n{}",
            String::from_utf8_lossy(&status.stderr)
        );
        return None;
    }
    let raw = fs::read(&out_path).ok()?;
    Some(
        raw.chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect(),
    )
}

/// Write `wav` to a fixture path and decode it through the validator.
fn validator_decode_wav(wav: &[u8], tag_label: &str) -> Option<Vec<i16>> {
    fs::create_dir_all(fixtures_dir()).ok();
    let in_path = fixtures_dir().join(format!("enc_validate_{tag_label}.wav"));
    fs::write(&in_path, wav).ok()?;
    validator_decode_file(&in_path, tag_label)
}

/// Shared driver for the WAV-tagged block encoders (MS / IMA-WAV). Encodes
/// `pcm` (interleaved, `channels` lanes) into whole blocks of `block_size`
/// bytes, wraps them in a WAV with `format_tag` + `fmt_ext`, decodes via
/// the validator, and asserts each lane's reconstruction tracks the input.
#[allow(clippy::too_many_arguments)]
fn encoder_validate_wav(
    tag_label: &str,
    format_tag: u16,
    channels: usize,
    block_size: usize,
    spb_frames: usize,
    pcm: &[i16],
    encode: impl Fn(&[i16]) -> Vec<u8>,
    fmt_ext: &[u8],
    min_xcorr: f64,
) {
    if !have_validator() {
        eprintln!("validator absent — skipping encoder-validate {tag_label}");
        return;
    }

    // Encode whole blocks only; a block holds `spb_frames` frames =
    // `spb_frames * channels` interleaved samples.
    let block_samples = spb_frames * channels;
    let mut data = Vec::new();
    let mut encoded_frames = 0usize;
    for chunk in pcm.chunks(block_samples) {
        if chunk.len() < block_samples {
            break;
        }
        let blk = encode(chunk);
        assert_eq!(
            blk.len(),
            block_size,
            "{tag_label}: encoder emitted {} bytes, expected block size {block_size}",
            blk.len()
        );
        data.extend_from_slice(&blk);
        encoded_frames += spb_frames;
    }
    assert!(encoded_frames > 0, "{tag_label}: no full blocks encoded");

    let wav = build_wav(
        format_tag,
        channels as u16,
        22050,
        block_size as u16,
        4,
        fmt_ext,
        &data,
    );

    let Some(decoded) = validator_decode_wav(&wav, tag_label) else {
        return;
    };
    assert_eq!(decoded.len() % channels, 0, "{tag_label}: ragged decode");
    let decoded_frames = decoded.len() / channels;

    // The validator decodes whole blocks; allow a small frame tolerance.
    assert!(
        (decoded_frames as i64 - encoded_frames as i64).abs()
            <= (encoded_frames as i64 / 50 + spb_frames as i64),
        "{tag_label}: validator decoded {decoded_frames} frames, expected ~{encoded_frames}",
    );

    let in_frames = pcm.len() / channels;
    let cmp_frames = decoded_frames.min(in_frames);
    for ch in 0..channels {
        let in_lane = lane(&pcm[..cmp_frames * channels], channels, ch);
        let out_lane = lane(&decoded[..cmp_frames * channels], channels, ch);
        let score = xcorr(&out_lane, &in_lane);
        assert!(
            score > min_xcorr,
            "{tag_label} ch{ch}: validator reconstruction of our encoder bytes too divergent: xcorr {score:.4} <= {min_xcorr}",
        );
    }
}

#[test]
fn ms_mono_encoder_bytes_decode_in_validator() {
    let block_size = 1024usize;
    let spb = Variant::Ms
        .samples_per_block(1, block_size)
        .expect("MS block geometry");
    let pcm = sine_pcm(spb * 6, 440.0, 22050.0, 9000.0);
    let ext = ms_fmt_ext(spb as u16);
    encoder_validate_wav(
        "ms_mono",
        0x0002,
        1,
        block_size,
        spb,
        &pcm,
        |chunk| ms_encode_block(chunk, 1, block_size).unwrap(),
        &ext,
        0.97,
    );
}

#[test]
fn ms_stereo_encoder_bytes_decode_in_validator() {
    let block_size = 1024usize;
    let spb = Variant::Ms
        .samples_per_block(2, block_size)
        .expect("MS stereo block geometry");
    let pcm = interleaved_sine(2, spb * 6, 440.0, 22050.0, 9000.0);
    let ext = ms_fmt_ext(spb as u16);
    encoder_validate_wav(
        "ms_stereo",
        0x0002,
        2,
        block_size,
        spb,
        &pcm,
        |chunk| ms_encode_block(chunk, 2, block_size).unwrap(),
        &ext,
        0.97,
    );
}

#[test]
fn ima_wav_mono_encoder_bytes_decode_in_validator() {
    let block_size = 1024usize;
    let spb = Variant::ImaWav
        .samples_per_block(1, block_size)
        .expect("IMA-WAV block geometry");
    let pcm = sine_pcm(spb * 6, 440.0, 22050.0, 9000.0);
    let ext = ima_wav_fmt_ext(spb as u16);
    encoder_validate_wav(
        "ima_wav_mono",
        0x0011,
        1,
        block_size,
        spb,
        &pcm,
        |chunk| ima_encode_block(chunk, 1, block_size).unwrap(),
        &ext,
        0.97,
    );
}

#[test]
fn ima_wav_stereo_encoder_bytes_decode_in_validator() {
    let block_size = 1024usize;
    let spb = Variant::ImaWav
        .samples_per_block(2, block_size)
        .expect("IMA-WAV stereo block geometry");
    let pcm = interleaved_sine(2, spb * 6, 440.0, 22050.0, 9000.0);
    let ext = ima_wav_fmt_ext(spb as u16);
    encoder_validate_wav(
        "ima_wav_stereo",
        0x0011,
        2,
        block_size,
        spb,
        &pcm,
        |chunk| ima_encode_block(chunk, 2, block_size).unwrap(),
        &ext,
        0.97,
    );
}

// ---------------------------------------------------------------------------
// IMA-QT (`adpcm_ima_qt`, QuickTime `ima4`) encoder validation via CAF.
//
// IMA-QT has no WAV tag, so the container is a CAF. We assemble a minimal
// CAF (`desc` + `data`) around our encoder's `34 * channels`-byte blocks
// and let the validator decode it. This mirrors the *decode-side* CAF
// extraction in `wav_decode.rs`, but in the opposite direction: there the
// validator wrote the CAF and we read it; here we write the CAF and the
// validator reads it.
// ---------------------------------------------------------------------------

fn push_u64_be(v: &mut Vec<u8>, x: u64) {
    v.extend_from_slice(&x.to_be_bytes());
}
fn push_u32_be(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_be_bytes());
}

/// Build a CAF file wrapping raw `ima4` block bytes. The `desc` chunk
/// declares the fixed 34-byte / 64-frame `ima4` packet geometry; the
/// `data` chunk carries a 4-byte edit-count prefix followed by the blocks.
fn build_caf(channels: u16, sample_rate: f64, data: &[u8]) -> Vec<u8> {
    let mut caf = Vec::new();
    // File header: 'caff', version 1, flags 0.
    caf.extend_from_slice(b"caff");
    caf.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);

    // desc chunk (32-byte body): see the CAF Audio Description.
    caf.extend_from_slice(b"desc");
    push_u64_be(&mut caf, 32);
    caf.extend_from_slice(&sample_rate.to_be_bytes()); // mSampleRate (f64 BE)
    caf.extend_from_slice(b"ima4"); // mFormatID
    push_u32_be(&mut caf, 0); // mFormatFlags
    push_u32_be(&mut caf, (ima_qt::QT_BLOCK_SIZE * channels as usize) as u32); // mBytesPerPacket
    push_u32_be(&mut caf, ima_qt::QT_SAMPLES_PER_BLOCK as u32); // mFramesPerPacket
    push_u32_be(&mut caf, channels as u32); // mChannelsPerFrame
    push_u32_be(&mut caf, 4); // mBitsPerChannel

    // data chunk: 8-byte size = 4-byte edit count + payload.
    caf.extend_from_slice(b"data");
    push_u64_be(&mut caf, (data.len() + 4) as u64);
    push_u32_be(&mut caf, 0); // mEditCount
    caf.extend_from_slice(data);
    caf
}

/// Encode `pcm` (interleaved, `channels` lanes) into `ima4` blocks, wrap
/// in a CAF, decode via the validator, and check each lane.
fn encoder_validate_ima_qt(tag_label: &str, channels: usize, pcm: &[i16], min_xcorr: f64) {
    if !have_validator() {
        eprintln!("validator absent — skipping encoder-validate {tag_label}");
        return;
    }

    let block_frames = ima_qt::QT_SAMPLES_PER_BLOCK;
    let block_samples = block_frames * channels;
    let mut data = Vec::new();
    let mut encoded_frames = 0usize;
    for chunk in pcm.chunks(block_samples) {
        if chunk.len() < block_samples {
            break;
        }
        let blk = ima_qt_encode_block(chunk, channels).unwrap();
        assert_eq!(
            blk.len(),
            ima_qt::QT_BLOCK_SIZE * channels,
            "{tag_label}: ima4 block size mismatch"
        );
        data.extend_from_slice(&blk);
        encoded_frames += block_frames;
    }
    assert!(encoded_frames > 0, "{tag_label}: no full ima4 blocks");

    let caf = build_caf(channels as u16, 22050.0, &data);
    fs::create_dir_all(fixtures_dir()).ok();
    let in_path = fixtures_dir().join(format!("enc_validate_{tag_label}.caf"));
    fs::write(&in_path, &caf).unwrap();

    let Some(decoded) = validator_decode_file(&in_path, tag_label) else {
        return;
    };
    assert_eq!(decoded.len() % channels, 0, "{tag_label}: ragged decode");
    let decoded_frames = decoded.len() / channels;
    assert!(
        (decoded_frames as i64 - encoded_frames as i64).abs() <= block_frames as i64,
        "{tag_label}: validator decoded {decoded_frames} frames, expected ~{encoded_frames}",
    );

    let in_frames = pcm.len() / channels;
    let cmp_frames = decoded_frames.min(in_frames);
    for ch in 0..channels {
        let in_lane = lane(&pcm[..cmp_frames * channels], channels, ch);
        let out_lane = lane(&decoded[..cmp_frames * channels], channels, ch);
        let score = xcorr(&out_lane, &in_lane);
        assert!(
            score > min_xcorr,
            "{tag_label} ch{ch}: ima4 validator reconstruction too divergent: xcorr {score:.4} <= {min_xcorr}",
        );
    }
}

#[test]
fn ima_qt_mono_encoder_bytes_decode_in_validator() {
    let pcm = sine_pcm(ima_qt::QT_SAMPLES_PER_BLOCK * 20, 440.0, 22050.0, 9000.0);
    encoder_validate_ima_qt("ima_qt_mono", 1, &pcm, 0.97);
}

#[test]
fn ima_qt_stereo_encoder_bytes_decode_in_validator() {
    let pcm = interleaved_sine(2, ima_qt::QT_SAMPLES_PER_BLOCK * 20, 440.0, 22050.0, 9000.0);
    encoder_validate_ima_qt("ima_qt_stereo", 2, &pcm, 0.97);
}

// ---------------------------------------------------------------------------
// Broadband-content + non-default-geometry encoder validation.
//
// A pure tone barely exercises the encoder's adaptive machinery. These
// cases feed a four-partial broadband signal (so the MS per-block
// coefficient search and the IMA step adaptation actually have to track a
// moving spectrum) and, for MS / IMA-WAV, also a *smaller* block size so
// the encoder writes a non-default `wSamplesPerBlock` the validator must
// honour to split the stream correctly.
// ---------------------------------------------------------------------------

#[test]
fn ms_broadband_small_block_encoder_bytes_decode_in_validator() {
    // 256-byte block → samplesPerBlock differs from the 1024-byte default,
    // so the validator relies on our header's wSamplesPerBlock to frame.
    let block_size = 256usize;
    let spb = Variant::Ms
        .samples_per_block(1, block_size)
        .expect("MS small-block geometry");
    let pcm = broadband_pcm(spb * 8, 22050.0, 8000.0);
    let ext = ms_fmt_ext(spb as u16);
    encoder_validate_wav(
        "ms_broadband",
        0x0002,
        1,
        block_size,
        spb,
        &pcm,
        |chunk| ms_encode_block(chunk, 1, block_size).unwrap(),
        &ext,
        0.92,
    );
}

#[test]
fn ima_wav_broadband_small_block_encoder_bytes_decode_in_validator() {
    let block_size = 256usize;
    let spb = Variant::ImaWav
        .samples_per_block(1, block_size)
        .expect("IMA-WAV small-block geometry");
    let pcm = broadband_pcm(spb * 8, 22050.0, 8000.0);
    let ext = ima_wav_fmt_ext(spb as u16);
    encoder_validate_wav(
        "ima_wav_broadband",
        0x0011,
        1,
        block_size,
        spb,
        &pcm,
        |chunk| ima_encode_block(chunk, 1, block_size).unwrap(),
        &ext,
        0.92,
    );
}

#[test]
fn ima_qt_broadband_encoder_bytes_decode_in_validator() {
    let pcm = broadband_pcm(ima_qt::QT_SAMPLES_PER_BLOCK * 24, 22050.0, 8000.0);
    encoder_validate_ima_qt("ima_qt_broadband", 1, &pcm, 0.92);
}
