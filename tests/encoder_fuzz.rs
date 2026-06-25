//! Structured-malformation / never-panic robustness tests for every
//! ADPCM **encoder** variant exposed by this crate. Symmetric
//! counterpart to [`decoder_fuzz`]: where `decoder_fuzz` enumerates
//! adversarial **packets**, this file enumerates adversarial **PCM
//! inputs**, block-size choices, and seeded encoder state.
//!
//! For each variant the encoder must:
//!
//! - Accept any well-shaped PCM buffer and produce either `Ok(Vec<u8>)`
//!   or `Err(Error::Invalid | Error::Unsupported)`; never panic, never
//!   debug-overflow, never index out of bounds.
//! - For block-oriented variants: report `Err` cleanly when given a
//!   sample count that doesn't match the declared block size, and
//!   succeed when the count matches.
//! - For stream-oriented variants: advance per-channel state across
//!   `encode_packet` invocations and never panic on out-of-spec seeds.
//!
//! Every input is synthesised in-test from a tiny LCG (no external
//! crates). No `docs/` fixtures or external files are read.

use oxideav_adpcm::{
    dialogic,
    encoder::{self, QT_BLOCK_BYTES_PER_CHANNEL, QT_SAMPLES_PER_BLOCK},
    yamaha, yamaha_a,
};

// ----- deterministic generator ---------------------------------------

struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_add(0x9E3779B97F4A7C15))
    }
    fn next_u64(&mut self) -> u64 {
        // Same LCG constants as `decoder_fuzz.rs` for reproducibility.
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
        self.0
    }
    fn next_i16(&mut self) -> i16 {
        self.next_u64() as i16
    }
    fn pcm(&mut self, n: usize) -> Vec<i16> {
        (0..n).map(|_| self.next_i16()).collect()
    }
}

// ---------- MS-ADPCM encoder ----------

#[test]
fn ms_encoder_rejects_short_pcm_cleanly() {
    // The MS encoder needs at least 2 prelude samples per channel + enough
    // body samples to fill the block. A buffer that's too short should
    // surface as `Err` (Invalid) rather than panic.
    let mut rng = Lcg::new(0x00AD_0001u64);
    for _ in 0..256 {
        let chs: usize = if (rng.next_u64() & 1) != 0 { 2 } else { 1 };
        // Pick a block size in [7*chs - 4, 1024].
        let lo = (7usize * chs).saturating_sub(4);
        let block_size = lo + (rng.next_u64() as usize % 1024);
        // Pick a per-channel sample count in [0, 600] — sometimes too few,
        // sometimes enough, sometimes too many.
        let s_per_ch = (rng.next_u64() as usize) % 600;
        let total = s_per_ch * chs;
        let pcm = rng.pcm(total);
        // Must NOT panic, regardless of whether result is Ok or Err.
        let _ = encoder::encode_block(&pcm, chs, block_size);
    }
}

#[test]
fn ms_encoder_extreme_pcm_never_panics() {
    // Pure-edge PCM: i16::MIN, i16::MAX, alternating ±, DC. The decoder
    // already handled adversarial deltas in the prior round; the encoder
    // should also handle adversarial sample values without overflowing
    // the simulate-then-advance loop's intermediate i32 arithmetic.
    let cases: &[fn(usize) -> Vec<i16>] = &[
        |n| vec![i16::MAX; n],
        |n| vec![i16::MIN; n],
        |n| {
            (0..n)
                .map(|i| if i & 1 == 0 { i16::MAX } else { i16::MIN })
                .collect()
        },
        |n| vec![0; n],
    ];
    for chs in [1usize, 2] {
        for &mk in cases {
            // 256-byte mono block needs 500 samples; stereo needs 244.
            let total = if chs == 1 { 500 } else { 244 * 2 };
            let pcm = mk(total);
            let r = encoder::encode_block(&pcm, chs, 256);
            // The encoder accepts exact-count PCM, so this must return Ok.
            assert!(r.is_ok(), "MS encode extreme PCM chs={chs}: {:?}", r.err());
            assert_eq!(r.unwrap().len(), 256);
        }
    }
}

// ---------- IMA-ADPCM-WAV encoder ----------

