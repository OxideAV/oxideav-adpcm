//! G.723/G.721-in-WAV sub-block bit-cell layer — byte-exact pins against
//! the staged packing vectors (`docs/audio/adpcm/g72x-wav/`), the one
//! surviving archived-catalogue bit row, and codec-level round trips.
//!
//! The container layer (which bit of which byte holds each sample's
//! code bits) is validated independently of the ADPCM math: the staged
//! note reconstructs the grid from the documented packing convention
//! plus the surviving stereo-3-bit "Byte 3" row, and publishes four
//! byte-exact packing vectors. A re-implementation must reproduce those
//! bytes exactly.

use oxideav_adpcm::g726::{
    self, wav_block_align, wav_decode_packet, wav_encode_packet, wav_pack_codes,
    wav_rate_supported, wav_strip_aux, wav_subblock_bytes, wav_unpack_codes, Rate, State,
};
use oxideav_adpcm::CODEC_ID_G726;
use oxideav_core::{CodecId, CodecParameters, CodecRegistry, Frame, Packet, TimeBase};

// ---------------------------------------------------------------------------
// Staged packing vectors (docs/audio/adpcm/g72x-wav/bitcell-vectors.txt)
// ---------------------------------------------------------------------------

#[test]
fn staged_vector_mono_3bit() {
    let codes = [0u8, 1, 2, 3, 4, 5, 6, 7];
    let packed = wav_pack_codes(&codes, Rate::R24, 1).unwrap();
    assert_eq!(packed, [0x05, 0x39, 0x77]);
    assert_eq!(wav_unpack_codes(&packed, Rate::R24, 1).unwrap(), codes);
}

#[test]
fn staged_vector_mono_5bit() {
    let codes = [0u8, 3, 6, 9, 12, 15, 18, 21];
    let packed = wav_pack_codes(&codes, Rate::R40, 1).unwrap();
    assert_eq!(packed, [0x00, 0xcc, 0x96, 0x3e, 0x55]);
    assert_eq!(wav_unpack_codes(&packed, Rate::R40, 1).unwrap(), codes);
}

#[test]
fn staged_vector_stereo_3bit() {
    // AL AR BL BR CL CR DL DR EL ER FL FR GL GR HL HR
    let codes = [0u8, 7, 1, 6, 2, 5, 3, 4, 4, 3, 5, 2, 6, 1, 7, 0];
    let packed = wav_pack_codes(&codes, Rate::R24, 2).unwrap();
    assert_eq!(packed, [0x1c, 0xe5, 0x5c, 0x8e, 0xac, 0x78]);
    assert_eq!(wav_unpack_codes(&packed, Rate::R24, 2).unwrap(), codes);
}

#[test]
fn staged_vector_stereo_5bit() {
    let codes = [0u8, 21, 3, 18, 6, 15, 9, 12, 12, 9, 15, 6, 18, 3, 21, 0];
    let packed = wav_pack_codes(&codes, Rate::R40, 2).unwrap();
    assert_eq!(
        packed,
        [0x05, 0x47, 0x23, 0x3d, 0x2c, 0x62, 0x5e, 0x69, 0x0e, 0xa0]
    );
    assert_eq!(wav_unpack_codes(&packed, Rate::R40, 2).unwrap(), codes);
}

