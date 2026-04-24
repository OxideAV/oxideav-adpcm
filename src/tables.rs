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
pub const IMA_INDEX_ADJUST: [i32; 16] = [
    -1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8,
];

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
pub const YAMAHA_INDEX_SCALE: [i32; 8] = [230, 230, 230, 230, 307, 409, 512, 614];

/// Minimum step value (Y8950 manual: Δ saturates at 127).
pub const YAMAHA_STEP_MIN: i32 = 127;
/// Maximum step value (Y8950 manual: Δ saturates at 24576).
pub const YAMAHA_STEP_MAX: i32 = 24576;

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
    fn yamaha_tables_have_expected_shape() {
        assert_eq!(YAMAHA_DIFF_LOOKUP.len(), 8);
        assert_eq!(YAMAHA_INDEX_SCALE.len(), 8);
        assert_eq!(YAMAHA_DIFF_LOOKUP[0], 1);
        assert_eq!(YAMAHA_DIFF_LOOKUP[7], 15);
    }
}
