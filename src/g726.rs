//! ITU-T **G.726** ADPCM — 40 / 32 / 24 / 16 kbit/s narrowband speech.
//!
//! Bit-exact transcription of Recommendation G.726 (12/1990) §4.2
//! ("Description of variables and detailed specification of sub-blocks"),
//! staged at `docs/audio/adpcm/g726/T-REC-G.726-199012-I.pdf`. The three
//! per-rate table roles (RECONST / FUNCTW / FUNCTF) live in
//! [`crate::tables`]; the encoder-side quantizer decision ladders
//! (Tables 7-10/G.726, cross-checked against the synchronous-coding
//! Tables 16-19/G.726) are private constants next to the QUAN block
//! below.
//!
//! All four rates share one state machine ([`State`]); only the
//! quantizer codebook, the scale-factor multiplier `W(I)` and the
//! adaptation-speed control `F(I)` differ per rate (§4 of the
//! Recommendation). Every sub-block is implemented with the exact
//! masked / wrapped integer arithmetic of the spec so a conformant
//! implementation is reproducible word-for-word; `DQ` uses the 16-bit
//! signed-magnitude representation, which §4.2 (Table 6 note b) permits
//! for every rate and mandates for 40 kbit/s.
//!
//! # Interface
//!
//! The Recommendation's PCM interface is either 14-bit uniform or
//! log-companded A-law / µ-law per G.711. Both are implemented:
//!
//! * [`State::encode_i16`] / [`State::decode_i16`] map standard 16-bit
//!   PCM onto the 14-bit uniform interface (`>> 2` on input, clamp +
//!   `<< 2` on output); [`State::encode_step`] / [`State::decode_step`]
//!   expose the raw 14-bit words.
//! * [`State::encode_law`] / [`State::decode_law`] speak G.711
//!   log-PCM directly: the §4.2.1 EXPAND input conversion on the
//!   encoder side, and the full §4.2.8 output chain — COMPRESS,
//!   re-EXPAND, and the SYNC synchronous coding adjustment (Tables
//!   16-19/G.726) — on the decoder side. This is the interface the
//!   ITU conformance test sequences (Appendix II) exercise; the
//!   in-tree vector suite pins it byte-exactly.
//!
//! On the wire a G.726 stream is a headerless run of 2/3/4/5-bit codes.
//! Two packing conventions exist: **MSB-first** (each code inserted from
//! the byte's most-significant end — the network/RTP order) and
//! **LSB-first** (each code inserted from the least-significant end).
//! [`pack_codes`] / [`unpack_codes`] convert whole buffers;
//! [`State`]-independent [`BitPacker`] / [`BitUnpacker`] carry partial
//! bytes across packet boundaries for the streaming codec paths (at 3
//! and 5 bits per code a code routinely straddles a byte boundary).

use crate::tables;

/// G.726 operating rate — the per-sample code width.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Rate {
    /// 16 kbit/s — 2 bits per sample (Annex D tables).
    R16,
    /// 24 kbit/s — 3 bits per sample (Annex C tables).
    R24,
    /// 32 kbit/s — 4 bits per sample (the G.721 successor; Table 12).
    R32,
    /// 40 kbit/s — 5 bits per sample (Annex A tables).
    R40,
}

impl Rate {
    /// Every rate, ascending by code width.
    pub const fn all() -> &'static [Rate] {
        &[Rate::R16, Rate::R24, Rate::R32, Rate::R40]
    }

    /// Bits per coded sample: 2, 3, 4 or 5.
    pub const fn bits(self) -> u8 {
        match self {
            Rate::R16 => 2,
            Rate::R24 => 3,
            Rate::R32 => 4,
            Rate::R40 => 5,
        }
    }

    /// Inverse of [`Rate::bits`].
    pub const fn from_bits(bits: u8) -> Option<Rate> {
        match bits {
            2 => Some(Rate::R16),
            3 => Some(Rate::R24),
            4 => Some(Rate::R32),
            5 => Some(Rate::R40),
            _ => None,
        }
    }

    /// Bit-rate in bit/s at the fixed 8 kHz sampling rate.
    pub const fn bitrate(self) -> u32 {
        8000 * self.bits() as u32
    }

    /// Mask covering one code word (`2^bits - 1`).
    const fn code_mask(self) -> u32 {
        (1u32 << self.bits()) - 1
    }

    /// RECONST table for this rate (Tables 11-14/G.726), indexed by `I`.
    fn dqln_table(self) -> &'static [u16] {
        match self {
            Rate::R16 => &tables::G726_DQLN_16,
            Rate::R24 => &tables::G726_DQLN_24,
            Rate::R32 => &tables::G726_DQLN_32,
            Rate::R40 => &tables::G726_DQLN_40,
        }
    }

    /// FUNCTW table (`W(I)`, §4.2.4), indexed by the code magnitude.
    fn wi_table(self) -> &'static [u16] {
        match self {
            Rate::R16 => &tables::G726_WI_16,
            Rate::R24 => &tables::G726_WI_24,
            Rate::R32 => &tables::G726_WI_32,
            Rate::R40 => &tables::G726_WI_40,
        }
    }

    /// FUNCTF table (`F(I)`, §4.2.5), indexed by the code magnitude.
    fn fi_table(self) -> &'static [u16] {
        match self {
            Rate::R16 => &tables::G726_FI_16,
            Rate::R24 => &tables::G726_FI_24,
            Rate::R32 => &tables::G726_FI_32,
            Rate::R40 => &tables::G726_FI_40,
        }
    }

    /// Code magnitude `IM` per the FUNCTW / FUNCTF sign-fold: `I & m`
    /// when the sign bit is clear, `(max - I) & m` when it is set.
    fn code_magnitude(self, i: u32) -> u32 {
        let bits = self.bits() as u32;
        let mag_mask = (1u32 << (bits - 1)) - 1;
        if i >> (bits - 1) == 0 {
            i & mag_mask
        } else {
            (self.code_mask() - i) & mag_mask
        }
    }
}

// ---------------------------------------------------------------------------
// QUAN decision ladders (encoder side)
// ---------------------------------------------------------------------------
//
// Tables 7-10/G.726 give the quantizer decision intervals over the
// normalized log difference DLN (a 12-bit two's-complement word). Each
// ladder below lists the *lower bound* of every magnitude bucket in
// signed form, highest magnitude first; a DLN below the last bound is
// the "negative log-zero" region that emits the all-ones sign-flipped
// code (there is no such region at 16 kbit/s — its magnitude-0 code is a
// real reconstruction level). The same intervals reappear as the
// synchronous-coding `ID` definitions in Tables 16-19/G.726.

/// 40 kbit/s decision lower bounds for magnitudes 15..=1 (Table 7).
const QUAN_40: [i32; 15] = [
    553, 528, 502, 475, 445, 413, 378, 339, 298, 250, 198, 139, 68, -16, -122,
];
/// 32 kbit/s decision lower bounds for magnitudes 7..=1 (Table 8).
const QUAN_32: [i32; 7] = [400, 349, 300, 246, 178, 80, -124];
/// 24 kbit/s decision lower bounds for magnitudes 3..=1 (Table 9).
const QUAN_24: [i32; 3] = [331, 218, 8];
/// 16 kbit/s decision lower bound for magnitude 1 (Table 10).
const QUAN_16: [i32; 1] = [261];

/// QUAN (§4.2.2, encoder only): map the sign `DS` and 12-bit
/// two's-complement `DLN` to the transmitted code `I`.
fn quan(rate: Rate, ds: u32, dln: u32) -> u32 {
    // 12-bit TC → signed.
    let s = if dln >= 2048 {
        dln as i32 - 4096
    } else {
        dln as i32
    };
    let ladder: &[i32] = match rate {
        Rate::R16 => &QUAN_16,
        Rate::R24 => &QUAN_24,
        Rate::R32 => &QUAN_32,
        Rate::R40 => &QUAN_40,
    };
    let top = ladder.len() as u32; // largest magnitude
    let mut mag: Option<u32> = None;
    for (k, &lo) in ladder.iter().enumerate() {
        if s >= lo {
            mag = Some(top - k as u32);
            break;
        }
    }
    let full = rate.code_mask();
    match (mag, rate) {
        // 16 kbit/s: everything below the single threshold is the real
        // magnitude-0 level (Table 14 has no log-zero code).
        (None, Rate::R16) => {
            if ds == 0 {
                0
            } else {
                full
            }
        }
        // Higher rates: below the ladder floor lies the log-zero region;
        // both signs emit the all-ones code (Tables 7-9, the
        // "2048-…" rows), which RECONST maps back to DQLN = -2048.
        (None, _) => full,
        (Some(m), _) => {
            if ds == 0 {
                m
            } else {
                full - m
            }
        }
    }
}

// ---------------------------------------------------------------------------
// §4.2 sub-block primitives
// ---------------------------------------------------------------------------

/// Exponent convention of LOG (§4.2.2): `floor(log2(x))`, 0 for x <= 1.
fn exp_log(x: u32) -> u32 {
    if x < 2 {
        0
    } else {
        31 - x.leading_zeros()
    }
}

/// Exponent convention of FLOATA / FLOATB / FMULT (§4.2.6): the bit
/// length of `x` (0 for 0, `floor(log2(x)) + 1` otherwise).
fn exp_float(x: u32) -> u32 {
    32 - x.leading_zeros()
}

