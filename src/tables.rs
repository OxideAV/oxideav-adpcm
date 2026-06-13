//! Normative ADPCM spec tables.
//!
//! All constants in this module are **uncopyrightable facts** transcribed
//! from the relevant public specs — see [`crate`] docs for the source
//! citation for each group.

// ---------------- Microsoft ADPCM ----------------
//
// Source: Microsoft ADPCM (WAVEFORMATEX tag 0x0002) algorithm — see
// <https://wiki.multimedia.cx/index.php/Microsoft_ADPCM>.

/// 16-entry delta-adaptation multipliers, indexed by the raw 4-bit nibble.
///
/// The per-step update is `delta' = (delta * ADAPTATION[nibble]) / 256`,
/// saturated to a minimum of 16.
pub const MS_ADAPTATION: [i32; 16] = [
    230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230,
];

/// 7-entry default Coefficient 1 (weight of `sample1`) — divisor 256.
pub const MS_ADAPT_COEFF1: [i32; 7] = [256, 512, 0, 192, 240, 460, 392];

/// 7-entry default Coefficient 2 (weight of `sample2`) — divisor 256.
pub const MS_ADAPT_COEFF2: [i32; 7] = [0, -256, 0, 64, 0, -208, -232];

// ---------------- IMA ADPCM ----------------
//
// Sources:
// * <https://wiki.multimedia.cx/index.php/IMA_ADPCM>
// * IMA "Recommended Practices for Digital Audio — DVI Standard Version".
//
// These two tables are identical across every IMA variant we handle (WAV,
// QuickTime, and any future IMA flavour).

/// 89-entry quantiser step-size table. Indices 0..=88 are valid.
pub const IMA_STEP_SIZE: [i16; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

/// 16-entry index-adjustment table. Indexed by the 4-bit nibble.
pub const IMA_INDEX_ADJUST: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];

/// 8-entry index-adjustment table for **3-bit** IMA / DVI ADPCM, indexed
/// by the full 3-bit code (1 sign + 2 magnitude bits).
///
/// The 3-bit mode (WAV `wBitsPerSample = 3` on tag `0x0011`) shares the
/// 89-entry [`IMA_STEP_SIZE`] table with the 4-bit mode but uses this
/// smaller adjustment table: the two low-magnitude codes shrink the
/// index by 1, the two high-magnitude codes grow it by 1 / 2. As with
/// the 4-bit table, the sign bit does not affect the adjustment —
/// entries `0..=3` mirror `4..=7`.
pub const IMA3_INDEX_ADJUST: [i32; 8] = [-1, -1, 1, 2, -1, -1, 1, 2];

// ---------------- Yamaha ADPCM ----------------
//
// Source: Yamaha Y8950 (MSX-AUDIO) Application Manual, section I-4 "Outline
// of ADPCM Voice Analysis/Synthesis", Table I-2 "Quantization width rate of
// change".  The decode recurrence published there is
//
//   X(n+1) = X(n) + (1 - 2*L4) * (L3 + L2/2 + L1/4 + 1/8) * Δ(n)
//   Δ(n+1) = f(L3, L2, L1) * Δ(n)
//
// with the f() multiplier listed in Table I-2. Converting the published
// decimals to the int/256 form used below:
//
//   0.9 → 230/256    1.2 → 307/256    1.6 → 410/256
//   2.0 → 512/256    2.4 → 614/256
//
// Per the manual the step size is clamped to the Δ range [127, 24576] —
// this crate uses the standard 127..=24576 published bounds.

/// 8-entry magnitude contribution lookup.
///
/// The decoder output contribution for nibble magnitude `m ∈ 0..=7` is
/// `(YAMAHA_DIFF_LOOKUP[m] * step) >> 3`. The table encodes `8*(m + 0.5)`
/// = `8*L3 + 4*L2 + 2*L1 + 1` from the Y8950 recurrence (so m=0 → 1,
/// m=1 → 3, m=7 → 15). The sign is applied separately from the high bit of
/// the nibble (L4).
pub const YAMAHA_DIFF_LOOKUP: [i32; 8] = [1, 3, 5, 7, 9, 11, 13, 15];

