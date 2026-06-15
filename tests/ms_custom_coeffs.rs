//! Microsoft ADPCM custom predictor coefficient sets (`WAVEFORMATEX`
//! `wNumCoef` / `aCoeff[]`).
//!
//! Per the Microsoft ADPCM spec (`ADPCMWAVEFORMAT`), the per-block
//! `bPredictor` header byte is an *index into the `aCoeff` array declared
//! in the file header*. A stream begins with the seven standard preset
//! pairs but an encoder may append further custom sets, in which case a
//! block can carry a predictor index >= 7. A decoder that only knows the
//! seven presets cannot decode such a block; one that reads the trailer's
//! `aCoeff` table can.
//!
//! These tests drive the registry decode path the way a WAV demuxer would:
//! it parses the `WAVEFORMATEX` trailer into `CodecParameters::extradata`,
//! then builds the `adpcm_ms` decoder, which resolves the custom table and
//! decodes a block addressing the eighth (custom) coefficient set.
//!
//! Self-contained: no fixtures, no external binaries.

use oxideav_adpcm::{ms, register_codecs, CODEC_ID_MS};
use oxideav_core::{CodecId, CodecParameters, CodecRegistry, Frame, Packet, TimeBase};

/// Build a `WAVEFORMATEX` trailer (the bytes following the 16/18-byte base)
/// declaring the seven standard pairs plus the supplied custom pairs.
fn ms_trailer(samples_per_block: u16, custom: &[(i16, i16)]) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(&samples_per_block.to_le_bytes());
    let num = (ms::STANDARD_COEFFS.len() + custom.len()) as u16;
    t.extend_from_slice(&num.to_le_bytes());
    for &(c1, c2) in &ms::STANDARD_COEFFS {
        t.extend_from_slice(&(c1 as i16).to_le_bytes());
        t.extend_from_slice(&(c2 as i16).to_le_bytes());
    }
    for &(c1, c2) in custom {
        t.extend_from_slice(&c1.to_le_bytes());
        t.extend_from_slice(&c2.to_le_bytes());
    }
    t
}

/// One mono MS-ADPCM block whose `bPredictor` selects coefficient set
/// `predictor_index`, with the given initial delta / history samples and a
/// single body byte of two zero nibbles.
fn ms_block(predictor_index: u8, delta: i16, s1: i16, s2: i16) -> Vec<u8> {
    let mut b = Vec::new();
    b.push(predictor_index);
    b.extend_from_slice(&delta.to_le_bytes());
    b.extend_from_slice(&s1.to_le_bytes());
    b.extend_from_slice(&s2.to_le_bytes());
    b.push(0x00);
    b
}

fn decode_via_registry(extradata: Vec<u8>, block: Vec<u8>) -> oxideav_core::Result<Vec<i16>> {
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);
    let mut params = CodecParameters::audio(CodecId::new(CODEC_ID_MS));
    params.sample_rate = Some(22_050);
    params.channels = Some(1);
    params.extradata = extradata;
    let mut dec = reg.first_decoder(&params).expect("ms decoder factory");
    let pkt = Packet::new(0, TimeBase::new(1, 22_050), block);
    dec.send_packet(&pkt)?;
    let Frame::Audio(af) = dec.receive_frame()? else {
        panic!("expected audio frame");
    };
    Ok(af.data[0]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect())
}

#[test]
fn standard_trailer_decodes_standard_blocks() {
    // The classic cbSize=32 trailer (no custom sets) decodes a normal block.
    let extra = ms_trailer(0x01F4, &[]);
    let block = ms_block(1, 16, 1000, 2000);
    let pcm = decode_via_registry(extra, block).expect("standard decode");
    // prelude = [s2, s1] = [2000, 1000].
    assert_eq!(&pcm[..2], &[2000, 1000]);
}

#[test]
fn custom_eighth_set_block_decodes_only_with_trailer() {
    // Custom set 7 = standard index-1 second-order pair (512, -256).
    let custom = [(512i16, -256i16)];
    let block = ms_block(7, 16, 1000, 2000);

    // Without the trailer the standard 7-set decoder rejects index 7.
    let no_trailer = decode_via_registry(Vec::new(), block.clone());
    assert!(
        no_trailer.is_err(),
        "predictor index 7 must be rejected when no custom table is declared"
    );

    // With the trailer it decodes; first body sample uses the custom pair:
    // predicted = (1000*512 + 2000*-256) >> 8 = 0.
    let pcm = decode_via_registry(ms_trailer(0x01F4, &custom), block).expect("custom decode");
    assert_eq!(&pcm[..2], &[2000, 1000]); // prelude unchanged
    assert_eq!(pcm[2], 0, "custom-set prediction");
}

#[test]
fn malformed_trailer_is_rejected_at_construction() {
    // wNumCoef = 8 declared but the eighth pair's bytes are missing.
    let mut t = ms_trailer(0x01F4, &[]);
    t[2] = 8; // patch wNumCoef to 8
    t[3] = 0;
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);
    let mut params = CodecParameters::audio(CodecId::new(CODEC_ID_MS));
    params.channels = Some(1);
    params.extradata = t;
    assert!(
        reg.first_decoder(&params).is_err(),
        "a truncated aCoeff trailer must fail decoder construction"
    );
}