/// LOG (§4.2.2): 16-bit TC difference signal → (11-bit SM log `DL`,
/// sign `DS`).
fn log(d: u32) -> (u32, u32) {
    let ds = d >> 15;
    let dqm = if ds == 0 { d } else { (65536 - d) & 32767 };
    let exp = exp_log(dqm).min(14);
    let mant = ((dqm << 7) >> exp) & 127;
    ((exp << 7) + mant, ds)
}

/// SUBTB (§4.2.2): scale the log difference by the scale factor.
fn subtb(dl: u32, y: u32) -> u32 {
    (dl + 4096 - (y >> 2)) & 4095
}

/// RECONST (§4.2.3): code `I` → (12-bit TC `DQLN`, sign `DQS`).
fn reconst(rate: Rate, i: u32) -> (u32, u32) {
    let dqln = rate.dqln_table()[i as usize] as u32;
    let dqs = i >> (rate.bits() as u32 - 1);
    (dqln, dqs)
}

/// ADDA (§4.2.3): add the scale factor back in the log domain.
fn adda(dqln: u32, y: u32) -> u32 {
    (dqln + (y >> 2)) & 4095
}

/// ANTILOG (§4.2.3): log-domain `DQL` + sign `DQS` → 16-bit
/// signed-magnitude `DQ`.
fn antilog(dql: u32, dqs: u32) -> u32 {
    let ds = dql >> 11;
    let dex = (dql >> 7) & 15;
    let dmn = dql & 127;
    let dqt = (1 << 7) + dmn;
    let dqmag = if ds == 0 { (dqt << 7) >> (14 - dex) } else { 0 };
    (dqs << 15) + dqmag
}

/// FILTD (§4.2.4): fast scale-factor update (1/32 time constant).
fn filtd(wi: u32, y: u32) -> u32 {
    let dif = ((wi << 5) + 131072 - y) & 131071;
    let difs = dif >> 16;
    let difsx = if difs == 0 {
        dif >> 5
    } else {
        (dif >> 5) + 4096
    };
    (y + difsx) & 8191
}

/// LIMB (§4.2.4): clamp the fast scale factor to [1.06, 10.00].
fn limb(yut: u32) -> u32 {
    let geul = ((yut + 11264) & 16383) >> 13;
    let gell = ((yut + 15840) & 16383) >> 13;
    if gell == 1 {
        544 // lower limit (1.06)
    } else if geul == 0 {
        5120 // upper limit (10.00)
    } else {
        yut
    }
}

/// FILTE (§4.2.4): slow scale-factor update (1/64 time constant).
fn filte(yup: u32, yl: u32) -> u32 {
    let dif = (yup + ((1048576 - yl) >> 6)) & 16383;
    let difs = dif >> 13;
    let difsx = if difs == 0 { dif } else { dif + 507904 };
    (yl + difsx) & 524287
}

/// MIX (§4.2.4): combine the fast and slow scale factors under the
/// speed-control parameter `AL`.
fn mix(al: u32, yu: u32, yl: u32) -> u32 {
    let dif = (yu + 16384 - (yl >> 6)) & 16383;
    let difs = dif >> 13;
    let difm = if difs == 0 { dif } else { (16384 - dif) & 8191 };
    let prodm = (difm * al) >> 6;
    let prod = if difs == 0 {
        prodm
    } else {
        (16384 - prodm) & 16383
    };
    ((yl >> 6) + prod) & 8191
}

/// FILTA (§4.2.5): short-term F(I) average (1/32 time constant).
fn filta(fi: u32, dms: u32) -> u32 {
    let dif = ((fi << 9) + 8192 - dms) & 8191;
    let difs = dif >> 12;
    let difsx = if difs == 0 {
        dif >> 5
    } else {
        (dif >> 5) + 3840
    };
    (difsx + dms) & 4095
}

/// FILTB (§4.2.5): long-term F(I) average (1/128 time constant).
fn filtb(fi: u32, dml: u32) -> u32 {
    let dif = ((fi << 11) + 32768 - dml) & 32767;
    let difs = dif >> 14;
    let difsx = if difs == 0 {
        dif >> 7
    } else {
        (dif >> 7) + 16128
    };
    (difsx + dml) & 16383
}

/// SUBTC (§4.2.5): compare the short/long-term averages and threshold
/// into the 1-bit speed-control update `AX`.
fn subtc(dmsp: u32, dmlp: u32, tdp: u32, y: u32) -> u32 {
    let dif = ((dmsp << 2) + 32768 - dmlp) & 32767;
    let difs = dif >> 14;
    let difm = if difs == 0 {
        dif
    } else {
        (32768 - dif) & 16383
    };
    let dthr = dmlp >> 3;
    if y >= 1536 && difm < dthr && tdp == 0 {
        0
    } else {
        1
    }
}

/// FILTC (§4.2.5): low-pass the speed-control parameter (1/16).
fn filtc(ax: u32, ap: u32) -> u32 {
    let dif = ((ax << 9) + 2048 - ap) & 2047;
    let difs = dif >> 10;
    let difsx = if difs == 0 {
        dif >> 4
    } else {
        (dif >> 4) + 896
    };
    (difsx + ap) & 1023
}

/// LIMA (§4.2.5): limit the speed-control parameter to [0, 1].
fn lima(ap: u32) -> u32 {
    if ap >= 256 {
        64
    } else {
        ap >> 2
    }
}

/// FMULT (§4.2.6): multiply a 16-bit TC predictor coefficient with an
/// 11-bit floating-point signal word; 16-bit TC partial product.
fn fmult(an: u32, srn: u32) -> u32 {
    let ans = an >> 15;
    let anmag = if ans == 0 {
        an >> 2
    } else {
        (16384 - (an >> 2)) & 8191
    };
    let anexp = exp_float(anmag);
    let anmant = if anmag == 0 {
        1 << 5
    } else {
        (anmag << 6) >> anexp
    };
    let srns = srn >> 10;
    let srnexp = (srn >> 6) & 15;
    let srnmant = srn & 63;
    let wans = srns ^ ans;
    let wanexp = srnexp + anexp;
    let wanmant = ((srnmant * anmant) + 48) >> 4;
    let wanmag = if wanexp <= 26 {
        (wanmant << 7) >> (26 - wanexp)
    } else {
        ((wanmant << 7) << (wanexp - 26)) & 32767
    };
    if wans == 0 {
        wanmag
    } else {
        (65536 - wanmag) & 65535
    }
}

/// FLOATA (§4.2.6): 16-bit signed-magnitude `DQ` → 11-bit float.
fn floata(dq: u32) -> u32 {
    let dqs = dq >> 15;
    let mag = dq & 32767;
    let exp = exp_float(mag);
    let mant = if mag == 0 { 1 << 5 } else { (mag << 6) >> exp };
    (dqs << 10) + (exp << 6) + mant
}

/// FLOATB (§4.2.6): 16-bit two's-complement `SR` → 11-bit float.
fn floatb(sr: u32) -> u32 {
    let srs = sr >> 15;
    let mag = if srs == 0 { sr } else { (65536 - sr) & 32767 };
    let exp = exp_float(mag);
    let mant = if mag == 0 { 1 << 5 } else { (mag << 6) >> exp };
    (srs << 10) + (exp << 6) + mant
}

/// Signed-magnitude `DQ` → 16-bit two's complement (shared by ADDB /
/// ADDC, §4.2.6).
fn dq_to_tc(dq: u32) -> u32 {
    if dq >> 15 == 0 {
        dq
    } else {
        (65536 - (dq & 32767)) & 65535
    }
}

/// ADDB (§4.2.6): reconstructed signal `SR = DQ + SE` (16-bit TC).
fn addb(dq: u32, se: u32) -> u32 {
    let sei = if se >> 14 == 0 { se } else { (1 << 15) + se };
    (dq_to_tc(dq) + sei) & 65535
}

/// ADDC (§4.2.6): sign of `DQ + SEZ` → (`PK0`, `SIGPK`).
fn addc(dq: u32, sez: u32) -> (u32, u32) {
    let sezi = if sez >> 14 == 0 { sez } else { (1 << 15) + sez };
    let dqsez = (dq_to_tc(dq) + sezi) & 65535;
    let pk0 = dqsez >> 15;
    let sigpk = if dqsez == 0 { 1 } else { 0 };
    (pk0, sigpk)
}

/// UPA1 (§4.2.6): first pole-coefficient update (±3/256 gain, 1/256
/// leak).
fn upa1(pk0: u32, pk1: u32, a1: u32, sigpk: u32) -> u32 {
    let pks = pk0 ^ pk1;
    let uga1 = if sigpk == 1 {
        0
    } else if pks == 0 {
        192
    } else {
        65344
    };
    let ula1 = if a1 >> 15 == 0 {
        (65536 - (a1 >> 8)) & 65535
    } else {
        (65536 - ((a1 >> 8) + 65280)) & 65535
    };
    let ua1 = (uga1 + ula1) & 65535;
    (a1 + ua1) & 65535
}