/// 8-entry step-adaptation multiplier, indexed by |nibble| (low 3 bits).
/// `step' = (step * YAMAHA_INDEX_SCALE[mag]) >> 8`, then clamped to
/// `[STEP_MIN, STEP_MAX]`.
///
/// These are the AICA FQ8005 / Y8950 constants: the manual's decimal
/// change-rates `{0.8984375, 1.19921875, 1.59765625, 2.0, 2.3984375}`
/// multiplied by 256 (`0.8984375·256 = 230`, `2.3984375·256 = 614`).
/// The YM2608 OPNA datasheet prints *slightly different* exact fractions
/// for the same curve — see [`YAMAHA_INDEX_SCALE_OPNA`].
pub const YAMAHA_INDEX_SCALE: [i32; 8] = [230, 230, 230, 230, 307, 409, 512, 614];

/// 8-entry step-adaptation multiplier for the **YM2608 (OPNA)** chip,
/// indexed by |nibble| (low 3 bits). `step' = (step *
/// YAMAHA_INDEX_SCALE_OPNA[mag]) >> 6`, then clamped to
/// `[STEP_MIN, STEP_MAX]`.
///
/// Transcribed verbatim from the *YM2608 (OPNA) Application Manual*,
/// Table 5-1 ("ADPCM data and quantization-width change rate"), which
/// lists the change rate `f` as the fractions
/// `{57/64, 77/64, 102/64, 128/64, 153/64}` (×64 numerators below):
///
/// | `L3 L2 L1` | `f` (Table 5-1) |
/// |------------|-----------------|
/// | 000…011    | 57/64  ≈ 0.890625 |
/// | 100        | 77/64  ≈ 1.203125 |
/// | 101        | 102/64 ≈ 1.59375  |
/// | 110        | 128/64 = 2.0      |
/// | 111        | 153/64 ≈ 2.390625 |
///
/// Distinct from [`YAMAHA_INDEX_SCALE`] (the AICA/Y8950 rounding of the
/// same `~1.1^M` curve): the OPNA small-magnitude multiplier is 57/64 =
/// 0.890625 vs AICA's 115/128 = 0.8984375, and the large-magnitude
/// entries differ in the low bits too. Pick the table for the chip being
/// emulated — see [`crate::yamaha::Chip`].
pub const YAMAHA_INDEX_SCALE_OPNA: [i32; 8] = [57, 57, 57, 57, 77, 102, 128, 153];

/// Minimum step value (Y8950 manual: Δ saturates at 127).
pub const YAMAHA_STEP_MIN: i32 = 127;
/// Maximum step value (Y8950 manual: Δ saturates at 24576).
pub const YAMAHA_STEP_MAX: i32 = 24576;

// ---------------- OKI / Dialogic ADPCM (VOX) ----------------
//
// Source: Dialogic Corporation, *Dialogic ADPCM Algorithm*, doc 00-1366-001
// (1988). The 49-entry calculated-step-size table is Table 2 of the app
// note; the 8-entry magnitude-indexed adjustment is the row-collapsed form
// of Table 1 (the sign bit `B3` is ignored when looking up `M`, so codes
// `0xxx` and `1xxx` with the same magnitude share a row).
//
// [`IMA_STEP_SIZE`] above shares the same `~1.1^n` step-size geometry
// but starts 8 entries earlier (at value 7); the Dialogic Table 2
// values 16..1552 correspond to `IMA_STEP_SIZE[8..57]`. The OKI variant
// uses a different magnitude→adjust mapping (-1/-1/-1/-1/+2/+4/+6/+8
// vs IMA's -1/-1/-1/-1/+2/+4/+6/+8 — same numerically) but a smaller
// table and 12-bit clamping. We expose the OKI 49-entry table as its
// own constant so callers can verify shape directly against the
// Dialogic app note.

/// 49-entry calculated step-size table (Dialogic app note Table 2).
///
/// Indexed by the running step pointer (Dialogic numbers them "entry 1..49";
/// we 0-index here). After applying `OKI_INDEX_ADJUST` the pointer is
/// clamped to `0..=48`.
pub const OKI_STEP_SIZE: [i16; 49] = [
    16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130,
    143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796,
    876, 963, 1060, 1166, 1282, 1411, 1552,
];

/// 8-entry magnitude-indexed step-pointer adjustment (Dialogic app note
/// Table 1, row-collapsed: the sign bit is dropped, leaving the 3-bit
/// magnitude 0..=7 as the index). Codes with magnitude < 4 shrink the
/// step; codes with magnitude ≥ 4 grow it.
pub const OKI_INDEX_ADJUST: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

/// Minimum value of the running step pointer after `OKI_INDEX_ADJUST` is
/// applied (Dialogic Table 2 entry numbering starts at 1; here 0).
pub const OKI_STEP_INDEX_MIN: i32 = 0;
/// Maximum value of the running step pointer (Dialogic Table 2 last entry
/// — 1411 sits at index 47, the final entry 1552 at index 48).
pub const OKI_STEP_INDEX_MAX: i32 = 48;

