//! Registry-path integration coverage for `adpcm_g726` — the ITU-T
//! G.726 narrowband ADPCM variant (Rec. G.726, 12/1990).
//!
//! Exercises the `CodecRegistry` factories end-to-end: option parsing
//! (`bits_per_sample` rate select, `bit_order` packing select), the
//! encode → decode round trip at every rate under both bit orders, the
//! bit-level packet-boundary continuity that distinguishes G.726 from
//! the nibble-aligned variants, and the factory reject paths.

use oxideav_adpcm::{g726, CODEC_ID_G726};
use oxideav_core::{
    CodecId, CodecParameters, CodecRegistry, CodecTag, Decoder, Frame, Packet, ProbeContext,
    TimeBase,
};

fn registry() -> CodecRegistry {
    let mut reg = CodecRegistry::new();
    oxideav_adpcm::register_codecs(&mut reg);
    reg
}

fn params(options: &[(&str, &str)]) -> CodecParameters {
    let mut p = CodecParameters::audio(CodecId::new(CODEC_ID_G726));
    p.sample_rate = Some(8000);
    p.channels = Some(1);
    for (k, v) in options {
        p.options.insert(k.to_string(), v.to_string());
    }
    p
}

fn sine_pcm(n: usize, hz: f64, amp: f64) -> Vec<i16> {
    (0..n)
        .map(|k| (amp * (2.0 * std::f64::consts::PI * hz * k as f64 / 8000.0).sin()) as i16)
        .collect()
}

fn snr_db(reference: &[i16], decoded: &[i16]) -> f64 {
    let mut sig = 0f64;
    let mut err = 0f64;
    for (r, d) in reference.iter().zip(decoded) {
        sig += (*r as f64) * (*r as f64);
        let e = *r as f64 - *d as f64;
        err += e * e;
    }
    10.0 * (sig / err.max(1e-9)).log10()
}