/// The single bit row that survived in the archived catalogue —
/// stereo 3-bit "Byte 3" = `CR1 CR0 DL2 DL1 DL0 DR2 DR1 DR0` — is the
/// anchor that proves the whole reconstructed grid. Rebuild that byte
/// bit-by-bit from labelled sample codes, independently of the packer,
/// and check the packer places it at byte index 2.
#[test]
fn surviving_spec_row_anchors_the_grid() {
    // Arbitrary distinct codes for time indices C and D, both channels.
    let (cl, cr, dl, dr) = (0b010u8, 0b101u8, 0b011u8, 0b100u8);
    let mut codes = [0u8; 16];
    codes[4] = cl; // CL
    codes[5] = cr; // CR
    codes[6] = dl; // DL
    codes[7] = dr; // DR
    let packed = wav_pack_codes(&codes, Rate::R24, 2).unwrap();
    let byte3 = ((cr >> 1) & 1) << 7 // CR1
        | (cr & 1) << 6              // CR0
        | ((dl >> 2) & 1) << 5       // DL2
        | ((dl >> 1) & 1) << 4       // DL1
        | (dl & 1) << 3              // DL0
        | ((dr >> 2) & 1) << 2       // DR2
        | ((dr >> 1) & 1) << 1       // DR1
        | (dr & 1); //                  DR0
    assert_eq!(
        packed[2], byte3,
        "stereo-3-bit Byte 3 must match the surviving spec row"
    );
}

/// At the 4-bit (G.721, tag 0x0040) rate the same MSB-first convention
/// degenerates to plain big-nibble-first nibble packing.
#[test]
fn four_bit_rate_is_nibble_packing() {
    let codes = [0x1u8, 0x2, 0x3, 0x4, 0x5, 0x6, 0x7, 0x8];
    let packed = wav_pack_codes(&codes, Rate::R32, 1).unwrap();
    assert_eq!(packed, [0x12, 0x34, 0x56, 0x78]);
    assert_eq!(wav_unpack_codes(&packed, Rate::R32, 1).unwrap(), codes);
}

// ---------------------------------------------------------------------------
// Framing contract
// ---------------------------------------------------------------------------

