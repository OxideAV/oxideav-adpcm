//! 3-bit IMA/DVI ADPCM (WAV tag `0x0011`, `wBitsPerSample = 3`) —
//! round-trip, framing, registry-option, and robustness coverage.

use oxideav_adpcm::encoder::ima_encode_block_3bit;
use oxideav_adpcm::ima_wav::{decode_block_3bit, GROUP_BYTES_3BIT, GROUP_SAMPLES_3BIT};
use oxideav_adpcm::{register_codecs, CODEC_ID_IMA_WAV};
use oxideav_core::{AudioFrame, CodecId, CodecParameters, CodecRegistry, Frame, Packet, TimeBase};

fn sine_pcm(n: usize, hz: f64, sample_rate: f64, amp: f64) -> Vec<i16> {
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64 / sample_rate;
        let s = (2.0 * std::f64::consts::PI * hz * t).sin() * amp;
        v.push(s.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16);
    }
    v
}

fn rms_error(a: &[i16], b: &[i16]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let mut sse = 0f64;
    for i in 0..n {
        let d = a[i] as f64 - b[i] as f64;
        sse += d * d;
    }
    (sse / n as f64).sqrt()
}

/// Deterministic in-test LCG (same parameters as the other fuzz suites).
struct Lcg(u64);
impl Lcg {
    fn next_u8(&mut self) -> u8 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
        (self.0 >> 33) as u8
    }
}

#[test]
fn mono_3bit_round_trip_sine_low_error() {
    // Mono, 256-byte block: header 4, body 252 = 21 × 12B groups →
    // 1 + 21*32 = 673 samples per block.
    let samples_per_block = 1 + 21 * GROUP_SAMPLES_3BIT;
    let n_blocks = 4;
    let pcm = sine_pcm(samples_per_block * n_blocks, 440.0, 22050.0, 16000.0);
    let mut decoded = Vec::new();
    for chunk in pcm.chunks(samples_per_block) {
        let blk = ima_encode_block_3bit(chunk, 1, 256).unwrap();
        assert_eq!(blk.len(), 256);
        let d = decode_block_3bit(&blk, 1).unwrap();
        assert_eq!(d.len(), samples_per_block);
        decoded.extend_from_slice(&d);
    }
    let rms = rms_error(&decoded, &pcm);
    // 3-bit coding has only 4 magnitude levels (vs 8 in 4-bit mode) so
    // the bound is looser than the 4-bit suite's 1500 — but the search
    // encoder should still stay well under 10% of full scale.
    assert!(rms < 3000.0, "3-bit IMA-WAV mono round-trip RMS {rms}");
}

#[test]
fn stereo_3bit_round_trip_low_error() {
    // Stereo, 248-byte block: header 8, body 240 = 10 × 24B groups →
    // 1 + 10*32 = 321 samples per channel.
    let samples_per_block = 1 + 10 * GROUP_SAMPLES_3BIT;
    let n = samples_per_block * 3;
    let l = sine_pcm(n, 440.0, 22050.0, 8000.0);
    let r = sine_pcm(n, 660.0, 22050.0, 8000.0);
    let mut pcm = Vec::with_capacity(n * 2);
    for i in 0..n {
        pcm.push(l[i]);
        pcm.push(r[i]);
    }
    let mut decoded_l = Vec::new();
    let mut decoded_r = Vec::new();
    for chunk in pcm.chunks(samples_per_block * 2) {
        let blk = ima_encode_block_3bit(chunk, 2, 248).unwrap();
        assert_eq!(blk.len(), 248);
        let d = decode_block_3bit(&blk, 2).unwrap();
        assert_eq!(d.len(), samples_per_block * 2);
        for i in 0..samples_per_block {
            decoded_l.push(d[i * 2]);
            decoded_r.push(d[i * 2 + 1]);
        }
    }
    let rms_l = rms_error(&decoded_l, &l);
    let rms_r = rms_error(&decoded_r, &r);
    assert!(rms_l < 3000.0, "3-bit IMA-WAV stereo L RMS {rms_l}");
    assert!(rms_r < 3000.0, "3-bit IMA-WAV stereo R RMS {rms_r}");
}

#[test]
fn emitted_sample_count_formula_holds_across_group_counts() {
    // 1 + groups*32 samples per channel for every (channels, groups).
    for channels in 1..=8usize {
        for groups in 0..=3usize {
            let block_len = 4 * channels + groups * GROUP_BYTES_3BIT * channels;
            let block = vec![0u8; block_len];
            let pcm = decode_block_3bit(&block, channels).unwrap();
            assert_eq!(
                pcm.len(),
                (1 + groups * GROUP_SAMPLES_3BIT) * channels,
                "channels={channels} groups={groups}"
            );
        }
    }
}

