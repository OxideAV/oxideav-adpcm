//! Microsoft ADPCM decoder (`adpcm_ms`, WAVEFORMATEX tag `0x0002`).
//!
//! Algorithm (transcribed from the public Microsoft ADPCM spec):
//!
//! 1. Each block starts with a header: per-channel predictor index byte,
//!    then per-channel 16-bit signed initial delta, then two 16-bit signed
//!    initial samples `sample1` and `sample2` (sample1 is decoded first).
//! 2. The rest of the block is a stream of 4-bit nibbles, **hi nibble
//!    first** within each byte, interleaved by channel in the stereo case.
//! 3. For each nibble:
//!    - `signed_nibble = (nibble ^ 8) - 8`  (4-bit two's complement)
//!    - `predicted = (sample1 * coef1 + sample2 * coef2) / 256`
//!    - `new_sample = predicted + signed_nibble * delta`
//!    - clamp to i16, append as output
//!    - shift history: `sample2 = sample1; sample1 = new_sample`
//!    - `delta = max(16, (delta * adaptation[nibble]) / 256)`
//!
//! The block lays down `sample2` then `sample1` as the first two PCM
//! outputs before any nibble is processed, so each block produces
//! `2 + 2 * (body_bytes / channels)` samples per channel (2 prelude + one
//! sample per nibble, each body byte carrying two nibbles).

use crate::tables::{MS_ADAPTATION, MS_ADAPT_COEFF1, MS_ADAPT_COEFF2};
use oxideav_core::{Error, Result};

/// One Microsoft-ADPCM predictor coefficient set: `(iCoef1, iCoef2)`, the
/// 8.8 fixed-point weights of `sample1` / `sample2` (the spec stores them
/// as signed values to be divided by 256 — see [`crate::ms`] module docs).
pub type CoefPair = (i32, i32);

/// The seven standard preset coefficient pairs (`aCoeff` rows 0..=6) every
/// Microsoft-ADPCM stream begins with, as
/// `(iCoef1, iCoef2)`. A file that declares no extra coefficient sets in
/// its `WAVEFORMATEX` trailer uses exactly these; a file that adds custom
/// sets must still list these first seven unchanged (per the spec).
pub const STANDARD_COEFFS: [CoefPair; 7] = [
    (MS_ADAPT_COEFF1[0], MS_ADAPT_COEFF2[0]),
    (MS_ADAPT_COEFF1[1], MS_ADAPT_COEFF2[1]),
    (MS_ADAPT_COEFF1[2], MS_ADAPT_COEFF2[2]),
    (MS_ADAPT_COEFF1[3], MS_ADAPT_COEFF2[3]),
    (MS_ADAPT_COEFF1[4], MS_ADAPT_COEFF2[4]),
    (MS_ADAPT_COEFF1[5], MS_ADAPT_COEFF2[5]),
    (MS_ADAPT_COEFF1[6], MS_ADAPT_COEFF2[6]),
];