/// UPA2 (§4.2.6): second pole-coefficient update (±1/128 gain with the
/// `f(a1)` correction, 1/128 leak).
fn upa2(pk0: u32, pk1: u32, pk2: u32, a1: u32, a2: u32, sigpk: u32) -> u32 {
    let pks1 = pk0 ^ pk1;
    let pks2 = pk0 ^ pk2;
    let uga2a = if pks2 == 0 { 16384 } else { 114688 };
    let fa1 = if a1 >> 15 == 0 {
        // f(a1) limited at +1/2.
        if a1 <= 8191 {
            a1 << 2
        } else {
            8191 << 2
        }
    } else {
        // f(a1) limited at -1/2.
        if a1 >= 57345 {
            (a1 << 2) & 131071
        } else {
            24577 << 2
        }
    };
    let fa = if pks1 == 1 {
        fa1
    } else {
        (131072 - fa1) & 131071
    };
    let uga2b = (uga2a + fa) & 131071;
    let uga2s = uga2b >> 16;
    let uga2 = if sigpk == 1 {
        0
    } else if uga2s == 0 {
        uga2b >> 7
    } else {
        (uga2b >> 7) + 64512
    };
    let ula2 = if a2 >> 15 == 0 {
        (65536 - (a2 >> 7)) & 65535
    } else {
        (65536 - ((a2 >> 7) + 65024)) & 65535
    };
    let ua2 = (uga2 + ula2) & 65535;
    (a2 + ua2) & 65535
}

/// LIMC (§4.2.6): clamp `a2` to ±0.75.
fn limc(a2t: u32) -> u32 {
    const A2UL: u32 = 12288; // +0.75
    const A2LL: u32 = 53248; // -0.75
    if (32768..=A2LL).contains(&a2t) {
        A2LL
    } else if (A2UL..=32767).contains(&a2t) {
        A2UL
    } else {
        a2t
    }
}

/// LIMD (§4.2.6): clamp `a1` to ±(1 - 2^-4 - a2).
fn limd(a1t: u32, a2p: u32) -> u32 {
    const OME: u32 = 15360; // 1 - 1/16
    let a1ul = (OME + 65536 - a2p) & 65535;
    let a1ll = (a2p + 65536 - OME) & 65535;
    if a1t >= 32768 && a1t <= a1ll {
        a1ll
    } else if (a1ul..=32767).contains(&a1t) {
        a1ul
    } else {
        a1t
    }
}

/// UPB (§4.2.6): sixth-order (zero) coefficient update. The leak factor
/// is 1/512 at 40 kbit/s and 1/256 at the lower rates.
fn upb(rate: Rate, un: u32, bn: u32, dq: u32) -> u32 {
    let dqmag = dq & 32767;
    let ugbn = if dqmag == 0 {
        0
    } else if un == 0 {
        128
    } else {
        65408
    };
    let ulbn = match rate {
        Rate::R40 => {
            if bn >> 15 == 0 {
                (65536 - (bn >> 9)) & 65535
            } else {
                (65536 - ((bn >> 9) + 65408)) & 65535
            }
        }
        _ => {
            if bn >> 15 == 0 {
                (65536 - (bn >> 8)) & 65535
            } else {
                (65536 - ((bn >> 8) + 65280)) & 65535
            }
        }
    };
    let ubn = (ugbn + ulbn) & 65535;
    (bn + ubn) & 65535
}

/// TONE (§4.2.7): partial-band (tone) detection from the `a2` pole.
fn tone(a2p: u32) -> u32 {
    if (32768..53760).contains(&a2p) {
        1
    } else {
        0
    }
}