/// Lower bound of the 12-bit signed reconstructed predictor `X`. OKI MSM
/// silicon clamps the running reconstruction to a 12-bit signed range; the
/// app-note pseudocode does not list a bound but the chips do.
pub const OKI_PREDICTOR_MIN: i32 = -2048;
/// Upper bound of the 12-bit signed reconstructed predictor `X`.
pub const OKI_PREDICTOR_MAX: i32 = 2047;

// ---------------- Yamaha ADPCM-A (YM2610 rhythm channels) ----------------
//
// Yamaha shipped *two* related but distinct 4-bit ADPCM schemes; the
// ADPCM-B tables above target the variable-rate single-channel codec on
// the Y8950 / YM2608-B / AICA / YMZ280B. The constants in this block
// target the **second** Yamaha scheme — **ADPCM-A**, the 6 fixed-rate
// rhythm/percussion channels unique to the YM2610 (and the YM2608's
// rhythm ROM). Per the staged trace doc
// `audio/adpcm/yamaha/yamaha-adpcm.md` §3, the exact ADPCM-A step table
// is **not** printed numerically in the vendor datasheets — it is the
// independent-RE consensus of the NeoGeo Development Wiki and the
// MAME/ymfm hardware reverse-engineering effort, verified against real
// YM2608/YM2610 silicon. The provenance is RE-of-hardware, not
// extraction from any general-purpose multimedia decoder.
//
// 12-bit signed reconstructed signal (`-2048 ..= 2047`), 4-bit nibble
// (`s mmm`: 1 sign + 3 magnitude bits), 49-entry step table (identical
// to OKI/Dialogic Table 2), 16-entry index adjustment indexed by the
// full 4-bit nibble. The index-adjust differs from OKI's variant:
// ADPCM-A uses `{-1,-1,-1,-1, 2, 5, 7, 9, ...}` whereas OKI uses
// `{-1,-1,-1,-1, 2, 4, 6, 8}` — the magnitude-7 step pointer grows by
// 9 (not 8) per the YM2610 silicon RE.
//
// Per-sample decode recurrence (from the trace doc):
//
//     delta = (step_size[idx] * (mmm * 2 + 1)) / 8           // (mmm + 0.5) form
//     if sign:  acc -= delta   else   acc += delta
//     acc  = clamp(acc, -2048, 2047)                         // 12-bit signed
//     idx  = clamp(idx + step_adj[nibble], 0, 48)
//
// The `(mmm * 2 + 1) / 8` form (= `mmm / 4 + 1/16` after the >>3) is the
// chip's rounding rule for the residual contribution. This matches the
// "(step_size[idx] * mmm) / 8 + step_size[idx] / 16" identity in the
// trace doc (both expand to the same integer formula once distributed).

/// 49-entry step-size table for Yamaha ADPCM-A.
///
/// Numerically identical to [`OKI_STEP_SIZE`] (both derive from the same
/// `~16 * 1.1^x` step-size geometry); kept as a separate constant for
/// docs / call-site provenance clarity. The pointer is clamped to
/// `0 ..= 48` after each [`YAMAHA_A_INDEX_ADJUST`] update.
pub const YAMAHA_A_STEP_SIZE: [i16; 49] = [
    16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130,
    143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796,
    876, 963, 1060, 1166, 1282, 1411, 1552,
];

/// 16-entry index-adjustment table for Yamaha ADPCM-A, indexed by the
/// raw 4-bit nibble. The four small-magnitude codes shrink the pointer
/// by 1; the four large-magnitude codes grow it by `{2, 5, 7, 9}`
/// (vs OKI's `{2, 4, 6, 8}`). The high (sign) bit doesn't affect the
/// adjustment — entries `0..7` mirror `8..15`.
pub const YAMAHA_A_INDEX_ADJUST: [i32; 16] =
    [-1, -1, -1, -1, 2, 5, 7, 9, -1, -1, -1, -1, 2, 5, 7, 9];

/// Minimum value of the running step pointer (49-entry table starts at
/// index 0 = step value 16).
pub const YAMAHA_A_STEP_INDEX_MIN: i32 = 0;
/// Maximum value of the running step pointer (49-entry table ends at
/// index 48 = step value 1552).
pub const YAMAHA_A_STEP_INDEX_MAX: i32 = 48;

