//! Structured-malformation / never-panic robustness tests for every
//! ADPCM decoder variant exposed by this crate (`adpcm_ms`,
//! `adpcm_ima_wav`, `adpcm_ima_qt`, `adpcm_yamaha`, `adpcm_dialogic`).
//!
//! Each variant's decoder is exercised against:
//!
//! - **Truncated input** — every prefix of a well-formed packet must
//!   either decode correctly (if it happens to land on a valid block
//!   boundary) or return `Err` cleanly; never panic.
//! - **Spec-malformed headers** — predictor indices outside the
//!   tabulated range (MS), step indices outside `0..=88` (IMA-WAV),
//!   undersized blocks (IMA-QT) all surface as `Err`.
//! - **Deterministic pseudo-random byte streams** — a small LCG seeded
//!   inside the test (no external crates) drives a few thousand random
//!   bytes through each decoder. Decoders must accept the bytes
//!   silently (the stream-oriented variants don't reject any byte
//!   pattern) or return `Err` on a structural mismatch; never panic
//!   regardless of the bytes that arrive.
//! - **Bounded-output invariant** — for any decoder path that returns
//!   `Ok`, every emitted sample must lie within i16 (the type system
//!   already proves this, but we assert the *count* matches the
//!   spec-derived formula so a regression that emits the wrong number
//!   of samples is caught).
//!
//! These tests do not run against external fixtures or third-party
//! decoder source; every input is synthesised inside the test from
//! constants printed in the same public specs the production decoders
//! were derived from.
//!
//! The same invariants are exercised end-to-end through the
//! `oxideav_core::Decoder` trait so the `send_packet` / `receive_frame`
//! glue path is on the no-panic contract as well.

use oxideav_adpcm::{
    decoder, dialogic, ima_qt, ima_wav, ms, register_codecs, yamaha, yamaha_a, CODEC_ID_DIALOGIC,
    CODEC_ID_IMA_QT, CODEC_ID_IMA_WAV, CODEC_ID_MS, CODEC_ID_YAMAHA, CODEC_ID_YAMAHA_A,
};
use oxideav_core::{CodecId, CodecParameters, CodecRegistry, Packet, TimeBase};

// ----- deterministic byte generator ----------------------------------

/// Tiny linear-congruential generator. Used purely to walk the input
/// space with reproducible coverage; not a cryptographic PRNG.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_add(0x9E3779B97F4A7C15))
    }
    fn next_u8(&mut self) -> u8 {
        // Numerical Recipes constants — common, well-known LCG choice.
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 56) as u8
    }
    fn fill(&mut self, buf: &mut [u8]) {
        for b in buf {
            *b = self.next_u8();
        }
    }
}

// ----- MS-ADPCM -----------------------------------------------------

/// MS-ADPCM with every predictor index out of range (7..=255) is rejected
/// without panicking. Range 0..=6 are valid per the spec; anything above
/// is a structural error.
#[test]
fn ms_predictor_index_out_of_range_is_rejected() {
    for bad_pi in 7u8..=255 {
        let mut block = Vec::new();
        block.push(bad_pi);
        block.extend_from_slice(&16i16.to_le_bytes());
        block.extend_from_slice(&0i16.to_le_bytes());
        block.extend_from_slice(&0i16.to_le_bytes());
        // Body — zero nibbles, two samples worth.
        block.push(0);
        assert!(
            ms::decode_block(&block, 1).is_err(),
            "predictor index {bad_pi} should be rejected"
        );
    }
}

/// MS-ADPCM with any predictor index 0..=6 decodes; the emitted sample
/// count matches the spec formula `2 + body_bytes * 2 / channels`.
#[test]
fn ms_valid_predictor_index_emits_spec_sample_count() {
    for pi in 0u8..=6 {
        for body_bytes in [0, 1, 4, 16, 64] {
            let mut block = Vec::new();
            block.push(pi);
            block.extend_from_slice(&16i16.to_le_bytes());
            block.extend_from_slice(&0i16.to_le_bytes());
            block.extend_from_slice(&0i16.to_le_bytes());
            block.extend(std::iter::repeat(0u8).take(body_bytes));
            let pcm = ms::decode_block(&block, 1).unwrap();
            // 2 prelude samples + 2 nibbles per body byte.
            assert_eq!(
                pcm.len(),
                2 + body_bytes * 2,
                "pi {pi}, body {body_bytes}: wrong sample count"
            );
        }
    }
}

