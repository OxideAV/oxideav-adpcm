//! ITU-T G.726 conformance — the official Appendix II test sequences,
//! byte-exact, both directions, all four rates, both companding laws.
//!
//! Fixtures (`tests/fixtures/g726/`) are the black-box input → output
//! digital test sequences of Recommendation G.726 Appendix II ("Test
//! sequences"), staged from `docs/audio/adpcm/g726/conformance/`
//! (16-bit little-endian words, payload right-justified: G.711 PCM
//! code words in `.o` / `nrm.*` / `ovr.*` / `pcm_init.*`, ADPCM code
//! words in `.i` / `i<rate>` / `i_ini_*`). Only the conformance
//! *data* is used — no reference implementation of any kind.
//!
//! Naming: `<t><k><rate>f<leg>.<ext>` — `t` = `r`eset / `h`oming,
//! `k` = `n`ormal / o`v`erload / `i` codeword-sweep, `leg` = payload
//! law of the exercised path (`a` A-law, `m` µ-law, `x` cross A→µ,
//! `c` cross µ→A), `.i` = ADPCM codes, `.o` = decoded log-PCM.
//!
//! These vectors arbitrate every sub-LSB latitude of the §4.2
//! arithmetic (e.g. FMULT truncate-vs-round): the test design
//! guarantees any deviation from the bit-exact description flips at
//! least one output word, so each assertion below is a proof of exact
//! conformance, not a similarity metric.

use std::fs;
use std::path::PathBuf;

use oxideav_adpcm::g726::{Law, Rate, State};

/// Raw 16-bit LE words (the initialization files carry an annotation
/// trailer with non-payload words, so no high-byte check here).
fn fixture16(name: &str) -> Vec<u16> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests");
    p.push("fixtures");
    p.push("g726");
    p.push(name);
    let raw = fs::read(&p).unwrap_or_else(|e| panic!("reading {}: {e}", p.display()));
    assert_eq!(raw.len() % 2, 0, "{name}: odd byte count");
    raw.chunks_exact(2)
        .map(|w| u16::from_le_bytes([w[0], w[1]]))
        .collect()
}

/// Payload words — every word must be a right-justified octet.
fn fixture(name: &str) -> Vec<u8> {
    fixture16(name)
        .into_iter()
        .map(|w| {
            assert!(w <= 0xFF, "{name}: non-octet 16-bit word {w:#06x}");
            w as u8
        })
        .collect()
}

const RATES: [(Rate, &str); 4] = [
    (Rate::R16, "16"),
    (Rate::R24, "24"),
    (Rate::R32, "32"),
    (Rate::R40, "40"),
];

/// Decode `codes` (one right-justified ADPCM word per byte) through a
/// fresh (reset, Table 6 column 4) decoder with `law` output.
fn decode_reset(codes: &[u8], rate: Rate, law: Law) -> Vec<u8> {
    let mut st = State::new(rate);
    codes.iter().map(|&c| st.decode_law(c, law)).collect()
}

/// Encode `pcm` (one G.711 code word per byte) through a fresh
/// encoder.
fn encode_reset(pcm: &[u8], rate: Rate, law: Law) -> Vec<u8> {
    let mut st = State::new(rate);
    pcm.iter().map(|&s| st.encode_law(s, law)).collect()
}

fn assert_words_eq(got: &[u8], want: &[u8], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: length");
    if let Some(k) = (0..got.len()).find(|&k| got[k] != want[k]) {
        panic!(
            "{what}: first mismatch at word {k}: got {:#04x}, want {:#04x} \
             ({} of {} words differ)",
            got[k],
            want[k],
            got.iter().zip(want).filter(|(a, b)| a != b).count(),
            got.len()
        );
    }
}

// ---------------------------------------------------------------------------
// Reset test sequences (Table 6 column 4 initial state)
// ---------------------------------------------------------------------------

#[test]
fn reset_encoder_normal_and_overload_all_rates_both_laws() {
    for (rate, r) in RATES {
        for (k, src_a, src_m) in [("n", "nrm.a", "nrm.m"), ("v", "ovr.a", "ovr.m")] {
            let got = encode_reset(&fixture(src_a), rate, Law::ALaw);
            assert_words_eq(
                &got,
                &fixture(&format!("r{k}{r}fa.i")),
                &format!("r{k}{r}fa.i"),
            );
            let got = encode_reset(&fixture(src_m), rate, Law::ULaw);
            assert_words_eq(
                &got,
                &fixture(&format!("r{k}{r}fm.i")),
                &format!("r{k}{r}fm.i"),
            );
        }
    }
}