#[test]
fn ima_wav_encoder_rejects_size_mismatch_cleanly() {
    let mut rng = Lcg::new(0x001A_0002u64);
    for _ in 0..256 {
        let chs = ((rng.next_u64() as usize) & 0x07) + 1;
        let block_size = 4 * chs + (rng.next_u64() as usize % 8192);
        let total = (rng.next_u64() as usize) % 4000;
        let pcm = rng.pcm(total);
        let _ = encoder::ima_encode_block(&pcm, chs, block_size);
    }
}

#[test]
fn ima_wav_encoder_extreme_pcm_never_panics() {
    // 256-byte mono block: samples_per_block = 1 + 63*8 = 505.
    let pcm = vec![i16::MAX; 505];
    let r = encoder::ima_encode_block(&pcm, 1, 256).expect("IMA-WAV i16::MAX mono");
    assert_eq!(r.len(), 256);
    let pcm = vec![i16::MIN; 505];
    let r = encoder::ima_encode_block(&pcm, 1, 256).expect("IMA-WAV i16::MIN mono");
    assert_eq!(r.len(), 256);
}

#[test]
fn ima_wav_encoder_high_channel_count_succeeds() {
    // 8 channels exercises the upper bound the factory accepts.
    // body = 256 - 32 = 224; group_bytes = 32; groups = 7;
    // samples_per_channel = 1 + 7*8 = 57; total = 57*8 = 456.
    let pcm = vec![0i16; 57 * 8];
    let r = encoder::ima_encode_block(&pcm, 8, 256).expect("IMA-WAV 8ch");
    assert_eq!(r.len(), 256);
}

#[test]
fn ima_wav_encoder_rejects_zero_channels() {
    let r = encoder::ima_encode_block(&[], 0, 256);
    assert!(r.is_err());
    let r = encoder::ima_encode_block(&[], 9, 256);
    assert!(r.is_err());
}

// ---------- IMA-ADPCM-QT encoder ----------

#[test]
fn ima_qt_encoder_rejects_off_size_inputs() {
    // Sweep adversarial sample counts against the fixed 64-per-channel
    // requirement.
    let mut rng = Lcg::new(0x0007_0003u64);
    for _ in 0..128 {
        let chs = if (rng.next_u64() & 1) != 0 { 2 } else { 1 };
        let total = (rng.next_u64() as usize) % 200;
        let pcm = rng.pcm(total);
        let r = encoder::ima_qt_encode_block(&pcm, chs);
        // Only the exact count succeeds.
        if total == QT_SAMPLES_PER_BLOCK * chs {
            assert!(r.is_ok());
        } else {
            assert!(r.is_err());
        }
    }
}

#[test]
fn ima_qt_encoder_extreme_pcm_never_panics() {
    let pcm = vec![i16::MAX; QT_SAMPLES_PER_BLOCK];
    let r = encoder::ima_qt_encode_block(&pcm, 1).expect("QT max mono");
    assert_eq!(r.len(), QT_BLOCK_BYTES_PER_CHANNEL);
    let pcm = vec![i16::MIN; QT_SAMPLES_PER_BLOCK * 2];
    let r = encoder::ima_qt_encode_block(&pcm, 2).expect("QT min stereo");
    assert_eq!(r.len(), QT_BLOCK_BYTES_PER_CHANNEL * 2);
}

// ---------- Yamaha ADPCM-B encoder ----------

#[test]
fn yamaha_b_encoder_random_pcm_never_panics() {
    let mut rng = Lcg::new(0x00BB_0004u64);
    for chs in [1usize, 2] {
        let mut state = vec![yamaha::Channel::default(); chs];
        for _ in 0..64 {
            let n_per_ch = (rng.next_u64() as usize) % 256;
            let pcm = rng.pcm(n_per_ch * chs);
            let out = yamaha::encode_packet(&pcm, &mut state);
            // Output byte count = ceil(samples / 2) per the encoder
            // contract. Just assert it's bounded (never larger than the
            // input sample count).
            assert!(out.len() <= pcm.len() + 1);
        }
    }
}

#[test]
fn yamaha_b_encoder_with_seeded_state_never_panics() {
    // Pre-seed the channel state with adversarial values (out-of-spec
    // step / predictor). The encoder shares the decoder's state update
    // path; both must clamp rather than overflow.
    let mut rng = Lcg::new(0x00BB_0005u64);
    for _ in 0..32 {
        let mut state = [yamaha::Channel {
            predictor: rng.next_i16() as i32 * 200, // possibly out of i16
            step: (rng.next_u64() as i32) % 10_000,
            ..yamaha::Channel::default()
        }];
        let n = (rng.next_u64() as usize) % 128;
        let pcm = rng.pcm(n);
        let _ = yamaha::encode_packet(&pcm, &mut state);
    }
}

