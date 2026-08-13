//! Black-box G.726 variable-bit-rate conformance against the staged
//! demo reference vectors.
//!
//! The staged rate-schedule note (`docs/audio/adpcm/g726/
//! vbr-demo-rate-schedule.md`) recovers the invocation behind the
//! `voice*` reference set staged alongside the Appendix II corpus
//! (`docs/audio/adpcm/g726/conformance/`): the rate list
//! `16-24-32-40-32-24` kbit/s applied cyclically over `voice.src`,
//! three law legs, each a full encode-then-decode run. The note flags
//! one unknown — the per-rate frame length, inferred there as 256
//! samples. These tests settle that inference empirically: the
//! **linear leg (`voicevbr.lrf`) reproduces bit-exactly over all
//! 52 736 samples at a 16-sample frame period** — 3 295 mid-stream
//! rate switches with full state carriage — and does *not* reproduce
//! at 256. The model proven here:
//!
//! * input: 16-bit LE linear words, companded onto the A-law lattice
//!   (`compress_i16`, matching the note's quote that the linear path
//!   is A-law companded before encoding);
//! * codec: one `State` pair on the §4.2.1/§4.2.8 A-law interface,
//!   `set_rate` every 16 samples through the six-entry cycle, state
//!   carried across every switch (Table 6 delayed variables retained);
//! * output: the decoder's adjusted A-law word expanded back to
//!   16-bit linear (`expand_i16`).
//!
//! The A-law and µ-law legs (`voicevbr.arf` / `voicevbr.urf`) are
//! *not* pinned: their input interpretation is not reproducible from
//! the staged `voice.src` under any byte-level model (log-PCM
//! passthrough of the low bytes matches both legs bit-exactly through
//! sample 82 and then provably admits no shared continuation, while
//! every companding model fails from sample 0), so reconstructing them
//! needs upstream input-handling documentation — a docs ask, not a
//! codec gap. The codec's law interfaces are already proven bit-exact
//! by the Appendix II corpus in `tests/g726_conformance.rs`.
//!
//! The vectors are non-normative demo data and are not copied into
//! this repository: the tests read them from the directory named by
//! the `OXIDEAV_G726_VBR_DIR` environment variable (the staged
//! `conformance/` directory) and skip cleanly when it is unset or the
//! files are absent.

use oxideav_adpcm::g726::{compress_i16, expand_i16, Law, Rate, State};
use std::path::{Path, PathBuf};

/// The recovered demo schedule: rises to 40 kbit/s then descends back
/// through 32 to 24 before wrapping.
const RATE_CYCLE: [Rate; 6] = [
    Rate::R16,
    Rate::R24,
    Rate::R32,
    Rate::R40,
    Rate::R32,
    Rate::R24,
];

/// The demo's per-rate frame length in samples — established
/// empirically by `linear_leg_reproduces_reference_bit_exactly`
/// (the staged note's 256-sample inference does not reproduce; see
/// `frame_period_is_16_samples_not_256`).
const FRAME_SAMPLES: usize = 16;

/// `voice.src` length in 16-bit words (52 736 = 3 296 frames of 16;
/// the six-entry cycle does not divide 3 296, so the run stops
/// mid-cycle exactly as the staged note describes).
const SRC_WORDS: usize = 52_736;

fn vector_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("OXIDEAV_G726_VBR_DIR")?);
    dir.is_dir().then_some(dir)
}

fn read_words(dir: &Path, name: &str) -> Option<Vec<u16>> {
    let bytes = std::fs::read(dir.join(name)).ok()?;
    assert_eq!(bytes.len() % 2, 0, "{name}: odd byte length");
    Some(
        bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect(),
    )
}

struct Vectors {
    src: Vec<u16>,
    lrf: Vec<u16>,
}