/// TRANS (§4.2.7): transition detection — a large `DQ` while a tone is
/// active triggers the predictor reset.
fn trans(td: u32, yl: u32, dq: u32) -> u32 {
    let dqmag = dq & 32767;
    let ylint = yl >> 15;
    let ylfrac = (yl >> 10) & 31;
    let thr1 = (32 + ylfrac) << ylint;
    // 16-bit signed-magnitude DQ ⇒ the YLINT > 9 cap (Table 6 note b).
    let thr2 = if ylint > 9 { 31 << 10 } else { thr1 };
    let dqthr = (thr2 + (thr2 >> 1)) >> 1;
    if dqmag > dqthr && td == 1 {
        1
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// G.711 log-PCM interface (§4.2.1 EXPAND, §4.2.8 COMPRESS + SYNC)
// ---------------------------------------------------------------------------

/// G.711 companding law of the PCM interface (`LAW` input pin of
/// Figures 4 and 11/G.726).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Law {
    /// `LAW = 1` — A-law. Code words carry the G.711 even-bit
    /// inversion (§4.2.1 note: "S (and SP) includes even bit
    /// inversion"), i.e. positive zero is `0xD5`.
    ALaw,
    /// `LAW = 0` — µ-law. Code words are the transmitted (inverted)
    /// character signals; positive zero is `0xFF`.
    ULaw,
}

/// A-law code word → 13-bit signed-magnitude `SS` (sign bit 1 =
/// negative), per Tables 1a/1b col. 6→7 of G.711: segment 0 outputs
/// the odd mid-rise levels `2m + 1`, segment `s >= 1` outputs
/// `(2m + 33) << (s - 1)`.
fn alaw_to_ss(s: u8) -> u32 {
    let b = (s ^ 0x55) as u32; // undo even-bit inversion
    let pos = b >> 7; // A-law character sign bit: 1 = positive
    let seg = (b >> 4) & 7;
    let mant = b & 15;
    let mag = if seg == 0 {
        (mant << 1) + 1
    } else {
        ((mant << 1) + 33) << (seg - 1)
    };
    ((1 - pos) << 12) | mag
}

/// µ-law code word → 14-bit signed-magnitude `SS` (sign bit 1 =
/// negative), per Tables 2a/2b col. 6→7 of G.711: the bias-33
/// mid-tread ladder `((2m + 33) << s) - 33` (segment-0 levels
/// 0, 2, …, 30; overall maximum 8031).
fn ulaw_to_ss(s: u8) -> u32 {
    let b = !s as u32 & 0xFF; // undo the transmitted inversion
    let neg = b >> 7; // 1 = negative
    let seg = (b >> 4) & 7;
    let mant = b & 15;
    let mag = (((mant << 1) + 33) << seg) - 33;
    (neg << 13) | mag
}

/// A-law quantizer level (0..=127) for a 12-bit-domain magnitude with
/// the *positive*-side interval convention: a G.711 Table 1a decision
/// value belongs to the interval above it (`IMAG = 2` already encodes
/// level 1, Table 15/G.726).
fn alaw_level_pos(v: u32) -> u32 {
    if v >= 4096 {
        // Beyond the virtual decision value: maximum PCM code word
        // (note below Table 15/G.726).
        return 127;
    }
    if v < 32 {
        return v >> 1; // segment 0, step 2
    }
    let seg = exp_log(v) - 4; // 32<<(seg-1) <= v < 32<<seg
    (seg << 4) | ((v - (32 << (seg - 1))) >> seg)
}

/// A-law quantizer level for the *negative* side: a Table 1b decision
/// value belongs to the interval below it (`IMAG = 2` still encodes
/// level 0, Table 15/G.726), i.e. the positive ladder shifted by one.
fn alaw_level_neg(v: u32) -> u32 {
    if v >= 4096 {
        return 127;
    }
    alaw_level_pos(v.saturating_sub(1))
}

/// µ-law quantizer level (0..=127) for a 13-bit-domain magnitude; the
/// same convention serves both signs (Table 15/G.726: `IMAG = 1`
/// encodes level 1 for `IS = 0` and `IS = 1` alike). Bias-33 segment
/// search over the Table 2a/2b decision values.
fn ulaw_level(v: u32) -> u32 {
    if v >= 8159 {
        // Virtual decision value x128 = 8159: maximum PCM code word.
        return 127;
    }
    let b = v + 33; // 33 <= b < 8192 ⇒ segment 0..=7
    let seg = exp_log(b) - 5;
    (seg << 4) | ((b >> (seg + 1)) & 15)
}

/// EXPAND (§4.2.1): G.711 log-PCM code word → 14-bit two's-complement
/// uniform PCM `SL`. The A-law 13-bit signed-magnitude value is
/// doubled into the µ-law-scaled 14-bit domain (`SSQ = SSM << 1`).
pub fn expand(s: u8, law: Law) -> u32 {
    let (sss, ssq) = match law {
        Law::ULaw => {
            let ss = ulaw_to_ss(s);
            (ss >> 13, ss & 8191)
        }
        Law::ALaw => {
            let ss = alaw_to_ss(s);
            (ss >> 12, (ss & 4095) << 1)
        }
    };
    if sss == 0 {
        ssq
    } else {
        (16384 - ssq) & 16383
    }
}

/// COMPRESS (§4.2.8, decoder only): 16-bit two's-complement
/// reconstructed signal `SR` → G.711 log-PCM code word `SP`.
///
/// The A-law path halves the magnitude back into the 12-bit domain
/// with the spec's asymmetric rounding (`IM >> 1` positive,
/// `(IM + 1) >> 1` negative) before quantizing; magnitudes beyond the
/// virtual decision value saturate to the maximum code word.
pub fn compress(sr: u32, law: Law) -> u8 {
    let is = sr >> 15;
    let im = if is == 0 { sr } else { (65536 - sr) & 32767 };
    match law {
        Law::ULaw => {
            let lvl = ulaw_level(im);
            // Character signal: complement of (sign | segment |
            // quantization); sign bit 1 for negative values.
            (!((is << 7) | lvl)) as u8
        }
        Law::ALaw => {
            let lvl = if is == 0 {
                alaw_level_pos(im >> 1)
            } else {
                alaw_level_neg((im + 1) >> 1)
            };
            // Character sign bit 1 = positive; even-bit inversion on
            // the wire.
            ((((1 - is) << 7) | lvl) ^ 0x55) as u8
        }
    }
}

/// [`expand`] to standard 16-bit PCM: the 14-bit uniform word scaled
/// up (`<< 2`), matching the [`State::decode_i16`] convention.
pub fn expand_i16(s: u8, law: Law) -> i16 {
    let sl = expand(s, law);
    let v = if sl >> 13 == 0 {
        sl as i32
    } else {
        sl as i32 - 16384
    };
    (v << 2) as i16
}

/// [`compress`] from standard 16-bit PCM: scaled down (`>> 2`) onto
/// the 14-bit uniform interface, matching [`State::encode_i16`].
pub fn compress_i16(s: i16, law: Law) -> u8 {
    compress(((s >> 2) as i32 as u32) & 0xFFFF, law)
}

/// `SP+` / `SP−` of the SYNC block: the PCM code word of the next more
/// positive (`up = true`) or more negative output level, clamped at
/// the extremes. Pinned by the Table 20/G.726 worked examples — in
/// particular the µ-law dual zero: stepping down from `+0` skips `-0`
/// (same output level) to `-2`, while stepping up from `-2` lands on
/// `-0`.
fn law_neighbor(sp: u8, law: Law, up: bool) -> u8 {
    match law {
        Law::ALaw => {
            let b = (sp ^ 0x55) as u32;
            let pos = b >> 7; // 1 = positive
            let lvl = b & 0x7F;
            let (pos, lvl) = if up == (pos == 1) {
                // Away from zero on this side of the ladder.
                (pos, (lvl + 1).min(127))
            } else if lvl > 0 {
                (pos, lvl - 1)
            } else {
                // Cross the origin: A-law has no zero level, so
                // ±level-0 are adjacent.
                (1 - pos, 0)
            };
            (((pos << 7) | lvl) ^ 0x55) as u8
        }
        Law::ULaw => {
            let b = !sp as u32 & 0xFF;
            let neg = b >> 7; // 1 = negative
            let lvl = b & 0x7F;
            let (neg, lvl) = if up == (neg == 0) {
                (neg, (lvl + 1).min(127))
            } else if lvl > 0 {
                (neg, lvl - 1)
            } else {
                // Cross the origin, skipping the other sign's zero
                // code (it is the *same* output level, not a more
                // positive/negative one).
                (1 - neg, 1)
            };
            (!((neg << 7) | lvl)) as u8
        }
    }
}

/// SYNC (§4.2.8, decoder only): synchronous coding adjustment for
/// tandem codings. Re-encodes the compressed output and nudges `SP`
/// one PCM level so a subsequent encoder reproduces the received code.
///
/// The `ID` definitions of Tables 16-19/G.726 are exactly the QUAN
/// decision intervals folded onto the signed code ordering, so `ID`
/// is computed by reusing [`quan`] and applying the same `IM` sign
/// fold as the received code.
fn sync(rate: Rate, i: u32, sp: u8, dlnx: u32, dsx: u32, law: Law) -> u8 {
    let half = 1u32 << (rate.bits() - 1);
    // IM: signed reordering of the code space — positive codes above
    // negative ones (e.g. 40 kbit/s: IM = I + 16 when I >= 0, I & 15
    // otherwise).
    let fold = |c: u32| {
        if c & half == 0 {
            c + half
        } else {
            c & (half - 1)
        }
    };
    let im = fold(i);
    let id = fold(quan(rate, dsx, dlnx));
    match id.cmp(&im) {
        core::cmp::Ordering::Equal => sp,
        core::cmp::Ordering::Less => law_neighbor(sp, law, true),
        core::cmp::Ordering::Greater => law_neighbor(sp, law, false),
    }
}

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

/// Complete per-channel G.726 codec state — the delayed variables of
/// Table 6/G.726, seeded with the optional-reset values (column 4).
///
/// The encoder and decoder share this state machine: the encoder is the
/// decoder plus the adaptive quantizer front-end (§2.3 / §6 of the
/// decode-trace), so a decoder fed an encoder's codes reproduces the
/// encoder's reconstructed-signal trajectory exactly.
#[derive(Clone, Debug)]
pub struct State {
    rate: Rate,
    /// Slow quantizer scale factor `YL` (19-bit SM; reset 34816).
    yl: u32,
    /// Fast quantizer scale factor `YU` (13-bit SM; reset 544).
    yu: u32,
    /// Short-term F(I) average `DMS` (12-bit SM; reset 0).
    dms: u32,
    /// Long-term F(I) average `DML` (14-bit SM; reset 0).
    dml: u32,
    /// Speed-control parameter `AP` (10-bit SM; reset 0).
    ap: u32,
    /// Second-order pole coefficients `A1`, `A2` (16-bit TC; reset 0).
    a1: u32,
    a2: u32,
    /// Sixth-order zero coefficients `B1..B6` (16-bit TC; reset 0).
    b: [u32; 6],
    /// Delayed quantized differences `DQ1..DQ6` (11-bit float; reset 32).
    dq: [u32; 6],
    /// Delayed reconstructed signals `SR1`, `SR2` (11-bit float; reset 32).
    sr: [u32; 2],
    /// Delayed `p(k)` signs `PK1`, `PK2` (reset 0).
    pk1: u32,
    pk2: u32,
    /// Delayed tone-detect flag `TD` (reset 0).
    td: u32,
}

impl State {
    /// Fresh codec state at `rate`, per the Table 6/G.726 reset column.
    pub fn new(rate: Rate) -> Self {
        State {
            rate,
            yl: 34816,
            yu: 544,
            dms: 0,
            dml: 0,
            ap: 0,
            a1: 0,
            a2: 0,
            b: [0; 6],
            dq: [32; 6],
            sr: [32; 2],
            pk1: 0,
            pk2: 0,
            td: 0,
        }
    }

    /// The operating rate this state was created with.
    pub fn rate(&self) -> Rate {
        self.rate
    }

    /// Switch the operating rate at a sample boundary, **carrying the
    /// codec state over**.
    ///
    /// §4 of the Recommendation defines the four rates over one shared
    /// state machine — only the quantizer codebook, the `W(I)` scale
    /// multipliers and the `F(I)` speed-control function are
    /// rate-scoped, and every Table 6 delayed variable is
    /// rate-independent (this implementation uses the 16-bit
    /// signed-magnitude `DQ` form at every rate, so no representation
    /// changes either). Appendix I.1 relies on exactly this property:
    /// DCME equipment alternates 32 kbit/s with 24/16 kbit/s coding
    /// sample-by-sample to hit a fractional average rate, without
    /// resetting the predictor. An encoder/decoder pair that switches
    /// rates on the same sample schedule stays in exact lockstep.
    pub fn set_rate(&mut self, rate: Rate) {
        self.rate = rate;
    }

    /// Apply the optional reset input `R = 1` (Table 5/G.726): force
    /// every delayed variable to its specified condition (the Table 6
    /// reset column) so the codec enters a known state. The operating
    /// rate is retained.
    pub fn reset(&mut self) {
        *self = State::new(self.rate);
    }

    /// FMULT × 8 + ACCUM (§4.2.6): signal estimate `SE` and partial
    /// (sixth-order) estimate `SEZ`, both 15-bit TC.
    fn predict(&self) -> (u32, u32) {
        let mut sezi = 0u32;
        for n in 0..6 {
            sezi = (sezi + fmult(self.b[n], self.dq[n])) & 65535;
        }
        let sei =
            (((sezi + fmult(self.a2, self.sr[1])) & 65535) + fmult(self.a1, self.sr[0])) & 65535;
        (sei >> 1, sezi >> 1)
    }

    /// Current scale factor `Y` (13-bit SM) from the delayed state.
    fn scale_factor(&self) -> u32 {
        mix(lima(self.ap), self.yu, self.yl)
    }

    /// Inverse quantizer + full state update for code `i`; returns the
    /// 16-bit two's-complement reconstructed signal `SR`. `se`, `sez`
    /// and `y` must come from [`Self::predict`] / [`Self::scale_factor`]
    /// *before* any update (all delay blocks advance simultaneously,
    /// §4 timing note).
    fn update(&mut self, i: u32, y: u32, se: u32, sez: u32) -> u32 {
        let rate = self.rate;
        // Inverse adaptive quantizer (§4.2.3).
        let (dqln, dqs) = reconst(rate, i);
        let dq = antilog(adda(dqln, y), dqs);
        // Quantizer scale-factor adaptation (§4.2.4).
        let im = rate.code_magnitude(i);
        let wi = rate.wi_table()[im as usize] as u32;
        let yup = limb(filtd(wi, y));
        let ylp = filte(yup, self.yl);
        // Adaptation speed control (§4.2.5).
        let fi = rate.fi_table()[im as usize] as u32;
        let dmsp = filta(fi, self.dms);
        let dmlp = filtb(fi, self.dml);
        // Adaptive predictor + reconstructed signal (§4.2.6).
        let (pk0, sigpk) = addc(dq, sez);
        let sr = addb(dq, se);
        let sr0 = floatb(sr);
        let dq0 = floata(dq);
        let a2p = limc(upa2(pk0, self.pk1, self.pk2, self.a1, self.a2, sigpk));
        let a1p = limd(upa1(pk0, self.pk1, self.a1, sigpk), a2p);
        let mut bp = [0u32; 6];
        for n in 0..6 {
            // XOR (§4.2.6): sign of DQ vs sign of the delayed DQn float.
            let un = (dq >> 15) ^ (self.dq[n] >> 10);
            bp[n] = upb(rate, un, self.b[n], dq);
        }
        // Tone and transition detector (§4.2.7). TRANS reads the
        // *delayed* TD and YL; TONE reads the freshly limited a2.
        let tdp = tone(a2p);
        let tr = trans(self.td, self.yl, dq);
        // Speed control tail (§4.2.5): SUBTC needs Y and TDP.
        let app = filtc(subtc(dmsp, dmlp, tdp, y), self.ap);
        let apr = if tr == 1 { 256 } else { app }; // TRIGA
                                                   // TRIGB + DELAY: advance every delayed variable at once.
        if tr == 1 {
            self.a1 = 0;
            self.a2 = 0;
            self.b = [0; 6];
            self.td = 0;
        } else {
            self.a1 = a1p;
            self.a2 = a2p;
            self.b = bp;
            self.td = tdp;
        }
        for n in (1..6).rev() {
            self.dq[n] = self.dq[n - 1];
        }
        self.dq[0] = dq0;
        self.sr[1] = self.sr[0];
        self.sr[0] = sr0;
        self.pk2 = self.pk1;
        self.pk1 = pk0;
        self.ap = apr;
        self.yu = yup;
        self.yl = ylp;
        self.dms = dmsp;
        self.dml = dmlp;
        sr
    }

    /// Shared encoder core over the 14-bit TC uniform word `SL`.
    fn encode_sl(&mut self, sl: u32) -> u8 {
        let (se, sez) = self.predict();
        let y = self.scale_factor();
        // SUBTA (§4.2.1): D = SL - SE with 16-bit sign extension.
        let sli = if sl >> 13 == 0 { sl } else { 49152 + sl };
        let sei = if se >> 14 == 0 { se } else { 32768 + se };
        let d = (sli + 65536 - sei) & 65535;
        // Adaptive quantizer (§4.2.2).
        let (dl, ds) = log(d);
        let i = quan(self.rate, ds, subtb(dl, y));
        self.update(i, y, se, sez);
        i as u8
    }

    /// Encode one 14-bit uniform-PCM sample (`-8192..=8191`; values
    /// outside are clamped) into a G.726 code word (2/3/4/5 bits,
    /// right-aligned).
    pub fn encode_step(&mut self, sl14: i16) -> u8 {
        self.encode_sl((sl14.clamp(-8192, 8191) as i32 as u32) & 16383)
    }

    /// Encode one G.711 log-PCM code word (§4.2.1 EXPAND front-end,
    /// Figure 4/G.726) into a G.726 code word.
    pub fn encode_law(&mut self, s: u8, law: Law) -> u8 {
        self.encode_sl(expand(s, law))
    }

    /// Decode one G.726 code word into a G.711 log-PCM code word —
    /// the full §4.2.8 output chain: COMPRESS the reconstructed
    /// signal, re-EXPAND it, re-quantize the resulting difference and
    /// apply the SYNC synchronous coding adjustment. This is the
    /// decoder the ITU conformance sequences specify.
    pub fn decode_law(&mut self, code: u8, law: Law) -> u8 {
        let i = code as u32 & self.rate.code_mask();
        let (se, sez) = self.predict();
        let y = self.scale_factor();
        let sr = self.update(i, y, se, sez);
        // COMPRESS + EXPAND (§4.2.8): SP, then its uniform-domain
        // requantization SLX.
        let sp = compress(sr, law);
        let slx = expand(sp, law);
        // SUBTA + LOG + SUBTB with SLX in place of SL.
        let sli = if slx >> 13 == 0 { slx } else { 49152 + slx };
        let sei = if se >> 14 == 0 { se } else { 32768 + se };
        let dx = (sli + 65536 - sei) & 65535;
        let (dlx, dsx) = log(dx);
        let dlnx = subtb(dlx, y);
        sync(self.rate, i, sp, dlnx, dsx, law)
    }

    /// Decode one G.726 code word into the reconstructed 14-bit uniform
    /// PCM sample (`-8192..=8191`). Code bits above the rate's width are
    /// ignored.
    pub fn decode_step(&mut self, code: u8) -> i16 {
        let i = code as u32 & self.rate.code_mask();
        let (se, sez) = self.predict();
        let y = self.scale_factor();
        let sr = self.update(i, y, se, sez);
        // SR is a 16-bit TC word; the uniform-PCM interface is 14-bit,
        // so saturate (the spec's §4.2.8 COMPRESS saturates through the
        // G.711 law tables; the linear interface clamps directly).
        (sr as u16 as i16).clamp(-8192, 8191)
    }

    /// Encode one standard 16-bit PCM sample (maps onto the 14-bit
    /// uniform interface via `>> 2`).
    pub fn encode_i16(&mut self, s: i16) -> u8 {
        self.encode_step(s >> 2)
    }

    /// Decode one code word to standard 16-bit PCM (14-bit uniform
    /// output `<< 2`).
    pub fn decode_i16(&mut self, code: u8) -> i16 {
        self.decode_step(code) << 2
    }
}

// ---------------------------------------------------------------------------
// Bit packing
// ---------------------------------------------------------------------------

/// In-byte code packing order for the headerless G.726 stream.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum BitOrder {
    /// Each code is inserted from the byte's most-significant end — the
    /// network / RTP convention (the first code of a byte occupies its
    /// top bits).
    #[default]
    MsbFirst,
    /// Each code is inserted from the byte's least-significant end (the
    /// first code of a byte occupies its bottom bits).
    LsbFirst,
}