// ---------- Yamaha ADPCM-A encoder ----------

#[test]
fn yamaha_a_encoder_random_pcm_never_panics() {
    let mut rng = Lcg::new(0x00AA_0006u64);
    let mut state = [yamaha_a::Channel::default()];
    for _ in 0..64 {
        let n = (rng.next_u64() as usize) % 256;
        let pcm = rng.pcm(n);
        let out_w16 = yamaha_a::encode_packet(&pcm, &mut state, yamaha_a::Output::Wide16);
        assert!(out_w16.len() <= pcm.len() + 1);
        // Reset between Output modes so the state seed is bounded the
        // same way as in the W16 path.
        state[0] = yamaha_a::Channel::default();
        let out_n12 = yamaha_a::encode_packet(&pcm, &mut state, yamaha_a::Output::Native12);
        assert!(out_n12.len() <= pcm.len() + 1);
    }
}

#[test]
fn yamaha_a_encoder_extreme_state_seed_never_panics() {
    let mut state = [yamaha_a::Channel {
        acc: 1_000_000, // way outside 12-bit range
        step_index: -100,
    }];
    let _ = yamaha_a::encode_packet(&[0; 64], &mut state, yamaha_a::Output::Wide16);
    state[0] = yamaha_a::Channel {
        acc: -1_000_000,
        step_index: 100, // outside the 49-entry table range
    };
    let _ = yamaha_a::encode_packet(&[0; 64], &mut state, yamaha_a::Output::Native12);
}

// ---------- Dialogic / OKI VOX encoder ----------

#[test]
fn dialogic_encoder_random_pcm_never_panics() {
    let mut rng = Lcg::new(0x00D1_0007u64);
    for order in [
        dialogic::NibbleOrder::HiFirst,
        dialogic::NibbleOrder::LoFirst,
    ] {
        let mut state = dialogic::Channel::default();
        for _ in 0..64 {
            let n = (rng.next_u64() as usize) % 256;
            let pcm = rng.pcm(n);
            let out = dialogic::encode_packet(&pcm, &mut state, order);
            assert!(out.len() <= pcm.len().div_ceil(2) + 1);
        }
    }
    let _ = rng.next_u64();
}

#[test]
fn dialogic_encoder_wide16_random_pcm_never_panics() {
    let mut rng = Lcg::new(0x00D1_0008u64);
    let mut state = dialogic::Channel::default();
    for _ in 0..64 {
        let n = (rng.next_u64() as usize) % 256;
        let pcm = rng.pcm(n);
        let out = dialogic::encode_packet_wide16(&pcm, &mut state, dialogic::NibbleOrder::HiFirst);
        assert!(out.len() <= pcm.len().div_ceil(2));
    }
}

#[test]
fn dialogic_encoder_extreme_state_seed_never_panics() {
    let mut state = dialogic::Channel {
        predictor: 500_000, // way outside 12-bit range
        step_index: -10,
    };
    let _ = dialogic::encode_packet(&[0; 64], &mut state, dialogic::NibbleOrder::HiFirst);
    state = dialogic::Channel {
        predictor: -500_000,
        step_index: 500, // outside the 49-entry table range
    };
    let _ = dialogic::encode_packet(&[0; 64], &mut state, dialogic::NibbleOrder::LoFirst);
}

#[test]
fn dialogic_encode_packet_multi_random_pcm_never_panics() {
    // The stereo (multi-channel) VOX encode path must tolerate any
    // interleaved PCM length, odd or even, under both nibble orders and
    // for 1..=2 channels, without panicking or mis-sizing the output.
    let mut rng = Lcg::new(0x00D1_000Au64);
    for order in [
        dialogic::NibbleOrder::HiFirst,
        dialogic::NibbleOrder::LoFirst,
    ] {
        for chs in [1usize, 2usize] {
            let mut state = vec![dialogic::Channel::default(); chs];
            for _ in 0..64 {
                // Interleaved sample count is sometimes odd (not a whole
                // number of frames) to stress the trailing-pad path.
                let n = (rng.next_u64() as usize) % 257;
                let pcm = rng.pcm(n);
                let out = dialogic::encode_packet_multi(&pcm, &mut state, order);
                assert_eq!(out.len(), n.div_ceil(2));
            }
        }
    }
}

