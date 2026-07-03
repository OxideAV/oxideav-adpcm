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
use oxideav_adpcm::g726::{decode_packet, BitOrder, BitUnpacker, Rate, State};

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
});