/// Incremental bit packer that carries a partial byte across calls —
/// at 3 and 5 bits per code a code word routinely straddles a byte
/// boundary, so a streaming encoder cannot flush whole bytes per
/// sample.
#[derive(Clone, Debug, Default)]
pub struct BitPacker {
    order: BitOrder,
    acc: u32,
    nbits: u32,
}

impl BitPacker {
    /// New packer with the given in-byte order.
    pub fn new(order: BitOrder) -> Self {
        BitPacker {
            order,
            acc: 0,
            nbits: 0,
        }
    }

    /// Append one right-aligned `bits`-wide code; push completed bytes
    /// to `out`.
    pub fn push(&mut self, code: u32, bits: u8, out: &mut Vec<u8>) {
        let bits = bits as u32;
        let code = code & ((1 << bits) - 1);
        match self.order {
            BitOrder::MsbFirst => {
                self.acc = (self.acc << bits) | code;
            }
            BitOrder::LsbFirst => {
                self.acc |= code << self.nbits;
            }
        }
        self.nbits += bits;
        while self.nbits >= 8 {
            match self.order {
                BitOrder::MsbFirst => {
                    self.nbits -= 8;
                    out.push((self.acc >> self.nbits) as u8);
                    self.acc &= (1 << self.nbits) - 1;
                }
                BitOrder::LsbFirst => {
                    out.push((self.acc & 0xFF) as u8);
                    self.acc >>= 8;
                    self.nbits -= 8;
                }
            }
        }
    }

    /// Number of bits currently buffered (0..=7).
    pub fn pending_bits(&self) -> u32 {
        self.nbits
    }

    /// Flush a partial byte, zero-padding the unused positions. No-op
    /// when the packer is byte-aligned.
    pub fn flush(&mut self, out: &mut Vec<u8>) {
        if self.nbits == 0 {
            return;
        }
        match self.order {
            BitOrder::MsbFirst => out.push((self.acc << (8 - self.nbits)) as u8),
            BitOrder::LsbFirst => out.push((self.acc & 0xFF) as u8),
        }
        self.acc = 0;
        self.nbits = 0;
    }
}

/// Incremental bit unpacker — the exact inverse of [`BitPacker`],
/// carrying a partial code across packet boundaries.
#[derive(Clone, Debug, Default)]
pub struct BitUnpacker {
    order: BitOrder,
    acc: u32,
    nbits: u32,
}

impl BitUnpacker {
    /// New unpacker with the given in-byte order.
    pub fn new(order: BitOrder) -> Self {
        BitUnpacker {
            order,
            acc: 0,
            nbits: 0,
        }
    }

    /// Feed `bytes` and emit every completed `bits`-wide code into
    /// `out`; up to `bits - 1` residual bits are retained for the next
    /// call.
    pub fn feed(&mut self, bytes: &[u8], bits: u8, out: &mut Vec<u8>) {
        let bits = bits as u32;
        for &byte in bytes {
            match self.order {
                BitOrder::MsbFirst => {
                    self.acc = (self.acc << 8) | byte as u32;
                    self.nbits += 8;
                    while self.nbits >= bits {
                        self.nbits -= bits;
                        out.push(((self.acc >> self.nbits) & ((1 << bits) - 1)) as u8);
                    }
                    self.acc &= (1 << self.nbits) - 1;
                }
                BitOrder::LsbFirst => {
                    self.acc |= (byte as u32) << self.nbits;
                    self.nbits += 8;
                    while self.nbits >= bits {
                        out.push((self.acc & ((1 << bits) - 1)) as u8);
                        self.acc >>= bits;
                        self.nbits -= bits;
                    }
                }
            }
        }
    }

    /// Number of residual bits buffered (0..=bits-1 after a `feed`).
    pub fn pending_bits(&self) -> u32 {
        self.nbits
    }

    /// Drop any residual bits (stream reset).
    pub fn reset(&mut self) {
        self.acc = 0;
        self.nbits = 0;
    }
}