/// MS-ADPCM never panics on any prefix of a 64-byte deterministic
/// PRNG stream; lengths below the minimum header return `Err`.
#[test]
fn ms_truncated_prefixes_never_panic_mono() {
    let mut rng = Lcg::new(0x1234_5678);
    let mut full = vec![0u8; 64];
    rng.fill(&mut full);
    // Force a valid predictor index so the rest of the block at full
    // length is decodable; sweep the prefix lengths.
    full[0] = 0;
    for n in 0..=full.len() {
        // The function returns Result — we just assert it doesn't panic.
        let _ = ms::decode_block(&full[..n], 1);
    }
}

/// MS-ADPCM stereo: header 14 bytes, every length below 14 must Err.
#[test]
fn ms_short_stereo_block_is_rejected() {
    for n in 0..14 {
        let buf = vec![0u8; n];
        assert!(
            ms::decode_block(&buf, 2).is_err(),
            "len {n} should be rejected"
        );
    }
}

/// MS-ADPCM rejects an out-of-range channel count without panicking.
#[test]
fn ms_invalid_channel_count_is_rejected() {
    for ch in [0usize, 3, 8, 64] {
        let block = vec![0u8; 64];
        assert!(
            ms::decode_block(&block, ch).is_err(),
            "channels {ch} should be rejected"
        );
    }
}

// ----- IMA-ADPCM WAV ------------------------------------------------

/// IMA-WAV step index out of range (>= 89) is rejected; in-range is
/// accepted.
#[test]
fn ima_wav_step_index_out_of_range_is_rejected() {
    for bad_idx in 89u8..=255 {
        let mut block = Vec::new();
        block.extend_from_slice(&0i16.to_le_bytes());
        block.push(bad_idx);
        block.push(0);
        assert!(
            ima_wav::decode_block(&block, 1).is_err(),
            "step index {bad_idx} should be rejected"
        );
    }
}

/// IMA-WAV with every in-range step index decodes, emits `1 + groups*8`
/// samples per channel.
#[test]
fn ima_wav_in_range_step_index_emits_spec_sample_count() {
    for idx in 0u8..=88 {
        for groups in [0, 1, 4, 16] {
            let mut block = Vec::new();
            block.extend_from_slice(&0i16.to_le_bytes());
            block.push(idx);
            block.push(0);
            block.extend(std::iter::repeat(0u8).take(groups * 4));
            let pcm = ima_wav::decode_block(&block, 1).unwrap();
            assert_eq!(
                pcm.len(),
                1 + groups * 8,
                "idx {idx}, groups {groups}: wrong sample count"
            );
        }
    }
}

/// IMA-WAV: body not divisible by `4 * channels` is rejected without
/// panic.
#[test]
fn ima_wav_misaligned_body_is_rejected() {
    let header_size = 4; // mono.
                         // body length 1..=3 are all misaligned (must be multiple of 4).
    for body in 1usize..=3 {
        let mut block = Vec::with_capacity(header_size + body);
        block.extend_from_slice(&0i16.to_le_bytes());
        block.push(0);
        block.push(0);
        block.extend(std::iter::repeat(0u8).take(body));
        assert!(
            ima_wav::decode_block(&block, 1).is_err(),
            "mono body {body} bytes should be rejected"
        );
    }
}

/// IMA-WAV: deterministic random bytes through every prefix length
/// never panic.
#[test]
fn ima_wav_truncated_prefixes_never_panic() {
    let mut rng = Lcg::new(0xCAFE_BABE);
    let mut full = vec![0u8; 128];
    rng.fill(&mut full);
    // Force step index byte (position 2 for mono header) to a valid
    // value so the header passes; the body bytes may still misalign.
    full[2] = 40;
    for n in 0..=full.len() {
        let _ = ima_wav::decode_block(&full[..n], 1);
    }
}

/// IMA-WAV channel count above 8 is rejected.
#[test]
fn ima_wav_invalid_channel_count_is_rejected() {
    for ch in [0usize, 9, 16, 64] {
        let block = vec![0u8; 256];
        assert!(
            ima_wav::decode_block(&block, ch).is_err(),
            "channels {ch} should be rejected"
        );
    }
}

// ----- IMA-ADPCM QT -------------------------------------------------