#[test]
fn dialogic_encode_packet_multi_extreme_state_seed_never_panics() {
    // Per-channel adversarial seeds (out-of-range predictor / step index)
    // must clamp, not overflow, in the shared decode_nibble advance path.
    let mut state = vec![
        dialogic::Channel {
            predictor: 900_000,
            step_index: -50,
        },
        dialogic::Channel {
            predictor: -900_000,
            step_index: 900,
        },
    ];
    let _ = dialogic::encode_packet_multi(&[0; 65], &mut state, dialogic::NibbleOrder::HiFirst);
    let _ = dialogic::encode_packet_multi_wide16(
        &[i16::MIN, i16::MAX, 0, -1, 1, 12345],
        &mut state,
        dialogic::NibbleOrder::LoFirst,
    );
}

// ---------- Cross-variant trait-level end-to-end ----------

#[test]
fn registry_encoder_never_panics_on_zero_length_pcm() {
    use oxideav_adpcm::{
        register_codecs, CODEC_ID_DIALOGIC, CODEC_ID_IMA_QT, CODEC_ID_IMA_WAV, CODEC_ID_MS,
        CODEC_ID_YAMAHA, CODEC_ID_YAMAHA_A,
    };
    use oxideav_core::{AudioFrame, CodecId, CodecParameters, CodecRegistry, Frame};
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);
    for id in [
        CODEC_ID_MS,
        CODEC_ID_IMA_WAV,
        CODEC_ID_IMA_QT,
        CODEC_ID_YAMAHA,
        CODEC_ID_YAMAHA_A,
        CODEC_ID_DIALOGIC,
    ] {
        let mut p = CodecParameters::audio(CodecId::new(id));
        p.sample_rate = Some(8000);
        p.channels = Some(1);
        let mut enc = reg.first_encoder(&p).expect("encoder factory");
        // Zero-length input AudioFrame must not panic.
        let af = AudioFrame {
            samples: 0,
            pts: Some(0),
            data: vec![Vec::new()],
        };
        let _ = enc.send_frame(&Frame::Audio(af));
        // Flush must not panic either (it drains the partial-block tail).
        let _ = enc.flush();
    }
}

#[test]
fn registry_encoder_handles_random_pcm_bytes_without_panic() {
    use oxideav_adpcm::{
        register_codecs, CODEC_ID_DIALOGIC, CODEC_ID_IMA_QT, CODEC_ID_IMA_WAV, CODEC_ID_MS,
        CODEC_ID_YAMAHA, CODEC_ID_YAMAHA_A,
    };
    use oxideav_core::{AudioFrame, CodecId, CodecParameters, CodecRegistry, Frame};
    let mut rng = Lcg::new(0x00E3_0009u64);
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);
    for id in [
        CODEC_ID_MS,
        CODEC_ID_IMA_WAV,
        CODEC_ID_IMA_QT,
        CODEC_ID_YAMAHA,
        CODEC_ID_YAMAHA_A,
        CODEC_ID_DIALOGIC,
    ] {
        for chs in [1u16, 2u16] {
            // ADPCM-A registry path is mono only; Dialogic now accepts
            // 1..=2 channels (stereo via nibble interleave).
            if chs == 2 && id == CODEC_ID_YAMAHA_A {
                continue;
            }
            let mut p = CodecParameters::audio(CodecId::new(id));
            p.sample_rate = Some(8000);
            p.channels = Some(chs);
            let mut enc = reg.first_encoder(&p).expect("encoder factory");
            // Generate a chunk of random PCM (sample-aligned).
            let n_samples_per_ch = 256;
            let pcm: Vec<i16> = rng.pcm(n_samples_per_ch * chs as usize);
            let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
            let af = AudioFrame {
                samples: n_samples_per_ch as u32,
                pts: Some(0),
                data: vec![bytes],
            };
            let _ = enc.send_frame(&Frame::Audio(af));
            let _ = enc.flush();
            // Drain whatever packets emerged without inspecting their
            // content — the contract here is "no panic regardless of
            // input bytes," not "produces a specific output."
            while enc.receive_packet().is_ok() {}
        }
    }
}
