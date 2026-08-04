#![no_main]

//! Coverage-guided fuzz harness for the **G.726** decoder
//! (`oxideav_adpcm::g726`) — all four rates, both bit orders.
//!
//! G.726 is stream-oriented at the *bit* level: the decoder carries a
//! partial code word across packet boundaries at the 3- and 5-bit
//! rates. The harness therefore checks a real correctness property on
//! top of never-panic: decoding the payload as ONE packet and as TWO
//! packets split at a fuzz-chosen offset must produce identical PCM —
//! the `BitUnpacker` residue plus the codec state machine make the
//! stream packetization-invariant.
//!
//! Byte 0 selects (rate, order); byte 1 the split point; the rest is
//! the packed payload. Every byte pattern is a valid G.726 stream (the
//! codec has no syntax to violate), so the assertions are exact.

use libfuzzer_sys::fuzz_target;
use oxideav_adpcm::g726::{
    decode_packet, wav_block_align, wav_decode_packet, wav_strip_aux, wav_subblock_bytes, BitOrder,
    BitUnpacker, Law, Rate, State,
};

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let rate = match data[0] & 3 {
        0 => Rate::R16,
        1 => Rate::R24,
        2 => Rate::R32,
        _ => Rate::R40,
    };
    let order = if data[0] & 4 == 0 {
        BitOrder::MsbFirst
    } else {
        BitOrder::LsbFirst
    };
    let payload = &data[2..];
    let split = (data[1] as usize) % (payload.len() + 1);

    // One-shot decode.
    let mut st = State::new(rate);
    let mut un = BitUnpacker::new(order);
    let mut whole = Vec::new();
    decode_packet(payload, &mut st, &mut un, &mut whole);
    // Every full code in the payload produces exactly one sample.
    assert_eq!(
        whole.len(),
        payload.len() * 8 / rate.bits() as usize,
        "sample count != whole codes in payload"
    );

    // Split decode across a packet boundary (possibly mid-code).
    let mut st2 = State::new(rate);
    let mut un2 = BitUnpacker::new(order);
    let mut split_out = Vec::new();
    decode_packet(&payload[..split], &mut st2, &mut un2, &mut split_out);
    decode_packet(&payload[split..], &mut st2, &mut un2, &mut split_out);
    assert_eq!(whole, split_out, "packet split changed the decode");

    // The G.711 log-PCM output chain (§4.2.8 COMPRESS + SYNC) has no
    // partial domain either: run the same code stream through the law
    // decoder under the law picked by the selector byte. State updates
    // are law-independent, so this also cross-checks that the law path
    // consumes exactly one sample per code.
    let law = if data[0] & 8 == 0 {
        Law::ALaw
    } else {
        Law::ULaw
    };
    let mut st3 = State::new(rate);
    let mut un3 = BitUnpacker::new(order);
    let mut codes = Vec::new();
    un3.feed(payload, rate.bits(), &mut codes);
    let law_out: Vec<u8> = codes.iter().map(|&c| st3.decode_law(c, law)).collect();
    assert_eq!(law_out.len(), whole.len(), "law path sample count diverged");

    // --- G.723/G.721-in-WAV sub-block framing leg -----------------------
    // The WAV container carries only the 3-/4-/5-bit rates; fold R16
    // onto R24 so every input exercises the leg. Channel count and the
    // per-block aux prefix come from the selector bytes.
    let wav_rate = if rate == Rate::R16 { Rate::R24 } else { rate };
    let channels = 1 + ((data[0] >> 4) & 1) as usize;
    let aux = (data[1] & 7) as usize;

    // Aux stripping never panics and never grows the payload.
    let block = wav_block_align(wav_rate, channels as u16, aux);
    if let Ok(payload_only) = wav_strip_aux(payload, block, aux) {
        assert!(payload_only.len() <= payload.len());

        // Decode a whole-sub-block prefix; split at a sub-block
        // boundary must be state-transparent.
        let sub = wav_subblock_bytes(wav_rate, channels as u16);
        let whole_subs = payload_only.len() / sub * sub;
        let subs = &payload_only[..whole_subs];
        let mut states: Vec<State> = (0..channels).map(|_| State::new(wav_rate)).collect();
        let one = wav_decode_packet(subs, &mut states).expect("whole sub-blocks decode");
        assert_eq!(
            one.len(),
            whole_subs / sub * 8 * channels,
            "wav framing sample count drifted"
        );

        let cut = (data[1] as usize % (whole_subs / sub + 1)) * sub;
        let mut states2: Vec<State> = (0..channels).map(|_| State::new(wav_rate)).collect();
        let mut two = wav_decode_packet(&subs[..cut], &mut states2).unwrap();
        two.extend_from_slice(&wav_decode_packet(&subs[cut..], &mut states2).unwrap());
        assert_eq!(one, two, "sub-block split changed the wav decode");

        // A ragged tail (not a whole sub-block) must error, not panic.
        if payload_only.len() != whole_subs {
            let mut states3: Vec<State> = (0..channels).map(|_| State::new(wav_rate)).collect();
            assert!(wav_decode_packet(&payload_only, &mut states3).is_err());
        }
    }
});