/// Parse a Microsoft-ADPCM `WAVEFORMATEX` trailer (the bytes that follow
/// the 16/18-byte `WAVEFORMATEX` base, i.e. the codec `extradata`) into the
/// declared coefficient table.
///
/// The trailer layout (spec `ADPCMWAVEFORMAT`, after the `WAVEFORMATEX`
/// base) is:
///
/// ```text
/// WORD               wSamplesPerBlock   // count of samples per block
/// WORD               wNumCoef           // number of coefficient sets
/// ADPCMCOEFSET       aCoeff[wNumCoef]   // each: i16 iCoef1, i16 iCoef2
/// ```
///
/// All fields are little-endian. The coefficients are stored as 16-bit
/// signed values (the spec's `int` members; `cbSize = 32` for the standard
/// seven pairs ⇒ 2 + 2 + 7·4 = 32 bytes ⇒ 2 bytes per coefficient).
///
/// Returns:
/// - `Ok(None)` when `extradata` is empty (or shorter than the 4-byte
///   `wSamplesPerBlock` + `wNumCoef` preamble) — the caller falls back to
///   [`STANDARD_COEFFS`].
/// - `Ok(Some(table))` with `wNumCoef` parsed pairs.
/// - `Err` when the declared `wNumCoef` would run past the end of
///   `extradata` (truncated table) or fewer than the mandatory seven sets
///   are declared (`wNumCoef < 7`), or any of the declared first seven
///   pairs disagrees with the spec presets (the spec requires the first
///   seven to be exactly the standard table).
pub fn parse_extradata_coeffs(extradata: &[u8]) -> Result<Option<Vec<CoefPair>>> {
    if extradata.len() < 4 {
        // No (or too-short) trailer — fall back to the standard table.
        return Ok(None);
    }
    // wSamplesPerBlock is informational for the coefficient parse; we read
    // past it to reach wNumCoef. (The decoder derives its own per-block
    // sample count from the block size.)
    let num_coef = u16::from_le_bytes([extradata[2], extradata[3]]) as usize;
    if num_coef < 7 {
        return Err(Error::invalid(format!(
            "adpcm_ms: extradata declares wNumCoef={num_coef}, but the spec \
             requires at least the 7 standard coefficient sets"
        )));
    }
    let need = 4 + num_coef * 4;
    if extradata.len() < need {
        return Err(Error::invalid(format!(
            "adpcm_ms: extradata too short for wNumCoef={num_coef}: need \
             {need} bytes, got {}",
            extradata.len()
        )));
    }
    let mut coeffs = Vec::with_capacity(num_coef);
    for i in 0..num_coef {
        let off = 4 + i * 4;
        let c1 = i16::from_le_bytes([extradata[off], extradata[off + 1]]) as i32;
        let c2 = i16::from_le_bytes([extradata[off + 2], extradata[off + 3]]) as i32;
        coeffs.push((c1, c2));
    }
    // The spec mandates the first seven sets be exactly the presets.
    for (i, std) in STANDARD_COEFFS.iter().enumerate() {
        if coeffs[i] != *std {
            return Err(Error::invalid(format!(
                "adpcm_ms: extradata coefficient set {i} = {:?} disagrees \
                 with the mandatory preset {:?}",
                coeffs[i], std
            )));
        }
    }
    Ok(Some(coeffs))
}

/// Build the Microsoft-ADPCM `extradata` trailer — the inverse of
/// [`parse_extradata_coeffs`].
///
/// Produces the `ADPCMWAVEFORMAT` body that follows the `WAVEFORMATEX`
/// base, **excluding** the leading `cbSize` word (matching this crate's
/// `CodecParameters::extradata` convention: the bytes start at
/// `wSamplesPerBlock`, exactly what [`parse_extradata_coeffs`] consumes).
/// A WAV muxer writing a `fmt ` chunk prepends `cbSize = returned.len()`.
///
/// Layout (all little-endian):
///
/// ```text
/// WORD          wSamplesPerBlock
/// WORD          wNumCoef
/// ADPCMCOEFSET  aCoeff[wNumCoef]   // each: i16 iCoef1, i16 iCoef2
/// ```
///
/// `coeffs` must list at least the seven mandatory presets first, exactly
/// equal to [`STANDARD_COEFFS`] — the same constraint
/// [`parse_extradata_coeffs`] enforces on the read side, so the produced
/// trailer always round-trips back through the parser. Passing
/// [`STANDARD_COEFFS`] yields the classic `cbSize = 32` trailer.
///
/// Returns `Err` if `coeffs` has fewer than seven sets or its first seven
/// disagree with the spec presets.
pub fn build_extradata(samples_per_block: u16, coeffs: &[CoefPair]) -> Result<Vec<u8>> {
    if coeffs.len() < 7 {
        return Err(Error::invalid(format!(
            "adpcm_ms: build_extradata needs at least the 7 standard \
             coefficient sets, got {}",
            coeffs.len()
        )));
    }
    for (i, std) in STANDARD_COEFFS.iter().enumerate() {
        if coeffs[i] != *std {
            return Err(Error::invalid(format!(
                "adpcm_ms: build_extradata coefficient set {i} = {:?} \
                 disagrees with the mandatory preset {:?}",
                coeffs[i], std
            )));
        }
    }
    let mut ext = Vec::with_capacity(4 + coeffs.len() * 4);
    ext.extend_from_slice(&samples_per_block.to_le_bytes());
    ext.extend_from_slice(&(coeffs.len() as u16).to_le_bytes());
    for &(c1, c2) in coeffs {
        ext.extend_from_slice(&(c1 as i16).to_le_bytes());
        ext.extend_from_slice(&(c2 as i16).to_le_bytes());
    }
    Ok(ext)
}

