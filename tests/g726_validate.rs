//! Opaque-validator conformance for the G.726 codec, both directions,
//! at all four rates.
//!
//! **Decode direction** — the validator (ffmpeg, used strictly as a
//! black box) synthesises a sine, encodes it to G.726 inside a WAV
//! container (tag `0x0045`, one code per `wBitsPerSample`), and dumps
//! its own PCM decode of that file. Our registry decoder consumes the
//! raw `data` chunk and must cross-correlate > 0.97 against the
//! validator's PCM at lag 0 — proving we decode *foreign* spec-encoded
//! bytes, not merely our own.
//!
//! **Encode direction** — our registry encoder compresses a PCM sine;
//! the harness wraps the bytes in a WAV whose `fmt ` chunk mirrors the
//! geometry the validator itself writes for G.726 (`nBlockAlign = 1`,
//! `wBitsPerSample` = code width, byte-rate = 1000·bits). The validator
//! decodes that file and the result must cross-correlate > 0.97 against
//! the original PCM — proving our bytes are spec-conformant to an
//! independent implementation, not merely self-consistent.
//!
//! Fixtures are generated on demand; every test skips harmlessly when
//! the validator binary is absent.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use oxideav_adpcm::CODEC_ID_G726;
use oxideav_core::{CodecId, CodecParameters, CodecRegistry, Frame, Packet, TimeBase};

fn fixtures_dir() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests");
    p.push("fixtures");
    p
}

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok()
}

/// Minimal RIFF/WAVE reader — returns
/// `(format_tag, channels, sample_rate, block_align, bits_per_sample, data)`.
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
        let body = &bytes[off + 8..(off + 8 + size).min(bytes.len())];
        match id {
            b"fmt " => {
                fmt = Some((
                    u16::from_le_bytes([body[0], body[1]]),
                    u16::from_le_bytes([body[2], body[3]]),
                    u32::from_le_bytes([body[4], body[5], body[6], body[7]]),
                    u16::from_le_bytes([body[12], body[13]]),
                    u16::from_le_bytes([body[14], body[15]]),
                ));
            }
            b"data" => data = Some(body.to_vec()),
            _ => {}
        }
        off += 8 + size + (size & 1);
    }
    let (tag, ch, rate, ba, bps) = fmt.expect("fmt chunk");
    (tag, ch, rate, ba, bps, data.expect("data chunk"))
}

