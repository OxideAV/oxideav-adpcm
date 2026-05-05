//! End-to-end integration test: parse a ffmpeg-produced WAV file, feed
//! each data chunk block through our decoder, and compare the result to
//! a reference PCM dump produced by ffmpeg itself (used as an opaque
//! oracle). Because ADPCM is lossy the comparison tolerates small
//! sample-by-sample deviations (we don't promise bit-exactness with
//! ffmpeg, only "plausible" reconstruction); but we do insist on:
//! decoded sample count matches the reference, and the magnitudes track
//! each other (cross-correlation at lag 0 is high).
//!
//! Fixtures are generated on demand — see `ensure_fixtures()`. If
//! `ffmpeg` is not on $PATH, the tests are skipped with a harmless
//! `eprintln!`; CI uses `ubuntu-latest` images which ship ffmpeg.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use oxideav_adpcm::{ima_wav, ms, yamaha, CODEC_ID_IMA_WAV, CODEC_ID_MS, CODEC_ID_YAMAHA};
use oxideav_core::CodecRegistry;
use oxideav_core::{CodecId, CodecParameters, Frame, Packet, TimeBase};

fn fixtures_dir() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests");
    p.push("fixtures");
    p
}

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok()
}

fn ensure_fixture(name: &str, build: impl FnOnce() -> bool) -> Option<PathBuf> {
    let path = fixtures_dir().join(name);
    if path.exists() {
        return Some(path);
    }
    if !have_ffmpeg() {
        eprintln!("ffmpeg not installed — skipping test that needs {name}");
        return None;
    }
    fs::create_dir_all(fixtures_dir()).ok();
    if !build() {
        eprintln!("fixture generation failed for {name}; skipping");
        return None;
    }
    path.exists().then_some(path)
}

fn ensure_sine_fixture(codec: &str, ext: &str, fmt: Option<&str>) -> Option<PathBuf> {
    let name = format!("sine_{codec}.{ext}");
    ensure_fixture(&name, || {
        let out = fixtures_dir().join(&name);
        let mut cmd = Command::new("ffmpeg");
        cmd.args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=0.5:sample_rate=22050",
            "-c:a",
            codec,
        ]);
        if let Some(f) = fmt {
            cmd.args(["-f", f]);
        }
        cmd.arg(&out);
        cmd.output().map(|o| o.status.success()).unwrap_or(false)
    })
}

