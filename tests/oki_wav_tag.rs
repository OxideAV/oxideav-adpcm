//! WAV-container OKI ADPCM (`WAVE_FORMAT_OKI_ADPCM` = `0x0010`).
//!
//! The OKI MSM6258/6585/6295 chip-set ADPCM algorithm (the `.vox`
//! codec) also has a WAV-container framing assigned the wave-format tag
//! `0x0010` (SDL_sound "OKI ADPCM Wave Types": the format is "created
//! and read by the OKI ADPCM chip set"). Its 4-bit body is the canonical
//! VOX layout — two samples per byte, high nibble first — so a WAV
//! demuxer that has parsed `WAVEFORMATEX::wFormatTag = 0x0010` resolves
//! straight to the `adpcm_dialogic` decoder and decodes byte-identically
//! to the headerless `.vox` path.
//!
//! These tests are self-contained: no fixtures, no external binaries.
//! The "OKI body" is synthesised with the crate's own typed Dialogic
//! encoder and the registry decode is checked against the typed Dialogic
//! decode of the same bytes.

use oxideav_adpcm::{
    dialogic::{self, Channel, NibbleOrder, Output},
    register_codecs, CODEC_ID_DIALOGIC,
};
use oxideav_core::{
    CodecId, CodecParameters, CodecRegistry, CodecTag, Frame, Packet, ProbeContext, TimeBase,
};

/// Build a deterministic 12-bit-linear PCM sweep for the encoder input.
fn synth_pcm(n: usize) -> Vec<i16> {
    // A small ramp + alternation; values stay well inside i16 and produce
    // a mix of step-up / step-down codes (exercising the adaptation table).
    (0..n)
        .map(|i| {
            let t = i as i32;
            let v = ((t * 37) % 4000) - 2000 + if i % 2 == 0 { 120 } else { -120 };
            v as i16
        })
        .collect()
}

/// Encode PCM into the canonical OKI / VOX 4-bit body (high nibble first).
fn encode_oki_body(pcm: &[i16]) -> Vec<u8> {
    let mut st = Channel::default();
    dialogic::encode_packet(pcm, &mut st, NibbleOrder::HiFirst)
}

#[test]
fn registry_resolves_wave_format_oki_tag_to_dialogic() {
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);
    let tag = CodecTag::wave_format(0x0010);
    let id = reg
        .resolve_tag_ref(&ProbeContext::new(&tag))
        .expect("WAVE_FORMAT_OKI_ADPCM (0x0010) must resolve to a codec");
    assert_eq!(id.as_str(), CODEC_ID_DIALOGIC);
}

#[test]
fn oki_wav_body_decodes_identically_to_typed_vox_path() {
    let pcm = synth_pcm(512);
    let body = encode_oki_body(&pcm);

    // Reference: the typed Dialogic decode of the same body, the path a
    // `.vox` reader uses (HiFirst nibble order, 16-bit widened output).
    let mut ref_state = [Channel::default()];
    let reference =
        dialogic::decode_packet(&body, &mut ref_state, NibbleOrder::HiFirst, Output::Wide16);

    // Registry path: build the decoder the way a WAV demuxer would after
    // reading wFormatTag = 0x0010, then drive the body through it.
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);
    let mut params = CodecParameters::audio(CodecId::new(CODEC_ID_DIALOGIC));
    params.sample_rate = Some(8_000);
    params.channels = Some(1);
    let mut dec = reg
        .first_decoder(&params)
        .expect("dialogic decoder factory");

    let tb = TimeBase::new(1, 8_000);
    let pkt = Packet::new(0, tb, body);
    dec.send_packet(&pkt).expect("send OKI body");
    let Frame::Audio(af) = dec.receive_frame().expect("decode OKI body") else {
        panic!("expected audio frame");
    };

    let decoded: Vec<i16> = af.data[0]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();

    assert_eq!(
        decoded.len(),
        reference.len(),
        "registry OKI decode produced a different sample count than the typed VOX path"
    );
    assert_eq!(
        decoded, reference,
        "registry OKI (tag 0x0010) decode must be byte-identical to the typed .vox decode"
    );
    // Two samples per body byte is the OKI/VOX framing invariant.
    assert_eq!(decoded.len(), pcm.len());
}

#[test]
fn oki_wav_empty_body_is_accepted() {
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);
    let mut params = CodecParameters::audio(CodecId::new(CODEC_ID_DIALOGIC));
    params.sample_rate = Some(8_000);
    params.channels = Some(1);
    let mut dec = reg
        .first_decoder(&params)
        .expect("dialogic decoder factory");
    let pkt = Packet::new(0, TimeBase::new(1, 8_000), Vec::new());
    dec.send_packet(&pkt).expect("empty packet accepted");
    let _ = dec.receive_frame();
}
