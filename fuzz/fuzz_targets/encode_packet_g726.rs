#![no_main]

//! Coverage-guided fuzz harness for the **G.726** encoder
//! (`oxideav_adpcm::g726`) — all four rates, both bit orders.
//!
//! Properties checked beyond never-panic:
//!
//! - The packed stream length is exactly `ceil(n_samples · bits / 8)`
//!   after the flush (the packer never gains or drops bits).
//! - Frame splits are wire-invariant: encoding the same PCM in one
//!   call or in two chunks yields byte-identical output (the packer
//!   carries sub-byte residue across calls).
//! - The stream round-trips: a fresh decoder consumes the bytes and
//!   emits at least as many samples as were encoded (flush padding may
//!   add trailing codes), and re-decoding after a split produces the
//!   identical reconstruction.

use libfuzzer_sys::fuzz_target;
use oxideav_adpcm::g726::{
    decode_packet, encode_packet, wav_decode_packet, wav_encode_packet, wav_subblock_bytes,
    BitOrder, BitPacker, BitUnpacker, Rate, State,
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
    let split_sel = data[1] as usize;
    let pcm: Vec<i16> = data[2..]
        .chunks(2)
        .map(|c| {
            let lo = c[0] as u16;
            let hi = *c.get(1).unwrap_or(&0) as u16;
            (lo | (hi << 8)) as i16
        })
        .collect();
    if pcm.is_empty() {
        return;
    }

    // One-shot encode.
    let mut st = State::new(rate);
    let mut pk = BitPacker::new(order);
    let mut bytes = encode_packet(&pcm, &mut st, &mut pk);
    pk.flush(&mut bytes);
    assert_eq!(
        bytes.len(),
        (pcm.len() * rate.bits() as usize).div_ceil(8),
        "packed length drifted from ceil(n*bits/8)"
    );

    // Split encode must be byte-identical.
    let split = split_sel % (pcm.len() + 1);
    let mut st2 = State::new(rate);
    let mut pk2 = BitPacker::new(order);
    let mut bytes2 = encode_packet(&pcm[..split], &mut st2, &mut pk2);
    bytes2.extend_from_slice(&encode_packet(&pcm[split..], &mut st2, &mut pk2));
    pk2.flush(&mut bytes2);
    assert_eq!(bytes, bytes2, "frame split changed the wire bytes");

    // Round-trip decode: sample count covers the input (plus padding).
    let mut dst = State::new(rate);
    let mut un = BitUnpacker::new(order);
    let mut decoded = Vec::new();
    decode_packet(&bytes, &mut dst, &mut un, &mut decoded);
    assert!(
        decoded.len() >= pcm.len(),
        "decode produced fewer samples than encoded"
    );

    // --- G.723/G.721-in-WAV sub-block framing leg -----------------------
    // Encode the same PCM (truncated to whole 8-sample-per-channel
    // sub-blocks) through the WAV layer: length is exact, a frame split
    // at a sub-block boundary is wire-invariant, and the bytes decode
    // back to the same sample count.
    let wav_rate = if rate == Rate::R16 { Rate::R24 } else { rate };
    let channels = 1 + ((data[0] >> 4) & 1) as usize;
    let group = 8 * channels;
    let whole = pcm.len() / group * group;
    if whole == 0 {
        return;
    }
    let wav_pcm = &pcm[..whole];
    let mut enc: Vec<State> = (0..channels).map(|_| State::new(wav_rate)).collect();
    let bytes_w = wav_encode_packet(wav_pcm, &mut enc).expect("whole sub-blocks encode");
    let sub = wav_subblock_bytes(wav_rate, channels as u16);
    assert_eq!(bytes_w.len(), whole / group * sub, "wav packed length");

    let cut = (split_sel % (whole / group + 1)) * group;
    let mut enc2: Vec<State> = (0..channels).map(|_| State::new(wav_rate)).collect();
    let mut bytes_split = wav_encode_packet(&wav_pcm[..cut], &mut enc2).unwrap();
    bytes_split.extend_from_slice(&wav_encode_packet(&wav_pcm[cut..], &mut enc2).unwrap());
    assert_eq!(
        bytes_w, bytes_split,
        "sub-block frame split changed the wire"
    );

    let mut dec: Vec<State> = (0..channels).map(|_| State::new(wav_rate)).collect();
    let back = wav_decode_packet(&bytes_w, &mut dec).unwrap();
    assert_eq!(back.len(), whole, "wav round trip lost samples");
});