#[test]
fn reset_decoder_same_law_legs_all_rates() {
    for (rate, r) in RATES {
        for k in ["n", "v"] {
            let ia = fixture(&format!("r{k}{r}fa.i"));
            let im = fixture(&format!("r{k}{r}fm.i"));
            let got = decode_reset(&ia, rate, Law::ALaw);
            assert_words_eq(
                &got,
                &fixture(&format!("r{k}{r}fa.o")),
                &format!("r{k}{r}fa.o"),
            );
            let got = decode_reset(&im, rate, Law::ULaw);
            assert_words_eq(
                &got,
                &fixture(&format!("r{k}{r}fm.o")),
                &format!("r{k}{r}fm.o"),
            );
        }
    }
}

#[test]
fn reset_decoder_cross_law_legs_all_rates() {
    for (rate, r) in RATES {
        for k in ["n", "v"] {
            // fx: A-law-path ADPCM decoded with µ-law output.
            let ia = fixture(&format!("r{k}{r}fa.i"));
            let got = decode_reset(&ia, rate, Law::ULaw);
            assert_words_eq(
                &got,
                &fixture(&format!("r{k}{r}fx.o")),
                &format!("r{k}{r}fx.o"),
            );
            // fc: µ-law-path ADPCM decoded with A-law output.
            let im = fixture(&format!("r{k}{r}fm.i"));
            let got = decode_reset(&im, rate, Law::ALaw);
            assert_words_eq(
                &got,
                &fixture(&format!("r{k}{r}fc.o")),
                &format!("r{k}{r}fc.o"),
            );
        }
    }
}

#[test]
fn reset_decoder_codeword_sweep_all_rates_both_laws() {
    for (rate, r) in RATES {
        let sweep = fixture(&format!("i{r}"));
        let got = decode_reset(&sweep, rate, Law::ALaw);
        assert_words_eq(&got, &fixture(&format!("ri{r}fa.o")), &format!("ri{r}fa.o"));
        let got = decode_reset(&sweep, rate, Law::ULaw);
        assert_words_eq(&got, &fixture(&format!("ri{r}fm.o")), &format!("ri{r}fm.o"));
    }
}

// ---------------------------------------------------------------------------
// Homing (no-reset) test sequences
// ---------------------------------------------------------------------------
//
// The `h*` vectors exercise implementations without a reset input: the
// codec is first driven ("homed") to a known state by the Appendix II
// initialization sequences — `pcm_init.<law>` PCM words for the
// encoder, `i_ini_<rate>.<law>` ADPCM words for the decoder — then the
// test sequence is processed. The homed state differs from the Table 6
// reset column, so the `h*` references differ from their `r*` twins.
//
// The binary initialization files are 3584 words: a 3496-word homing
// payload followed by an 88-word annotation trailer inherited from the
// historical hex-ASCII distribution (zero padding, a 0x2010 marker and
// the repeated homing word spelled out as ASCII hex). Only the payload
// is signal; `init_payload` pins the layout before slicing it off.

const INIT_WORDS: usize = 3584;
const INIT_PAYLOAD_WORDS: usize = 3496;

fn init_payload(name: &str) -> Vec<u8> {
    let all = fixture16(name);
    assert_eq!(all.len(), INIT_WORDS, "{name}: unexpected length");
    assert!(
        all[INIT_PAYLOAD_WORDS..INIT_PAYLOAD_WORDS + 32]
            .iter()
            .all(|&w| w == 0),
        "{name}: annotation trailer not where expected"
    );
    all[..INIT_PAYLOAD_WORDS]
        .iter()
        .map(|&w| {
            assert!(w <= 0xFF, "{name}: non-octet payload word {w:#06x}");
            w as u8
        })
        .collect()
}

/// Fresh decoder driven to the homed state by `i_ini_<rate>.<law>`.
fn homed_decoder(rate: Rate, r: &str, law: Law) -> State {
    let l = match law {
        Law::ALaw => "a",
        Law::ULaw => "m",
    };
    let mut st = State::new(rate);
    for &c in &init_payload(&format!("i_ini_{r}.{l}")) {
        st.decode_law(c, law);
    }
    st
}

/// Fresh encoder driven to the homed state by `pcm_init.<law>`.
fn homed_encoder(rate: Rate, law: Law) -> State {
    let l = match law {
        Law::ALaw => "a",
        Law::ULaw => "m",
    };
    let mut st = State::new(rate);
    for &s in &init_payload(&format!("pcm_init.{l}")) {
        st.encode_law(s, law);
    }
    st
}