fn ensure_pcm_fixture(source: &str, name: &str) -> Option<PathBuf> {
    let src = fixtures_dir().join(source);
    if !src.exists() {
        return None;
    }
    ensure_fixture(name, || {
        let out = fixtures_dir().join(name);
        Command::new("ffmpeg")
            .args([
                "-y",
                "-i",
                src.to_str().unwrap(),
                "-f",
                "s16le",
                "-ar",
                "22050",
                "-ac",
                "1",
                out.to_str().unwrap(),
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

/// Very small RIFF / WAVE parser — just enough to find `fmt ` and `data`.
/// Returns `(format_tag, channels, sample_rate, block_align,
/// bits_per_sample, data)`.
fn parse_wav(bytes: &[u8]) -> (u16, u16, u32, u16, u16, Vec<u8>) {
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    let mut off = 12usize;
    let mut fmt: Option<(u16, u16, u32, u16, u16)> = None;
    let mut data: Option<Vec<u8>> = None;
    while off + 8 <= bytes.len() {
        let id = &bytes[off..off + 4];
        let size = u32::from_le_bytes([
            bytes[off + 4],
            bytes[off + 5],
            bytes[off + 6],
            bytes[off + 7],
        ]) as usize;
        let body_start = off + 8;
        let body_end = body_start + size;
        match id {
            b"fmt " => {
                let b = &bytes[body_start..body_end];
                let format_tag = u16::from_le_bytes([b[0], b[1]]);
                let channels = u16::from_le_bytes([b[2], b[3]]);
                let samples_per_sec = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
                let block_align = u16::from_le_bytes([b[12], b[13]]);
                let bits_per_sample = u16::from_le_bytes([b[14], b[15]]);
                fmt = Some((
                    format_tag,
                    channels,
                    samples_per_sec,
                    block_align,
                    bits_per_sample,
                ));
            }
            b"data" => {
                data = Some(bytes[body_start..body_end].to_vec());
            }
            _ => {}
        }
        off = body_end + (size & 1); // pad to even boundary
    }
    let (f, c, r, ba, bps) = fmt.expect("fmt");
    let d = data.expect("data");
    (f, c, r, ba, bps, d)
}

/// Compute a quick waveform-similarity score: 1.0 = identical, 0.0 or
/// negative = uncorrelated. Uses the normalised cross-correlation
/// truncated to the shorter of the two arrays.
fn xcorr(a: &[i16], b: &[i16]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let mut num = 0f64;
    let mut da = 0f64;
    let mut db = 0f64;
    for i in 0..n {
        let ai = a[i] as f64;
        let bi = b[i] as f64;
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

fn decode_wav_with_our_decoder(codec_id: &str, wav: &[u8]) -> Vec<i16> {
    let (_format_tag, channels, sample_rate, block_align, _bits, data) = parse_wav(wav);
    let mut params = CodecParameters::audio(CodecId::new(codec_id));
    params.sample_rate = Some(sample_rate);
    params.channels = Some(channels);
    let mut reg = CodecRegistry::new();
    oxideav_adpcm::register_codecs(&mut reg);
    let mut dec = reg
        .make_decoder(&params)
        .expect("our decoder supports the parsed format");

    let mut pcm = Vec::<i16>::new();
    // For MS/IMA-WAV each block is `block_align` bytes; for Yamaha there
    // is no concept of a block, so we feed a single packet.
    let is_blocked = matches!(codec_id, CODEC_ID_MS | CODEC_ID_IMA_WAV);
    let tb = TimeBase::new(1, sample_rate as i64);
    if is_blocked {
        let ba = block_align as usize;
        assert!(ba > 0);
        for chunk in data.chunks(ba) {
            if chunk.len() < ba {
                break; // last short chunk: trailing padding, ignore
            }
            let pkt = Packet::new(0, tb, chunk.to_vec());
            dec.send_packet(&pkt).unwrap();
            let Frame::Audio(af) = dec.receive_frame().unwrap() else {
                panic!("expected audio frame");
            };
            let bytes = &af.data[0];
            for c in bytes.chunks_exact(2) {
                pcm.push(i16::from_le_bytes([c[0], c[1]]));
            }
        }
    } else {
        // Yamaha: single packet.
        let pkt = Packet::new(0, tb, data);
        dec.send_packet(&pkt).unwrap();
        let Frame::Audio(af) = dec.receive_frame().unwrap() else {
            panic!("expected audio frame");
        };
        let bytes = &af.data[0];
        for c in bytes.chunks_exact(2) {
            pcm.push(i16::from_le_bytes([c[0], c[1]]));
        }
    }
    pcm
}

fn load_pcm(path: &Path) -> Vec<i16> {
    let raw = fs::read(path).unwrap();
    raw.chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// Run a round-trip against ffmpeg-generated fixtures for a given codec.
fn check_variant(codec: &str, codec_id: &str) {
    let Some(wav_path) = ensure_sine_fixture(codec, "wav", None) else {
        return;
    };
    let Some(ref_pcm) = ensure_pcm_fixture(
        wav_path.file_name().unwrap().to_str().unwrap(),
        &format!("sine_{codec}.pcm"),
    ) else {
        return;
    };

    let wav = fs::read(&wav_path).unwrap();
    let ours = decode_wav_with_our_decoder(codec_id, &wav);
    let reference = load_pcm(&ref_pcm);

    // Sample counts should match (ADPCM block sizing may leave a small
    // trailing remainder).
    let expected = reference.len();
    let got = ours.len();
    assert!(
        (got as i64 - expected as i64).abs() <= (expected as i64 / 100 + 64),
        "{codec}: sample count drift — ours {got} vs ref {expected}",
    );

    let score = xcorr(&ours, &reference);
    assert!(
        score > 0.98,
        "{codec}: low waveform similarity with ffmpeg reference: {score:.4}",
    );
}

#[test]
fn ms_adpcm_vs_ffmpeg_reference() {
    check_variant("adpcm_ms", CODEC_ID_MS);
}

#[test]
fn ima_wav_adpcm_vs_ffmpeg_reference() {
    check_variant("adpcm_ima_wav", CODEC_ID_IMA_WAV);
}

#[test]
fn yamaha_adpcm_vs_ffmpeg_reference() {
    check_variant("adpcm_yamaha", CODEC_ID_YAMAHA);
}

// Low-level unit: feeding a hand-crafted MS block through the decoder
// succeeds and yields the documented prelude samples.
#[test]
fn ms_decoder_end_to_end_hand_block() {
    let mut block = Vec::new();
    block.push(0); // predictor index 0 → coef1=256, coef2=0.
    block.extend_from_slice(&16i16.to_le_bytes()); // initial delta
    block.extend_from_slice(&1000i16.to_le_bytes()); // sample1
    block.extend_from_slice(&2000i16.to_le_bytes()); // sample2
    let pcm = ms::decode_block(&block, 1).unwrap();
    assert_eq!(pcm, vec![2000, 1000]);
}

// Low-level unit: IMA-WAV seed sample round-trips.
#[test]
fn ima_wav_decoder_end_to_end_hand_block() {
    let mut block = Vec::new();
    block.extend_from_slice(&(-1000i16).to_le_bytes());
    block.push(10); // step_index
    block.push(0); // reserved
    let pcm = ima_wav::decode_block(&block, 1).unwrap();
    assert_eq!(pcm, vec![-1000]);
}

// Yamaha is continuous: zero-byte packet produces no samples.
#[test]
fn yamaha_empty_packet() {
    let mut st = [yamaha::Channel::default()];
    let out = yamaha::decode_packet(&[], &mut st);
    assert!(out.is_empty());
}
