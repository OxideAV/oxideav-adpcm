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
    CODEC_ID_YAMAHA, CODEC_ID_YAMAHA_A,
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

/// Build interleaved PCM for `channels` channels: channel `c` carries a
/// sine at a distinct frequency so the round trip can verify each lane
/// landed on the right output slot (a channel-interleave regression would
/// scramble the per-lane spectra and blow the per-lane RMS bound).
fn multi_channel_pcm(channels: usize, total_samples: usize, sample_rate: f64) -> Vec<i16> {
    // A spread of audibly-distinct tones, one per channel (mono uses the
    // first). The amplitude is backed off as the channel count grows is
    // unnecessary — each lane is independent — so a fixed amp keeps the
    // per-lane bound comparable across layouts.
    const FREQS: [f64; 8] = [220.0, 330.0, 440.0, 550.0, 660.0, 770.0, 880.0, 990.0];
    let amp = if channels == 1 { 12000.0 } else { 8000.0 };
    let lanes: Vec<Vec<i16>> = (0..channels)
        .map(|c| sine_pcm(total_samples, FREQS[c % FREQS.len()], sample_rate, amp))
        .collect();
    let mut v = Vec::with_capacity(total_samples * channels);
    for i in 0..total_samples {
        for lane in &lanes {
            v.push(lane[i]);
        }
    }
    v
}

fn round_trip(
    codec_id: &str,
    channels: u16,
    total_samples: usize,
    sample_rate: u32,
) -> (Vec<i16>, Vec<i16>) {
    round_trip_opts(codec_id, channels, total_samples, sample_rate, &[])
}

fn round_trip_opts(
    codec_id: &str,
    channels: u16,
    total_samples: usize,
    sample_rate: u32,
    options: &[(&str, &str)],
) -> (Vec<i16>, Vec<i16>) {
    // Build PCM — one distinct tone per channel.
    let pcm: Vec<i16> = multi_channel_pcm(channels as usize, total_samples, sample_rate as f64);
    let pcm_bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();

    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);

    let mut params = CodecParameters::audio(CodecId::new(codec_id));
    params.sample_rate = Some(sample_rate);
    params.channels = Some(channels);
    for (k, v) in options {
        params.options.insert(*k, *v);
    }

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
    // With the |Δ|-based `delta` seeding heuristic, RMS on a 22.05 kHz
    // 440 Hz amp=12000 sine sits around 100. The bound below pins the
    // improvement so a regression in the seed loop trips the test.
    assert!(rms < 250.0, "MS mono registry round-trip RMS {rms}");
}

#[test]
fn ms_stereo_round_trip_via_registry() {
    let (pcm, decoded) = round_trip(CODEC_ID_MS, 2, 2000, 22050);
    assert!(decoded.len() >= pcm.len());
    let rms = rms_error(&decoded, &pcm);
    assert!(rms < 250.0, "MS stereo registry round-trip RMS {rms}");
}

#[test]
fn ima_wav_mono_round_trip_via_registry() {
    let (pcm, decoded) = round_trip(CODEC_ID_IMA_WAV, 1, 2000, 22050);
    assert!(decoded.len() >= pcm.len());
    let rms = rms_error(&decoded, &pcm);
    // With the mean-|Δ|-derived step_index seed (matching the IMA-QT
    // encoder), RMS on a 22.05 kHz 440 Hz amp=12000 sine sits around 90.
    assert!(rms < 250.0, "IMA-WAV mono registry round-trip RMS {rms}");
}

#[test]
fn ima_wav_stereo_round_trip_via_registry() {
    let (pcm, decoded) = round_trip(CODEC_ID_IMA_WAV, 2, 2000, 22050);
    assert!(decoded.len() >= pcm.len());
    let rms = rms_error(&decoded, &pcm);
    assert!(rms < 250.0, "IMA-WAV stereo registry round-trip RMS {rms}");
}

/// Per-lane RMS error: de-interleave `channels` lanes from both buffers
/// and return the worst lane's RMS. A channel-interleave bug scrambles
/// the lanes, so a per-lane bound catches what a global RMS would mask.
fn worst_lane_rms(pcm: &[i16], decoded: &[i16], channels: usize) -> f64 {
    let frames = (pcm.len().min(decoded.len())) / channels;
    let mut worst = 0f64;
    for c in 0..channels {
        let mut sse = 0f64;
        for f in 0..frames {
            let d = pcm[f * channels + c] as f64 - decoded[f * channels + c] as f64;
            sse += d * d;
        }
        let rms = (sse / frames.max(1) as f64).sqrt();
        if rms > worst {
            worst = rms;
        }
    }
    worst
}

