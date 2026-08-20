//! Muxer-facing container fields on `Encoder::output_params`.
//!
//! The core `CodecParameters::tag` contract says encoders populate the
//! wire tag so muxers know which `wFormatTag` / FourCC to write; the
//! same logic applies to the WAV `fmt ` extension carried as
//! `extradata`. These tests pin what each ADPCM encoder advertises —
//! and close the loop: a decoder built **straight from the encoder's
//! `output_params`** (no hand-set options) must reconstruct the block
//! framing from the advertised trailer and decode a whole multi-block
//! stream exactly as the per-block reference does.

use oxideav_adpcm::{
    ima_wav, ms, register_codecs, CODEC_ID_DIALOGIC, CODEC_ID_G726, CODEC_ID_IMA_QT,
    CODEC_ID_IMA_WAV, CODEC_ID_MS, CODEC_ID_YAMAHA, CODEC_ID_YAMAHA_A,
};
use oxideav_core::{
    AudioFrame, CodecId, CodecParameters, CodecRegistry, CodecTag, Frame, Packet, TimeBase,
};

fn registry() -> CodecRegistry {
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);
    reg
}

fn params_for(codec_id: &str, channels: u16) -> CodecParameters {
    let mut p = CodecParameters::audio(CodecId::new(codec_id));
    p.sample_rate = Some(22_050);
    p.channels = Some(channels);
    p
}

fn wave_tag(t: u16) -> Option<CodecTag> {
    Some(CodecTag::wave_format(t))
}

#[test]
fn encoders_advertise_their_canonical_wire_tag() {
    let reg = registry();
    let cases: &[(&str, Option<CodecTag>)] = &[
        (CODEC_ID_MS, wave_tag(0x0002)),
        (CODEC_ID_IMA_WAV, wave_tag(0x0011)),
        (CODEC_ID_IMA_QT, Some(CodecTag::fourcc(b"ima4"))),
        (CODEC_ID_YAMAHA, wave_tag(0x0020)),
        (CODEC_ID_YAMAHA_A, None),
        (CODEC_ID_DIALOGIC, wave_tag(0x0010)),
        (CODEC_ID_G726, wave_tag(0x0045)), // raw framing default
    ];
    for (id, expect) in cases {
        let enc = reg.first_encoder(&params_for(id, 1)).unwrap();
        assert_eq!(
            enc.output_params().tag,
            *expect,
            "{id}: wrong advertised wire tag"
        );
    }
}

#[test]
fn caller_supplied_tag_round_trips_untouched() {
    // A stream demuxed under an alias tag must re-mux under that tag,
    // not the canonical one (the core round-trip contract).
    let reg = registry();
    for (id, alias) in [
        (CODEC_ID_DIALOGIC, 0x0203u16),
        (CODEC_ID_G726, 0x0064),
        (CODEC_ID_MS, 0x0002),
    ] {
        let mut p = params_for(id, 1);
        p.tag = wave_tag(alias);
        let enc = reg.first_encoder(&p).unwrap();
        assert_eq!(enc.output_params().tag, wave_tag(alias), "{id}");
    }
}

#[test]
fn g726_wav_framing_tag_follows_rate() {
    let reg = registry();
    for (bits, expect) in [(4u8, 0x0040u16), (3, 0x0014), (5, 0x0014)] {
        let mut p = params_for(CODEC_ID_G726, 1);
        p.sample_rate = Some(8_000);
        p.options.insert("framing", "wav");
        p.options.insert("bits_per_sample", bits.to_string());
        let enc = reg.first_encoder(&p).unwrap();
        assert_eq!(
            enc.output_params().tag,
            wave_tag(expect),
            "framing=wav bits={bits}"
        );
        // The aux-free one-field fmt extension rides along.
        assert_eq!(
            enc.output_params().extradata,
            vec![0, 0],
            "framing=wav bits={bits}: nAuxBlockSize=0 extension expected"
        );
    }
    // Raw framing: no fmt extension is defined.
    let enc = reg.first_encoder(&params_for(CODEC_ID_G726, 1)).unwrap();
    assert!(enc.output_params().extradata.is_empty());
}

#[test]
fn ms_encoder_advertises_adpcmwaveformat_trailer() {
    let reg = registry();
    for channels in [1u16, 2] {
        let enc = reg
            .first_encoder(&params_for(CODEC_ID_MS, channels))
            .unwrap();
        let extra = &enc.output_params().extradata;
        // wSamplesPerBlock + wNumCoef + 7 aCoeff pairs = 32 bytes.
        assert_eq!(extra.len(), 32, "{channels}ch");
        let spb = u16::from_le_bytes([extra[0], extra[1]]) as usize;
        // Default geometry: 256-byte blocks.
        assert_eq!(
            Some(spb),
            oxideav_adpcm::Variant::Ms.samples_per_block(channels, 256),
            "{channels}ch: advertised wSamplesPerBlock disagrees with the default geometry"
        );
        // The trailer round-trips through the decoder-side parser to the
        // standard coefficient table.
        let parsed = ms::parse_extradata_coeffs(extra).unwrap().unwrap();
        assert_eq!(&parsed[..], &ms::STANDARD_COEFFS[..]);
    }
}