/// Lower bound of the 12-bit signed reconstructed sample.
pub const YAMAHA_A_PREDICTOR_MIN: i32 = -2048;
/// Upper bound of the 12-bit signed reconstructed sample.
pub const YAMAHA_A_PREDICTOR_MAX: i32 = 2047;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ms_tables_have_expected_shape() {
        assert_eq!(MS_ADAPTATION.len(), 16);
        assert_eq!(MS_ADAPT_COEFF1.len(), 7);
        assert_eq!(MS_ADAPT_COEFF2.len(), 7);
        // Adaptation table is symmetric around index 7/8 in the sense
        // that indices 0..=7 and 15..=8 form mirror images (the sign bit
        // of the nibble flips direction but the magnitude step is the
        // same). Spot-check both ends of the table.
        assert_eq!(MS_ADAPTATION[0], MS_ADAPTATION[15]);
        assert_eq!(MS_ADAPTATION[7], 614);
        assert_eq!(MS_ADAPTATION[8], 768);
    }

    #[test]
    fn ima_tables_have_expected_shape() {
        assert_eq!(IMA_STEP_SIZE.len(), 89);
        assert_eq!(IMA_INDEX_ADJUST.len(), 16);
        assert_eq!(IMA_STEP_SIZE[0], 7);
        assert_eq!(IMA_STEP_SIZE[88], 32767);
        // Small magnitudes reduce the index; large ones increase it.
        assert_eq!(IMA_INDEX_ADJUST[0], -1);
        assert_eq!(IMA_INDEX_ADJUST[7], 8);
    }

    #[test]
    fn ima3_index_adjust_has_expected_shape() {
        assert_eq!(IMA3_INDEX_ADJUST.len(), 8);
        // Sign bit (code bit 2) does not affect the adjustment: the lower
        // half mirrors the upper half.
        for m in 0..4 {
            assert_eq!(IMA3_INDEX_ADJUST[m], IMA3_INDEX_ADJUST[m + 4]);
        }
        // Low magnitudes shrink the index; high magnitudes grow it.
        assert_eq!(IMA3_INDEX_ADJUST[0], -1);
        assert_eq!(IMA3_INDEX_ADJUST[1], -1);
        assert_eq!(IMA3_INDEX_ADJUST[2], 1);
        assert_eq!(IMA3_INDEX_ADJUST[3], 2);
    }

    #[test]
    fn yamaha_tables_have_expected_shape() {
        assert_eq!(YAMAHA_DIFF_LOOKUP.len(), 8);
        assert_eq!(YAMAHA_INDEX_SCALE.len(), 8);
        assert_eq!(YAMAHA_DIFF_LOOKUP[0], 1);
        assert_eq!(YAMAHA_DIFF_LOOKUP[7], 15);
    }

    #[test]
    fn yamaha_opna_index_scale_matches_table_5_1() {
        // YM2608 OPNA Application Manual Table 5-1: change rate f as the
        // fractions {57/64, 77/64, 102/64, 128/64, 153/64}; below stored
        // as the ×64 numerators (update shifts >> 6).
        assert_eq!(YAMAHA_INDEX_SCALE_OPNA.len(), 8);
        assert_eq!(YAMAHA_INDEX_SCALE_OPNA, [57, 57, 57, 57, 77, 102, 128, 153]);
        // The four small-magnitude codes share the single down-rate 57/64.
        for m in 0..4 {
            assert_eq!(YAMAHA_INDEX_SCALE_OPNA[m], 57);
        }
        // 128/64 is exactly 2.0 — the only entry that is an exact integer
        // multiplier in both the OPNA and AICA roundings.
        assert_eq!(YAMAHA_INDEX_SCALE_OPNA[6], 128);
        // The OPNA and AICA tables are distinct roundings of the same
        // curve: small-magnitude differs (57/64 vs 230/256) but the 2.0
        // entry agrees once both are scaled to a common denominator
        // (128/64 = 512/256).
        assert_ne!(
            YAMAHA_INDEX_SCALE_OPNA[0] * 4,
            YAMAHA_INDEX_SCALE[0],
            "57*4=228 != 230: OPNA and AICA small-mag rounding must differ"
        );
        assert_eq!(
            YAMAHA_INDEX_SCALE_OPNA[6] * 4,
            YAMAHA_INDEX_SCALE[6],
            "128/64 and 512/256 both equal 2.0"
        );
    }

    #[test]
    fn oki_tables_have_expected_shape() {
        assert_eq!(OKI_STEP_SIZE.len(), 49);
        assert_eq!(OKI_INDEX_ADJUST.len(), 8);
        // First and last spec-listed entries from Dialogic app note Table 2.
        assert_eq!(OKI_STEP_SIZE[0], 16);
        assert_eq!(OKI_STEP_SIZE[47], 1411);
        assert_eq!(OKI_STEP_SIZE[48], 1552);
        // Index-adjust contract: four small mags shrink, four large mags grow.
        assert_eq!(OKI_INDEX_ADJUST[0], -1);
        assert_eq!(OKI_INDEX_ADJUST[3], -1);
        assert_eq!(OKI_INDEX_ADJUST[4], 2);
        assert_eq!(OKI_INDEX_ADJUST[7], 8);
    }

    #[test]
    fn yamaha_a_tables_have_expected_shape() {
        // 49-entry step table, 16-entry adjust table (sign-bit-mirrored
        // halves), 12-bit predictor clamp range.
        assert_eq!(YAMAHA_A_STEP_SIZE.len(), 49);
        assert_eq!(YAMAHA_A_INDEX_ADJUST.len(), 16);
        // First/last entries from the RE-derived table.
        assert_eq!(YAMAHA_A_STEP_SIZE[0], 16);
        assert_eq!(YAMAHA_A_STEP_SIZE[48], 1552);
        // Adjust table: lower half mirrors upper half (sign bit doesn't
        // affect step adjustment).
        for m in 0..8 {
            assert_eq!(YAMAHA_A_INDEX_ADJUST[m], YAMAHA_A_INDEX_ADJUST[m + 8]);
        }
        // Small magnitudes shrink; large magnitudes grow by 2/5/7/9.
        assert_eq!(YAMAHA_A_INDEX_ADJUST[0], -1);
        assert_eq!(YAMAHA_A_INDEX_ADJUST[3], -1);
        assert_eq!(YAMAHA_A_INDEX_ADJUST[4], 2);
        assert_eq!(YAMAHA_A_INDEX_ADJUST[5], 5);
        assert_eq!(YAMAHA_A_INDEX_ADJUST[6], 7);
        assert_eq!(YAMAHA_A_INDEX_ADJUST[7], 9);
        // 12-bit predictor range.
        assert_eq!(YAMAHA_A_PREDICTOR_MIN, -2048);
        assert_eq!(YAMAHA_A_PREDICTOR_MAX, 2047);
    }

    #[test]
    fn yamaha_a_step_table_matches_oki_step_table_numerically() {
        // Both ADPCM-A and OKI/Dialogic VOX use the same 49-entry
        // `~16 * 1.1^x` step-size geometry; the index-adjust tables
        // differ but the step values are identical.
        for i in 0..49 {
            assert_eq!(
                YAMAHA_A_STEP_SIZE[i], OKI_STEP_SIZE[i],
                "Yamaha ADPCM-A vs OKI step table mismatch at index {i}"
            );
        }
    }

    #[test]
    fn yamaha_a_adjust_differs_from_oki_adjust_at_large_magnitudes() {
        // Documents the one numeric distinction between the two related
        // tables: the magnitude-5/6/7 step growth differs.
        // OKI: {2, 4, 6, 8}; ADPCM-A: {2, 5, 7, 9}.
        assert_eq!(OKI_INDEX_ADJUST[4], 2);
        assert_eq!(YAMAHA_A_INDEX_ADJUST[4], 2);
        assert_eq!(OKI_INDEX_ADJUST[5], 4);
        assert_eq!(YAMAHA_A_INDEX_ADJUST[5], 5);
        assert_eq!(OKI_INDEX_ADJUST[6], 6);
        assert_eq!(YAMAHA_A_INDEX_ADJUST[6], 7);
        assert_eq!(OKI_INDEX_ADJUST[7], 8);
        assert_eq!(YAMAHA_A_INDEX_ADJUST[7], 9);
    }

    #[test]
    fn oki_step_size_is_ima_step_size_8_to_57() {
        // Dialogic Table 2 (49 entries, starting at value 16) is the
        // 8..57 slice of IMA's 89-entry table (which starts at 7, with
        // 8 small-magnitude pre-roll entries). Confirms both
        // transcriptions match the shared textbook step geometry.
        for i in 0..49 {
            assert_eq!(
                OKI_STEP_SIZE[i] as i32,
                IMA_STEP_SIZE[i + 8] as i32,
                "OKI[{i}] vs IMA[{}]",
                i + 8
            );
        }
    }
}