#[test]
fn ima_wav_four_channel_round_trip_via_registry() {
    // 4.0 layout: four independent 4-byte-group-per-channel IMA-WAV
    // blocks per packet. Each lane carries a distinct tone, so a
    // per-lane RMS bound pins both the channel-interleave indexing and
    // the per-channel predictor/step isolation. The 4-bit IMA-WAV path
    // supports 1..=8 channels in the decoder, the `ima_encode_block`
    // function and the factory; this is the first end-to-end exercise
    // above stereo.
    let (pcm, decoded) = round_trip(CODEC_ID_IMA_WAV, 4, 2000, 22050);
    assert!(decoded.len() >= pcm.len());
    let rms = worst_lane_rms(&pcm, &decoded, 4);
    assert!(rms < 250.0, "IMA-WAV 4ch worst-lane round-trip RMS {rms}");
}

#[test]
fn ima_wav_six_channel_round_trip_via_registry() {
    // 5.1 layout (six channels). Same per-lane guarantee as the 4.0
    // case, one step further up the channel count.
    let (pcm, decoded) = round_trip(CODEC_ID_IMA_WAV, 6, 2000, 22050);
    assert!(decoded.len() >= pcm.len());
    let rms = worst_lane_rms(&pcm, &decoded, 6);
    assert!(rms < 250.0, "IMA-WAV 6ch worst-lane round-trip RMS {rms}");
}