#[test]
fn registry_decoder_honours_bits_per_sample_3_option() {
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);

    // Encode one 3-bit mono block directly.
    let samples_per_block = 1 + 21 * GROUP_SAMPLES_3BIT;
    let pcm = sine_pcm(samples_per_block, 440.0, 22050.0, 12000.0);
    let blk = ima_encode_block_3bit(&pcm, 1, 256).unwrap();

    // Decode through the registry with the option set.
    let mut p = CodecParameters::audio(CodecId::new(CODEC_ID_IMA_WAV));
    p.sample_rate = Some(22_050);
    p.channels = Some(1);
    p.options.insert("bits_per_sample", "3");
    let mut dec = reg.first_decoder(&p).unwrap();
    let tb = TimeBase::new(1, 22_050);
    dec.send_packet(&Packet::new(0, tb, blk).with_pts(0))
        .unwrap();
    let frame = dec.receive_frame().unwrap();
    let Frame::Audio(af) = frame else {
        panic!("expected audio frame")
    };
    assert_eq!(af.samples as usize, samples_per_block);
    // Reconstruct i16 and sanity-check the error against the input.
    let decoded: Vec<i16> = af.data[0]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();
    let rms = rms_error(&decoded, &pcm);
    assert!(rms < 3000.0, "registry 3-bit decode RMS {rms}");
}

#[test]
fn registry_encoder_honours_bits_per_sample_3_option() {
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);
    let mut p = CodecParameters::audio(CodecId::new(CODEC_ID_IMA_WAV));
    p.sample_rate = Some(22_050);
    p.channels = Some(1);
    p.options.insert("bits_per_sample", "3");
    let mut enc = reg.first_encoder(&p).unwrap();

    // Default 3-bit mono block size is 256 (4 + 21*12) → 673 samples.
    let samples_per_block = 1 + 21 * GROUP_SAMPLES_3BIT;
    let pcm = sine_pcm(samples_per_block, 440.0, 22050.0, 12000.0);
    let pcm_bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
    let af = AudioFrame {
        samples: samples_per_block as u32,
        pts: Some(0),
        data: vec![pcm_bytes],
    };
    enc.send_frame(&Frame::Audio(af)).unwrap();
    let pkt = enc.receive_packet().unwrap();
    assert_eq!(pkt.data.len(), 256);
    // The emitted block decodes as a 3-bit block to the right length.
    let d = decode_block_3bit(&pkt.data, 1).unwrap();
    assert_eq!(d.len(), samples_per_block);
    let rms = rms_error(&d, &pcm);
    assert!(rms < 3000.0, "registry 3-bit encode RMS {rms}");
}

#[test]
fn registry_rejects_unsupported_bits_per_sample_values() {
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);
    for bits in ["2", "5", "8", "0", "banana"] {
        let mut p = CodecParameters::audio(CodecId::new(CODEC_ID_IMA_WAV));
        p.sample_rate = Some(22_050);
        p.channels = Some(1);
        p.options.insert("bits_per_sample", bits);
        assert!(
            reg.first_decoder(&p).is_err(),
            "decoder accepted bits_per_sample={bits}"
        );
        assert!(
            reg.first_encoder(&p).is_err(),
            "encoder accepted bits_per_sample={bits}"
        );
    }
    // bits_per_sample on a fixed-width variant: 4 passes through, 3 is
    // rejected (only IMA-WAV defines a 3-bit mode).
    let mut p = CodecParameters::audio(CodecId::new(oxideav_adpcm::CODEC_ID_MS));
    p.sample_rate = Some(22_050);
    p.channels = Some(1);
    p.options.insert("bits_per_sample", "4");
    assert!(reg.first_decoder(&p).is_ok());
    let mut p3 = CodecParameters::audio(CodecId::new(oxideav_adpcm::CODEC_ID_MS));
    p3.sample_rate = Some(22_050);
    p3.channels = Some(1);
    p3.options.insert("bits_per_sample", "3");
    assert!(reg.first_decoder(&p3).is_err());
}

#[test]
fn decode_3bit_never_panics_on_random_bytes() {
    let mut lcg = Lcg(0x3b17_ad9c_2026_0611);
    for round in 0..2000 {
        let len = (lcg.next_u8() as usize) % 80;
        let buf: Vec<u8> = (0..len).map(|_| lcg.next_u8()).collect();
        for channels in [1usize, 2, 8] {
            // Ok or Err both fine — never panic.
            let _ = decode_block_3bit(&buf, channels);
        }
        let _ = round;
    }
}

