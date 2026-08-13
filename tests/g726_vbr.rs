//! G.726 variable-bit-rate operation — mid-stream rate switching with
//! state carriage, per `State::set_rate`.
//!
//! §4 of the Recommendation defines the four rates over one shared
//! state machine, and Appendix I.1 (DCME) relies on switching rates
//! sample-by-sample without resetting the predictor. The staged
//! demo-schedule note (`docs/audio/adpcm/g726/vbr-demo-rate-schedule.md`)
//! documents the conventional block-cyclic form of that operation: the
//! rate list `16-24-32-40-32-24` kbit/s applied cyclically, stopping
//! mid-cycle wherever the input ends. (The demo's true switching
//! period is 16 samples — established bit-exactly against the staged
//! reference vectors in `tests/g726_vbr_conformance.rs`, correcting
//! the note's 256-sample inference.) These property tests keep a
//! 256-sample block so the per-block SNR statistics are stable; the
//! schedule shape and state-carriage semantics are the same.

use oxideav_adpcm::g726::{compress_i16, expand_i16, Law, Rate, State};

/// The documented demo rate cycle: rises to 40 kbit/s then descends
/// back through 32 to 24 before wrapping — six entries, not a rotation
/// of the four-rate list.
const SCHEDULE: [Rate; 6] = [
    Rate::R16,
    Rate::R24,
    Rate::R32,
    Rate::R40,
    Rate::R32,
    Rate::R24,
];

/// Rate-switch period used by these property tests. Longer than the
/// demo's true 16-sample period (see `g726_vbr_conformance.rs`) so
/// each block carries enough samples for meaningful SNR floors.
const PERIOD: usize = 256;

fn schedule_rate(sample_idx: usize) -> Rate {
    SCHEDULE[(sample_idx / PERIOD) % SCHEDULE.len()]
}

