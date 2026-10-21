//! adpcm_ima_qt through the registry: a block whose preamble agrees with
//! the channel's running state continues from the full-precision
//! predictor, as macOS's IMA4 decoder does.

use oxideav_adpcm::{CODEC_ID_IMA_QT, register_codecs};
use oxideav_core::{CodecId, CodecParameters, CodecRegistry, Frame, Packet, TimeBase};

fn decode(packets: &[Vec<u8>], channels: u16) -> Vec<i16> {
    let mut reg = CodecRegistry::new();
    register_codecs(&mut reg);
    let mut params = CodecParameters::audio(CodecId::new(CODEC_ID_IMA_QT));
    params.sample_rate = Some(44_100);
    params.channels = Some(channels);
    let mut dec = reg.first_decoder(&params).unwrap();
    let mut out = Vec::new();
    for p in packets {
        dec.send_packet(&Packet::new(0, TimeBase::new(1, 44_100), p.clone()))
            .unwrap();
        let Frame::Audio(af) = dec.receive_frame().unwrap() else {
            panic!("audio")
        };
        out.extend(
            af.data[0]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&c| i16::from_le_bytes(c)),
        );
    }
    out
}

#[test]
fn second_packet_continues_the_first() {
    // Packet 1 starts from predictor 0, step index 0; each code-1 nibble
    // adds 1, so it ends at 64 with the index still 0. Packet 2's
    // preamble (top 9 bits 0, index 0) agrees with that state, and its
    // zero nibbles hold the predictor: macOS continues at 64, where a
    // reseed from the preamble would restart at 0.
    let mut first = vec![0x11u8; 34];
    first[0] = 0;
    first[1] = 0;
    let second = vec![0u8; 34];
    let both = decode(&[first, second.clone()], 1);
    assert_eq!(both[63], 64);
    assert!(both[64..].iter().all(|&v| v == 64), "{:?}", &both[64..72]);
    // A fresh decoder seeds the same packet from its preamble.
    assert!(decode(&[second], 1).iter().all(|&v| v == 0));
}