/// Pack a whole buffer of right-aligned codes at `rate` into bytes; a
/// trailing partial byte is zero-padded.
pub fn pack_codes(codes: &[u8], rate: Rate, order: BitOrder) -> Vec<u8> {
    let mut out = Vec::with_capacity((codes.len() * rate.bits() as usize).div_ceil(8));
    let mut packer = BitPacker::new(order);
    for &c in codes {
        packer.push(c as u32, rate.bits(), &mut out);
    }
    packer.flush(&mut out);
    out
}

/// Unpack a whole buffer of bytes at `rate` into right-aligned codes;
/// trailing bits that do not fill a whole code are dropped.
pub fn unpack_codes(bytes: &[u8], rate: Rate, order: BitOrder) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() * 8 / rate.bits() as usize);
    let mut unpacker = BitUnpacker::new(order);
    unpacker.feed(bytes, rate.bits(), &mut out);
    out
}

// ---------------------------------------------------------------------------
// Packet-level helpers
// ---------------------------------------------------------------------------

/// Decode a packed G.726 byte stream continuing from `state` /
/// `unpacker`; returns 16-bit PCM. Residual sub-code bits stay in the
/// unpacker for the next packet (the stream is headerless and
/// continuous).
pub fn decode_packet(
    bytes: &[u8],
    state: &mut State,
    unpacker: &mut BitUnpacker,
    out: &mut Vec<i16>,
) {
    let mut codes = Vec::with_capacity(bytes.len() * 8 / state.rate.bits() as usize + 1);
    unpacker.feed(bytes, state.rate.bits(), &mut codes);
    out.reserve(codes.len());
    for c in codes {
        out.push(state.decode_i16(c));
    }
}