/// A speech-band test signal busy enough to keep the adaptation logic
/// (fast/slow scale factors, pole/zero predictor) moving across the
/// whole schedule.
fn test_signal(n: usize) -> Vec<i16> {
    (0..n)
        .map(|i| {
            let t = i as f64 / 8000.0;
            let v = 9000.0 * (2.0 * std::f64::consts::PI * 310.0 * t).sin()
                + 4000.0 * (2.0 * std::f64::consts::PI * 1210.0 * t).sin()
                + 1500.0 * (2.0 * std::f64::consts::PI * 2530.0 * t + 0.8).sin();
            v as i16
        })
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

/// Encoder and decoder that switch rates on the same sample schedule
/// stay in lockstep: the decoded signal keeps tracking the input across
/// every switch, including the wrap that lands mid-cycle (2050 samples
/// = 8 whole blocks + 2 samples, and 8 blocks is one full cycle plus
/// two — the schedule is applied cyclically from block 0).
#[test]
fn scheduled_switching_tracks_input_across_all_boundaries() {
    let n = 2050usize;
    let pcm = test_signal(n);
    let mut enc = State::new(schedule_rate(0));
    let mut dec = State::new(schedule_rate(0));
    let mut out = Vec::with_capacity(n);
    for (i, &s) in pcm.iter().enumerate() {
        let r = schedule_rate(i);
        enc.set_rate(r);
        dec.set_rate(r);
        let code = enc.encode_i16(s);
        out.push(dec.decode_i16(code));
    }
    // Whole-stream fidelity despite six rate switches per cycle. The
    // 16 kbit/s opening block bounds the global figure; per-block SNR
    // must recover at the higher rates.
    assert!(snr_db(&pcm, &out) > 5.0, "global SNR collapsed");
    for (block, floor) in [(2usize, 12.0), (3, 18.0)] {
        let lo = block * PERIOD;
        let hi = lo + PERIOD;
        let snr = snr_db(&pcm[lo..hi], &out[lo..hi]);
        assert!(
            snr > floor,
            "block {block} ({:?}): SNR {snr:.1} dB under {floor}",
            schedule_rate(lo)
        );
    }
}

/// The §4.2.8 SYNC tandem-transparency guarantee survives rate
/// switching: with both stages switching on the same schedule, a
/// decoded-then-re-encoded-then-re-decoded log-PCM stream is
/// word-identical to the first decode from the second stage onward.
#[test]
fn tandem_transparency_holds_across_rate_switches() {
    for law in [Law::ALaw, Law::ULaw] {
        let n = SCHEDULE.len() * PERIOD;
        let pcm = test_signal(n);

        // Stage 1: encode + decode on the schedule (log-PCM interface).
        let mut enc1 = State::new(schedule_rate(0));
        let mut dec1 = State::new(schedule_rate(0));
        let mut law1 = Vec::with_capacity(n);
        for (i, &s) in pcm.iter().enumerate() {
            let r = schedule_rate(i);
            enc1.set_rate(r);
            dec1.set_rate(r);
            let code = enc1.encode_law(compress_i16(s, law), law);
            law1.push(dec1.decode_law(code, law));
        }

        // Stage 2: re-encode stage 1's law words on the same schedule.
        let mut enc2 = State::new(schedule_rate(0));
        let mut dec2 = State::new(schedule_rate(0));
        let mut law2 = Vec::with_capacity(n);
        for (i, &w) in law1.iter().enumerate() {
            let r = schedule_rate(i);
            enc2.set_rate(r);
            dec2.set_rate(r);
            let code = enc2.encode_law(w, law);
            law2.push(dec2.decode_law(code, law));
        }

        // Stage 3: and once more.
        let mut enc3 = State::new(schedule_rate(0));
        let mut dec3 = State::new(schedule_rate(0));
        let mut law3 = Vec::with_capacity(n);
        for (i, &w) in law2.iter().enumerate() {
            let r = schedule_rate(i);
            enc3.set_rate(r);
            dec3.set_rate(r);
            let code = enc3.encode_law(w, law);
            law3.push(dec3.decode_law(code, law));
        }

        assert_eq!(
            law2, law3,
            "{law:?}: SYNC tandem transparency broke across rate switches"
        );
        // The law words sit on the law lattice; expanding them must not
        // change between stages either.
        let pcm2: Vec<i16> = law2.iter().map(|&w| expand_i16(w, law)).collect();
        let pcm3: Vec<i16> = law3.iter().map(|&w| expand_i16(w, law)).collect();
        assert_eq!(pcm2, pcm3);
    }
}

/// State carriage across a switch is load-bearing: a decoder that
/// resets its predictor at a rate boundary diverges from the one that
/// carries state (which is why `set_rate` retains every Table 6
/// delayed variable).
#[test]
fn resetting_at_a_switch_diverges_from_carrying_state() {
    let n = 2 * PERIOD;
    let pcm = test_signal(n);
    let mut enc = State::new(schedule_rate(0));
    let mut codes = Vec::with_capacity(n);
    for (i, &s) in pcm.iter().enumerate() {
        enc.set_rate(schedule_rate(i));
        codes.push(enc.encode_i16(s));
    }

    let mut carry = State::new(schedule_rate(0));
    let mut reset = State::new(schedule_rate(0));
    let mut out_carry = Vec::with_capacity(n);
    let mut out_reset = Vec::with_capacity(n);
    for (i, &c) in codes.iter().enumerate() {
        let r = schedule_rate(i);
        carry.set_rate(r);
        if i == PERIOD {
            // Wrong-headed receiver: full reset at the switch.
            reset = State::new(r);
        } else {
            reset.set_rate(r);
        }
        out_carry.push(carry.decode_i16(c));
        out_reset.push(reset.decode_i16(c));
    }
    // Identical until the switch…
    assert_eq!(out_carry[..PERIOD], out_reset[..PERIOD]);
    // …then the resetting receiver falls off the encoder's trajectory.
    assert_ne!(
        out_carry[PERIOD..],
        out_reset[PERIOD..],
        "a state reset at the boundary should not be transparent"
    );
    let snr_carry = snr_db(&pcm[PERIOD..], &out_carry[PERIOD..]);
    let snr_reset = snr_db(&pcm[PERIOD..], &out_reset[PERIOD..]);
    assert!(
        snr_carry > snr_reset,
        "carrying state ({snr_carry:.1} dB) must beat resetting ({snr_reset:.1} dB)"
    );
}

/// Appendix I.1's strongest form: the rate may change at *every*
/// sample, at arbitrary (non-block-aligned) positions. A deterministic
/// pseudo-random per-sample rate walk keeps encoder and decoder in
/// lockstep (the decoder tracks the input through every one of the
/// thousands of switches), and the §4.2.8 SYNC tandem-transparency
/// guarantee still holds word-for-word with all stages switching on
/// the same random walk.
#[test]
fn per_sample_random_rate_walk_keeps_lockstep_and_tandem_transparency() {
    let n = 4096usize;
    let pcm = test_signal(n);

    // xorshift32 rate walk — switches on average every sample, with
    // runs of equal rates mixed in.
    let mut seed = 0x1234_5678u32;
    let mut rates = Vec::with_capacity(n);
    for _ in 0..n {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        rates.push(match seed % 4 {
            0 => Rate::R16,
            1 => Rate::R24,
            2 => Rate::R32,
            _ => Rate::R40,
        });
    }

    // Lockstep on the linear interface.
    let mut enc = State::new(rates[0]);
    let mut dec = State::new(rates[0]);
    let mut out = Vec::with_capacity(n);
    for (i, &s) in pcm.iter().enumerate() {
        enc.set_rate(rates[i]);
        dec.set_rate(rates[i]);
        out.push(dec.decode_i16(enc.encode_i16(s)));
    }
    // Frequent 16 kbit/s samples bound the figure; it must still track.
    let snr = snr_db(&pcm, &out);
    assert!(
        snr > 4.0,
        "per-sample switching lost lockstep ({snr:.1} dB)"
    );

    // Tandem transparency on both law interfaces under the same walk.
    for law in [Law::ALaw, Law::ULaw] {
        let stage = |input: &[u8]| -> Vec<u8> {
            let mut e = State::new(rates[0]);
            let mut d = State::new(rates[0]);
            input
                .iter()
                .enumerate()
                .map(|(i, &w)| {
                    e.set_rate(rates[i]);
                    d.set_rate(rates[i]);
                    d.decode_law(e.encode_law(w, law), law)
                })
                .collect()
        };
        let law0: Vec<u8> = pcm.iter().map(|&s| compress_i16(s, law)).collect();
        let law1 = stage(&law0);
        let law2 = stage(&law1);
        let law3 = stage(&law2);
        assert_eq!(
            law2, law3,
            "{law:?}: tandem transparency broke under per-sample switching"
        );
    }
}