fn xorshift(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

#[test]
fn pack_unpack_round_trip_all_supported_modes() {
    let mut seed = 0x2470_c0de;
    for &rate in &[Rate::R24, Rate::R32, Rate::R40] {
        for channels in 1u16..=2 {
            let group = 8 * channels as usize;
            for subblocks in [1usize, 2, 16, 33] {
                let codes: Vec<u8> = (0..group * subblocks)
                    .map(|_| (xorshift(&mut seed) & ((1 << rate.bits()) - 1)) as u8)
                    .collect();
                let packed = wav_pack_codes(&codes, rate, channels).unwrap();
                assert_eq!(packed.len(), subblocks * wav_subblock_bytes(rate, channels));
                assert_eq!(wav_unpack_codes(&packed, rate, channels).unwrap(), codes);
            }
        }
    }
}

#[test]
fn unsupported_rate_and_geometry_are_rejected() {
    // 2-bit / 16 kbit/s has no tag in the archived catalogue.
    assert!(!wav_rate_supported(Rate::R16));
    assert!(wav_pack_codes(&[0u8; 8], Rate::R16, 1).is_err());
    assert!(wav_unpack_codes(&[0u8; 2], Rate::R16, 1).is_err());
    // Channel counts outside 1..=2.
    assert!(wav_pack_codes(&[0u8; 24], Rate::R24, 3).is_err());
    assert!(wav_pack_codes(&[], Rate::R24, 0).is_err());
    // Partial sub-blocks in either direction.
    assert!(wav_pack_codes(&[0u8; 7], Rate::R24, 1).is_err());
    assert!(wav_pack_codes(&[0u8; 12], Rate::R24, 2).is_err());
    assert!(wav_unpack_codes(&[0u8; 4], Rate::R24, 1).is_err());
    assert!(wav_unpack_codes(&[0u8; 9], Rate::R40, 2).is_err());
}

#[test]
fn strip_aux_walks_block_boundaries() {
    // 3-bit mono with a 4-byte auxiliary prefix: nBlockAlign = 48 + 4.
    let aux = 4usize;
    let block = wav_block_align(Rate::R24, 1, aux);
    assert_eq!(block, 52);
    let mut data = Vec::new();
    for b in 0..3u8 {
        data.extend_from_slice(&[0xAA; 4]); // aux prefix
        data.extend_from_slice(&[b; 48]); // payload
    }
    let stripped = wav_strip_aux(&data, block, aux).unwrap();
    assert_eq!(stripped.len(), 3 * 48);
    for (i, chunk) in stripped.chunks(48).enumerate() {
        assert!(chunk.iter().all(|&x| x == i as u8));
    }

    // Truncation tolerance: a short trailing block keeps its payload…
    let mut short = data.clone();
    short.truncate(2 * block + aux + 10);
    let stripped = wav_strip_aux(&short, block, aux).unwrap();
    assert_eq!(stripped.len(), 2 * 48 + 10);
    // …and a tail inside the prefix contributes nothing.
    let mut inside = data.clone();
    inside.truncate(2 * block + 2);
    assert_eq!(wav_strip_aux(&inside, block, aux).unwrap().len(), 2 * 48);

    // aux = 0 is the identity.
    assert_eq!(wav_strip_aux(&data, 48, 0).unwrap(), data);
    // A prefix that swallows the whole block is invalid.
    assert!(wav_strip_aux(&data, block, block).is_err());
    assert!(wav_strip_aux(&data, 0, 1).is_err());
}

// ---------------------------------------------------------------------------
// Codec-level round trips (per-channel state plumbing)
// ---------------------------------------------------------------------------

fn sine(n: usize, freq: f64, amp: f64, phase: f64) -> Vec<i16> {
    (0..n)
        .map(|i| {
            (amp * (2.0 * std::f64::consts::PI * freq * i as f64 / 8000.0 + phase).sin()) as i16
        })
        .collect()
}

fn correlation(a: &[i16], b: &[i16]) -> f64 {
    assert_eq!(a.len(), b.len());
    let (mut sab, mut saa, mut sbb) = (0f64, 0f64, 0f64);
    for (&x, &y) in a.iter().zip(b) {
        let (x, y) = (x as f64, y as f64);
        sab += x * y;
        saa += x * x;
        sbb += y * y;
    }
    sab / (saa.sqrt() * sbb.sqrt()).max(1e-9)
}

#[test]
fn mono_wav_round_trip_matches_raw_stream_decode() {
    // The WAV framing is a pure container transform at whole-byte
    // granularity: for mono, N sub-blocks carry the same MSB-first code
    // stream the raw telephony path would, so the two decodes must be
    // sample-identical.
    for &rate in &[Rate::R24, Rate::R32, Rate::R40] {
        let pcm = sine(1024, 320.0, 12000.0, 0.0);
        let mut enc = [State::new(rate)];
        let bytes = wav_encode_packet(&pcm, &mut enc).unwrap();
        assert_eq!(bytes.len(), 1024 / 8 * wav_subblock_bytes(rate, 1));

        let mut dec = [State::new(rate)];
        let wav_out = wav_decode_packet(&bytes, &mut dec).unwrap();

        let mut raw_state = State::new(rate);
        let mut unpacker = g726::BitUnpacker::new(g726::BitOrder::MsbFirst);
        let mut raw_out = Vec::new();
        g726::decode_packet(&bytes, &mut raw_state, &mut unpacker, &mut raw_out);

        assert_eq!(wav_out, raw_out, "rate {rate:?}");
        assert!(
            correlation(&pcm, &wav_out) > 0.9,
            "rate {rate:?}: round-trip correlation too low"
        );
    }
}

#[test]
fn stereo_lanes_are_independent_codecs() {
    // Two very different signals, one per channel; each lane must come
    // back tracking its own input.
    let n = 2048usize;
    let left = sine(n, 250.0, 14000.0, 0.0);
    let right = sine(n, 1450.0, 6000.0, 1.1);
    let mut interleaved = Vec::with_capacity(2 * n);
    for i in 0..n {
        interleaved.push(left[i]);
        interleaved.push(right[i]);
    }
    for &rate in &[Rate::R24, Rate::R32, Rate::R40] {
        let mut enc = [State::new(rate), State::new(rate)];
        let bytes = wav_encode_packet(&interleaved, &mut enc).unwrap();
        assert_eq!(bytes.len(), n / 8 * wav_subblock_bytes(rate, 2));

        let mut dec = [State::new(rate), State::new(rate)];
        let out = wav_decode_packet(&bytes, &mut dec).unwrap();
        assert_eq!(out.len(), 2 * n);
        let out_l: Vec<i16> = out.iter().step_by(2).copied().collect();
        let out_r: Vec<i16> = out.iter().skip(1).step_by(2).copied().collect();
        assert!(
            correlation(&left, &out_l) > 0.9,
            "rate {rate:?}: left lane lost"
        );
        assert!(
            correlation(&right, &out_r) > 0.9,
            "rate {rate:?}: right lane lost"
        );
        // Cross-lane leakage would show up as high L/R correlation of
        // the *outputs* despite uncorrelated inputs.
        assert!(
            correlation(&out_l, &out_r).abs() < 0.3,
            "rate {rate:?}: lanes leaked into each other"
        );
    }
}

#[test]
fn state_carries_across_packets_no_per_block_reset() {
    // Decoding one long payload in a single call and sub-block by
    // sub-block must agree exactly — the codec is stream-oriented
    // inside the container framing.
    for &rate in &[Rate::R24, Rate::R32, Rate::R40] {
        let pcm = sine(512, 440.0, 15000.0, 0.3);
        let mut enc = [State::new(rate)];
        let bytes = wav_encode_packet(&pcm, &mut enc).unwrap();

        let mut one = [State::new(rate)];
        let whole = wav_decode_packet(&bytes, &mut one).unwrap();

        let mut split_states = [State::new(rate)];
        let mut split = Vec::new();
        for chunk in bytes.chunks(wav_subblock_bytes(rate, 1)) {
            split.extend_from_slice(&wav_decode_packet(chunk, &mut split_states).unwrap());
        }
        assert_eq!(whole, split, "rate {rate:?}");
    }
}

// ---------------------------------------------------------------------------
// Registry path (`framing=wav` codec option)
// ---------------------------------------------------------------------------

fn wav_params(channels: u16, bits: &str, extra: &[(&str, &str)]) -> CodecParameters {
    let mut p = CodecParameters::audio(CodecId::new(CODEC_ID_G726));
    p.sample_rate = Some(8000);
    p.channels = Some(channels);
    p.options.insert("framing", "wav");
    p.options.insert("bits_per_sample", bits);
    for (k, v) in extra {
        p.options.insert(*k, *v);
    }
    p
}

fn registry() -> CodecRegistry {
    let mut reg = CodecRegistry::new();
    oxideav_adpcm::register_codecs(&mut reg);
    reg
}

/// Feed `bytes` to a registry decoder in `pkt_len`-byte packets and
/// collect the interleaved PCM.
fn registry_decode(p: &CodecParameters, bytes: &[u8], pkt_len: usize) -> Vec<i16> {
    let reg = registry();
    let mut dec = reg.first_decoder(p).expect("decoder factory");
    let tb = TimeBase::new(1, 8000);
    let mut out = Vec::new();
    for chunk in bytes.chunks(pkt_len.max(1)) {
        dec.send_packet(&Packet::new(0, tb, chunk.to_vec()))
            .expect("send_packet");
        if let Ok(Frame::Audio(af)) = dec.receive_frame() {
            for pair in af.data[0].chunks_exact(2) {
                out.push(i16::from_le_bytes([pair[0], pair[1]]));
            }
        }
    }
    out
}

#[test]
fn registry_wav_decode_matches_direct_api() {
    for &(rate, bits) in &[(Rate::R24, "3"), (Rate::R32, "4"), (Rate::R40, "5")] {
        for channels in 1u16..=2 {
            let n = 512 * channels as usize;
            let pcm = sine(n, 620.0, 11000.0, 0.4);
            let mut enc: Vec<State> = (0..channels).map(|_| State::new(rate)).collect();
            let bytes = wav_encode_packet(&pcm, &mut enc).unwrap();

            let mut direct: Vec<State> = (0..channels).map(|_| State::new(rate)).collect();
            let want = wav_decode_packet(&bytes, &mut direct).unwrap();

            // Odd packet split (7 bytes) exercises sub-block straddling.
            let got = registry_decode(&wav_params(channels, bits, &[]), &bytes, 7);
            assert_eq!(got, want, "rate {rate:?} ch {channels}");
        }
    }
}

#[test]
fn registry_wav_decode_strips_aux_across_packet_splits() {
    let rate = Rate::R24;
    let aux = 4usize;
    let block = wav_block_align(rate, 1, aux);
    let pcm = sine(3 * 128, 300.0, 13000.0, 0.0); // three full blocks
    let mut enc = [State::new(rate)];
    let payload = wav_encode_packet(&pcm, &mut enc).unwrap();
    assert_eq!(payload.len(), 3 * 48);

    // Interleave a 4-byte aux prefix ahead of every 48-byte block.
    let mut data = Vec::new();
    for chunk in payload.chunks(48) {
        data.extend_from_slice(&[0xEE; 4]);
        data.extend_from_slice(chunk);
    }
    assert_eq!(data.len(), 3 * block);

    let p = wav_params(1, "3", &[("aux_block_size", "4")]);
    // Whole-buffer decode and a 5-byte packet split (which lands
    // mid-prefix and mid-sub-block) must agree with the aux-free path.
    let mut direct = [State::new(rate)];
    let want = wav_decode_packet(&payload, &mut direct).unwrap();
    assert_eq!(registry_decode(&p, &data, data.len()), want);
    assert_eq!(registry_decode(&p, &data, 5), want);
}

#[test]
fn registry_wav_decoder_reset_reseeds_lanes_and_block_pos() {
    let rate = Rate::R40;
    let pcm = sine(2 * 256, 900.0, 10000.0, 0.0);
    let mut enc = [State::new(rate), State::new(rate)];
    let bytes = wav_encode_packet(&pcm, &mut enc).unwrap();

    let reg = registry();
    let p = wav_params(2, "5", &[]);
    let mut dec = reg.first_decoder(&p).expect("decoder factory");
    let tb = TimeBase::new(1, 8000);
    let run = |dec: &mut Box<dyn oxideav_core::Decoder>| -> Vec<u8> {
        dec.send_packet(&Packet::new(0, tb, bytes.clone())).unwrap();
        match dec.receive_frame() {
            Ok(Frame::Audio(af)) => af.data[0].clone(),
            other => panic!("expected audio frame, got {other:?}"),
        }
    };
    let first = run(&mut dec);
    dec.reset().unwrap();
    let second = run(&mut dec);
    assert_eq!(first, second, "reset must re-seed both lanes");
}

#[test]
fn registry_wav_option_validation() {
    let reg = registry();
    // 2-bit rate has no WAV tag.
    assert!(reg.first_decoder(&wav_params(1, "2", &[])).is_err());
    // The bit-cell grid fixes MSB-first packing.
    assert!(reg
        .first_decoder(&wav_params(1, "4", &[("bit_order", "lsb")]))
        .is_err());
    // …but an explicit msb (the grid's order) stays accepted.
    assert!(reg
        .first_decoder(&wav_params(1, "4", &[("bit_order", "msb")]))
        .is_ok());
    // Stereo needs the WAV framing; the raw telephony stream is mono.
    let mut raw_stereo = CodecParameters::audio(CodecId::new(CODEC_ID_G726));
    raw_stereo.sample_rate = Some(8000);
    raw_stereo.channels = Some(2);
    raw_stereo.options.insert("bits_per_sample", "4");
    assert!(reg.first_decoder(&raw_stereo).is_err());
    assert!(reg.first_decoder(&wav_params(2, "4", &[])).is_ok());
    // Three channels exceed the container's stereo interleave.
    assert!(reg.first_decoder(&wav_params(3, "4", &[])).is_err());
    // Unknown framing value.
    let mut bogus = wav_params(1, "4", &[]);
    bogus.options.insert("framing", "subblock");
    assert!(reg.first_decoder(&bogus).is_err());
    // aux_block_size demands framing=wav…
    let mut aux_raw = wav_params(1, "4", &[("aux_block_size", "4")]);
    aux_raw.options.insert("framing", "raw");
    assert!(reg.first_decoder(&aux_raw).is_err());
    // …and must be numeric.
    assert!(reg
        .first_decoder(&wav_params(1, "4", &[("aux_block_size", "some")]))
        .is_err());
    // framing is a G.726-only option.
    let mut ms = CodecParameters::audio(CodecId::new(oxideav_adpcm::CODEC_ID_MS));
    ms.sample_rate = Some(8000);
    ms.channels = Some(1);
    ms.options.insert("framing", "wav");
    assert!(reg.first_decoder(&ms).is_err());
}

/// Drive interleaved PCM through the registry encoder in
/// `frame_len`-frame chunks (interleaved samples = frames × channels)
/// and collect the emitted bytes including the flush tail.
fn registry_encode(p: &CodecParameters, pcm: &[i16], frame_len: usize) -> Vec<u8> {
    let reg = registry();
    let channels = p.channels.unwrap_or(1) as usize;
    let mut enc = reg.first_encoder(p).expect("encoder factory");
    let mut bytes = Vec::new();
    for chunk in pcm.chunks(frame_len * channels) {
        let mut data = Vec::with_capacity(chunk.len() * 2);
        for s in chunk {
            data.extend_from_slice(&s.to_le_bytes());
        }
        enc.send_frame(&Frame::Audio(oxideav_core::AudioFrame {
            samples: (chunk.len() / channels) as u32,
            pts: None,
            data: vec![data],
        }))
        .expect("send_frame");
        while let Ok(pkt) = enc.receive_packet() {
            bytes.extend_from_slice(&pkt.data);
        }
    }
    enc.flush().expect("flush");
    while let Ok(pkt) = enc.receive_packet() {
        bytes.extend_from_slice(&pkt.data);
    }
    bytes
}

#[test]
fn registry_wav_encoder_matches_direct_api() {
    // Whatever the frame chop, the registry encoder must emit the exact
    // byte stream of the direct wav_encode_packet API (sub-block
    // buffering is invisible on the wire).
    for &(rate, bits) in &[(Rate::R24, "3"), (Rate::R32, "4"), (Rate::R40, "5")] {
        for channels in 1u16..=2 {
            let frames = 512usize;
            let pcm = sine(frames * channels as usize, 730.0, 12000.0, 0.2);
            let mut direct: Vec<State> = (0..channels).map(|_| State::new(rate)).collect();
            let want = wav_encode_packet(&pcm, &mut direct).unwrap();
            for frame_len in [frames, 160, 7] {
                let got = registry_encode(&wav_params(channels, bits, &[]), &pcm, frame_len);
                assert_eq!(
                    got, want,
                    "rate {rate:?} ch {channels} frame_len {frame_len}"
                );
            }
        }
    }
}

#[test]
fn registry_wav_full_round_trip_with_flush_padding() {
    // A frame count that is NOT a multiple of 8 forces the flush path
    // to pad the last sub-block with silence; the decode must return
    // the padded frame count and track the input over the real frames.
    let frames = 1000usize; // 125 sub-blocks per lane
    let pcm = sine(frames, 350.0, 12000.0, 0.0);
    let p = wav_params(1, "4", &[]);
    let bytes = registry_encode(&p, &pcm, 96);
    let padded = frames.div_ceil(8) * 8;
    assert_eq!(bytes.len(), padded / 8 * wav_subblock_bytes(Rate::R32, 1));
    let out = registry_decode(&p, &bytes, 11);
    assert_eq!(out.len(), padded);
    assert!(correlation(&pcm, &out[..frames]) > 0.9);
}

#[test]
fn registry_wav_law_interface_round_trips() {
    // The G.711 log-PCM interface composes with the WAV framing: the
    // per-code codec path is unchanged, only the container transform
    // differs. Stereo + A-law at the 5-bit rate as the worst case.
    let frames = 1024usize;
    let left = sine(frames, 300.0, 12000.0, 0.0);
    let right = sine(frames, 800.0, 9000.0, 0.7);
    let mut pcm = Vec::with_capacity(2 * frames);
    for i in 0..frames {
        pcm.push(left[i]);
        pcm.push(right[i]);
    }
    let p = wav_params(2, "5", &[("law", "alaw")]);
    let bytes = registry_encode(&p, &pcm, 128);
    let out = registry_decode(&p, &bytes, 9);
    assert_eq!(out.len(), pcm.len());
    let out_l: Vec<i16> = out.iter().step_by(2).copied().collect();
    let out_r: Vec<i16> = out.iter().skip(1).step_by(2).copied().collect();
    assert!(correlation(&left, &out_l) > 0.9, "left lane");
    assert!(correlation(&right, &out_r) > 0.9, "right lane");
}

#[test]
fn registry_wav_encoder_option_validation() {
    let reg = registry();
    // Mirrors of the decoder factory gates.
    assert!(reg.first_encoder(&wav_params(1, "2", &[])).is_err());
    assert!(reg
        .first_encoder(&wav_params(1, "4", &[("bit_order", "lsb")]))
        .is_err());
    assert!(reg.first_encoder(&wav_params(3, "4", &[])).is_err());
    assert!(reg.first_encoder(&wav_params(2, "4", &[])).is_ok());
    // Raw framing stays mono-only on the encode side too.
    let mut raw_stereo = CodecParameters::audio(CodecId::new(CODEC_ID_G726));
    raw_stereo.sample_rate = Some(8000);
    raw_stereo.channels = Some(2);
    assert!(reg.first_encoder(&raw_stereo).is_err());
    // The encoder emits aux-free blocks: aux_block_size must be 0.
    assert!(reg
        .first_encoder(&wav_params(1, "4", &[("aux_block_size", "4")]))
        .is_err());
    assert!(reg
        .first_encoder(&wav_params(1, "4", &[("aux_block_size", "0")]))
        .is_ok());
}

#[test]
fn codec_entry_points_validate_states() {
    // Empty state set.
    assert!(wav_decode_packet(&[0u8; 3], &mut []).is_err());
    assert!(wav_encode_packet(&[0i16; 8], &mut []).is_err());
    // Rate disagreement between lanes.
    let mut mixed = [State::new(Rate::R24), State::new(Rate::R40)];
    assert!(wav_decode_packet(&[0u8; 8], &mut mixed).is_err());
    assert!(wav_encode_packet(&[0i16; 16], &mut mixed).is_err());
    // Unsupported rate at the codec level too.
    let mut r16 = [State::new(Rate::R16)];
    assert!(wav_decode_packet(&[0u8; 2], &mut r16).is_err());
    assert!(wav_encode_packet(&[0i16; 8], &mut r16).is_err());
    // A partial sub-block must not advance the encoder state: encode a
    // bad length, then a good one, and compare against a fresh state.
    let mut st = [State::new(Rate::R24)];
    let pcm = sine(64, 500.0, 9000.0, 0.0);
    assert!(wav_encode_packet(&pcm[..5], &mut st).is_err());
    let after_err = wav_encode_packet(&pcm, &mut st).unwrap();
    let mut fresh = [State::new(Rate::R24)];
    let clean = wav_encode_packet(&pcm, &mut fresh).unwrap();
    assert_eq!(after_err, clean, "failed encode leaked state changes");
}