#[test]
fn ima_wav_block_api_lane_assignment_six_channels() {
    // Direct block-API check, independent of the trait/factory path: feed
    // six lanes whose per-lane DC level is distinct and confirm each lane
    // decodes back to its own level. A bug in the `4*ch` group indexing or
    // the `sample_idx * channels + ch` output placement would cross lanes.
    use oxideav_adpcm::encoder::ima_encode_block;
    use oxideav_adpcm::ima_wav::decode_block;

    let channels = 6usize;
    // One 4-byte group per channel → 1 + 8 = 9 samples per channel.
    let block_size = 4 * channels + 4 * channels; // header + one group/ch
    let samples_per_channel = 9usize;
    // Lane c is a flat level c*2000 (well inside i16, distinct per lane).
    let mut pcm = vec![0i16; samples_per_channel * channels];
    for f in 0..samples_per_channel {
        for c in 0..channels {
            pcm[f * channels + c] = (c as i16) * 2000;
        }
    }
    let block = ima_encode_block(&pcm, channels, block_size).unwrap();
    let decoded = decode_block(&block, channels).unwrap();
    assert_eq!(decoded.len(), samples_per_channel * channels);
    // Sample 0 of each lane is the header predictor seed = that lane's
    // level exactly; later samples track it within a small ADPCM wobble.
    for c in 0..channels {
        let want = (c as i32) * 2000;
        assert_eq!(decoded[c] as i32, want, "lane {c} seed sample");
        for f in 0..samples_per_channel {
            let got = decoded[f * channels + c] as i32;
            assert!(
                (got - want).abs() < 512,
                "lane {c} sample {f}: got {got}, want near {want}"
            );
        }
    }
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
fn yamaha_mono_round_trip_via_registry() {
    // Yamaha is stream-oriented (no per-block header). 8 kHz mono with
    // a low-frequency sine — the Y8950's 127..24576 step range tracks
    // a low-freq sine cleanly once the step settles.
    let (pcm, decoded) = round_trip(CODEC_ID_YAMAHA, 1, 800, 8000);
    assert_eq!(decoded.len(), pcm.len());
    let rms = rms_error(&decoded, &pcm);
    assert!(rms < 3000.0, "Yamaha mono registry round-trip RMS {rms}");
}

#[test]
fn yamaha_stereo_round_trip_via_registry() {
    let (pcm, decoded) = round_trip(CODEC_ID_YAMAHA, 2, 800, 8000);
    assert_eq!(decoded.len(), pcm.len());
    let rms = rms_error(&decoded, &pcm);
    assert!(rms < 3000.0, "Yamaha stereo registry round-trip RMS {rms}");
}

#[test]
fn yamaha_a_mono_round_trip_via_registry() {
    // Yamaha ADPCM-A is single-channel by construction (one YM2610
    // rhythm channel). 12-bit silicon → wide-16 pipeline; the
    // registry-resolved decoder shifts 12-bit acc left by 4 to fill the
    // i16 output, so a moderate-amplitude sine fits cleanly. The
    // step pointer takes ~12 samples to ramp from 0, so we choose a
    // long enough run that the leading-edge transient is amortised.
    let (pcm, decoded) = round_trip(CODEC_ID_YAMAHA_A, 1, 800, 8000);
    assert_eq!(decoded.len(), pcm.len());
    let rms = rms_error(&decoded, &pcm);
    // 12-bit codec on a wide-16 pipeline: per-sample LSB is 16 (= 2^4);
    // a 12-bit codec on an 8 kHz 440 Hz sine of amplitude 12000 expects
    // RMS error in the 4500-6500 LSB range. We bound at 7000.
    assert!(
        rms < 7000.0,
        "Yamaha ADPCM-A mono registry round-trip RMS {rms} > 7000"
    );
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

#[test]
fn yamaha_opna_chip_round_trip_via_registry() {
    // `chip=opna` selects the YM2608 (OPNA) Application Manual Table 5-1
    // step constants (×64 numerators, `>> 6`) rather than the AICA default
    // (×256, `>> 8`). The encoder seeds its analysis state with the same
    // chip, so its bytes decode bit-exactly under the same option. The
    // round-trip must reconstruct the source within the same bound as the
    // default AICA path — the two chips track the same `~1.1^M` curve.
    let (pcm, decoded) = round_trip_opts(CODEC_ID_YAMAHA, 1, 800, 8000, &[("chip", "opna")]);
    assert_eq!(decoded.len(), pcm.len());
    let rms = rms_error(&decoded, &pcm);
    assert!(rms < 3000.0, "Yamaha OPNA chip round-trip RMS {rms}");
}

#[test]
fn yamaha_opna_and_aica_diverge_on_a_long_stream() {
    // The two chips round the step-adaptation curve differently, so a
    // stream encoded under one chip must NOT decode identically under the
    // other once the step has accumulated enough updates. This pins that
    // the `chip` option actually reaches the decode recurrence (a no-op
    // wiring bug would make the two outputs match).
    let (_pcm, aica) = round_trip_opts(CODEC_ID_YAMAHA, 1, 800, 8000, &[("chip", "aica")]);
    // Re-encode the SAME PCM under OPNA and decode under OPNA.
    let (_pcm2, opna) = round_trip_opts(CODEC_ID_YAMAHA, 1, 800, 8000, &[("chip", "opna")]);
    assert_eq!(aica.len(), opna.len());
    let differing = aica.iter().zip(&opna).filter(|(a, b)| a != b).count();
    assert!(
        differing > 0,
        "AICA and OPNA decode paths produced identical output — chip option not wired"
    );
}

#[test]
fn dialogic_lofirst_round_trip_via_registry() {
    // `nibble_order=lo` selects the MSM6258 low-nibble-first unpack. The
    // encoder packs in that order and the decoder reads it back, so the
    // round-trip reconstructs the source within the same bound as the
    // default HiFirst (Dialogic VOX / MSM6295) path.
    let (pcm, decoded) =
        round_trip_opts(CODEC_ID_DIALOGIC, 1, 800, 8000, &[("nibble_order", "lo")]);
    assert_eq!(decoded.len(), pcm.len());
    let rms = rms_error(&decoded, &pcm);
    assert!(rms < 6000.0, "Dialogic LoFirst round-trip RMS {rms}");
}

#[test]
fn unknown_chip_and_order_options_are_rejected() {
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);

    // Bad chip value on the right codec.
    let mut p = CodecParameters::audio(CodecId::new(CODEC_ID_YAMAHA));
    p.channels = Some(1);
    p.options.insert("chip", "ym9999");
    assert!(reg.first_decoder(&p).is_err(), "bad chip value accepted");
    assert!(
        reg.first_encoder(&p).is_err(),
        "bad chip value accepted (enc)"
    );

    // `chip` on a variant that has no chip selection.
    let mut p2 = CodecParameters::audio(CodecId::new(CODEC_ID_MS));
    p2.channels = Some(1);
    p2.options.insert("chip", "opna");
    assert!(reg.first_decoder(&p2).is_err(), "chip on MS accepted");

    // Bad nibble_order value, and nibble_order on a non-Dialogic variant.
    let mut p3 = CodecParameters::audio(CodecId::new(CODEC_ID_DIALOGIC));
    p3.channels = Some(1);
    p3.options.insert("nibble_order", "middle");
    assert!(reg.first_decoder(&p3).is_err(), "bad nibble_order accepted");

    let mut p4 = CodecParameters::audio(CodecId::new(CODEC_ID_YAMAHA));
    p4.channels = Some(1);
    p4.options.insert("nibble_order", "lo");
    assert!(
        reg.first_decoder(&p4).is_err(),
        "nibble_order on Yamaha accepted"
    );
}