#[test]
fn ima_wav_encoder_advertises_samples_per_block() {
    let reg = registry();
    for channels in [1u16, 2] {
        let enc = reg
            .first_encoder(&params_for(CODEC_ID_IMA_WAV, channels))
            .unwrap();
        let extra = &enc.output_params().extradata;
        assert_eq!(extra.len(), 2, "{channels}ch");
        let spb = u16::from_le_bytes([extra[0], extra[1]]) as usize;
        // Default 4-bit geometry: near-256-byte blocks, 8 samples per
        // 4-byte group per channel + the header seed sample.
        assert_eq!((spb - 1) % 8, 0, "{channels}ch: spb {spb} off-lattice");
        assert!(spb > 1, "{channels}ch");
    }
    // 3-bit mode re-derives the trailer for the 12-byte-group framing.
    let mut p = params_for(CODEC_ID_IMA_WAV, 1);
    p.options.insert("bits_per_sample", "3");
    let enc = reg.first_encoder(&p).unwrap();
    let extra = &enc.output_params().extradata;
    let spb = u16::from_le_bytes([extra[0], extra[1]]) as usize;
    assert_eq!(
        (spb - 1) % ima_wav::GROUP_SAMPLES_3BIT,
        0,
        "3-bit spb {spb} must be 1 + 32k"
    );
}

#[test]
fn dialogic_encoder_advertises_wpole_trailer() {
    let reg = registry();
    let enc = reg
        .first_encoder(&params_for(CODEC_ID_DIALOGIC, 1))
        .unwrap();
    assert_eq!(enc.output_params().extradata, vec![0, 0]);
}

#[test]
fn caller_supplied_extradata_wins() {
    let reg = registry();
    let mut p = params_for(CODEC_ID_MS, 1);
    // A (valid) caller trailer with a custom wSamplesPerBlock: the
    // encoder must not overwrite it.
    let custom = ms::build_extradata(500, &ms::STANDARD_COEFFS).unwrap();
    p.extradata = custom.clone();
    let enc = reg.first_encoder(&p).unwrap();
    assert_eq!(enc.output_params().extradata, custom);
}

// ---------------------------------------------------------------------------
// The loop-closure proof: encoder → output_params → decoder, no options.
// ---------------------------------------------------------------------------

fn synth_pcm(n: usize, channels: usize) -> Vec<i16> {
    (0..n * channels)
        .map(|i| {
            let t = (i / channels) as f64 / 22_050.0;
            let ph = (i % channels) as f64 * 0.5;
            ((t * 440.0 * std::f64::consts::TAU + ph).sin() * 9000.0) as i16
        })
        .collect()
}

#[test]
fn decoder_built_from_encoder_output_params_splits_blocks() {
    // Encode a signal, concatenate EVERY packet into one big multi-block
    // buffer (what a WAV data chunk is), then decode it through a
    // decoder built from the encoder's own output_params. The advertised
    // wSamplesPerBlock trailer is the only framing information — if the
    // decoder fails to re-derive nBlockAlign from it, the predictor
    // re-seeds are missed and the streams diverge.
    let reg = registry();
    for (codec_id, channels) in [
        (CODEC_ID_MS, 1u16),
        (CODEC_ID_MS, 2),
        (CODEC_ID_IMA_WAV, 1),
        (CODEC_ID_IMA_WAV, 2),
    ] {
        let params = params_for(codec_id, channels);
        let mut enc = reg.first_encoder(&params).unwrap();
        let pcm = synth_pcm(4096, channels as usize);
        let tb = TimeBase::new(1, 22_050);
        enc.send_frame(&Frame::Audio(AudioFrame {
            samples: (pcm.len() / channels as usize) as u32,
            pts: Some(0),
            data: vec![pcm.iter().flat_map(|s| s.to_le_bytes()).collect()],
        }))
        .unwrap();
        enc.flush().unwrap();

        let out_params = enc.output_params().clone();
        let mut stream = Vec::new();
        let mut reference = Vec::new();
        while let Ok(pkt) = enc.receive_packet() {
            // Per-block reference decode (each packet is one block).
            let block_pcm = match codec_id {
                CODEC_ID_MS => ms::decode_block(&pkt.data, channels as usize).unwrap(),
                _ => ima_wav::decode_block(&pkt.data, channels as usize).unwrap(),
            };
            reference.extend(block_pcm);
            stream.extend_from_slice(&pkt.data);
        }
        assert!(!stream.is_empty());

        // Decoder built from the encoder's advertised parameters — the
        // trailer must drive the block splitting.
        let mut dec = reg.first_decoder(&out_params).unwrap();
        dec.send_packet(&Packet::new(0, tb, stream)).unwrap();
        let Frame::Audio(af) = dec.receive_frame().unwrap() else {
            panic!("expected audio frame");
        };
        let got: Vec<i16> = af.data[0]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(
            got, reference,
            "{codec_id} {channels}ch: output_params-driven decode diverged from per-block"
        );
    }
}