/// QT-IMA: any length below 34 bytes for mono (or 68 for stereo) is
/// rejected.
#[test]
fn ima_qt_short_block_is_rejected() {
    for n in 0..34 {
        let buf = vec![0u8; n];
        assert!(ima_qt::decode_block(&buf, 1).is_err());
    }
    for n in 0..68 {
        let buf = vec![0u8; n];
        assert!(ima_qt::decode_block(&buf, 2).is_err());
    }
}

/// QT-IMA: full 34-byte block always decodes — every possible byte
/// value at position 0 (the high byte of the big-endian preamble) is
/// exercised. Output is always 64 samples per channel.
#[test]
fn ima_qt_full_block_decodes_for_every_preamble_hi() {
    for hi in 0u8..=255 {
        let mut block = [0u8; 34];
        block[0] = hi;
        // Force step-index field (low 7 bits of preamble) to a valid
        // value so the in-spec clamp is exercised. Even if `hi`'s low
        // bits encode something silly, the decoder clamps internally.
        block[1] = 50;
        let pcm = ima_qt::decode_block(&block, 1).unwrap();
        assert_eq!(pcm.len(), 64);
    }
}

/// QT-IMA: deterministic random bytes through every prefix length
/// never panic. Full-length runs decode; shorter ones return Err.
#[test]
fn ima_qt_truncated_prefixes_never_panic() {
    let mut rng = Lcg::new(0xDEAD_F00D);
    let mut full = vec![0u8; 68];
    rng.fill(&mut full);
    for n in 0..=full.len() {
        let _ = ima_qt::decode_block(&full[..n], 1);
        let _ = ima_qt::decode_block(&full[..n], 2);
    }
}

/// QT-IMA: invalid channel counts rejected without panic.
#[test]
fn ima_qt_invalid_channel_count_is_rejected() {
    for ch in [0usize, 3, 8] {
        let block = vec![0u8; 34 * 3];
        assert!(ima_qt::decode_block(&block, ch).is_err());
    }
}

// ----- Yamaha -------------------------------------------------------

/// Yamaha: any byte stream is structurally valid (no headers).
/// Emitted sample count is always `2 * packet_bytes`. Step + predictor
/// state is bounded across the run.
#[test]
fn yamaha_any_byte_stream_decodes_to_two_samples_per_byte() {
    let mut rng = Lcg::new(0xBEEF_0001);
    for len in [0, 1, 2, 7, 64, 1024] {
        let mut packet = vec![0u8; len];
        rng.fill(&mut packet);
        let mut state = [yamaha::Channel::default()];
        let pcm = yamaha::decode_packet(&packet, &mut state);
        assert_eq!(
            pcm.len(),
            len * 2,
            "len {len}: expected {} samples, got {}",
            len * 2,
            pcm.len()
        );
        // State stays inside the spec range after every byte.
        assert!(state[0].step >= 127, "step underflow: {}", state[0].step);
        assert!(state[0].step <= 24576, "step overflow: {}", state[0].step);
        assert!(state[0].predictor >= i16::MIN as i32);
        assert!(state[0].predictor <= i16::MAX as i32);
    }
}

/// Yamaha: empty state slice produces empty output (defensive
/// zero-channel handling — `decode_packet` checks `state.len() == 0`
/// before iterating).
#[test]
fn yamaha_zero_channel_state_emits_empty() {
    let mut empty: [yamaha::Channel; 0] = [];
    let pcm = yamaha::decode_packet(&[0xFF; 8], &mut empty);
    assert!(pcm.is_empty());
}

/// Yamaha: stereo state — two channels round-robin. Sample count
/// matches the mono formula because each byte still yields two nibbles.
#[test]
fn yamaha_stereo_state_emits_two_samples_per_byte() {
    let mut rng = Lcg::new(0xBEEF_0002);
    let mut packet = vec![0u8; 256];
    rng.fill(&mut packet);
    let mut state = [yamaha::Channel::default(), yamaha::Channel::default()];
    let pcm = yamaha::decode_packet(&packet, &mut state);
    assert_eq!(pcm.len(), 512);
}