/// Per-channel running state carried across the nibbles in a block.
#[derive(Clone, Copy, Debug)]
struct ChannelState {
    coef1: i32,
    coef2: i32,
    delta: i32,
    sample1: i32,
    sample2: i32,
}

fn decode_nibble(st: &mut ChannelState, nibble: u8) -> i16 {
    // Sign-extend 4-bit → i32 via (n ^ 8) - 8.
    let signed = ((nibble as i32) ^ 8) - 8;

    // Linear predictor (scaled by 256). The coefficient products stay
    // inside i64 to keep arbitrary `sample1`/`sample2` from overflowing
    // i32 — both are constrained to i16 by the time they re-enter as
    // history but the *first* call reads sample1/sample2 from the block
    // header, which can be any 16-bit value.
    let predicted =
        (st.sample1 as i64 * st.coef1 as i64 + st.sample2 as i64 * st.coef2 as i64) >> 8;

    // Add error term (saturating; `delta` is a header-supplied i32 that
    // can be arbitrary on malformed input).
    let new = predicted.saturating_add(signed as i64 * st.delta as i64);
    let out = clamp_i64_i16(new);

    // Shift history.
    st.sample2 = st.sample1;
    st.sample1 = out as i32;

    // Update delta (adapt step). i64 keeps the table-multiplied delta
    // from overflowing on inputs whose initial delta starts large; the
    // mathematical recurrence the spec describes uses real numbers.
    let mut d = ((MS_ADAPTATION[nibble as usize] as i64).saturating_mul(st.delta as i64)) >> 8;
    if d < 16 {
        d = 16;
    }
    // Cap at i32::MAX so the next iteration's `signed * delta` still
    // fits in i64 trivially; in well-formed streams `delta` never grows
    // anywhere near this cap.
    st.delta = d.min(i32::MAX as i64) as i32;

    out
}

fn clamp_i64_i16(x: i64) -> i16 {
    x.clamp(i16::MIN as i64, i16::MAX as i64) as i16
}

/// Decode a single Microsoft-ADPCM block with `channels` channels, using
/// the seven standard preset coefficient sets.
///
/// Returns a flat interleaved i16 vector (L, R, L, R, …) of `samples *
/// channels` i16 values. The sample count is derived from the block size
/// per the standard formula:
///
/// `samples_per_channel = 2 + (body_bytes * 2) / channels`
///
/// where body_bytes is the block size minus the header (7 bytes mono /
/// 14 bytes stereo).
///
/// Files whose `WAVEFORMATEX` trailer declares **custom** coefficient sets
/// (`wNumCoef > 7`) must decode through [`decode_block_with_coeffs`] with
/// the table from [`parse_extradata_coeffs`] — otherwise a per-block
/// `bPredictor` index into a custom set (≥ 7) is rejected here.
pub fn decode_block(block: &[u8], channels: usize) -> Result<Vec<i16>> {
    decode_block_with_coeffs(block, channels, &STANDARD_COEFFS)
}