fn load() -> Option<Vectors> {
    let Some(dir) = vector_dir() else {
        eprintln!("skipped: OXIDEAV_G726_VBR_DIR not set");
        return None;
    };
    let (Some(src), Some(lrf)) = (
        read_words(&dir, "voice.src"),
        read_words(&dir, "voicevbr.lrf"),
    ) else {
        eprintln!("skipped: voice.src / voicevbr.lrf absent under OXIDEAV_G726_VBR_DIR");
        return None;
    };
    assert_eq!(src.len(), SRC_WORDS, "voice.src: unexpected length");
    assert_eq!(lrf.len(), SRC_WORDS, "voicevbr.lrf: unexpected length");
    Some(Vectors { src, lrf })
}

/// One full linear-leg run at a given frame period, optionally
/// resetting the codec pair at every rate switch instead of carrying
/// state.
fn run_linear_leg(src: &[u16], period: usize, reset_at_switch: bool) -> Vec<u16> {
    let mut enc = State::new(RATE_CYCLE[0]);
    let mut dec = State::new(RATE_CYCLE[0]);
    let mut out = Vec::with_capacity(src.len());
    for (i, &word) in src.iter().enumerate() {
        if i % period == 0 {
            let rate = RATE_CYCLE[(i / period) % RATE_CYCLE.len()];
            if reset_at_switch && i > 0 {
                enc = State::new(rate);
                dec = State::new(rate);
            } else {
                enc.set_rate(rate);
                dec.set_rate(rate);
            }
        }
        let law_in = compress_i16(word as i16, Law::ALaw);
        let code = enc.encode_law(law_in, Law::ALaw);
        let law_out = dec.decode_law(code, Law::ALaw);
        out.push(expand_i16(law_out, Law::ALaw) as u16);
    }
    out
}

fn first_diff(a: &[u16], b: &[u16]) -> Option<usize> {
    a.iter().zip(b).position(|(x, y)| x != y)
}

/// The linear leg reproduces the staged reference byte-for-byte over
/// the whole file: 3 296 16-sample frames, 3 295 scheduled `set_rate`
/// calls (five distinct switch directions), the A-law compand front
/// end and the §4.2.8 output chain all bit-exact against an
/// independently generated black-box reference.
#[test]
fn linear_leg_reproduces_reference_bit_exactly() {
    let Some(v) = load() else { return };
    let ours = run_linear_leg(&v.src, FRAME_SAMPLES, false);
    assert_eq!(
        first_diff(&ours, &v.lrf),
        None,
        "linear leg diverges from the staged reference"
    );
    // Every output word sits on the A-law lattice by construction —
    // pin that the reference does too (it must, being the same chain).
    for (i, &w) in v.lrf.iter().enumerate() {
        let law = compress_i16(w as i16, Law::ALaw);
        assert_eq!(
            expand_i16(law, Law::ALaw) as u16,
            w,
            "reference word {i} is off the A-law lattice"
        );
    }
}

/// The reference was generated with state carried across rate
/// switches: a codec pair that resets at every switch falls off the
/// reference trajectory almost immediately. Together with the
/// bit-exact run above this proves the Table 6 state carriage in
/// `set_rate` is the demo's (and Appendix I.1's) semantics, not just
/// an internal convention.
#[test]
fn reference_run_carries_state_across_switches() {
    let Some(v) = load() else { return };
    let reset_run = run_linear_leg(&v.src, FRAME_SAMPLES, true);
    let d = first_diff(&reset_run, &v.lrf);
    assert!(
        d.is_some(),
        "resetting at every switch should not reproduce the reference"
    );
    // The zero-level preamble hides the damage briefly; divergence
    // must still appear within the first few frames of real signal.
    assert!(
        d.unwrap() < 4 * FRAME_SAMPLES + 128,
        "reset divergence appeared implausibly late (sample {})",
        d.unwrap()
    );
}

/// The staged note's flagged inference (256-sample frames) is wrong:
/// at a 256-sample period the run diverges from the reference, at 16
/// it is bit-exact. Pinned so the corrected figure cannot regress if
/// the note is ever taken at face value again.
#[test]
fn frame_period_is_16_samples_not_256() {
    let Some(v) = load() else { return };
    let at_256 = run_linear_leg(&v.src, 256, false);
    assert!(
        first_diff(&at_256, &v.lrf).is_some(),
        "a 256-sample period should not reproduce the reference"
    );
}