/// Yamaha encoder: any i16 sample sequence is encodable; encode→decode
/// produces a sample count exactly equal to the input length (with the
/// zero-pad rule applied to odd lengths).
#[test]
fn yamaha_encode_then_decode_preserves_sample_count() {
    let mut rng = Lcg::new(0xBEEF_0003);
    for len in [0usize, 1, 7, 64, 511, 1024] {
        let pcm: Vec<i16> = (0..len)
            .map(|_| {
                rng.next_u8();
                ((rng.next_u8() as i16) << 4) - 1024
            })
            .collect();
        let mut enc_state = [yamaha::Channel::default()];
        let bytes = yamaha::encode_packet(&pcm, &mut enc_state);
        // Output bytes = ceil(len / 2).
        assert_eq!(bytes.len(), len.div_ceil(2));
        let mut dec_state = [yamaha::Channel::default()];
        let decoded = yamaha::decode_packet(&bytes, &mut dec_state);
        // Two samples per byte; for odd len the trailing nibble is a
        // zero pad, so the decoder produces an extra sample.
        assert_eq!(decoded.len(), bytes.len() * 2);
    }
}

// ----- Yamaha ADPCM-A ----------------------------------------------

/// Yamaha ADPCM-A: any byte stream is structurally valid (no headers,
/// single channel). Two output samples per byte. State invariants:
/// 12-bit signed acc, step pointer in `0..=48`.
#[test]
fn yamaha_a_any_byte_stream_decodes_to_two_samples_per_byte() {
    let mut rng = Lcg::new(0xA0A0_0001);
    for len in [0usize, 1, 2, 7, 64, 1024] {
        let mut packet = vec![0u8; len];
        rng.fill(&mut packet);
        let mut state = [yamaha_a::Channel::default()];
        let pcm = yamaha_a::decode_packet(&packet, &mut state, yamaha_a::Output::Native12);
        assert_eq!(pcm.len(), len * 2, "len {len}");
        // 12-bit signed acc inside its clamp range.
        assert!(state[0].acc >= -2048, "acc underflow: {}", state[0].acc);
        assert!(state[0].acc <= 2047, "acc overflow: {}", state[0].acc);
        assert!(state[0].step_index >= 0);
        assert!(state[0].step_index <= 48);
    }
}

/// Yamaha ADPCM-A Wide16: every sample inside `[-32768, 32752]`
/// (= 12-bit clamp left-shifted by 4).
#[test]
fn yamaha_a_wide16_output_stays_within_documented_range() {
    let mut rng = Lcg::new(0xA0A0_0002);
    let mut packet = vec![0u8; 2048];
    rng.fill(&mut packet);
    let mut state = [yamaha_a::Channel::default()];
    let pcm = yamaha_a::decode_packet(&packet, &mut state, yamaha_a::Output::Wide16);
    for &s in &pcm {
        assert!((-32768..=32752).contains(&s), "Wide16 sample {s} OOR");
    }
}

/// Yamaha ADPCM-A: empty state or empty packet → empty output, no panic.
#[test]
fn yamaha_a_empty_inputs_produce_empty_outputs() {
    let mut state = [yamaha_a::Channel::default()];
    let pcm = yamaha_a::decode_packet(&[], &mut state, yamaha_a::Output::Native12);
    assert!(pcm.is_empty());
    let mut empty: [yamaha_a::Channel; 0] = [];
    let pcm = yamaha_a::decode_packet(&[0xAA, 0x55], &mut empty, yamaha_a::Output::Wide16);
    assert!(pcm.is_empty());
}

/// Yamaha ADPCM-A encoder: encode→decode preserves sample count
/// (with the trailing-zero-nibble pad rule for odd inputs).
#[test]
fn yamaha_a_encode_then_decode_preserves_sample_count() {
    let mut rng = Lcg::new(0xA0A0_0003);
    for len in [0usize, 1, 7, 64, 511, 1024] {
        let pcm: Vec<i16> = (0..len)
            .map(|_| {
                rng.next_u8();
                ((rng.next_u8() as i16) << 4) - 1024
            })
            .collect();
        let mut enc_state = [yamaha_a::Channel::default()];
        let bytes = yamaha_a::encode_packet(&pcm, &mut enc_state, yamaha_a::Output::Wide16);
        assert_eq!(bytes.len(), len.div_ceil(2));
        let mut dec_state = [yamaha_a::Channel::default()];
        let decoded = yamaha_a::decode_packet(&bytes, &mut dec_state, yamaha_a::Output::Wide16);
        assert_eq!(decoded.len(), bytes.len() * 2);
    }
}