#[test]
fn homing_encoder_normal_and_overload_all_rates_both_laws() {
    for (rate, r) in RATES {
        for (k, src_a, src_m) in [("n", "nrm.a", "nrm.m"), ("v", "ovr.a", "ovr.m")] {
            let mut st = homed_encoder(rate, Law::ALaw);
            let got: Vec<u8> = fixture(src_a)
                .iter()
                .map(|&s| st.encode_law(s, Law::ALaw))
                .collect();
            assert_words_eq(
                &got,
                &fixture(&format!("h{k}{r}fa.i")),
                &format!("h{k}{r}fa.i"),
            );
            let mut st = homed_encoder(rate, Law::ULaw);
            let got: Vec<u8> = fixture(src_m)
                .iter()
                .map(|&s| st.encode_law(s, Law::ULaw))
                .collect();
            assert_words_eq(
                &got,
                &fixture(&format!("h{k}{r}fm.i")),
                &format!("h{k}{r}fm.i"),
            );
        }
    }
}

#[test]
fn homing_decoder_same_law_legs_all_rates() {
    for (rate, r) in RATES {
        for k in ["n", "v"] {
            let ia = fixture(&format!("h{k}{r}fa.i"));
            let im = fixture(&format!("h{k}{r}fm.i"));
            let mut st = homed_decoder(rate, r, Law::ALaw);
            let got: Vec<u8> = ia.iter().map(|&c| st.decode_law(c, Law::ALaw)).collect();
            assert_words_eq(
                &got,
                &fixture(&format!("h{k}{r}fa.o")),
                &format!("h{k}{r}fa.o"),
            );
            let mut st = homed_decoder(rate, r, Law::ULaw);
            let got: Vec<u8> = im.iter().map(|&c| st.decode_law(c, Law::ULaw)).collect();
            assert_words_eq(
                &got,
                &fixture(&format!("h{k}{r}fm.o")),
                &format!("h{k}{r}fm.o"),
            );
        }
    }
}

#[test]
fn homing_decoder_cross_law_legs_all_rates() {
    // The decoder is homed with the initialization sequence of its
    // *output* law (the state trajectory depends only on the received
    // codes, but the two i_ini code streams differ, so the homed
    // states differ per law).
    for (rate, r) in RATES {
        for k in ["n", "v"] {
            let ia = fixture(&format!("h{k}{r}fa.i"));
            let im = fixture(&format!("h{k}{r}fm.i"));
            let mut st = homed_decoder(rate, r, Law::ULaw);
            let got: Vec<u8> = ia.iter().map(|&c| st.decode_law(c, Law::ULaw)).collect();
            assert_words_eq(
                &got,
                &fixture(&format!("h{k}{r}fx.o")),
                &format!("h{k}{r}fx.o"),
            );
            if k == "n" && rate == Rate::R16 {
                // Upstream generation quirk, pinned as shipped: the
                // distributed hn16fc.o was produced from the *reset*
                // state — decoding hn16fm.i after A-law homing (or
                // µ-law homing) does not reproduce it, while the
                // reset-state decode is byte-exact. Every other f*c
                // leg (hv16fc included) matches the homed procedure.
                let got = decode_reset(&im, rate, Law::ALaw);
                assert_words_eq(&got, &fixture("hn16fc.o"), "hn16fc.o (reset-state quirk)");
                continue;
            }
            let mut st = homed_decoder(rate, r, Law::ALaw);
            let got: Vec<u8> = im.iter().map(|&c| st.decode_law(c, Law::ALaw)).collect();
            assert_words_eq(
                &got,
                &fixture(&format!("h{k}{r}fc.o")),
                &format!("h{k}{r}fc.o"),
            );
        }
    }
}

#[test]
fn homing_decoder_codeword_sweep_all_rates_both_laws() {
    for (rate, r) in RATES {
        let sweep = fixture(&format!("i{r}"));
        let mut st = homed_decoder(rate, r, Law::ALaw);
        let got: Vec<u8> = sweep.iter().map(|&c| st.decode_law(c, Law::ALaw)).collect();
        assert_words_eq(&got, &fixture(&format!("hi{r}fa.o")), &format!("hi{r}fa.o"));
        let mut st = homed_decoder(rate, r, Law::ULaw);
        let got: Vec<u8> = sweep.iter().map(|&c| st.decode_law(c, Law::ULaw)).collect();
        assert_words_eq(&got, &fixture(&format!("hi{r}fm.o")), &format!("hi{r}fm.o"));
    }
}