/// Encode 16-bit PCM continuing from `state` / `packer`; returns packed
/// bytes. Codes that do not complete a byte stay buffered in the packer
/// (call [`BitPacker::flush`] at end of stream).
pub fn encode_packet(pcm: &[i16], state: &mut State, packer: &mut BitPacker) -> Vec<u8> {
    let mut out = Vec::with_capacity((pcm.len() * state.rate.bits() as usize).div_ceil(8));
    for &s in pcm {
        let code = state.encode_i16(s);
        packer.push(code as u32, state.rate.bits(), &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_accessors_round_trip() {
        for &r in Rate::all() {
            assert_eq!(Rate::from_bits(r.bits()), Some(r));
            assert_eq!(r.bitrate(), 8000 * r.bits() as u32);
        }
        for bad in [0u8, 1, 6, 8, 255] {
            assert_eq!(Rate::from_bits(bad), None);
        }
    }

    #[test]
    fn reconst_levels_quantize_back_to_their_own_code() {
        // Lockstep between the decoder-side output levels (Tables
        // 11-14) and the encoder-side decision intervals (Tables 7-10):
        // every reconstruction level must fall inside the decision
        // interval of the code that produced it, for both signs.
        for &rate in Rate::all() {
            let n = 1u32 << rate.bits();
            for i in 0..n {
                let (dqln, dqs) = reconst(rate, i);
                if dqln == 2048 {
                    // Log-zero codes (DQLN = -2048) are re-quantized as
                    // the sign-flip special; skip the identity check.
                    continue;
                }
                let requant = quan(rate, dqs, dqln);
                assert_eq!(
                    requant, i,
                    "{rate:?}: level DQLN={dqln} of code {i} re-quantized to {requant}"
                );
            }
        }
    }

    #[test]
    fn quan_covers_full_dln_domain_without_panicking() {
        for &rate in Rate::all() {
            for ds in 0..2u32 {
                for dln in 0..4096u32 {
                    let i = quan(rate, ds, dln);
                    assert!(i <= rate.code_mask(), "{rate:?}: code {i} out of range");
                }
            }
        }
    }

    #[test]
    fn log_antilog_are_approximate_inverses() {
        // LOG uses log2(1+x) ≈ x and ANTILOG uses 2^x ≈ 1+x; composing
        // them through a zero scale factor reproduces the magnitude to
        // within the 7-bit mantissa truncation.
        for d in [1u32, 2, 3, 7, 15, 100, 999, 4095, 8191, 16383, 32767] {
            let (dl, ds) = log(d);
            assert_eq!(ds, 0);
            // DL is 11-bit SM; feed it to ANTILOG as a positive DQL.
            let dq = antilog(dl, 0);
            let mag = dq & 32767;
            let err = (mag as i64 - d as i64).abs();
            // Mantissa truncation bounds the relative error at 2^-7.
            let tol = (d as i64 >> 6).max(1);
            assert!(
                err <= tol,
                "d={d}: antilog(log(d))={mag}, err {err} > {tol}"
            );
        }
    }

    #[test]
    fn antilog_of_log_zero_is_zero() {
        // DQL sign bit set (log-domain negative = magnitude < 1)
        // reconstructs to zero.
        assert_eq!(antilog(2048, 0), 0);
        assert_eq!(antilog(4095, 0), 0);
        // Sign bit propagates even for zero magnitude.
        assert_eq!(antilog(2048, 1), 1 << 15);
    }

    #[test]
    fn limb_clamps_fast_scale_factor_to_spec_range() {
        assert_eq!(limb(0), 544);
        assert_eq!(limb(543), 544);
        assert_eq!(limb(544), 544);
        assert_eq!(limb(545), 545);
        assert_eq!(limb(5119), 5119);
        assert_eq!(limb(5120), 5120);
        assert_eq!(limb(5121), 5120);
        assert_eq!(limb(8191), 5120);
    }

    #[test]
    fn lima_limits_speed_control() {
        assert_eq!(lima(0), 0);
        assert_eq!(lima(255), 63);
        assert_eq!(lima(256), 64);
        assert_eq!(lima(1023), 64);
    }

    #[test]
    fn tone_detects_only_strong_negative_a2() {
        // TDP = 1 iff a2 in two's complement is in (-1, -0.71875]:
        // 32768 <= A2P < 53760.
        assert_eq!(tone(0), 0);
        assert_eq!(tone(12288), 0); // +0.75
        assert_eq!(tone(32767), 0);
        assert_eq!(tone(32768), 1);
        assert_eq!(tone(53759), 1);
        assert_eq!(tone(53760), 0); // -0.71875 exactly → off
        assert_eq!(tone(65535), 0);
    }

    #[test]
    fn fmult_zero_coefficient_product_is_mantissa_floor_bounded() {
        // §4.2.6 gives a zero coefficient the mantissa floor `1 << 5`
        // (AnMANT = 32 when AnMAG = 0) with AnEXP = 0, so the partial
        // product is not exactly zero for large-exponent signal words —
        // it is bounded by the floor: at the maximum SRnEXP = 15,
        // WAnMANT = (63·32 + 48) >> 4 = 129 and
        // WAnMAG = (129 << 7) >> 11 = 8. Pin that exact ceiling.
        for srn in [0u32, 32, 100, 500, 1023] {
            let w = fmult(0, srn);
            assert!(
                w & 32767 <= 8,
                "fmult(0, {srn}) magnitude {} above the mantissa-floor bound",
                w & 32767
            );
        }
        assert_eq!(fmult(0, 1023) & 32767, 8, "max-exponent floor product");
        assert_eq!(fmult(0, 32) & 32767, 0, "zero-value float word");
    }

    #[test]
    fn fmult_unit_coefficient_doubles_float_magnitude() {
        // An = 16384 encodes +1.0 (1 integer + 14 fraction bits). The
        // WAn partial products carry one extra fractional bit — ACCUM
        // restores Q0 with its final `>> 1` — so multiplying 1.0 by a
        // float word yields 2× the word's linear magnitude, to within
        // the 6-bit mantissa truncation, as long as the doubled value
        // still fits the 15-bit product magnitude (v <= 8191 here).
        for v in [1u32, 5, 63, 64, 100, 1000, 4095, 8191] {
            let f = floatb(v);
            let w = fmult(16384, f);
            assert_eq!(w >> 15, 0, "sign flipped for v={v}");
            let err = (w as i64 - 2 * v as i64).abs();
            // Tolerance covers the 6-bit mantissa truncation plus the
            // spec's `+ 48` product-rounding bias (worth up to 8 LSB at
            // small mantissas).
            let tol = ((2 * v as i64) >> 4).max(8);
            assert!(err <= tol, "v={v}: fmult(1.0, float(v))={w}, err {err}");
        }
        // Beyond that range the spec's WAnEXP > 26 branch wraps the
        // product magnitude modulo 2^15 rather than saturating — pin
        // the documented wraparound: 2·16383 ≡ 256 with the mantissa
        // rounding folded in.
        assert_eq!(fmult(16384, floatb(16383)), 256, "WAnEXP > 26 wrap");
    }

    #[test]
    #[allow(clippy::identity_op)] // 2·IMAG spelled out to mirror Table 15
    fn compress_matches_table15_examples() {
        // Table 15/G.726: conversion in the vicinity of the origin.
        // IMAG for A-law is the post-halving magnitude, so feed SR
        // values that reproduce it: positive IM = 2·IMAG (IM >> 1),
        // negative IM = 2·IMAG - 1 or 2·IMAG ((IM + 1) >> 1).
        // µ-law positive rows (IS = 0, IMAG = SR).
        assert_eq!(compress(3, Law::ULaw), 0b1111_1101);
        assert_eq!(compress(2, Law::ULaw), 0b1111_1110);
        assert_eq!(compress(1, Law::ULaw), 0b1111_1110);
        assert_eq!(compress(0, Law::ULaw), 0b1111_1111);
        // µ-law negative rows (IS = 1): SR = 65536 - IMAG.
        assert_eq!(compress(65536 - 1, Law::ULaw), 0b0111_1110);
        assert_eq!(compress(65536 - 2, Law::ULaw), 0b0111_1110);
        assert_eq!(compress(65536 - 3, Law::ULaw), 0b0111_1101);
        // A-law positive rows: IMAG = IM >> 1.
        assert_eq!(compress(2 * 3, Law::ALaw), 0b1101_0100);
        assert_eq!(compress(2 * 2, Law::ALaw), 0b1101_0100);
        assert_eq!(compress(2 * 1, Law::ALaw), 0b1101_0101);
        assert_eq!(compress(0, Law::ALaw), 0b1101_0101);
        // A-law negative rows: IMAG = (IM + 1) >> 1 ⇒ IM = 2·IMAG - 1.
        assert_eq!(compress(65536 - (2 * 1 - 1), Law::ALaw), 0b0101_0101);
        assert_eq!(compress(65536 - (2 * 2 - 1), Law::ALaw), 0b0101_0101);
        assert_eq!(compress(65536 - (2 * 3 - 1), Law::ALaw), 0b0101_0100);
    }

    #[test]
    fn sync_neighbors_match_table20_examples() {
        // Table 20/G.726: re-encoding in the vicinity of the origin.
        // ID < IM ⇒ SP+, ID > IM ⇒ SP−.
        let up = |sp: u8, law: Law| law_neighbor(sp, law, true);
        let dn = |sp: u8, law: Law| law_neighbor(sp, law, false);
        // A-law rows.
        assert_eq!(dn(0b1101_0101, Law::ALaw), 0b0101_0101);
        assert_eq!(up(0b1101_0101, Law::ALaw), 0b1101_0100);
        assert_eq!(dn(0b0101_0101, Law::ALaw), 0b0101_0100);
        assert_eq!(up(0b0101_0101, Law::ALaw), 0b1101_0101);
        assert_eq!(dn(0b0101_0100, Law::ALaw), 0b0101_0111);
        assert_eq!(up(0b0101_0100, Law::ALaw), 0b0101_0101);
        // µ-law rows — including the dual-zero skip.
        assert_eq!(dn(0b1111_1110, Law::ULaw), 0b1111_1111);
        assert_eq!(up(0b1111_1110, Law::ULaw), 0b1111_1101);
        assert_eq!(dn(0b1111_1111, Law::ULaw), 0b0111_1110);
        assert_eq!(up(0b1111_1111, Law::ULaw), 0b1111_1110);
        assert_eq!(dn(0b0111_1110, Law::ULaw), 0b0111_1101);
        assert_eq!(up(0b0111_1110, Law::ULaw), 0b0111_1111);
        // Ladder extremes clamp (SP+ / SP− constrained to SP).
        assert_eq!(up(0xFF ^ 0x55, Law::ALaw), 0xFF ^ 0x55); // most positive A-law
        assert_eq!(dn(0x7F ^ 0x55, Law::ALaw), 0x7F ^ 0x55); // most negative A-law
        assert_eq!(up(0x80, Law::ULaw), 0x80); // most positive µ-law (!0x7F)
        assert_eq!(dn(0x00, Law::ULaw), 0x00); // most negative µ-law (!0xFF)
    }

    #[test]
    fn expand_compress_round_trip_every_code_word() {
        // Every G.711 code word expands to a uniform value that
        // compresses back to the same code word (the decoder output
        // levels sit inside their own decision intervals).
        for law in [Law::ALaw, Law::ULaw] {
            for s in 0..=255u8 {
                let sl = expand(s, law);
                // 14-bit TC → 16-bit TC for COMPRESS input.
                let sr = if sl >> 13 == 0 { sl } else { 49152 + sl };
                let back = compress(sr, law);
                // µ-law has two zero codes (0xFF / 0x7F) that share
                // one output level; COMPRESS canonicalizes -0 to +0.
                if law == Law::ULaw && s == 0x7F {
                    assert_eq!(back, 0xFF, "µ-law -0 canonicalizes to +0");
                } else {
                    assert_eq!(back, s, "{law:?} code {s:#04x} round trip");
                }
            }
        }
    }

    #[test]
    fn state_reset_matches_table6_reset_column() {
        for &rate in Rate::all() {
            let s = State::new(rate);
            assert_eq!(s.yl, 34816);
            assert_eq!(s.yu, 544);
            assert_eq!(s.dms, 0);
            assert_eq!(s.dml, 0);
            assert_eq!(s.ap, 0);
            assert_eq!(s.a1, 0);
            assert_eq!(s.a2, 0);
            assert_eq!(s.b, [0; 6]);
            assert_eq!(s.dq, [32; 6]);
            assert_eq!(s.sr, [32; 2]);
            assert_eq!(s.pk1, 0);
            assert_eq!(s.pk2, 0);
            assert_eq!(s.td, 0);
            // Reset scale factor: Y = YL>>6 = 544 (AL = 0 ⇒ pure slow).
            assert_eq!(s.scale_factor(), 544);
        }
    }

    #[test]
    fn silence_encodes_to_near_zero_reconstruction() {
        // A run of zero input keeps the reconstruction near zero — the
        // quantizer emits minimum-magnitude codes and the scale factor
        // stays at its floor.
        for &rate in Rate::all() {
            let mut enc = State::new(rate);
            let mut dec = State::new(rate);
            for k in 0..256 {
                let code = enc.encode_step(0);
                let out = dec.decode_step(code);
                assert!(
                    out.abs() <= 12,
                    "{rate:?} sample {k}: silence reconstructed as {out}"
                );
            }
        }
    }

    #[test]
    fn decoder_tracks_encoder_reconstruction_exactly() {
        // The encoder embeds the decoder (§6 of the decode trace):
        // feeding its code stream to a fresh decoder must reproduce
        // *identical* internal state at every sample. Drive with a
        // deterministic multi-tone signal that sweeps amplitude.
        for &rate in Rate::all() {
            let mut enc = State::new(rate);
            let mut dec = State::new(rate);
            let mut phase = 0f64;
            for k in 0..2000 {
                let a = 6000.0 * (1.0 + (k as f64 / 300.0).sin()) / 2.0;
                phase += 0.35 + 0.1 * (k as f64 / 100.0).cos();
                let s = (a * phase.sin()) as i16;
                let code = enc.encode_i16(s);
                let _ = dec.decode_i16(code);
                assert_eq!(enc.yl, dec.yl, "{rate:?} k={k}: yl diverged");
                assert_eq!(enc.yu, dec.yu, "{rate:?} k={k}: yu diverged");
                assert_eq!(enc.a1, dec.a1, "{rate:?} k={k}: a1 diverged");
                assert_eq!(enc.a2, dec.a2, "{rate:?} k={k}: a2 diverged");
                assert_eq!(enc.b, dec.b, "{rate:?} k={k}: b diverged");
                assert_eq!(enc.dq, dec.dq, "{rate:?} k={k}: dq diverged");
                assert_eq!(enc.sr, dec.sr, "{rate:?} k={k}: sr diverged");
                assert_eq!(enc.ap, dec.ap, "{rate:?} k={k}: ap diverged");
            }
        }
    }

    #[test]
    fn state_invariants_hold_under_random_codes() {
        // Feed 20k pseudo-random codes at each rate; every internal
        // variable must stay inside its Table 6 word size.
        let mut seed = 0x1234_5678u32;
        let mut rng = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for &rate in Rate::all() {
            let mut st = State::new(rate);
            for k in 0..20_000 {
                let code = (rng() & rate.code_mask()) as u8;
                let _ = st.decode_step(code);
                assert!(st.yl < 524288, "{rate:?} k={k}: yl={} overflow", st.yl);
                assert!(
                    (544..=5120).contains(&st.yu),
                    "{rate:?} k={k}: yu={} outside LIMB range",
                    st.yu
                );
                assert!(st.dms < 4096, "{rate:?} k={k}: dms overflow");
                assert!(st.dml < 16384, "{rate:?} k={k}: dml overflow");
                assert!(st.ap < 1024, "{rate:?} k={k}: ap overflow");
                for (n, &dq) in st.dq.iter().enumerate() {
                    assert!(dq < 2048, "{rate:?} k={k}: dq[{n}] overflow");
                }
                for &sr in &st.sr {
                    assert!(sr < 2048, "{rate:?} k={k}: sr float overflow");
                }
            }
        }
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

    #[test]
    fn sine_round_trip_snr_ordered_by_rate() {
        // 800 Hz sine at 8 kHz, ~ -12 dBFS. Higher code widths must
        // reconstruct better; every rate must clear a basic floor.
        let pcm: Vec<i16> = (0..4000)
            .map(|k| {
                (8000.0 * (2.0 * std::f64::consts::PI * 800.0 * k as f64 / 8000.0).sin()) as i16
            })
            .collect();
        let mut snrs = Vec::new();
        for &rate in Rate::all() {
            let mut enc = State::new(rate);
            let mut dec = State::new(rate);
            let decoded: Vec<i16> = pcm
                .iter()
                .map(|&s| {
                    let c = enc.encode_i16(s);
                    dec.decode_i16(c)
                })
                .collect();
            // Skip the adaptation transient for the SNR window.
            let snr = snr_db(&pcm[500..], &decoded[500..]);
            snrs.push((rate, snr));
        }
        for w in snrs.windows(2) {
            assert!(
                w[1].1 > w[0].1,
                "SNR not monotonic in rate: {:?} {:.1} dB vs {:?} {:.1} dB",
                w[0].0,
                w[0].1,
                w[1].0,
                w[1].1
            );
        }
        let floor = [
            (Rate::R16, 5.0),
            (Rate::R24, 12.0),
            (Rate::R32, 18.0),
            (Rate::R40, 24.0),
        ];
        for ((rate, snr), (frate, fl)) in snrs.iter().zip(floor) {
            assert_eq!(*rate, frate);
            assert!(snr > &fl, "{rate:?}: SNR {snr:.1} dB below {fl} dB floor");
        }
    }

    #[test]
    fn set_rate_lockstep_encoder_decoder_stay_exact_across_switches() {
        // Appendix I.1 rate alternation: an encoder/decoder pair that
        // switches rates on the same sample schedule keeps identical
        // state — the Table 6 variables are rate-independent, so a
        // mid-stream switch must not disturb exact tracking.
        let schedule = [
            (Rate::R32, 400usize),
            (Rate::R16, 400),
            (Rate::R32, 400),
            (Rate::R24, 400),
            (Rate::R40, 400),
            (Rate::R16, 400),
        ];
        let mut enc = State::new(Rate::R32);
        let mut dec = State::new(Rate::R32);
        let mut k = 0usize;
        for &(rate, n) in &schedule {
            enc.set_rate(rate);
            dec.set_rate(rate);
            for _ in 0..n {
                let t = k as f64 / 8000.0;
                let s = (7000.0
                    * (0.4 + 0.6 * (std::f64::consts::PI * t * 3.0).sin().abs())
                    * (2.0 * std::f64::consts::PI * 520.0 * t).sin())
                    as i16;
                let code = enc.encode_i16(s);
                let _ = dec.decode_i16(code);
                assert_eq!(enc.yl, dec.yl, "{rate:?} k={k}: yl diverged");
                assert_eq!(enc.yu, dec.yu, "{rate:?} k={k}: yu diverged");
                assert_eq!(enc.a1, dec.a1, "{rate:?} k={k}: a1 diverged");
                assert_eq!(enc.a2, dec.a2, "{rate:?} k={k}: a2 diverged");
                assert_eq!(enc.b, dec.b, "{rate:?} k={k}: b diverged");
                assert_eq!(enc.dq, dec.dq, "{rate:?} k={k}: dq diverged");
                assert_eq!(enc.sr, dec.sr, "{rate:?} k={k}: sr diverged");
                k += 1;
            }
        }
    }

    #[test]
    fn set_rate_alternation_beats_the_lower_pure_rate() {
        // The DCME 32k/24k alternation of Appendix I.1 should land
        // between the two pure rates in quality — assert it at least
        // clears the pure-24k floor on a tonal signal.
        let pcm: Vec<i16> = (0..4000)
            .map(|k| {
                (8000.0 * (2.0 * std::f64::consts::PI * 700.0 * k as f64 / 8000.0).sin()) as i16
            })
            .collect();
        let run = |rates: &dyn Fn(usize) -> Rate| -> f64 {
            let mut enc = State::new(rates(0));
            let mut dec = State::new(rates(0));
            let decoded: Vec<i16> = pcm
                .iter()
                .enumerate()
                .map(|(k, &s)| {
                    enc.set_rate(rates(k));
                    dec.set_rate(rates(k));
                    let c = enc.encode_i16(s);
                    dec.decode_i16(c)
                })
                .collect();
            snr_db(&pcm[500..], &decoded[500..])
        };
        let pure24 = run(&|_| Rate::R24);
        let pure32 = run(&|_| Rate::R32);
        // Alternate 32/24 per sample (≈3.5 bits/sample average).
        let alt = run(&|k| if k % 2 == 0 { Rate::R32 } else { Rate::R24 });
        assert!(
            alt > pure24,
            "32/24 alternation ({alt:.1} dB) below pure 24k ({pure24:.1} dB)"
        );
        assert!(
            alt < pure32 + 1.0,
            "32/24 alternation ({alt:.1} dB) implausibly above pure 32k ({pure32:.1} dB)"
        );
    }

    #[test]
    fn reset_restores_table6_state_and_keeps_rate() {
        let mut st = State::new(Rate::R40);
        for k in 0..500 {
            let _ = st.encode_i16(((k * 37) % 12000) as i16 - 6000);
        }
        assert_ne!(st.yu, 544, "state should have adapted before reset");
        st.reset();
        let fresh = State::new(Rate::R40);
        assert_eq!(st.rate(), Rate::R40);
        assert_eq!(st.yl, fresh.yl);
        assert_eq!(st.yu, fresh.yu);
        assert_eq!(st.dms, fresh.dms);
        assert_eq!(st.dml, fresh.dml);
        assert_eq!(st.ap, fresh.ap);
        assert_eq!(st.a1, fresh.a1);
        assert_eq!(st.a2, fresh.a2);
        assert_eq!(st.b, fresh.b);
        assert_eq!(st.dq, fresh.dq);
        assert_eq!(st.sr, fresh.sr);
        // And the stream restarts deterministically.
        let mut reference = State::new(Rate::R40);
        for s in [100i16, -350, 4000, -8000, 12345] {
            assert_eq!(st.encode_i16(s), reference.encode_i16(s));
        }
    }

    #[test]
    fn pack_unpack_round_trip_both_orders_all_rates() {
        let mut seed = 0xDEAD_BEEFu32;
        let mut rng = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for &rate in Rate::all() {
            for &order in &[BitOrder::MsbFirst, BitOrder::LsbFirst] {
                for n in [0usize, 1, 7, 8, 39, 40, 41, 800] {
                    let codes: Vec<u8> = (0..n).map(|_| (rng() & rate.code_mask()) as u8).collect();
                    let bytes = pack_codes(&codes, rate, order);
                    assert_eq!(
                        bytes.len(),
                        (n * rate.bits() as usize).div_ceil(8),
                        "{rate:?} {order:?} n={n}: packed length"
                    );
                    let back = unpack_codes(&bytes, rate, order);
                    // Zero-padding may synthesize trailing zero codes.
                    assert!(back.len() >= codes.len());
                    assert_eq!(&back[..codes.len()], &codes[..], "{rate:?} {order:?} n={n}");
                    for &extra in &back[codes.len()..] {
                        assert_eq!(extra, 0, "{rate:?} {order:?} n={n}: pad code non-zero");
                    }
                }
            }
        }
    }

    #[test]
    fn incremental_unpacker_matches_whole_buffer_across_splits() {
        // Codes that straddle byte AND packet boundaries reassemble
        // identically to the one-shot path.
        let mut seed = 0x0BAD_F00Du32;
        let mut rng = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for &rate in Rate::all() {
            for &order in &[BitOrder::MsbFirst, BitOrder::LsbFirst] {
                let bytes: Vec<u8> = (0..97).map(|_| (rng() & 0xFF) as u8).collect();
                let whole = unpack_codes(&bytes, rate, order);
                for split in [1usize, 2, 3, 5, 7, 48, 96] {
                    let mut un = BitUnpacker::new(order);
                    let mut got = Vec::new();
                    for chunk in bytes.chunks(split) {
                        un.feed(chunk, rate.bits(), &mut got);
                    }
                    assert_eq!(got, whole, "{rate:?} {order:?} split={split}");
                }
            }
        }
    }

    #[test]
    fn streaming_packet_helpers_are_inverse_across_packet_splits() {
        // encode_packet / decode_packet carry bit + codec state across
        // arbitrary frame splits; the reassembled decode must equal the
        // unsplit decode exactly.
        let pcm: Vec<i16> = (0..1603)
            .map(|k| {
                (7000.0 * (2.0 * std::f64::consts::PI * 433.0 * k as f64 / 8000.0).sin()) as i16
            })
            .collect();
        for &rate in Rate::all() {
            for &order in &[BitOrder::MsbFirst, BitOrder::LsbFirst] {
                // One-shot reference.
                let mut enc = State::new(rate);
                let mut packer = BitPacker::new(order);
                let mut bytes = encode_packet(&pcm, &mut enc, &mut packer);
                packer.flush(&mut bytes);
                let mut dec = State::new(rate);
                let mut un = BitUnpacker::new(order);
                let mut reference = Vec::new();
                decode_packet(&bytes, &mut dec, &mut un, &mut reference);
                assert!(reference.len() >= pcm.len());
                // Split encode at awkward boundaries.
                let mut enc2 = State::new(rate);
                let mut packer2 = BitPacker::new(order);
                let mut bytes2 = Vec::new();
                for chunk in pcm.chunks(97) {
                    bytes2.extend_from_slice(&encode_packet(chunk, &mut enc2, &mut packer2));
                }
                packer2.flush(&mut bytes2);
                assert_eq!(bytes2, bytes, "{rate:?} {order:?}: split encode differs");
                // Split decode at awkward boundaries.
                let mut dec2 = State::new(rate);
                let mut un2 = BitUnpacker::new(order);
                let mut got = Vec::new();
                for chunk in bytes.chunks(13) {
                    decode_packet(chunk, &mut dec2, &mut un2, &mut got);
                }
                assert_eq!(got, reference, "{rate:?} {order:?}: split decode differs");
            }
        }
    }
}