#[test]
fn decode_3bit_rejects_every_truncation_of_a_valid_block() {
    // Build a well-formed mono 2-group block and feed every proper
    // prefix: all must be clean Errs (header too short or body not a
    // whole number of 12-byte groups).
    let samples = 1 + 2 * GROUP_SAMPLES_3BIT;
    let pcm = sine_pcm(samples, 300.0, 8000.0, 9000.0);
    let blk = ima_encode_block_3bit(&pcm, 1, 4 + 2 * GROUP_BYTES_3BIT).unwrap();
    for cut in 0..blk.len() {
        let r = decode_block_3bit(&blk[..cut], 1);
        if cut >= 4 && (cut - 4) % GROUP_BYTES_3BIT == 0 {
            // Header + whole groups: a shorter-but-valid block.
            assert!(r.is_ok(), "cut={cut} should still parse");
        } else {
            assert!(r.is_err(), "cut={cut} should be rejected");
        }
    }
}

#[test]
fn encode_3bit_rejects_bad_framing() {
    // Sample-count mismatch.
    assert!(ima_encode_block_3bit(&[0i16; 10], 1, 256).is_err());
    // Block too small for the header.
    assert!(ima_encode_block_3bit(&[0i16; 1], 1, 2).is_err());
    // Body not a multiple of 12 bytes per channel.
    assert!(ima_encode_block_3bit(&[0i16; 33], 1, 4 + 8).is_err());
    // Channel bounds.
    assert!(ima_encode_block_3bit(&[], 0, 256).is_err());
    assert!(ima_encode_block_3bit(&[0i16; 9 * 33], 9, 9 * 16).is_err());
}

#[test]
fn encode_3bit_never_panics_on_adversarial_pcm() {
    let mut lcg = Lcg(0xdead_beef_0611_2026);
    let samples = 1 + GROUP_SAMPLES_3BIT;
    for _ in 0..500 {
        let pcm: Vec<i16> = (0..samples)
            .map(|_| i16::from_le_bytes([lcg.next_u8(), lcg.next_u8()]))
            .collect();
        let blk = ima_encode_block_3bit(&pcm, 1, 4 + GROUP_BYTES_3BIT).unwrap();
        // Whatever was emitted must round-trip through the decoder.
        let d = decode_block_3bit(&blk, 1).unwrap();
        assert_eq!(d.len(), samples);
    }
}

#[test]
fn registry_4bit_default_is_unchanged() {
    // Without the option, the registry path still decodes 4-bit blocks —
    // pins that the new option is strictly opt-in.
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);
    let mut p = CodecParameters::audio(CodecId::new(CODEC_ID_IMA_WAV));
    p.sample_rate = Some(22_050);
    p.channels = Some(1);
    let mut dec = reg.first_decoder(&p).unwrap();
    // A 4-bit mono block: 4B header + one 4B group → 9 samples.
    let mut blk = Vec::new();
    blk.extend_from_slice(&0i16.to_le_bytes());
    blk.push(0);
    blk.push(0);
    blk.extend_from_slice(&[0u8; 4]);
    let tb = TimeBase::new(1, 22_050);
    dec.send_packet(&Packet::new(0, tb, blk).with_pts(0))
        .unwrap();
    let Frame::Audio(af) = dec.receive_frame().unwrap() else {
        panic!("expected audio frame")
    };
    assert_eq!(af.samples, 9);
}

#[test]
fn trait_level_3bit_decoder_handles_errors_without_panicking() {
    // Random packets through the full Decoder trait with the 3-bit
    // option set: Ok or Err, never panic; receive_frame only succeeds
    // after a valid packet.
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);
    let mut p = CodecParameters::audio(CodecId::new(CODEC_ID_IMA_WAV));
    p.sample_rate = Some(22_050);
    p.channels = Some(1);
    p.options.insert("bits_per_sample", "3");
    let mut dec = reg.first_decoder(&p).unwrap();
    let tb = TimeBase::new(1, 22_050);
    let mut lcg = Lcg(0x0badc0de);
    for _ in 0..200 {
        let len = (lcg.next_u8() as usize) % 64;
        let buf: Vec<u8> = (0..len).map(|_| lcg.next_u8()).collect();
        // Err (any shape) is fine; Ok must yield a frame. Never panic.
        if dec
            .send_packet(&Packet::new(0, tb, buf).with_pts(0))
            .is_ok()
        {
            let f = dec.receive_frame();
            assert!(f.is_ok(), "send_packet Ok but receive_frame failed");
        }
    }
}
