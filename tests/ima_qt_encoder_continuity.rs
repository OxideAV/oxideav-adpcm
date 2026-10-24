//! adpcm_ima_qt encoder ↔ stream decoder agreement: when a block's
//! preamble agrees with the channel's running state, the stream decoder
//! keeps its full-precision predictor, so the encoder has to quantize
//! from that predictor too or every such block decodes offset by the
//! dropped low 7 bits.

use oxideav_adpcm::{CODEC_ID_IMA_QT, encoder, ima_qt, register_codecs};
use oxideav_core::{AudioFrame, CodecId, CodecParameters, CodecRegistry, Frame, Packet, TimeBase};

const BLOCKS: usize = 8;

/// A DC level whose top 9 bits are 0: every block's preamble seed is 0
/// with step index 0, which agrees with the running state once the
/// predictor has settled near the level.
fn dc_pcm() -> Vec<i16> {
    vec![100i16; ima_qt::QT_SAMPLES_PER_BLOCK * BLOCKS]
}

fn registry_round_trip(pcm: &[i16], quantizer: Option<&str>) -> Vec<i16> {
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);
    let mut p = CodecParameters::audio(CodecId::new(CODEC_ID_IMA_QT));
    p.sample_rate = Some(22_050);
    p.channels = Some(1);
    if let Some(q) = quantizer {
        p.options.insert("quantizer", q);
    }
    let mut enc = reg.first_encoder(&p).unwrap();
    enc.send_frame(&Frame::Audio(AudioFrame {
        samples: pcm.len() as u32,
        pts: Some(0),
        data: vec![pcm.iter().flat_map(|s| s.to_le_bytes()).collect()],
    }))
    .unwrap();
    enc.flush().unwrap();
    let mut packets = Vec::new();
    while let Ok(pkt) = enc.receive_packet() {
        packets.push(pkt.data);
    }

    let mut dec = reg.first_decoder(&p).unwrap();
    let mut out = Vec::new();
    for data in packets {
        dec.send_packet(&Packet::new(0, TimeBase::new(1, 22_050), data))
            .unwrap();
        let Frame::Audio(af) = dec.receive_frame().unwrap() else {
            panic!("audio")
        };
        out.extend(
            af.data[0]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&c| i16::from_le_bytes(c)),
        );
    }
    out
}

fn max_err_after_first_block(decoded: &[i16], pcm: &[i16]) -> i32 {
    decoded[ima_qt::QT_SAMPLES_PER_BLOCK..]
        .iter()
        .zip(&pcm[ima_qt::QT_SAMPLES_PER_BLOCK..])
        .map(|(&d, &s)| (d as i32 - s as i32).abs())
        .max()
        .unwrap()
}

#[test]
fn registry_encoder_tracks_the_stream_decoder() {
    let pcm = dc_pcm();
    for quantizer in [None, Some("reference")] {
        let decoded = registry_round_trip(&pcm, quantizer);
        assert_eq!(decoded.len(), pcm.len());
        let err = max_err_after_first_block(&decoded, &pcm);
        assert!(err <= 16, "{quantizer:?}: max error {err} after block 0");
    }
}

#[test]
fn stateless_blocks_drift_under_the_stream_decoder() {
    // Contrast: blocks encoded as if every preamble reseeds, decoded by
    // a stream decoder, land offset from the input. The continuous
    // encoder keeps the same stream within the quantizer's error.
    let pcm = dc_pcm();
    let mut dec_state = [ima_qt::QtChannel::default()];
    let mut stateless = Vec::new();
    for chunk in pcm.chunks(ima_qt::QT_SAMPLES_PER_BLOCK) {
        let blk = encoder::ima_qt_encode_block(chunk, 1).unwrap();
        stateless.extend(ima_qt::decode_block_continuous(&blk, 1, &mut dec_state).unwrap());
    }
    let drift = max_err_after_first_block(&stateless, &pcm);

    let mut enc_state = [oxideav_adpcm::ima_wav::ImaCodecState::default()];
    let mut dec_state = [ima_qt::QtChannel::default()];
    let mut continuous = Vec::new();
    for chunk in pcm.chunks(ima_qt::QT_SAMPLES_PER_BLOCK) {
        let blk = encoder::ima_qt_encode_block_continuous(chunk, 1, &mut enc_state).unwrap();
        continuous.extend(ima_qt::decode_block_continuous(&blk, 1, &mut dec_state).unwrap());
    }
    let err = max_err_after_first_block(&continuous, &pcm);
    assert!(drift > 32, "stateless drift {drift}");
    assert!(err <= 16, "continuous error {err}");
}

#[test]
fn continuous_encoder_rejects_a_state_count_mismatch() {
    let pcm = vec![0i16; ima_qt::QT_SAMPLES_PER_BLOCK * 2];
    let mut states = [oxideav_adpcm::ima_wav::ImaCodecState::default()];
    assert!(encoder::ima_qt_encode_block_continuous(&pcm, 2, &mut states).is_err());
}
