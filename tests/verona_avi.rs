//! End-to-end test against the user-reported file that kicked off this
//! crate: `verona60avi56k.avi` — an AVI with an adpcm_ms audio track and
//! msmpeg4v2 video.
//!
//! The test only runs when the fixture is present (downloaded manually
//! or by CI). It does **not** depend on any demuxer crate — the AVI file
//! is scanned for the `auds` WAVEFORMATEX and the 01wb packets with a
//! minimal inline reader, then those packets are fed one-by-one through
//! our decoder. Pass criterion: every packet decodes without error and
//! at least the first 10 000 decoded samples exist and aren't all zero.

use std::fs;
use std::path::PathBuf;

use oxideav_adpcm::CODEC_ID_MS;
use oxideav_codec::CodecRegistry;
use oxideav_core::{CodecId, CodecParameters, Frame, Packet, TimeBase};

fn fixture_path() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests");
    p.push("fixtures");
    p.push("verona.avi");
    p
}

/// Walk RIFF AVI chunks looking for `fmt ` inside `strl`/`strf` and the
/// `00wb` / `01wb` audio packets inside `movi`. Very minimal — we don't
/// care about seeking or video here.
fn scan_avi(bytes: &[u8]) -> (u16, u16, u32, u16, Vec<Vec<u8>>) {
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"AVI ");

    let mut format_tag = 0u16;
    let mut channels = 0u16;
    let mut samples_per_sec = 0u32;
    let mut block_align = 0u16;
    let mut packets: Vec<Vec<u8>> = Vec::new();
    // Track the most recent 'strh' fcc — we only want audio's fmt chunk.
    // For the verona file stream 0 is video and stream 1 is audio, so
    // we look for the second `strf` that follows a `auds` strh.
    let mut next_strf_is_audio = false;

    let mut off = 12usize;
    let limit = bytes.len();
    while off + 8 <= limit {
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
            b"LIST" => {
                // body: 4-byte list-type + nested chunks.
                let list_type = &bytes[body_start..body_start + 4];
                if list_type == b"movi" {
                    // Scan movi body for audio packets.
                    let mut m = body_start + 4;
                    while m + 8 <= body_end.min(limit) {
                        let cid = &bytes[m..m + 4];
                        let csize = u32::from_le_bytes([
                            bytes[m + 4],
                            bytes[m + 5],
                            bytes[m + 6],
                            bytes[m + 7],
                        ]) as usize;
                        let cstart = m + 8;
                        let cend = cstart + csize;
                        if cid == b"LIST" {
                            // rec  nested — skip to its body.
                            m = cstart + 4;
                            continue;
                        }
                        if &cid[2..4] == b"wb" && cend <= limit {
                            packets.push(bytes[cstart..cend].to_vec());
                        }
                        m = cend + (csize & 1);
                    }
                    off = body_end + (size & 1);
                    continue;
                }
                // Recurse into other LISTs (hdrl/strl/…).
                off = body_start + 4;
                continue;
            }
            b"strh" => {
                let fcc = &bytes[body_start..body_start + 4];
                next_strf_is_audio = fcc == b"auds";
            }
            b"strf" => {
                if next_strf_is_audio {
                    let b = &bytes[body_start..body_end];
                    if b.len() >= 16 {
                        format_tag = u16::from_le_bytes([b[0], b[1]]);
                        channels = u16::from_le_bytes([b[2], b[3]]);
                        samples_per_sec = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
                        block_align = u16::from_le_bytes([b[12], b[13]]);
                    }
                    next_strf_is_audio = false;
                }
            }
            _ => {}
        }
        off = body_end + (size & 1);
    }
    (format_tag, channels, samples_per_sec, block_align, packets)
}

#[test]
fn verona_adpcm_ms_audio_track_decodes() {
    let path = fixture_path();
    if !path.exists() {
        eprintln!(
            "fixture {} missing — skipping (run `scripts/fetch-fixtures.sh` or `curl` it down)",
            path.display()
        );
        return;
    }

    let bytes = fs::read(&path).expect("read verona.avi");
    let (format_tag, channels, sample_rate, block_align, packets) = scan_avi(&bytes);

    assert_eq!(format_tag, 0x0002, "fixture should be WAVE_FORMAT_ADPCM");
    assert!(channels > 0, "fixture must declare channels");
    assert!(sample_rate > 0, "fixture must declare sample rate");
    assert!(block_align > 0, "fixture must declare block_align");
    assert!(
        !packets.is_empty(),
        "fixture must contain at least one audio packet"
    );

    let mut reg = CodecRegistry::new();
    oxideav_adpcm::register(&mut reg);
    let mut params = CodecParameters::audio(CodecId::new(CODEC_ID_MS));
    params.sample_rate = Some(sample_rate);
    params.channels = Some(channels);
    let mut dec = reg.make_decoder(&params).expect("build adpcm_ms decoder");

    let tb = TimeBase::new(1, sample_rate as i64);
    let mut total_samples = 0usize;
    let mut nonzero_samples = 0usize;
    // Packets may each carry one or more adpcm_ms blocks; some AVI
    // encoders write a multi-block chunk. Split on block_align.
    for pkt_bytes in packets.iter().take(200) {
        for chunk in pkt_bytes.chunks(block_align as usize) {
            if chunk.len() < block_align as usize {
                break;
            }
            let pkt = Packet::new(1, tb, chunk.to_vec());
            dec.send_packet(&pkt).expect("send_packet");
            let Frame::Audio(af) = dec.receive_frame().expect("receive_frame") else {
                panic!("expected audio frame");
            };
            let bytes = &af.data[0];
            total_samples += bytes.len() / 2;
            for c in bytes.chunks_exact(2) {
                let s = i16::from_le_bytes([c[0], c[1]]);
                if s != 0 {
                    nonzero_samples += 1;
                }
            }
        }
    }

    assert!(total_samples > 10_000, "decoded only {total_samples} samples from verona.avi");
    // Speech audio is rarely all zeroes — at least 50% of the samples
    // should be nonzero.
    assert!(
        nonzero_samples * 2 > total_samples,
        "decoded stream looks silent: {nonzero_samples}/{total_samples} nonzero"
    );
}