/// Drive `pcm` through the registry encoder in `frame_len`-sample
/// frames, collect every emitted packet (including the flush tail),
/// then decode the byte stream through the registry decoder in
/// `pkt_len`-byte packets. Returns the decoded PCM.
fn round_trip(
    opts: &[(&str, &str)],
    pcm: &[i16],
    frame_len: usize,
    pkt_len: usize,
) -> (Vec<u8>, Vec<i16>) {
    let reg = registry();
    let p = params(opts);
    let mut enc = reg.first_encoder(&p).expect("encoder factory");
    let tb = TimeBase::new(1, 8000);
    let mut bytes = Vec::new();
    for chunk in pcm.chunks(frame_len) {
        let mut data = Vec::with_capacity(chunk.len() * 2);
        for s in chunk {
            data.extend_from_slice(&s.to_le_bytes());
        }
        enc.send_frame(&Frame::Audio(oxideav_core::AudioFrame {
            samples: chunk.len() as u32,
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

    let mut dec = reg.first_decoder(&p).expect("decoder factory");
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
    (bytes, out)
}

#[test]
fn registry_round_trip_all_rates_both_orders() {
    let pcm = sine_pcm(4000, 700.0, 9000.0);
    for bits in ["2", "3", "4", "5"] {
        for order in ["msb", "lsb"] {
            let opts = [("bits_per_sample", bits), ("bit_order", order)];
            let (bytes, decoded) = round_trip(&opts, &pcm, 160, 33);
            // Compressed size: n·bits/8, byte-rounded (flush pads).
            let bits_n: usize = bits.parse().unwrap();
            assert_eq!(
                bytes.len(),
                (pcm.len() * bits_n).div_ceil(8),
                "bits={bits} order={order}: packed length"
            );
            // Flush padding may append trailing zero codes.
            assert!(decoded.len() >= pcm.len(), "bits={bits} order={order}");
            let snr = snr_db(&pcm[500..], &decoded[500..pcm.len()]);
            let floor = match bits_n {
                2 => 4.0,
                3 => 11.0,
                4 => 17.0,
                _ => 23.0,
            };
            assert!(
                snr > floor,
                "bits={bits} order={order}: SNR {snr:.1} dB below {floor} dB"
            );
        }
    }
}

#[test]
fn registry_decode_is_packetization_invariant() {
    // The decoder carries codec state *and* partial code bits across
    // packets: any packet split of the same byte stream yields the
    // identical PCM. 3- and 5-bit rates make codes straddle bytes.
    let pcm = sine_pcm(1601, 433.0, 7000.0);
    for bits in ["2", "3", "4", "5"] {
        for order in ["msb", "lsb"] {
            let opts = [("bits_per_sample", bits), ("bit_order", order)];
            let (_, reference) = round_trip(&opts, &pcm, usize::MAX, usize::MAX);
            for pkt_len in [1usize, 3, 7, 16] {
                let (_, got) = round_trip(&opts, &pcm, 97, pkt_len);
                assert_eq!(
                    got, reference,
                    "bits={bits} order={order} pkt_len={pkt_len}: split decode diverged"
                );
            }
        }
    }
}

#[test]
fn registry_default_options_are_32kbit_msb() {
    // No options ⇒ 4-bit 32 kbit/s MSB-first; must byte-match an
    // explicit ("4", "msb") stream.
    let pcm = sine_pcm(800, 500.0, 6000.0);
    let (bytes_default, _) = round_trip(&[], &pcm, 200, 40);
    let (bytes_explicit, _) = round_trip(
        &[("bits_per_sample", "4"), ("bit_order", "msb")],
        &pcm,
        200,
        40,
    );
    assert_eq!(bytes_default, bytes_explicit);
    // And direct-API equivalence: same codes, same packing.
    let mut st = g726::State::new(g726::Rate::R32);
    let mut packer = g726::BitPacker::new(g726::BitOrder::MsbFirst);
    let mut direct = g726::encode_packet(&pcm, &mut st, &mut packer);
    packer.flush(&mut direct);
    assert_eq!(bytes_default, direct);
}

#[test]
fn factories_reject_bad_channel_counts_and_options() {
    let reg = registry();
    // Stereo is rejected on both paths (G.726 is single-channel).
    let mut p = params(&[]);
    p.channels = Some(2);
    assert!(reg.first_decoder(&p).is_err(), "decoder accepted stereo");
    assert!(reg.first_encoder(&p).is_err(), "encoder accepted stereo");
    // Out-of-range code widths.
    for bad in ["1", "6", "8", "0", "x"] {
        let p = params(&[("bits_per_sample", bad)]);
        assert!(
            reg.first_decoder(&p).is_err(),
            "decoder accepted bits_per_sample={bad}"
        );
        assert!(
            reg.first_encoder(&p).is_err(),
            "encoder accepted bits_per_sample={bad}"
        );
    }
    // Unknown bit orders.
    let p = params(&[("bit_order", "be")]);
    assert!(
        reg.first_decoder(&p).is_err(),
        "decoder accepted bit_order=be"
    );
    assert!(
        reg.first_encoder(&p).is_err(),
        "encoder accepted bit_order=be"
    );
    // The option is G.726-specific: another variant must reject it.
    let mut p = CodecParameters::audio(CodecId::new(oxideav_adpcm::CODEC_ID_YAMAHA));
    p.sample_rate = Some(8000);
    p.channels = Some(1);
    p.options.insert("bit_order", "msb");
    assert!(
        reg.first_decoder(&p).is_err(),
        "adpcm_yamaha decoder accepted a bit_order option"
    );
}

#[test]
fn wave_tag_0x0040_resolves_to_g726() {
    // WAVE_FORMAT_G721_ADPCM routes a WAV demuxer to the G.726 decoder
    // (32 kbit/s 4-bit default — G.726 consolidates G.721).
    let reg = registry();
    let tag = CodecTag::wave_format(0x0040);
    let id = reg
        .resolve_tag_ref(&ProbeContext::new(&tag))
        .expect("tag 0x0040 resolves");
    assert_eq!(id.as_str(), CODEC_ID_G726);
}

#[test]
fn decoder_reset_reseeds_codec_state_and_bit_buffer() {
    // After reset, replaying the same packets must reproduce the same
    // PCM — state and residual bits are both cleared.
    let pcm = sine_pcm(500, 650.0, 8000.0);
    let opts = [("bits_per_sample", "5"), ("bit_order", "msb")];
    let (bytes, _) = round_trip(&opts, &pcm, usize::MAX, usize::MAX);
    let reg = registry();
    let p = params(&opts);
    let mut dec = reg.first_decoder(&p).expect("decoder");
    let tb = TimeBase::new(1, 8000);
    let run = |dec: &mut Box<dyn Decoder>| -> Vec<i16> {
        let mut out = Vec::new();
        // Deliberately odd split so residual bits are live mid-stream.
        for chunk in bytes.chunks(7) {
            dec.send_packet(&Packet::new(0, tb, chunk.to_vec()))
                .unwrap();
            if let Ok(Frame::Audio(af)) = dec.receive_frame() {
                for pair in af.data[0].chunks_exact(2) {
                    out.push(i16::from_le_bytes([pair[0], pair[1]]));
                }
            }
        }
        out
    };
    let first = run(&mut dec);
    dec.reset().expect("reset");
    let second = run(&mut dec);
    assert_eq!(first, second, "reset did not fully re-seed the decoder");
}

// ----- G.711 log-PCM (`law`) interface --------------------------------

/// Load a conformance fixture (16-bit LE words, payload right-justified
/// octets) from `tests/fixtures/g726/`.
fn law_fixture(name: &str) -> Vec<u8> {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests");
    p.push("fixtures");
    p.push("g726");
    p.push(name);
    let raw = std::fs::read(&p).unwrap_or_else(|e| panic!("reading {}: {e}", p.display()));
    raw.chunks_exact(2)
        .map(|w| {
            assert_eq!(w[1], 0, "{name}: non-octet word");
            w[0]
        })
        .collect()
}

/// The registry decoder with `law` set reproduces the official ITU
/// Appendix II reset decoder vectors end to end: the per-word codes are
/// packed into the wire format, streamed through `send_packet` /
/// `receive_frame`, and the emitted 16-bit PCM must equal the expanded
/// reference law words exactly, at every rate under both laws.
#[test]
fn registry_law_decode_reproduces_conformance_vectors() {
    let reg = registry();
    let tb = TimeBase::new(1, 8000);
    for (bits, r) in [("2", "16"), ("3", "24"), ("4", "32"), ("5", "40")] {
        for (law, l) in [(g726::Law::ALaw, "a"), (g726::Law::ULaw, "m")] {
            let codes = law_fixture(&format!("rn{r}f{l}.i"));
            let want: Vec<i16> = law_fixture(&format!("rn{r}f{l}.o"))
                .iter()
                .map(|&sp| g726::expand_i16(sp, law))
                .collect();
            let rate = g726::Rate::from_bits(bits.parse().unwrap()).unwrap();
            let bytes = g726::pack_codes(&codes, rate, g726::BitOrder::MsbFirst);
            let lawname = if l == "a" { "alaw" } else { "ulaw" };
            let p = params(&[("bits_per_sample", bits), ("law", lawname)]);
            let mut dec = reg.first_decoder(&p).expect("law decoder");
            let mut got = Vec::new();
            for chunk in bytes.chunks(509) {
                dec.send_packet(&Packet::new(0, tb, chunk.to_vec()))
                    .unwrap();
                if let Ok(Frame::Audio(af)) = dec.receive_frame() {
                    for pair in af.data[0].chunks_exact(2) {
                        got.push(i16::from_le_bytes([pair[0], pair[1]]));
                    }
                }
            }
            assert_eq!(
                &got[..want.len().min(got.len())],
                &want[..want.len().min(got.len())],
                "rn{r}f{l}: registry law decode diverged"
            );
            assert!(got.len() >= want.len(), "rn{r}f{l}: short decode");
        }
    }
}

/// Law-interface encode → decode round trip: the output PCM sits on
/// the G.711 lattice (compress → expand idempotent on every emitted
/// sample) and still clears the linear path's SNR floor.
#[test]
fn registry_law_round_trip_stays_on_law_lattice() {
    let pcm = sine_pcm(4000, 700.0, 9000.0);
    for lawname in ["alaw", "ulaw"] {
        let law = if lawname == "alaw" {
            g726::Law::ALaw
        } else {
            g726::Law::ULaw
        };
        let opts = [("bits_per_sample", "5"), ("law", lawname)];
        let (_, decoded) = round_trip(&opts, &pcm, 160, 33);
        assert!(decoded.len() >= pcm.len(), "{lawname}: short decode");
        for (k, &s) in decoded.iter().enumerate() {
            assert_eq!(
                g726::expand_i16(g726::compress_i16(s, law), law),
                s,
                "{lawname}: sample {k} not on the law lattice"
            );
        }
        let snr = snr_db(&pcm[500..], &decoded[500..pcm.len()]);
        assert!(snr > 22.0, "{lawname}: SNR {snr:.1} dB below 22 dB");
    }
}

/// `law` option validation: unknown values are rejected on both
/// factories, `linear` is accepted as the explicit default, and the
/// option is G.726-specific.
#[test]
fn law_option_validation() {
    let reg = registry();
    for bad in ["a-law", "mu", "pcm", ""] {
        let p = params(&[("law", bad)]);
        assert!(reg.first_decoder(&p).is_err(), "decoder accepted law={bad}");
        assert!(reg.first_encoder(&p).is_err(), "encoder accepted law={bad}");
    }
    let p = params(&[("law", "linear")]);
    assert!(reg.first_decoder(&p).is_ok(), "decoder rejected law=linear");
    assert!(reg.first_encoder(&p).is_ok(), "encoder rejected law=linear");
    // The option is G.726-specific: another variant must reject it.
    let mut p = CodecParameters::audio(CodecId::new(oxideav_adpcm::CODEC_ID_YAMAHA));
    p.sample_rate = Some(8000);
    p.channels = Some(1);
    p.options.insert("law", "alaw");
    assert!(
        reg.first_decoder(&p).is_err(),
        "yamaha decoder accepted the G.726 law option"
    );
}