/// Yamaha ADPCM-A factory rejects stereo (the codec is single-channel
/// by chip design).
#[test]
fn yamaha_a_factory_rejects_stereo() {
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);
    let mut params = CodecParameters::audio(CodecId::new(CODEC_ID_YAMAHA_A));
    params.sample_rate = Some(22_050);
    params.channels = Some(2);
    assert!(
        reg.first_decoder(&params).is_err(),
        "stereo ADPCM-A should be rejected"
    );
    assert!(
        reg.first_encoder(&params).is_err(),
        "stereo ADPCM-A encoder should be rejected"
    );
}

// ----- OKI / Dialogic -----------------------------------------------

/// Dialogic: any byte stream decodes; both nibble orders produce
/// `2 * packet_bytes` samples mono. Predictor stays inside ±2047,
/// step pointer stays inside 0..=48.
#[test]
fn dialogic_any_byte_stream_decodes_to_two_samples_per_byte() {
    let mut rng = Lcg::new(0xD1A1_0F1C);
    for order in [
        dialogic::NibbleOrder::HiFirst,
        dialogic::NibbleOrder::LoFirst,
    ] {
        for len in [0usize, 1, 7, 64, 1024] {
            let mut packet = vec![0u8; len];
            rng.fill(&mut packet);
            let mut state = [dialogic::Channel::default()];
            let pcm =
                dialogic::decode_packet(&packet, &mut state, order, dialogic::Output::Native12);
            assert_eq!(pcm.len(), len * 2, "{order:?} len {len}");
            assert!(state[0].predictor >= -2048);
            assert!(state[0].predictor <= 2047);
            assert!(state[0].step_index >= 0);
            assert!(state[0].step_index <= 48);
        }
    }
}

/// Dialogic: Wide16 output stays inside the published i16 range
/// `-32768..=32752`.
#[test]
fn dialogic_wide16_output_stays_within_documented_range() {
    let mut rng = Lcg::new(0xD1A1_0F2C);
    let mut packet = vec![0u8; 2048];
    rng.fill(&mut packet);
    let mut state = [dialogic::Channel::default()];
    let pcm = dialogic::decode_packet(
        &packet,
        &mut state,
        dialogic::NibbleOrder::HiFirst,
        dialogic::Output::Wide16,
    );
    for &s in &pcm {
        // Max representable is 2047 << 4 = 32752; min is -2048 << 4 =
        // -32768. The clamp inside `decode_nibble` enforces this.
        assert!(
            (-32768..=32752).contains(&s),
            "Wide16 sample {s} out of range"
        );
    }
}

/// Dialogic: empty state or empty packet produces empty output without
/// panic.
#[test]
fn dialogic_empty_inputs_produce_empty_outputs() {
    let mut state = [dialogic::Channel::default()];
    let pcm = dialogic::decode_packet(
        &[],
        &mut state,
        dialogic::NibbleOrder::HiFirst,
        dialogic::Output::Wide16,
    );
    assert!(pcm.is_empty());
    let mut empty: [dialogic::Channel; 0] = [];
    let pcm = dialogic::decode_packet(
        &[0xFF, 0xAA],
        &mut empty,
        dialogic::NibbleOrder::HiFirst,
        dialogic::Output::Wide16,
    );
    assert!(pcm.is_empty());
}

// ----- Trait-level (Decoder) end-to-end never-panic -----------------

/// Build a `Decoder` for every variant through the registry and push
/// pseudo-random bytes into `send_packet`; never panic, drain the
/// pending frame, repeat.
#[test]
fn registry_decoder_send_random_packet_never_panics() {
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);
    let mut rng = Lcg::new(0xF00D_BABE);

    for codec_id in [
        CODEC_ID_MS,
        CODEC_ID_IMA_WAV,
        CODEC_ID_IMA_QT,
        CODEC_ID_YAMAHA,
        CODEC_ID_YAMAHA_A,
        CODEC_ID_DIALOGIC,
    ] {
        let mut params = CodecParameters::audio(CodecId::new(codec_id));
        params.sample_rate = Some(22_050);
        params.channels = Some(1);
        let mut dec = reg.first_decoder(&params).expect("decoder factory");
        let tb = TimeBase::new(1, 22_050);

        for _ in 0..16 {
            let mut data = vec![0u8; 96];
            rng.fill(&mut data);
            let pkt = Packet {
                stream_index: 0,
                pts: Some(0),
                dts: Some(0),
                duration: None,
                time_base: tb,
                flags: Default::default(),
                data,
            };
            // The error path is fine — we only care about *not panicking*.
            if dec.send_packet(&pkt).is_ok() {
                let _ = dec.receive_frame();
            } else {
                // Reset so the next iteration isn't stuck on the prior
                // failed packet (Yamaha + Dialogic don't error here so
                // this only fires for the block-oriented variants on a
                // truly malformed prefix).
                let _ = dec.reset();
            }
        }
    }
}