/// Decode a single Microsoft-ADPCM block against a caller-supplied
/// coefficient table.
///
/// `coeffs` is the resolved `aCoeff` array — either [`STANDARD_COEFFS`] or
/// the larger table a `WAVEFORMATEX` trailer declared (see
/// [`parse_extradata_coeffs`]). Each per-channel `bPredictor` header byte
/// indexes into this table; an index `>= coeffs.len()` is rejected as
/// malformed. Output framing is identical to [`decode_block`].
pub fn decode_block_with_coeffs(
    block: &[u8],
    channels: usize,
    coeffs: &[CoefPair],
) -> Result<Vec<i16>> {
    if channels == 0 || channels > 2 {
        return Err(Error::unsupported(format!(
            "adpcm_ms: channel count {channels} not supported (1 or 2)"
        )));
    }
    if coeffs.len() < 7 {
        return Err(Error::invalid(format!(
            "adpcm_ms: coefficient table has {} sets, need at least 7",
            coeffs.len()
        )));
    }
    let header_len = 7 * channels;
    if block.len() < header_len {
        return Err(Error::invalid(format!(
            "adpcm_ms: block too short ({} < header {header_len})",
            block.len()
        )));
    }

    // Parse header.
    let mut states: [ChannelState; 2] = [
        ChannelState {
            coef1: 0,
            coef2: 0,
            delta: 0,
            sample1: 0,
            sample2: 0,
        },
        ChannelState {
            coef1: 0,
            coef2: 0,
            delta: 0,
            sample1: 0,
            sample2: 0,
        },
    ];

    // Predictor indices (one byte per channel). The index addresses the
    // resolved `aCoeff` table — the standard 7 presets, plus any custom
    // sets a WAVEFORMATEX trailer declared.
    for (ch, st) in states.iter_mut().take(channels).enumerate() {
        let pi = block[ch] as usize;
        if pi >= coeffs.len() {
            return Err(Error::invalid(format!(
                "adpcm_ms: predictor index {pi} out of range 0..={}",
                coeffs.len() - 1
            )));
        }
        st.coef1 = coeffs[pi].0;
        st.coef2 = coeffs[pi].1;
    }

    // Initial delta (i16 LE, one per channel).
    for (ch, st) in states.iter_mut().take(channels).enumerate() {
        let off = channels + ch * 2;
        let d = i16::from_le_bytes([block[off], block[off + 1]]) as i32;
        st.delta = d;
    }

    // Initial sample1 (i16 LE, one per channel).
    for (ch, st) in states.iter_mut().take(channels).enumerate() {
        let off = channels + 2 * channels + ch * 2;
        let s = i16::from_le_bytes([block[off], block[off + 1]]) as i32;
        st.sample1 = s;
    }

    // Initial sample2 (i16 LE, one per channel).
    for (ch, st) in states.iter_mut().take(channels).enumerate() {
        let off = channels + 4 * channels + ch * 2;
        let s = i16::from_le_bytes([block[off], block[off + 1]]) as i32;
        st.sample2 = s;
    }

    let body = &block[header_len..];
    // Prelude: `sample2` first, then `sample1` per channel (this matches
    // the ordering Microsoft's spec puts into the decoded stream — they
    // are the oldest two samples of the block).
    let mut out = Vec::with_capacity((body.len() * 2 + 2 * channels) * 2);
    for ch in 0..channels {
        out.push(states[ch].sample2 as i16);
    }
    for ch in 0..channels {
        out.push(states[ch].sample1 as i16);
    }

    // Nibble stream. For each byte emit hi-nibble then lo-nibble; nibbles
    // alternate channels (L, R, L, R, ...) in stereo.
    let mut ch_cursor: usize = 0;
    for &byte in body {
        let hi = (byte >> 4) & 0x0F;
        let lo = byte & 0x0F;

        let s = decode_nibble(&mut states[ch_cursor], hi);
        out.push(s);
        ch_cursor = (ch_cursor + 1) % channels;

        let s = decode_nibble(&mut states[ch_cursor], lo);
        out.push(s);
        ch_cursor = (ch_cursor + 1) % channels;
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand-built minimal mono block that decodes to a known prefix.
    /// Predictor index 0 → coef1=256, coef2=0, so `predicted = sample1`.
    #[test]
    fn mono_minimal_block_header_only() {
        let mut block = Vec::new();
        block.push(0); // predictor index 0
        block.extend_from_slice(&100i16.to_le_bytes()); // initial delta
        block.extend_from_slice(&500i16.to_le_bytes()); // sample1
        block.extend_from_slice(&300i16.to_le_bytes()); // sample2
                                                        // No body bytes.
        let pcm = decode_block(&block, 1).unwrap();
        // Prelude emits sample2 then sample1.
        assert_eq!(pcm, vec![300, 500]);
    }

    #[test]
    fn mono_block_decodes_zero_nibbles_as_pure_prediction() {
        // With coef1=256 coef2=0 and nibble=0, new_sample = sample1.
        // So the predictor output is constant after the prelude.
        let mut block = Vec::new();
        block.push(0);
        block.extend_from_slice(&16i16.to_le_bytes());
        block.extend_from_slice(&1000i16.to_le_bytes()); // sample1
        block.extend_from_slice(&2000i16.to_le_bytes()); // sample2
        block.push(0x00); // two zero nibbles
        let pcm = decode_block(&block, 1).unwrap();
        // prelude: [sample2, sample1] = [2000, 1000]; then two nibble=0
        // decodes → both predict = sample1 = 1000 each iteration.
        assert_eq!(pcm, vec![2000, 1000, 1000, 1000]);
    }

    #[test]
    fn rejects_bad_predictor_index() {
        let mut block = Vec::new();
        block.push(7); // out of range
        block.extend_from_slice(&16i16.to_le_bytes());
        block.extend_from_slice(&0i16.to_le_bytes());
        block.extend_from_slice(&0i16.to_le_bytes());
        assert!(decode_block(&block, 1).is_err());
    }

    #[test]
    fn rejects_truncated_block() {
        assert!(decode_block(&[0u8; 3], 1).is_err());
        assert!(decode_block(&[0u8; 13], 2).is_err());
    }

    #[test]
    fn empty_extradata_falls_back_to_standard() {
        assert!(parse_extradata_coeffs(&[]).unwrap().is_none());
        // A 2-byte stub (only wSamplesPerBlock, no wNumCoef) is also too
        // short to carry a table → fall back.
        assert!(parse_extradata_coeffs(&[0x00, 0x02]).unwrap().is_none());
    }

    /// Build a WAVEFORMATEX trailer: wSamplesPerBlock, wNumCoef, then the
    /// seven standard pairs followed by `extra` custom pairs.
    fn trailer(spb: u16, extra: &[(i16, i16)]) -> Vec<u8> {
        let mut t = Vec::new();
        t.extend_from_slice(&spb.to_le_bytes());
        let num = (7 + extra.len()) as u16;
        t.extend_from_slice(&num.to_le_bytes());
        for &(c1, c2) in &STANDARD_COEFFS {
            t.extend_from_slice(&(c1 as i16).to_le_bytes());
            t.extend_from_slice(&(c2 as i16).to_le_bytes());
        }
        for &(c1, c2) in extra {
            t.extend_from_slice(&c1.to_le_bytes());
            t.extend_from_slice(&c2.to_le_bytes());
        }
        t
    }

    #[test]
    fn parses_standard_seven_pair_trailer() {
        // The classic cbSize=32 trailer (no extra sets).
        let t = trailer(0x01F4, &[]);
        assert_eq!(t.len(), 32);
        let coeffs = parse_extradata_coeffs(&t).unwrap().unwrap();
        assert_eq!(coeffs.len(), 7);
        assert_eq!(&coeffs[..], &STANDARD_COEFFS[..]);
    }

    #[test]
    fn parses_custom_eighth_pair() {
        let t = trailer(0x01F4, &[(128, -32)]);
        let coeffs = parse_extradata_coeffs(&t).unwrap().unwrap();
        assert_eq!(coeffs.len(), 8);
        assert_eq!(coeffs[7], (128, -32));
    }

    #[test]
    fn rejects_too_few_coef_sets() {
        // wNumCoef = 3 — below the mandatory 7.
        let mut t = Vec::new();
        t.extend_from_slice(&0x01F4u16.to_le_bytes());
        t.extend_from_slice(&3u16.to_le_bytes());
        t.extend_from_slice(&[0u8; 12]);
        assert!(parse_extradata_coeffs(&t).is_err());
    }

    #[test]
    fn rejects_truncated_table() {
        // wNumCoef = 8 but only the seven standard pairs' bytes present.
        let mut t = trailer(0x01F4, &[]);
        // Patch wNumCoef to 8 without appending the eighth pair's 4 bytes.
        t[2] = 8;
        t[3] = 0;
        assert!(parse_extradata_coeffs(&t).is_err());
    }

    #[test]
    fn rejects_altered_preset_pair() {
        // wNumCoef = 7 but the first pair is not the spec preset.
        let mut t = trailer(0x01F4, &[]);
        // Pair 0 starts at offset 4; clobber iCoef1 to a non-preset value.
        t[4] = 0xFF;
        t[5] = 0x00;
        assert!(parse_extradata_coeffs(&t).is_err());
    }

    #[test]
    fn custom_coeff_block_decodes_against_eighth_set() {
        // A block whose bPredictor = 7 (the custom set) must decode using
        // the custom (iCoef1, iCoef2); decode_block (standard 7) rejects it.
        let mut block = Vec::new();
        block.push(7); // predictor index → custom set
        block.extend_from_slice(&16i16.to_le_bytes()); // delta
        block.extend_from_slice(&1000i16.to_le_bytes()); // sample1
        block.extend_from_slice(&2000i16.to_le_bytes()); // sample2
        block.push(0x00); // two zero nibbles

        // Standard 7-set decode rejects index 7.
        assert!(decode_block(&block, 1).is_err());

        // Custom set 7 = (coef1=256, coef2=0) → identical to standard index
        // 0, so zero nibbles reproduce sample1 each step.
        let mut coeffs = STANDARD_COEFFS.to_vec();
        coeffs.push((256, 0));
        let pcm = decode_block_with_coeffs(&block, 1, &coeffs).unwrap();
        // prelude [sample2, sample1] = [2000, 1000]; then predict = sample1.
        assert_eq!(pcm, vec![2000, 1000, 1000, 1000]);

        // A different custom pair changes the prediction: coef1=512,coef2=-256
        // (the standard index-1 second-order pair) with the same block.
        let mut coeffs2 = STANDARD_COEFFS.to_vec();
        coeffs2.push((512, -256));
        let pcm2 = decode_block_with_coeffs(&block, 1, &coeffs2).unwrap();
        // first body nibble: predicted = (1000*512 + 2000*-256)>>8 = (512000-512000)>>8 = 0.
        assert_eq!(pcm2[2], 0);
    }

    #[test]
    fn build_extradata_round_trips_through_parser_standard() {
        // The classic cbSize=32 trailer: build it from the 7 presets,
        // parse it back, and confirm the coefficient table is identical.
        let ext = build_extradata(0x01F4, &STANDARD_COEFFS).unwrap();
        // 2 (spb) + 2 (numCoef) + 7*4 (coeff pairs) = 32 bytes (no cbSize).
        assert_eq!(ext.len(), 32);
        // wSamplesPerBlock is the first LE word.
        assert_eq!(u16::from_le_bytes([ext[0], ext[1]]), 0x01F4);
        // wNumCoef is the second LE word.
        assert_eq!(u16::from_le_bytes([ext[2], ext[3]]), 7);
        let parsed = parse_extradata_coeffs(&ext).unwrap().unwrap();
        assert_eq!(&parsed[..], &STANDARD_COEFFS[..]);
    }

    #[test]
    fn build_extradata_round_trips_with_custom_eighth_pair() {
        let mut coeffs = STANDARD_COEFFS.to_vec();
        coeffs.push((128, -32));
        let ext = build_extradata(1024, &coeffs).unwrap();
        // 4 + 8*4 = 36 bytes.
        assert_eq!(ext.len(), 36);
        let parsed = parse_extradata_coeffs(&ext).unwrap().unwrap();
        assert_eq!(parsed.len(), 8);
        assert_eq!(parsed[7], (128, -32));
        // The produced trailer is byte-identical to a hand-built one.
        assert_eq!(u16::from_le_bytes([ext[0], ext[1]]), 1024);
        assert_eq!(u16::from_le_bytes([ext[2], ext[3]]), 8);
    }

    #[test]
    fn build_extradata_rejects_short_or_altered_preset_tables() {
        // Fewer than 7 sets.
        assert!(build_extradata(256, &STANDARD_COEFFS[..3]).is_err());
        // First seven not the spec presets.
        let mut bad = STANDARD_COEFFS.to_vec();
        bad[0] = (1, 2);
        assert!(build_extradata(256, &bad).is_err());
    }

    #[test]
    fn stereo_prelude_interleaves_channels() {
        // 14-byte stereo header, no body bytes.
        let mut block = Vec::new();
        block.extend_from_slice(&[0, 0]); // predictor L, R
        block.extend_from_slice(&50i16.to_le_bytes()); // delta L
        block.extend_from_slice(&60i16.to_le_bytes()); // delta R
        block.extend_from_slice(&100i16.to_le_bytes()); // s1 L
        block.extend_from_slice(&200i16.to_le_bytes()); // s1 R
        block.extend_from_slice(&300i16.to_le_bytes()); // s2 L
        block.extend_from_slice(&400i16.to_le_bytes()); // s2 R
        let pcm = decode_block(&block, 2).unwrap();
        // Interleaved [L_s2, R_s2, L_s1, R_s1].
        assert_eq!(pcm, vec![300, 400, 100, 200]);
    }
}