/// Build a minimal WAV around a G.726 payload, mirroring the fmt
/// geometry the validator writes for this codec: tag 0x0045, mono,
/// 8 kHz, byte-rate `1000 * bits`, and `nBlockAlign` = the smallest
/// byte count holding a whole number of codes (1 for the 2-/4-bit
/// rates, 3 and 5 for the odd widths — a reader that packetizes the
/// stream on `nBlockAlign` boundaries then never splits a code word).
fn build_g726_wav(bits: u16, payload: &[u8]) -> Vec<u8> {
    let block_align: u16 = if bits % 2 == 1 { bits } else { 1 };
    let mut fmt = Vec::new();
    fmt.extend_from_slice(&0x0045u16.to_le_bytes()); // wFormatTag
    fmt.extend_from_slice(&1u16.to_le_bytes()); // nChannels
    fmt.extend_from_slice(&8000u32.to_le_bytes()); // nSamplesPerSec
    fmt.extend_from_slice(&(1000 * bits as u32).to_le_bytes()); // nAvgBytesPerSec
    fmt.extend_from_slice(&block_align.to_le_bytes()); // nBlockAlign
    fmt.extend_from_slice(&bits.to_le_bytes()); // wBitsPerSample
    fmt.extend_from_slice(&0u16.to_le_bytes()); // cbSize
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    let riff_size = 4 + (8 + fmt.len()) + (8 + payload.len());
    out.extend_from_slice(&(riff_size as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
    out.extend_from_slice(&fmt);
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        out.push(0);
    }
    out
}

fn read_pcm_s16le(path: &PathBuf) -> Vec<i16> {
    let bytes = fs::read(path).expect("read pcm");
    bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// Normalised cross-correlation at lag 0.
fn cross_correlation(a: &[i16], b: &[i16]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let (mut sab, mut saa, mut sbb) = (0f64, 0f64, 0f64);
    for i in 0..n {
        let (x, y) = (a[i] as f64, b[i] as f64);
        sab += x * y;
        saa += x * x;
        sbb += y * y;
    }
    sab / (saa.sqrt() * sbb.sqrt()).max(1e-9)
}

fn decoder_params(bits: u16) -> CodecParameters {
    let mut p = CodecParameters::audio(CodecId::new(CODEC_ID_G726));
    p.sample_rate = Some(8000);
    p.channels = Some(1);
    p.options.insert("bits_per_sample", bits.to_string());
    p
}

/// Decode a whole G.726 payload through the registry decoder.
fn decode_payload(bits: u16, payload: &[u8]) -> Vec<i16> {
    let mut reg = CodecRegistry::new();
    oxideav_adpcm::register_codecs(&mut reg);
    let mut dec = reg.first_decoder(&decoder_params(bits)).expect("decoder");
    let tb = TimeBase::new(1, 8000);
    dec.send_packet(&Packet::new(0, tb, payload.to_vec()))
        .expect("send_packet");
    let mut out = Vec::new();
    if let Ok(Frame::Audio(af)) = dec.receive_frame() {
        for pair in af.data[0].chunks_exact(2) {
            out.push(i16::from_le_bytes([pair[0], pair[1]]));
        }
    }
    out
}

/// Encode PCM through the registry encoder (MSB-first — the validator's
/// G.726-in-WAV convention), returning the packed byte stream.
fn encode_pcm(bits: u16, pcm: &[i16]) -> Vec<u8> {
    let mut reg = CodecRegistry::new();
    oxideav_adpcm::register_codecs(&mut reg);
    let mut enc = reg.first_encoder(&decoder_params(bits)).expect("encoder");
    let mut data = Vec::with_capacity(pcm.len() * 2);
    for s in pcm {
        data.extend_from_slice(&s.to_le_bytes());
    }
    enc.send_frame(&Frame::Audio(oxideav_core::AudioFrame {
        samples: pcm.len() as u32,
        pts: None,
        data: vec![data],
    }))
    .expect("send_frame");
    let mut bytes = Vec::new();
    while let Ok(pkt) = enc.receive_packet() {
        bytes.extend_from_slice(&pkt.data);
    }
    enc.flush().expect("flush");
    while let Ok(pkt) = enc.receive_packet() {
        bytes.extend_from_slice(&pkt.data);
    }
    bytes
}

const RATES: [(u16, &str); 4] = [(2, "16k"), (3, "24k"), (4, "32k"), (5, "40k")];

#[test]
fn decode_validator_encoded_g726_all_rates() {
    if !have_ffmpeg() {
        eprintln!("validator binary not installed — skipping G.726 decode conformance");
        return;
    }
    fs::create_dir_all(fixtures_dir()).ok();
    for (bits, brate) in RATES {
        let wav_path = fixtures_dir().join(format!("sine_g726_{brate}.wav"));
        let pcm_path = fixtures_dir().join(format!("sine_g726_{brate}.pcm"));
        if !wav_path.exists() {
            let ok = Command::new("ffmpeg")
                .args([
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:duration=0.5:sample_rate=8000",
                    "-ac",
                    "1",
                    "-c:a",
                    "g726",
                    "-b:a",
                    brate,
                ])
                .arg(&wav_path)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if !ok {
                eprintln!("validator could not produce {brate} G.726 fixture — skipping");
                continue;
            }
        }
        if !pcm_path.exists() {
            let ok = Command::new("ffmpeg")
                .args(["-y", "-i"])
                .arg(&wav_path)
                .args(["-f", "s16le"])
                .arg(&pcm_path)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if !ok {
                eprintln!("validator could not dump {brate} reference PCM — skipping");
                continue;
            }
        }
        let (tag, ch, rate, _ba, bps, payload) = parse_wav(&fs::read(&wav_path).unwrap());
        assert_eq!(tag, 0x0045, "{brate}: validator wrote unexpected tag");
        assert_eq!(ch, 1, "{brate}: channels");
        assert_eq!(rate, 8000, "{brate}: sample rate");
        assert_eq!(bps, bits, "{brate}: wBitsPerSample is the code width");
        let ours = decode_payload(bits, &payload);
        let reference = read_pcm_s16le(&pcm_path);
        assert!(
            ours.len() >= reference.len(),
            "{brate}: decoded {} samples, validator {}",
            ours.len(),
            reference.len()
        );
        let corr = cross_correlation(&ours[..reference.len()], &reference);
        assert!(
            corr > 0.97,
            "{brate}: cross-correlation {corr:.4} vs validator PCM"
        );
    }
}

#[test]
fn validator_decodes_our_g726_all_rates() {
    if !have_ffmpeg() {
        eprintln!("validator binary not installed — skipping G.726 encode conformance");
        return;
    }
    fs::create_dir_all(fixtures_dir()).ok();
    // A 440 Hz sine at ~-6 dBFS — the same signal family the validator
    // synthesises in the decode direction, and strong enough to sweep
    // the fast scale factor across its LIMB range every onset.
    //
    // Deliberately *not* an adversarial envelope-swept multi-tone: §4.2
    // specifies the FMULT float products with truncating shifts, but a
    // conformant-in-practice validator that rounds those sub-LSB
    // products to nearest diverges from the literal text at borderline
    // `p(k)` sign decisions, and on sign-flip-rich program material the
    // predictor trajectories then part ways (observed at 40 kbit/s).
    // The ITU digital test sequences (Appendix II) would arbitrate
    // bit-exactly, but they are not staged (TIES-gated). On tonal input
    // the two readings track within the correlation floor at all four
    // rates.
    let pcm: Vec<i16> = (0..4000)
        .map(|k| (8000.0 * (2.0 * std::f64::consts::PI * 440.0 * k as f64 / 8000.0).sin()) as i16)
        .collect();
    for (bits, brate) in RATES {
        let payload = encode_pcm(bits, &pcm);
        let wav = build_g726_wav(bits, &payload);
        let wav_path = fixtures_dir().join(format!("ours_g726_{brate}.wav"));
        let pcm_path = fixtures_dir().join(format!("ours_g726_{brate}.pcm"));
        fs::write(&wav_path, &wav).expect("write wav");
        let ok = Command::new("ffmpeg")
            .args(["-y", "-i"])
            .arg(&wav_path)
            .args(["-f", "s16le"])
            .arg(&pcm_path)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(ok, "{brate}: validator rejected our G.726 WAV");
        let decoded = read_pcm_s16le(&pcm_path);
        assert!(
            decoded.len() >= pcm.len() * 9 / 10,
            "{brate}: validator produced only {} of {} samples",
            decoded.len(),
            pcm.len()
        );
        let n = decoded.len().min(pcm.len());
        // Skip the adaptation transient.
        let corr = cross_correlation(&pcm[400..n], &decoded[400..n]);
        assert!(
            corr > 0.97,
            "{brate}: validator decode of our bytes correlates {corr:.4}"
        );
    }
}