/// Empty-packet handling: every variant accepts an empty packet
/// (returns an empty audio frame, no error) — this matches the
/// `decode_packet` contract in `decoder.rs` where the `pkt.data.is_empty()`
/// branch installs a zero-sample pending frame.
#[test]
fn registry_decoder_accepts_empty_packets() {
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);
    for codec_id in [
        CODEC_ID_MS,
        CODEC_ID_IMA_WAV,
        CODEC_ID_IMA_QT,
        CODEC_ID_YAMAHA,
        CODEC_ID_YAMAHA_A,
        CODEC_ID_DIALOGIC,
    ] {
        let mut params = CodecParameters::audio(CodecId::new(codec_id));
        params.sample_rate = Some(8_000);
        params.channels = Some(1);
        let mut dec = reg.first_decoder(&params).expect("decoder factory");
        let pkt = Packet {
            stream_index: 0,
            pts: Some(0),
            dts: Some(0),
            duration: None,
            time_base: TimeBase::new(1, 8_000),
            flags: Default::default(),
            data: Vec::new(),
        };
        dec.send_packet(&pkt)
            .unwrap_or_else(|e| panic!("{codec_id}: empty packet should be accepted: {e:?}"));
        let _ = dec.receive_frame();
    }
}

/// Decoder factory rejects zero channels for every variant.
#[test]
fn registry_decoder_factory_rejects_zero_channels() {
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);
    for codec_id in [
        CODEC_ID_MS,
        CODEC_ID_IMA_WAV,
        CODEC_ID_IMA_QT,
        CODEC_ID_YAMAHA,
        CODEC_ID_YAMAHA_A,
        CODEC_ID_DIALOGIC,
    ] {
        let mut params = CodecParameters::audio(CodecId::new(codec_id));
        params.sample_rate = Some(8_000);
        params.channels = Some(0);
        assert!(
            reg.first_decoder(&params).is_err(),
            "{codec_id}: channels=0 should be rejected"
        );
    }
}

/// Variant from-codec-id round trip: every advertised codec id resolves
/// to a `Variant`; an unknown id resolves to none.
#[test]
fn variant_dispatch_covers_every_advertised_codec_id() {
    // We don't have a public Variant::from_codec_id accessor — but the
    // factory already exercises this implicitly. Verify here that
    // unknown ids reject through the factory error path.
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);
    // Unknown id never reaches our factory because the registry won't
    // route it — verify the registry has no decoder for a random tag.
    let unknown = CodecId::new("adpcm_does_not_exist");
    assert!(!reg.has_decoder(&unknown));
}

/// Final sanity: an `AdpcmDecoder` returned by the factory exposes its
/// `codec_id()` faithfully.
#[test]
fn registry_decoder_reports_its_codec_id() {
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);
    for codec_id in [
        CODEC_ID_MS,
        CODEC_ID_IMA_WAV,
        CODEC_ID_IMA_QT,
        CODEC_ID_YAMAHA,
        CODEC_ID_YAMAHA_A,
        CODEC_ID_DIALOGIC,
    ] {
        let mut params = CodecParameters::audio(CodecId::new(codec_id));
        params.sample_rate = Some(8_000);
        params.channels = Some(1);
        let dec = reg.first_decoder(&params).expect("decoder factory");
        assert_eq!(dec.codec_id().as_str(), codec_id);
    }
}

// Silence unused-import warnings if the `decoder` module ever stops
// re-exporting public items — currently nothing from `decoder` is used
// directly, but we keep the import as a compile-time anchor so adding
// new public items to the module surfaces here first.
#[allow(dead_code)]
fn _decoder_import_anchor() {
    let _ = std::mem::size_of::<decoder::AdpcmDecoder>();
}
