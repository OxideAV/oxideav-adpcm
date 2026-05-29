//! End-to-end PCM → encode → decode → PCM round trip via the
//! [`oxideav_core`] codec registry, for MS-ADPCM and IMA-ADPCM-WAV.
//!
//! This exercises `register_codecs` wiring (encoder factory present,
//! decoder factory present) plus the trait-level `send_frame` /
//! `receive_packet` / `send_packet` / `receive_frame` flow. We don't
//! verify against ffmpeg here — the integration with the ffmpeg oracle
//! is in `wav_decode.rs`. We only verify that our own encoder produces
//! a stream our own decoder reconstructs with bounded RMS error against
//! the source.

use oxideav_adpcm::{
    register_codecs, CODEC_ID_DIALOGIC, CODEC_ID_IMA_QT, CODEC_ID_IMA_WAV, CODEC_ID_MS,
};
use oxideav_core::{AudioFrame, CodecId, CodecParameters, CodecRegistry, Frame, Packet, TimeBase};

fn sine_pcm(n: usize, hz: f64, sample_rate: f64, amp: f64) -> Vec<i16> {
    (0..n)
        .map(|i| {
            let t = i as f64 / sample_rate;
            ((2.0 * std::f64::consts::PI * hz * t).sin() * amp)
                .round()
                .clamp(i16::MIN as f64, i16::MAX as f64) as i16
        })
        .collect()
}

fn rms_error(a: &[i16], b: &[i16]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return f64::INFINITY;
    }
    let mut sse = 0f64;
    for i in 0..n {
        let d = a[i] as f64 - b[i] as f64;
        sse += d * d;
    }
    (sse / n as f64).sqrt()
}

fn round_trip(
    codec_id: &str,
    channels: u16,
    total_samples: usize,
    sample_rate: u32,
) -> (Vec<i16>, Vec<i16>) {
    // Build PCM.
    let pcm: Vec<i16> = if channels == 1 {
        sine_pcm(total_samples, 440.0, sample_rate as f64, 12000.0)
    } else {
        let l = sine_pcm(total_samples, 440.0, sample_rate as f64, 8000.0);
        let r = sine_pcm(total_samples, 660.0, sample_rate as f64, 8000.0);
        let mut v = Vec::with_capacity(total_samples * 2);
        for i in 0..total_samples {
            v.push(l[i]);
            v.push(r[i]);
        }
        v
    };
    let pcm_bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();

    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);

    let mut params = CodecParameters::audio(CodecId::new(codec_id));
    params.sample_rate = Some(sample_rate);
    params.channels = Some(channels);

    let mut enc = reg.first_encoder(&params).expect("encoder factory");
    let af = AudioFrame {
        samples: total_samples as u32,
        pts: Some(0),
        data: vec![pcm_bytes],
    };
    enc.send_frame(&Frame::Audio(af)).unwrap();
    enc.flush().unwrap();

    // Drain.
    let mut packets: Vec<Packet> = Vec::new();
    while let Ok(p) = enc.receive_packet() {
        packets.push(p);
    }

    let mut dec = reg.first_decoder(&params).expect("decoder factory");
    let tb = TimeBase::new(1, sample_rate as i64);
    let mut decoded = Vec::<i16>::new();
    for pkt in packets {
        let mut p = pkt;
        p.time_base = tb;
        dec.send_packet(&p).unwrap();
        let Frame::Audio(af) = dec.receive_frame().unwrap() else {
            panic!("expected audio frame");
        };
        for c in af.data[0].chunks_exact(2) {
            decoded.push(i16::from_le_bytes([c[0], c[1]]));
        }
    }
    (pcm, decoded)
}

#[test]
fn ms_mono_round_trip_via_registry() {
    let (pcm, decoded) = round_trip(CODEC_ID_MS, 1, 2000, 22050);
    // Encoder pads the tail to a full block; allow decoded.len() >= pcm.len().
    assert!(decoded.len() >= pcm.len());
    let rms = rms_error(&decoded, &pcm);
    assert!(rms < 1000.0, "MS mono registry round-trip RMS {rms}");
}

#[test]
fn ms_stereo_round_trip_via_registry() {
    let (pcm, decoded) = round_trip(CODEC_ID_MS, 2, 2000, 22050);
    assert!(decoded.len() >= pcm.len());
    let rms = rms_error(&decoded, &pcm);
    assert!(rms < 1500.0, "MS stereo registry round-trip RMS {rms}");
}

#[test]
fn ima_wav_mono_round_trip_via_registry() {
    let (pcm, decoded) = round_trip(CODEC_ID_IMA_WAV, 1, 2000, 22050);
    assert!(decoded.len() >= pcm.len());
    let rms = rms_error(&decoded, &pcm);
    assert!(rms < 1500.0, "IMA-WAV mono registry round-trip RMS {rms}");
}

#[test]
fn ima_wav_stereo_round_trip_via_registry() {
    let (pcm, decoded) = round_trip(CODEC_ID_IMA_WAV, 2, 2000, 22050);
    assert!(decoded.len() >= pcm.len());
    let rms = rms_error(&decoded, &pcm);
    assert!(rms < 1500.0, "IMA-WAV stereo registry round-trip RMS {rms}");
}

#[test]
fn ima_qt_mono_round_trip_via_registry() {
    // QT uses 64-sample blocks; 2048 samples = 32 blocks.
    let (pcm, decoded) = round_trip(CODEC_ID_IMA_QT, 1, 2048, 22050);
    assert!(decoded.len() >= pcm.len());
    let rms = rms_error(&decoded, &pcm);
    assert!(rms < 1500.0, "IMA-QT mono registry round-trip RMS {rms}");
}

#[test]
fn ima_qt_stereo_round_trip_via_registry() {
    let (pcm, decoded) = round_trip(CODEC_ID_IMA_QT, 2, 2048, 22050);
    assert!(decoded.len() >= pcm.len());
    let rms = rms_error(&decoded, &pcm);
    assert!(rms < 1500.0, "IMA-QT stereo registry round-trip RMS {rms}");
}

#[test]
fn dialogic_mono_round_trip_via_registry() {
    // OKI / Dialogic VOX. 8 kHz mono (the typical Dialogic telephony
    // rate). 12-bit silicon → 16-bit pipeline, so an i16 sine bounded
    // at amplitude 12000 stays clear of the 16-bit ceiling and rounds
    // cleanly through the encoder's `>> 4` narrowing step.
    let (pcm, decoded) = round_trip(CODEC_ID_DIALOGIC, 1, 800, 8000);
    // Stream encoder produces one byte per two samples (rounded up);
    // the decoder reconstructs exactly that many samples back.
    assert_eq!(decoded.len(), pcm.len());
    let rms = rms_error(&decoded, &pcm);
    // The OKI step table caps `ss` at 1552 (12-bit input); shifted to
    // 16-bit that's 1552 << 4 = 24832 LSB. Quantisation can briefly
    // mis-track during step ramp-up, so we allow up to ~6000 LSB RMS
    // here — well above noise floor, well below source amplitude.
    assert!(
        rms < 6000.0,
        "Dialogic VOX mono registry round-trip RMS {rms}"
    );
}
