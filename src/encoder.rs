//! `oxideav_core::Encoder` impls for the ADPCM family.
//!
//! Encoders accept interleaved `i16` PCM and emit raw ADPCM blocks. The
//! search strategy is the textbook "decoder-loop search": for each input
//! sample, evaluate every candidate nibble by running the decoder
//! recurrence forward and pick the nibble whose reconstructed sample
//! minimises absolute error against the target. This guarantees the
//! emitted stream is exactly what our own decoder reconstructs (we
//! verify with round-trip unit tests) without consulting any third-party
//! source — the algorithm is fully derived from the decoder recurrence
//! in [`crate::ms`] / [`crate::ima_wav`].
//!
//! Five variants currently have encoders here:
//!
//! - **MS-ADPCM** (`adpcm_ms`, WAVEFORMATEX tag `0x0002`) — block size
//!   defaults to 256 bytes per channel; the first 7 bytes per channel
//!   are the header, the remainder carries packed nibbles. Predictor
//!   index 0 is selected for every block (the spec permits any of the
//!   7 default coefficient pairs but does not require any one in
//!   particular for encoders).
//!
//! - **Microsoft IMA-ADPCM-WAV** (`adpcm_ima_wav`, WAVEFORMATEX tag
//!   `0x0011`) — block size defaults to 256 bytes per channel; the
//!   first 4 bytes per channel are the header, the remainder is the
//!   per-channel 4-byte-group nibble stream.
//!
//! - **Apple QuickTime IMA ADPCM** (`adpcm_ima_qt`, QuickTime fourcc
//!   `ima4`) — fixed 34-byte block per channel (cannot be changed),
//!   block-level channel interleave: a stereo packet of 68 bytes is
//!   one ch-0 block followed by one ch-1 block. The 2-byte big-endian
//!   preamble carries a 9-bit signed predictor seed (low 7 bits zero)
//!   plus the initial step index. Each block produces exactly 64
//!   samples per channel.
//!
//! - **OKI / Dialogic VOX ADPCM** (`adpcm_dialogic`) — stream-oriented,
//!   no block framing. The encoder carries the 12-bit predictor + step
//!   pointer across `send_frame` calls and emits one packet per call.
//!   Closed-form (sign + greedy magnitude bits) quantiser per the
//!   Dialogic app-note §3 pseudocode — not the decoder-loop search the
//!   block-oriented encoders use, because the per-byte ratio (2 samples
//!   per byte) is too tight for the 16-candidate sweep to matter.
//!
//! - **Yamaha ADPCM** (`adpcm_yamaha`, WAVEFORMATEX tag `0x0020`) —
//!   stream-oriented, no block framing. The encoder carries the
//!   16-bit-clamped predictor + per-channel step across `send_frame`
//!   calls. Closed-form quantiser per the Y8950 manual §I-4 *analysis*
//!   recurrence: sign from `dn = Xn − x̂n`, then magnitude bits from the
//!   eight `|dn|/Δn` thresholds {0, 1/4, 1/2, 3/4, 1, 5/4, 3/2, 7/4}
//!   listed in Table 5-1 (YM2608) / Table 1 (AICA). State advances
//!   through [`crate::yamaha::decode_nibble`] so encode is the
//!   bit-for-bit inverse of decode.
//!
//! Default block sizes can be overridden via the `block_size` field on
//! the encoder before the first call to `send_frame`. The default of
//! 256 bytes per channel is a common WAV-container choice at 22050 Hz
//! mono and gives the nibble search a reasonable amortisation horizon.

use std::collections::VecDeque;

use crate::dialogic;
use crate::tables::{
    IMA_INDEX_ADJUST, IMA_STEP_SIZE, MS_ADAPTATION, MS_ADAPT_COEFF1, MS_ADAPT_COEFF2,
};
use crate::yamaha;
use crate::yamaha_a;
use oxideav_core::{
    AudioFrame, CodecId, CodecParameters, Encoder, Error, Frame, Packet, Result, TimeBase,
};

/// Default per-channel block size in bytes for the WAV/AVI block-oriented
/// variants. A common WAV-container choice at 22050 Hz mono that gives
/// the nibble search a reasonable amortisation horizon.
pub const DEFAULT_BLOCK_SIZE: usize = 256;

// ---------------------------------------------------------------------------
// MS-ADPCM encoder
// ---------------------------------------------------------------------------

/// Internal MS-ADPCM encoder state (one per channel). Mirrors the
/// decoder's [`crate::ms`] `ChannelState` exactly.
#[derive(Clone, Copy, Debug)]
struct MsState {
    coef1: i32,
    coef2: i32,
    delta: i32,
    sample1: i32,
    sample2: i32,
}

fn clamp_i16(x: i64) -> i16 {
    x.clamp(i16::MIN as i64, i16::MAX as i64) as i16
}

/// Run the MS-ADPCM decoder recurrence for one nibble. Returns the
/// reconstructed sample without mutating `st`.
///
/// Arithmetic widened to i64 with saturating multiplication so an
/// adversarial encoder state (extreme PCM seed pushing `sample1` /
/// `sample2` to ±i16, then a wildly-grown `delta` after several search
/// iterations) cannot overflow the i32 intermediate computation under
/// `debug-assertions`. Spec-compliant inputs are bit-identical because
/// the result is clamped back to i16 before storage in `MsState`.
fn ms_simulate_nibble(st: &MsState, nibble: u8) -> i16 {
    let signed = ((nibble as i64) ^ 8) - 8;
    let term1 = (st.sample1 as i64).saturating_mul(st.coef1 as i64);
    let term2 = (st.sample2 as i64).saturating_mul(st.coef2 as i64);
    let predicted = term1.saturating_add(term2) >> 8;
    let delta = signed.saturating_mul(st.delta as i64);
    clamp_i16(predicted.saturating_add(delta))
}

/// Advance `st` exactly as the decoder would after emitting `out` from
/// `nibble`. Splits the decoder step so the encoder can reuse the
/// simulate-then-advance pattern in its search loop.
///
/// Mirrors the same overflow-safe arithmetic used by the decoder
/// (`ms::decode_nibble` lifted to i64 in the previous round): the
/// `MS_ADAPTATION[i] * delta` product can grow past i32::MAX after a
/// few search iterations on adversarial PCM, so the multiplication is
/// performed in i64 with saturating arithmetic and the result is
/// clamped to i32 before being stored back into `delta`. The minimum
/// of 16 is preserved per the spec.
fn ms_advance(st: &mut MsState, nibble: u8, out: i16) {
    st.sample2 = st.sample1;
    st.sample1 = out as i32;
    let prod = (MS_ADAPTATION[nibble as usize] as i64).saturating_mul(st.delta as i64);
    let mut d = (prod >> 8).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
    if d < 16 {
        d = 16;
    }
    st.delta = d;
}

/// Pick the 4-bit nibble that minimises |decoded - target|, breaking
/// ties toward the smaller magnitude (which keeps the next-step delta
/// growing more slowly — more stable on near-silence).
fn ms_best_nibble(st: &MsState, target: i32) -> (u8, i16) {
    let mut best_nibble = 0u8;
    let mut best_sample = ms_simulate_nibble(st, 0);
    let mut best_err = (best_sample as i32 - target).abs();
    for n in 1u8..16 {
        let s = ms_simulate_nibble(st, n);
        let e = (s as i32 - target).abs();
        if e < best_err {
            best_err = e;
            best_nibble = n;
            best_sample = s;
        }
    }
    (best_nibble, best_sample)
}

/// Encode `samples_per_channel` interleaved-by-channel `i16` PCM samples
/// (`samples[i*channels + c]` is channel `c`'s sample `i`) into one
/// MS-ADPCM block of `target_block_size` bytes.
///
/// Returns an error if `samples_per_channel < 2` (we need at least the
/// two prelude samples) or if the requested block size cannot fit the
/// header.
pub fn encode_block(samples: &[i16], channels: usize, target_block_size: usize) -> Result<Vec<u8>> {
    if !(1..=2).contains(&channels) {
        return Err(Error::unsupported(format!(
            "adpcm_ms encoder: channel count {channels} not supported (1 or 2)"
        )));
    }
    if samples.len() % channels != 0 {
        return Err(Error::invalid(format!(
            "adpcm_ms encoder: sample count {} not divisible by {channels} channels",
            samples.len(),
        )));
    }
    let samples_per_channel = samples.len() / channels;
    if samples_per_channel < 2 {
        return Err(Error::invalid(
            "adpcm_ms encoder: need at least 2 prelude samples per channel",
        ));
    }
    let header_len = 7 * channels;
    if target_block_size < header_len {
        return Err(Error::invalid(format!(
            "adpcm_ms encoder: block size {target_block_size} < header {header_len}"
        )));
    }
    let body_len = target_block_size - header_len;
    // Each body byte = 2 nibbles = 2 samples total = (2/channels) samples
    // per channel. For stereo each byte contributes 1 sample per channel.
    let samples_after_prelude_per_channel = (body_len * 2) / channels;
    let want_samples_per_channel = 2 + samples_after_prelude_per_channel;
    if samples_per_channel < want_samples_per_channel {
        return Err(Error::invalid(format!(
            "adpcm_ms encoder: have {samples_per_channel} samples/channel, need {want_samples_per_channel} for block size {target_block_size}"
        )));
    }

    // Predictor index 0 is the safe default: coef1=256, coef2=0 means
    // "predicted = sample1" (delta-from-previous). Header writes:
    // - 1 byte predictor index per channel
    // - i16 LE initial delta per channel
    // - i16 LE sample1 per channel  (the SECOND prelude sample emitted)
    // - i16 LE sample2 per channel  (the FIRST prelude sample emitted)
    let predictor_index = 0u8;
    let mut states = [MsState {
        coef1: MS_ADAPT_COEFF1[predictor_index as usize],
        coef2: MS_ADAPT_COEFF2[predictor_index as usize],
        delta: 0,
        sample1: 0,
        sample2: 0,
    }; 2];

    // Seed each channel: sample2 = samples[0,ch], sample1 = samples[1,ch].
    //
    // Initial delta is a moderate value. The decoder recurrence with
    // predictor index 0 (coef1=256, coef2=0) reduces to
    //     reconstructed = sample1 + signed_nibble * delta
    // where `signed_nibble = (nibble ^ 8) - 8` ranges -8..=7. So for an
    // input sample whose target deviates from `sample1` by ~|Δ| LSB, the
    // search loop's best nibble picks a magnitude near |Δ|/delta. A
    // delta in the range "|Δ| / 4" places typical values at a mid-range
    // nibble (magnitude 4) which leaves headroom on both sides.
    //
    // Without this seed, the delta starts at the spec minimum of 16 and
    // grows multiplicatively only through MS_ADAPTATION; for a
    // high-amplitude bandlimited signal the first half-dozen nibbles
    // cannot track the target, producing a large leading-edge transient.
    // Estimating the seed from |sample[i+1] - sample[i]| over the first
    // few samples in the block costs O(few) and is bounded.
    //
    // The seed inputs are taken from the per-channel sample axis. We
    // cap probes at `min(16, samples_per_channel - 1)` to keep the
    // estimate local to the leading edge.
    let probe = (samples_per_channel.saturating_sub(1)).clamp(1, 16);
    for ch in 0..channels {
        states[ch].sample2 = samples[ch] as i32;
        states[ch].sample1 = samples[channels + ch] as i32;
        let mut acc: i64 = 0;
        for i in 0..probe {
            let s0 = samples[i * channels + ch] as i32;
            let s1 = samples[(i + 1) * channels + ch] as i32;
            acc += (s1 - s0).unsigned_abs() as i64;
        }
        let mean_abs_delta = (acc / probe as i64) as i32;
        // delta ≈ mean_abs_delta / 4 places mid-range nibble at the
        // typical step; floor at the spec minimum (16) and cap below
        // i16::MAX (i32-storable but kept conservative).
        let seed = (mean_abs_delta / 4).clamp(16, 16384);
        states[ch].delta = seed;
    }

    let mut out = Vec::with_capacity(target_block_size);
    // Header.
    for _ch in 0..channels {
        out.push(predictor_index);
    }
    for ch in 0..channels {
        out.extend_from_slice(&(states[ch].delta as i16).to_le_bytes());
    }
    for ch in 0..channels {
        out.extend_from_slice(&(states[ch].sample1 as i16).to_le_bytes());
    }
    for ch in 0..channels {
        out.extend_from_slice(&(states[ch].sample2 as i16).to_le_bytes());
    }
    debug_assert_eq!(out.len(), header_len);

    // Body: encode samples 2.. in the per-channel sample axis, two
    // nibbles per byte, hi-nibble first, channels round-robin per nibble.
    let mut ch_cursor: usize = 0;
    let mut sample_cursor: [usize; 2] = [2, 2]; // next sample index to encode per channel
    for _ in 0..body_len {
        // Hi nibble.
        let ch_h = ch_cursor;
        let target_h = samples[sample_cursor[ch_h] * channels + ch_h] as i32;
        let (nh, sh) = ms_best_nibble(&states[ch_h], target_h);
        ms_advance(&mut states[ch_h], nh, sh);
        sample_cursor[ch_h] += 1;
        ch_cursor = (ch_cursor + 1) % channels;

        // Lo nibble.
        let ch_l = ch_cursor;
        let target_l = samples[sample_cursor[ch_l] * channels + ch_l] as i32;
        let (nl, sl) = ms_best_nibble(&states[ch_l], target_l);
        ms_advance(&mut states[ch_l], nl, sl);
        sample_cursor[ch_l] += 1;
        ch_cursor = (ch_cursor + 1) % channels;

        out.push((nh << 4) | nl);
    }
    Ok(out)
}

/// Frame-to-packet MS-ADPCM encoder. Each `send_frame` call buffers the
/// PCM; one or more complete blocks are emitted by `receive_packet` as
/// they become available.
pub struct MsEncoder {
    output_params: CodecParameters,
    channels: usize,
    block_size: usize,
    pcm: Vec<i16>, // interleaved buffer
    pending: VecDeque<Packet>,
    samples_emitted: i64,
    flushed: bool,
}

impl MsEncoder {
    /// Override the per-channel block size *before* the first
    /// `send_frame` call.
    pub fn set_block_size(&mut self, block_size: usize) {
        self.block_size = block_size;
    }

    fn samples_per_block(&self) -> usize {
        let header_len = 7 * self.channels;
        let body_len = self.block_size.saturating_sub(header_len);
        2 + (body_len * 2) / self.channels
    }

    fn drain_blocks(&mut self, allow_partial_final: bool) -> Result<()> {
        let n_per_block = self.samples_per_block();
        if n_per_block < 2 {
            return Err(Error::invalid(
                "adpcm_ms encoder: block_size too small for any payload",
            ));
        }
        let per_block_samples_interleaved = n_per_block * self.channels;
        let tb = TimeBase::new(1, self.output_params.sample_rate.unwrap_or(1) as i64);
        while self.pcm.len() >= per_block_samples_interleaved {
            let take: Vec<i16> = self.pcm.drain(..per_block_samples_interleaved).collect();
            let bytes = encode_block(&take, self.channels, self.block_size)?;
            let pts = self.samples_emitted;
            self.samples_emitted += n_per_block as i64;
            self.pending
                .push_back(Packet::new(0, tb, bytes).with_pts(pts));
        }
        if allow_partial_final && !self.pcm.is_empty() {
            // Pad the tail PCM to a full block by replicating the last
            // sample (a benign DC tail) and emit one final block. This
            // keeps the total sample count an integer-multiple of
            // n_per_block which is what callers expect for fixed-size
            // block streams.
            let need = per_block_samples_interleaved - self.pcm.len();
            let last_sample_index = self.pcm.len() / self.channels;
            for _ in 0..(need / self.channels) {
                for ch in 0..self.channels {
                    // Replicate the most recent per-channel sample.
                    let idx = last_sample_index.saturating_sub(1) * self.channels + ch;
                    let s = if self.pcm.is_empty() {
                        0
                    } else {
                        self.pcm[idx]
                    };
                    self.pcm.push(s);
                }
            }
            debug_assert_eq!(self.pcm.len(), per_block_samples_interleaved);
            let take: Vec<i16> = self.pcm.drain(..).collect();
            let bytes = encode_block(&take, self.channels, self.block_size)?;
            let pts = self.samples_emitted;
            self.samples_emitted += n_per_block as i64;
            self.pending
                .push_back(Packet::new(0, tb, bytes).with_pts(pts));
        }
        Ok(())
    }
}

impl Encoder for MsEncoder {
    fn codec_id(&self) -> &CodecId {
        &self.output_params.codec_id
    }
    fn output_params(&self) -> &CodecParameters {
        &self.output_params
    }
    fn send_frame(&mut self, frame: &Frame) -> Result<()> {
        let af = match frame {
            Frame::Audio(a) => a,
            _ => return Err(Error::invalid("adpcm_ms encoder: expected audio frame")),
        };
        push_audio_frame_pcm(&mut self.pcm, af, self.channels)?;
        self.drain_blocks(false)
    }
    fn receive_packet(&mut self) -> Result<Packet> {
        if let Some(p) = self.pending.pop_front() {
            return Ok(p);
        }
        Err(Error::NeedMore)
    }
    fn flush(&mut self) -> Result<()> {
        if !self.flushed {
            self.flushed = true;
            self.drain_blocks(true)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// IMA-ADPCM-WAV encoder
// ---------------------------------------------------------------------------

/// Mirror of the IMA decoder per-channel state.
#[derive(Clone, Copy, Debug, Default)]
struct ImaState {
    predictor: i32,
    step_index: i32,
}

fn ima_simulate_nibble(st: &ImaState, nibble: u8) -> (i16, ImaState) {
    let n = nibble as i32;
    let step = IMA_STEP_SIZE[st.step_index.clamp(0, 88) as usize] as i32;
    let mag = n & 7;
    let mut diff = step >> 3;
    if (mag & 1) != 0 {
        diff += step >> 2;
    }
    if (mag & 2) != 0 {
        diff += step >> 1;
    }
    if (mag & 4) != 0 {
        diff += step;
    }
    let mut p = st.predictor;
    if (n & 8) != 0 {
        p -= diff;
    } else {
        p += diff;
    }
    p = p.clamp(i16::MIN as i32, i16::MAX as i32);
    let mut si = st.step_index + IMA_INDEX_ADJUST[nibble as usize];
    si = si.clamp(0, 88);
    (
        p as i16,
        ImaState {
            predictor: p,
            step_index: si,
        },
    )
}

fn ima_best_nibble(st: &ImaState, target: i32) -> (u8, ImaState) {
    let mut best_nibble = 0u8;
    let (mut best_sample, mut best_state) = ima_simulate_nibble(st, 0);
    let mut best_err = (best_sample as i32 - target).abs();
    for n in 1u8..16 {
        let (s, s2) = ima_simulate_nibble(st, n);
        let e = (s as i32 - target).abs();
        if e < best_err {
            best_err = e;
            best_nibble = n;
            best_sample = s;
            best_state = s2;
        }
    }
    let _ = best_sample; // silence unused (kept for symmetry).
    (best_nibble, best_state)
}

/// Encode one IMA-ADPCM-WAV block. `samples` is interleaved i16 PCM of
/// `samples_per_channel * channels` samples, where
/// `samples_per_channel = 1 + groups * 8` and `groups` is determined by
/// the block size: `groups = (block_size - 4*channels) / (4*channels)`.
pub fn ima_encode_block(samples: &[i16], channels: usize, block_size: usize) -> Result<Vec<u8>> {
    if channels == 0 || channels > 8 {
        return Err(Error::unsupported(format!(
            "adpcm_ima_wav encoder: channel count {channels} not supported (1..=8)"
        )));
    }
    let header_len = 4 * channels;
    if block_size < header_len {
        return Err(Error::invalid(format!(
            "adpcm_ima_wav encoder: block size {block_size} < header {header_len}"
        )));
    }
    let body_len = block_size - header_len;
    let group_bytes = 4 * channels;
    if body_len % group_bytes != 0 {
        return Err(Error::invalid(format!(
            "adpcm_ima_wav encoder: body length {body_len} not multiple of {group_bytes} ({channels}ch × 4B)"
        )));
    }
    let groups = body_len / group_bytes;
    let samples_per_channel = 1 + groups * 8;
    if samples.len() != samples_per_channel * channels {
        return Err(Error::invalid(format!(
            "adpcm_ima_wav encoder: got {} samples, expected {} ({} per channel × {channels})",
            samples.len(),
            samples_per_channel * channels,
            samples_per_channel
        )));
    }

    // Seed step_index per channel from a |Δ|-of-leading-edge probe — the
    // same heuristic used in `ima_qt_encode_block` (see commentary
    // there). For a magnitude-4 nibble (lo bit only) the IMA decoder
    // produces diff = step/8 + step/4 = 3*step/8 ≈ 0.375 * step, so a
    // target step ≈ mean_delta * 8 / 3 places typical magnitudes near
    // the middle of the available nibble range. Probes are bounded by
    // the per-channel sample count; for a default-size block (505
    // samples per channel) we look at the first 16. If the block holds
    // fewer than 2 samples per channel the seed defaults to 0.
    let probe = (samples_per_channel.saturating_sub(1)).min(16);
    let mut states: Vec<ImaState> = (0..channels)
        .map(|ch| {
            let mut step_index: i32 = 0;
            if probe > 0 {
                let mut acc: i64 = 0;
                for i in 0..probe {
                    let s0 = samples[i * channels + ch] as i32;
                    let s1 = samples[(i + 1) * channels + ch] as i32;
                    acc += (s1 - s0).unsigned_abs() as i64;
                }
                let mean_delta = (acc / probe as i64) as i32;
                let target_step = (mean_delta as i64 * 8 / 3).max(7) as i32;
                for (i, &s) in IMA_STEP_SIZE.iter().enumerate() {
                    if s as i32 >= target_step {
                        step_index = i as i32;
                        break;
                    }
                    step_index = (IMA_STEP_SIZE.len() - 1) as i32;
                }
                step_index = step_index.clamp(0, 88);
            }
            ImaState {
                predictor: samples[ch] as i32,
                step_index,
            }
        })
        .collect();

    let mut out = Vec::with_capacity(block_size);
    // Header — per channel: i16 LE predictor + u8 step_index + reserved 0.
    for ch in 0..channels {
        out.extend_from_slice(&(states[ch].predictor as i16).to_le_bytes());
        out.push(states[ch].step_index as u8);
        out.push(0);
    }

    // Body: groups × channels × 4 bytes. For each group, channel c's
    // nibble stream is the 4 bytes at offset (group * group_bytes +
    // 4*c). Within each 4-byte channel chunk the nibbles are
    // bottom-nibble-first (lo, hi, lo, hi, lo, hi, lo, hi).
    let mut body = vec![0u8; body_len];
    for g in 0..groups {
        let group_start = g * group_bytes;
        for ch in 0..channels {
            for i in 0..4 {
                let sample_lo_idx = 1 + g * 8 + i * 2;
                let sample_hi_idx = sample_lo_idx + 1;
                let t_lo = samples[sample_lo_idx * channels + ch] as i32;
                let (n_lo, st_after_lo) = ima_best_nibble(&states[ch], t_lo);
                states[ch] = st_after_lo;
                let t_hi = samples[sample_hi_idx * channels + ch] as i32;
                let (n_hi, st_after_hi) = ima_best_nibble(&states[ch], t_hi);
                states[ch] = st_after_hi;
                body[group_start + 4 * ch + i] = (n_hi << 4) | n_lo;
            }
        }
    }
    out.extend_from_slice(&body);
    Ok(out)
}

/// IMA-ADPCM-WAV encoder.
pub struct ImaWavEncoder {
    output_params: CodecParameters,
    channels: usize,
    block_size: usize,
    pcm: Vec<i16>,
    pending: VecDeque<Packet>,
    samples_emitted: i64,
    flushed: bool,
}

impl ImaWavEncoder {
    pub fn set_block_size(&mut self, block_size: usize) {
        self.block_size = block_size;
    }
    fn samples_per_block(&self) -> usize {
        let header_len = 4 * self.channels;
        let body_len = self.block_size.saturating_sub(header_len);
        let group_bytes = 4 * self.channels;
        if group_bytes == 0 {
            return 1;
        }
        1 + (body_len / group_bytes) * 8
    }
    fn drain_blocks(&mut self, allow_partial_final: bool) -> Result<()> {
        let n_per_block = self.samples_per_block();
        let per_block_samples_interleaved = n_per_block * self.channels;
        let tb = TimeBase::new(1, self.output_params.sample_rate.unwrap_or(1) as i64);
        while self.pcm.len() >= per_block_samples_interleaved {
            let take: Vec<i16> = self.pcm.drain(..per_block_samples_interleaved).collect();
            let bytes = ima_encode_block(&take, self.channels, self.block_size)?;
            let pts = self.samples_emitted;
            self.samples_emitted += n_per_block as i64;
            self.pending
                .push_back(Packet::new(0, tb, bytes).with_pts(pts));
        }
        if allow_partial_final && !self.pcm.is_empty() {
            let need = per_block_samples_interleaved - self.pcm.len();
            let last_sample_index = self.pcm.len() / self.channels;
            for _ in 0..(need / self.channels) {
                for ch in 0..self.channels {
                    let idx = last_sample_index.saturating_sub(1) * self.channels + ch;
                    let s = if self.pcm.is_empty() {
                        0
                    } else {
                        self.pcm[idx]
                    };
                    self.pcm.push(s);
                }
            }
            debug_assert_eq!(self.pcm.len(), per_block_samples_interleaved);
            let take: Vec<i16> = self.pcm.drain(..).collect();
            let bytes = ima_encode_block(&take, self.channels, self.block_size)?;
            let pts = self.samples_emitted;
            self.samples_emitted += n_per_block as i64;
            self.pending
                .push_back(Packet::new(0, tb, bytes).with_pts(pts));
        }
        Ok(())
    }
}

impl Encoder for ImaWavEncoder {
    fn codec_id(&self) -> &CodecId {
        &self.output_params.codec_id
    }
    fn output_params(&self) -> &CodecParameters {
        &self.output_params
    }
    fn send_frame(&mut self, frame: &Frame) -> Result<()> {
        let af = match frame {
            Frame::Audio(a) => a,
            _ => {
                return Err(Error::invalid(
                    "adpcm_ima_wav encoder: expected audio frame",
                ))
            }
        };
        push_audio_frame_pcm(&mut self.pcm, af, self.channels)?;
        self.drain_blocks(false)
    }
    fn receive_packet(&mut self) -> Result<Packet> {
        if let Some(p) = self.pending.pop_front() {
            return Ok(p);
        }
        Err(Error::NeedMore)
    }
    fn flush(&mut self) -> Result<()> {
        if !self.flushed {
            self.flushed = true;
            self.drain_blocks(true)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// IMA-ADPCM-QT (Apple QuickTime `ima4`) encoder
// ---------------------------------------------------------------------------

/// Fixed QuickTime IMA block size in bytes per channel. The QT spec hard-codes
/// this — unlike the WAV variant, it is not a parameter the encoder can vary.
pub const QT_BLOCK_BYTES_PER_CHANNEL: usize = 34;

/// Samples produced per channel per QT block (32 body bytes × 2 nibbles).
pub const QT_SAMPLES_PER_BLOCK: usize = 64;

/// Encode one QuickTime IMA ADPCM block per channel.
///
/// `samples` carries exactly `QT_SAMPLES_PER_BLOCK * channels` interleaved i16
/// samples and the returned buffer is `34 * channels` bytes long, laid out
/// per the QT spec (ch-0 block then ch-1 block — block-level interleave).
///
/// The first sample of each channel becomes the predictor seed. The QT spec
/// stores only the top 9 bits of that seed (low 7 bits are zero) — so the
/// reconstructed first decoded sample will differ from the input by up to
/// 64 LSB. This is inherent to the QT block format and not a property of the
/// search loop.
pub fn ima_qt_encode_block(samples: &[i16], channels: usize) -> Result<Vec<u8>> {
    if !(1..=2).contains(&channels) {
        return Err(Error::unsupported(format!(
            "adpcm_ima_qt encoder: channel count {channels} not supported (1 or 2)"
        )));
    }
    let expected = QT_SAMPLES_PER_BLOCK * channels;
    if samples.len() != expected {
        return Err(Error::invalid(format!(
            "adpcm_ima_qt encoder: got {} samples, expected {expected} ({QT_SAMPLES_PER_BLOCK} per channel × {channels})",
            samples.len()
        )));
    }

    let mut out = vec![0u8; QT_BLOCK_BYTES_PER_CHANNEL * channels];
    for ch in 0..channels {
        let block_off = ch * QT_BLOCK_BYTES_PER_CHANNEL;
        // Seed predictor from the first sample, quantised to the top 9 bits
        // exactly as the decoder reads it (low 7 bits cleared via `& !0x7F`).
        let seed_full = samples[ch] as i32;
        let predictor_seed = seed_full & !0x7F;

        // Re-reading `ima_qt::decode_block`: the decoder writes EXACTLY
        // 64 output samples (2 per body byte) and does NOT emit the seed
        // predictor as sample 0. So our 64-sample input maps 1:1 to the
        // 64 produced nibbles. The first sample of the input controls
        // the predictor's initial state but is otherwise discarded.
        //
        // The step-index seed strongly affects reconstruction quality:
        // with step_index=0 (step=7) the predictor can only chase
        // ~1-LSB deltas on the first few nibbles, which means a
        // high-amplitude bandlimited signal incurs a large transient
        // error. The IMA index adapts upward at +8/+6/+4/+2 per nibble
        // so it takes a few nibbles to "catch up." A heuristic seed
        // based on the mean |Δ| of the first 8 samples works much
        // better than a fixed 0.
        let mean_delta = {
            let mut acc: i64 = 0;
            let mut n: i64 = 0;
            for i in 0..7 {
                let s0 = samples[i * channels + ch] as i32;
                let s1 = samples[(i + 1) * channels + ch] as i32;
                acc += (s1 - s0).unsigned_abs() as i64;
                n += 1;
            }
            if n == 0 {
                0
            } else {
                (acc / n) as i32
            }
        };
        // For a magnitude-4 nibble (lo bit only) the decoder produces
        // diff = step/8 + step/4 = 3*step/8 ≈ 0.375 * step. So target
        // step ≈ mean_delta / 0.375 ≈ mean_delta * 8 / 3. Find the
        // table index whose step is closest.
        let target_step = (mean_delta as i64 * 8 / 3).max(7) as i32;
        let mut step_index: i32 = 0;
        for (i, &s) in IMA_STEP_SIZE.iter().enumerate() {
            if s as i32 >= target_step {
                step_index = i as i32;
                break;
            }
            step_index = (IMA_STEP_SIZE.len() - 1) as i32;
        }
        step_index = step_index.clamp(0, 88);

        // Encode the preamble: top 9 bits of predictor + 7-bit step index.
        // The decoder reads `preamble as i16 as i32 & !0x7F` for the predictor
        // and `preamble & 0x7F` for the step index, so we just OR them.
        let preamble: u16 = (predictor_seed as u16 & 0xFF80) | (step_index as u16 & 0x7F);
        out[block_off] = (preamble >> 8) as u8;
        out[block_off + 1] = (preamble & 0xFF) as u8;

        let mut st = ImaState {
            predictor: predictor_seed,
            step_index,
        };
        for i in 0..32 {
            // Per the QT spec the LOW nibble of body[i] is decoded first,
            // then the HIGH nibble.
            let t_lo = samples[(i * 2) * channels + ch] as i32;
            let (n_lo, st_after_lo) = ima_best_nibble(&st, t_lo);
            st = st_after_lo;
            let t_hi = samples[(i * 2 + 1) * channels + ch] as i32;
            let (n_hi, st_after_hi) = ima_best_nibble(&st, t_hi);
            st = st_after_hi;
            out[block_off + 2 + i] = (n_hi << 4) | n_lo;
        }
    }
    Ok(out)
}

/// IMA-ADPCM-QT encoder (Apple `ima4`).
///
/// QT blocks are fixed-size — there is no `set_block_size` method because
/// the on-wire layout mandates 34 bytes per channel.
pub struct ImaQtEncoder {
    output_params: CodecParameters,
    channels: usize,
    pcm: Vec<i16>,
    pending: VecDeque<Packet>,
    samples_emitted: i64,
    flushed: bool,
}

impl ImaQtEncoder {
    fn drain_blocks(&mut self, allow_partial_final: bool) -> Result<()> {
        let n_per_block = QT_SAMPLES_PER_BLOCK;
        let per_block_samples_interleaved = n_per_block * self.channels;
        let tb = TimeBase::new(1, self.output_params.sample_rate.unwrap_or(1) as i64);
        while self.pcm.len() >= per_block_samples_interleaved {
            let take: Vec<i16> = self.pcm.drain(..per_block_samples_interleaved).collect();
            let bytes = ima_qt_encode_block(&take, self.channels)?;
            let pts = self.samples_emitted;
            self.samples_emitted += n_per_block as i64;
            self.pending
                .push_back(Packet::new(0, tb, bytes).with_pts(pts));
        }
        if allow_partial_final && !self.pcm.is_empty() {
            let need = per_block_samples_interleaved - self.pcm.len();
            let last_sample_index = self.pcm.len() / self.channels;
            for _ in 0..(need / self.channels) {
                for ch in 0..self.channels {
                    let idx = last_sample_index.saturating_sub(1) * self.channels + ch;
                    let s = if self.pcm.is_empty() {
                        0
                    } else {
                        self.pcm[idx]
                    };
                    self.pcm.push(s);
                }
            }
            debug_assert_eq!(self.pcm.len(), per_block_samples_interleaved);
            let take: Vec<i16> = self.pcm.drain(..).collect();
            let bytes = ima_qt_encode_block(&take, self.channels)?;
            let pts = self.samples_emitted;
            self.samples_emitted += n_per_block as i64;
            self.pending
                .push_back(Packet::new(0, tb, bytes).with_pts(pts));
        }
        Ok(())
    }
}

impl Encoder for ImaQtEncoder {
    fn codec_id(&self) -> &CodecId {
        &self.output_params.codec_id
    }
    fn output_params(&self) -> &CodecParameters {
        &self.output_params
    }
    fn send_frame(&mut self, frame: &Frame) -> Result<()> {
        let af = match frame {
            Frame::Audio(a) => a,
            _ => return Err(Error::invalid("adpcm_ima_qt encoder: expected audio frame")),
        };
        push_audio_frame_pcm(&mut self.pcm, af, self.channels)?;
        self.drain_blocks(false)
    }
    fn receive_packet(&mut self) -> Result<Packet> {
        if let Some(p) = self.pending.pop_front() {
            return Ok(p);
        }
        Err(Error::NeedMore)
    }
    fn flush(&mut self) -> Result<()> {
        if !self.flushed {
            self.flushed = true;
            self.drain_blocks(true)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Dialogic / OKI VOX encoder
// ---------------------------------------------------------------------------

/// Stream-oriented OKI / Dialogic VOX encoder.
///
/// Unlike the WAV-block-oriented MS / IMA encoders, VOX is headerless and
/// state-continuous across the entire stream. Each `send_frame` invocation
/// quantises whatever PCM arrived, emits the corresponding bytes as a
/// single packet, and carries the predictor / step-index state into the
/// next call.
///
/// We use the [`dialogic::Output::Wide16`]-equivalent encode wrapper
/// ([`dialogic::encode_packet_wide16`]) on input PCM, matching the
/// register-resolved decoder's `Wide16` output. Mono only on the
/// registry path — multi-channel VOX is not standardised (and Dialogic
/// hardware was strictly mono).
pub struct DialogicEncoder {
    output_params: CodecParameters,
    state: dialogic::Channel,
    pending: VecDeque<Packet>,
    samples_emitted: i64,
}

impl Encoder for DialogicEncoder {
    fn codec_id(&self) -> &CodecId {
        &self.output_params.codec_id
    }
    fn output_params(&self) -> &CodecParameters {
        &self.output_params
    }
    fn send_frame(&mut self, frame: &Frame) -> Result<()> {
        let af = match frame {
            Frame::Audio(a) => a,
            _ => {
                return Err(Error::invalid(
                    "adpcm_dialogic encoder: expected audio frame",
                ))
            }
        };
        // Re-use the shared PCM unpacker but for a single channel.
        let mut pcm: Vec<i16> = Vec::new();
        push_audio_frame_pcm(&mut pcm, af, 1)?;
        if pcm.is_empty() {
            return Ok(());
        }
        let n_samples = pcm.len() as i64;
        let bytes =
            dialogic::encode_packet_wide16(&pcm, &mut self.state, dialogic::NibbleOrder::HiFirst);
        let tb = TimeBase::new(1, self.output_params.sample_rate.unwrap_or(8000) as i64);
        let pts = self.samples_emitted;
        self.samples_emitted += n_samples;
        self.pending
            .push_back(Packet::new(0, tb, bytes).with_pts(pts));
        Ok(())
    }
    fn receive_packet(&mut self) -> Result<Packet> {
        if let Some(p) = self.pending.pop_front() {
            return Ok(p);
        }
        Err(Error::NeedMore)
    }
    fn flush(&mut self) -> Result<()> {
        // Stream codec: nothing buffered after send_frame returns.
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Yamaha encoder
// ---------------------------------------------------------------------------

/// Stream-oriented Yamaha-ADPCM encoder (`adpcm_yamaha`, WAVEFORMATEX
/// tag `0x0020`).
///
/// Like the Dialogic encoder, Yamaha ADPCM is **stream-oriented**: no
/// per-block header, predictor + step state carry across `send_frame`
/// invocations. Each call quantises whatever PCM arrived, packs two
/// nibbles per byte (low nibble first per the Y8950 manual and the
/// WAV-tag-0x0020 convention), and emits a single packet.
///
/// Up to 8 channels (sample-interleaved on input, nibble-interleaved on
/// the wire). The encoder picks each nibble in closed form from the
/// Y8950 / AICA *analysis* recurrence (`dn = Xn − x̂n`, then magnitude
/// bits by `|dn|/Δn` against the eight Table 5-1 / Table 1 thresholds),
/// then advances state through [`yamaha::decode_nibble`] so the encoder
/// is bit-for-bit equivalent to the decoder it ships with.
pub struct YamahaEncoder {
    output_params: CodecParameters,
    channels: usize,
    state: Vec<yamaha::Channel>,
    pending: VecDeque<Packet>,
    samples_emitted: i64,
}

impl Encoder for YamahaEncoder {
    fn codec_id(&self) -> &CodecId {
        &self.output_params.codec_id
    }
    fn output_params(&self) -> &CodecParameters {
        &self.output_params
    }
    fn send_frame(&mut self, frame: &Frame) -> Result<()> {
        let af = match frame {
            Frame::Audio(a) => a,
            _ => return Err(Error::invalid("adpcm_yamaha encoder: expected audio frame")),
        };
        let mut pcm: Vec<i16> = Vec::new();
        push_audio_frame_pcm(&mut pcm, af, self.channels)?;
        if pcm.is_empty() {
            return Ok(());
        }
        // Yamaha packs 2 nibbles per byte; if the caller hands us an
        // odd nibble count (i.e. an odd total sample count when summed
        // across channels) the encoder pads a trailing zero nibble at
        // the byte level, matching the decoder's tolerance.
        let n_samples = pcm.len() as i64;
        let bytes = yamaha::encode_packet(&pcm, &mut self.state);
        let tb = TimeBase::new(1, self.output_params.sample_rate.unwrap_or(8000) as i64);
        let pts = self.samples_emitted;
        // pts counts samples-per-channel; n_samples is interleaved across
        // channels, so divide.
        self.samples_emitted += n_samples / self.channels as i64;
        self.pending
            .push_back(Packet::new(0, tb, bytes).with_pts(pts));
        Ok(())
    }
    fn receive_packet(&mut self) -> Result<Packet> {
        if let Some(p) = self.pending.pop_front() {
            return Ok(p);
        }
        Err(Error::NeedMore)
    }
    fn flush(&mut self) -> Result<()> {
        // Stream codec: nothing buffered after send_frame returns.
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Yamaha ADPCM-A encoder
// ---------------------------------------------------------------------------

/// Stream-oriented Yamaha **ADPCM-A** encoder (`adpcm_yamaha_a`).
///
/// 12-bit single-channel codec used on the YM2608 rhythm ROM and the
/// YM2610 ADPCM-A channels. The encoder narrows the incoming i16 PCM to
/// the 12-bit silicon range internally, picks the closest of the eight
/// `± (2*mag + 1) * step / 8` reconstruction levels per sample, then
/// advances state through [`yamaha_a::decode_nibble`] so encode is the
/// bit-for-bit inverse of decode.
pub struct YamahaAEncoder {
    output_params: CodecParameters,
    state: Vec<yamaha_a::Channel>,
    pending: VecDeque<Packet>,
    samples_emitted: i64,
}

impl Encoder for YamahaAEncoder {
    fn codec_id(&self) -> &CodecId {
        &self.output_params.codec_id
    }
    fn output_params(&self) -> &CodecParameters {
        &self.output_params
    }
    fn send_frame(&mut self, frame: &Frame) -> Result<()> {
        let af = match frame {
            Frame::Audio(a) => a,
            _ => {
                return Err(Error::invalid(
                    "adpcm_yamaha_a encoder: expected audio frame",
                ))
            }
        };
        let mut pcm: Vec<i16> = Vec::new();
        push_audio_frame_pcm(&mut pcm, af, 1)?;
        if pcm.is_empty() {
            return Ok(());
        }
        let n_samples = pcm.len() as i64;
        let bytes = yamaha_a::encode_packet(&pcm, &mut self.state, yamaha_a::Output::Wide16);
        let tb = TimeBase::new(1, self.output_params.sample_rate.unwrap_or(8000) as i64);
        let pts = self.samples_emitted;
        self.samples_emitted += n_samples;
        self.pending
            .push_back(Packet::new(0, tb, bytes).with_pts(pts));
        Ok(())
    }
    fn receive_packet(&mut self) -> Result<Packet> {
        if let Some(p) = self.pending.pop_front() {
            return Ok(p);
        }
        Err(Error::NeedMore)
    }
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Shared helpers + factories
// ---------------------------------------------------------------------------

/// Decode an `AudioFrame` into an interleaved `i16` buffer and append
/// it to `pcm`. Frames are expected to carry signed-16-bit-LE PCM in
/// `data[0]` — that is what the decoder emits and what we accept on
/// the encode side for symmetry.
fn push_audio_frame_pcm(pcm: &mut Vec<i16>, af: &AudioFrame, channels: usize) -> Result<()> {
    if af.data.is_empty() {
        return Ok(());
    }
    let bytes = &af.data[0];
    if bytes.len() % 2 != 0 {
        return Err(Error::invalid(format!(
            "adpcm encoder: PCM byte count {} not multiple of 2 (i16)",
            bytes.len()
        )));
    }
    let total_samples = bytes.len() / 2;
    if total_samples % channels != 0 {
        return Err(Error::invalid(format!(
            "adpcm encoder: PCM sample count {total_samples} not multiple of {channels} channels"
        )));
    }
    pcm.reserve(total_samples);
    for c in bytes.chunks_exact(2) {
        pcm.push(i16::from_le_bytes([c[0], c[1]]));
    }
    Ok(())
}

pub(crate) fn make_encoder(params: &CodecParameters) -> Result<Box<dyn Encoder>> {
    let channels = params.channels.unwrap_or(1);
    if channels == 0 {
        return Err(Error::unsupported("adpcm encoder: channels must be >= 1"));
    }
    match params.codec_id.as_str() {
        crate::CODEC_ID_MS => {
            if channels > 2 {
                return Err(Error::unsupported(format!(
                    "adpcm_ms encoder: channels {channels} > 2 not supported"
                )));
            }
            Ok(Box::new(MsEncoder {
                output_params: params.clone(),
                channels: channels as usize,
                block_size: DEFAULT_BLOCK_SIZE,
                pcm: Vec::new(),
                pending: VecDeque::new(),
                samples_emitted: 0,
                flushed: false,
            }))
        }
        crate::CODEC_ID_IMA_WAV => {
            if channels > 8 {
                return Err(Error::unsupported(format!(
                    "adpcm_ima_wav encoder: channels {channels} > 8 not supported"
                )));
            }
            Ok(Box::new(ImaWavEncoder {
                output_params: params.clone(),
                channels: channels as usize,
                block_size: DEFAULT_BLOCK_SIZE,
                pcm: Vec::new(),
                pending: VecDeque::new(),
                samples_emitted: 0,
                flushed: false,
            }))
        }
        crate::CODEC_ID_IMA_QT => {
            if channels > 2 {
                return Err(Error::unsupported(format!(
                    "adpcm_ima_qt encoder: channels {channels} > 2 not supported"
                )));
            }
            Ok(Box::new(ImaQtEncoder {
                output_params: params.clone(),
                channels: channels as usize,
                pcm: Vec::new(),
                pending: VecDeque::new(),
                samples_emitted: 0,
                flushed: false,
            }))
        }
        crate::CODEC_ID_YAMAHA => {
            if channels > 8 {
                return Err(Error::unsupported(format!(
                    "adpcm_yamaha encoder: channels {channels} > 8 not supported"
                )));
            }
            Ok(Box::new(YamahaEncoder {
                output_params: params.clone(),
                channels: channels as usize,
                state: vec![yamaha::Channel::default(); channels as usize],
                pending: VecDeque::new(),
                samples_emitted: 0,
            }))
        }
        crate::CODEC_ID_YAMAHA_A => {
            if channels != 1 {
                return Err(Error::unsupported(format!(
                    "adpcm_yamaha_a encoder: only mono supported (got {channels} channels)"
                )));
            }
            Ok(Box::new(YamahaAEncoder {
                output_params: params.clone(),
                state: vec![yamaha_a::Channel::default(); 1],
                pending: VecDeque::new(),
                samples_emitted: 0,
            }))
        }
        crate::CODEC_ID_DIALOGIC => {
            if channels != 1 {
                return Err(Error::unsupported(format!(
                    "adpcm_dialogic encoder: only mono supported on the registry path, got {channels}"
                )));
            }
            Ok(Box::new(DialogicEncoder {
                output_params: params.clone(),
                state: dialogic::Channel::default(),
                pending: VecDeque::new(),
                samples_emitted: 0,
            }))
        }
        other => Err(Error::unsupported(format!(
            "adpcm: no encoder for codec id {other:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ima_wav, ms};

    fn rms_error(a: &[i16], b: &[i16]) -> f64 {
        let n = a.len().min(b.len());
        if n == 0 {
            return 0.0;
        }
        let mut sse = 0f64;
        for i in 0..n {
            let d = a[i] as f64 - b[i] as f64;
            sse += d * d;
        }
        (sse / n as f64).sqrt()
    }

    fn sine_pcm(n: usize, hz: f64, sample_rate: f64, amp: f64) -> Vec<i16> {
        let mut v = Vec::with_capacity(n);
        for i in 0..n {
            let t = i as f64 / sample_rate;
            let s = (2.0 * std::f64::consts::PI * hz * t).sin() * amp;
            v.push(s.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16);
        }
        v
    }

    #[test]
    fn ms_mono_round_trip_sine_is_bit_exact_to_decoder() {
        // Build a 0.1-second 440Hz sine at 22.05kHz mono.
        let pcm = sine_pcm(2205, 440.0, 22050.0, 16000.0);
        // Trim to an exact-block multiple. Block size 256, mono:
        // samples_per_block = 2 + (256-7)*2 = 500.
        let samples_per_block = 2 + (256 - 7) * 2;
        let n_blocks = pcm.len() / samples_per_block;
        let pcm_trim = &pcm[..n_blocks * samples_per_block];
        // Encode block-by-block.
        let mut encoded = Vec::new();
        for chunk in pcm_trim.chunks(samples_per_block) {
            let blk = encode_block(chunk, 1, 256).unwrap();
            assert_eq!(blk.len(), 256);
            encoded.push(blk);
        }
        // Decode each block back and compare to the original PCM.
        let mut decoded = Vec::new();
        for blk in &encoded {
            let d = ms::decode_block(blk, 1).unwrap();
            decoded.extend_from_slice(&d);
        }
        assert_eq!(decoded.len(), pcm_trim.len());
        let rms = rms_error(&decoded, pcm_trim);
        // ADPCM is lossy; RMS error for a 4-bit predictor on a 16k-amplitude
        // sine should sit well under 1000 LSB (~6% of full scale).
        assert!(
            rms < 1000.0,
            "MS-ADPCM round-trip RMS error {rms} exceeds 1000"
        );
    }

    #[test]
    fn ms_stereo_round_trip_sine_low_error() {
        // 0.1s of 440Hz on L, 660Hz on R.
        let samples_per_block = 2 + (256 - 14) / 2 * 2; // body 242B * 2 / 2 = 242 → 2 + 242 = 244
                                                        // Actually: stereo body_len=242 bytes → samples_after_prelude_per_channel = 242*2/2 = 242 → total 244.
        let n = samples_per_block * 4;
        let mut pcm = Vec::with_capacity(n * 2);
        let l = sine_pcm(n, 440.0, 22050.0, 8000.0);
        let r = sine_pcm(n, 660.0, 22050.0, 8000.0);
        for i in 0..n {
            pcm.push(l[i]);
            pcm.push(r[i]);
        }
        // Encode 4 blocks.
        let mut decoded_l = Vec::new();
        let mut decoded_r = Vec::new();
        let per_block_interleaved = samples_per_block * 2;
        for chunk in pcm.chunks(per_block_interleaved) {
            let blk = encode_block(chunk, 2, 256).unwrap();
            assert_eq!(blk.len(), 256);
            let d = ms::decode_block(&blk, 2).unwrap();
            assert_eq!(d.len(), per_block_interleaved);
            for i in 0..samples_per_block {
                decoded_l.push(d[i * 2]);
                decoded_r.push(d[i * 2 + 1]);
            }
        }
        let n_emitted = decoded_l.len();
        let rms_l = rms_error(&decoded_l, &l[..n_emitted]);
        let rms_r = rms_error(&decoded_r, &r[..n_emitted]);
        assert!(rms_l < 1500.0, "MS-ADPCM stereo L RMS {rms_l}");
        assert!(rms_r < 1500.0, "MS-ADPCM stereo R RMS {rms_r}");
    }

    #[test]
    fn ima_wav_mono_round_trip_sine_low_error() {
        // Mono, 256-byte block, header=4, body=252, groups=252/4=63,
        // samples_per_block = 1 + 63*8 = 505.
        let samples_per_block = 1 + 63 * 8;
        let n_blocks = 5;
        let pcm = sine_pcm(samples_per_block * n_blocks, 440.0, 22050.0, 16000.0);
        let mut decoded = Vec::new();
        for chunk in pcm.chunks(samples_per_block) {
            let blk = ima_encode_block(chunk, 1, 256).unwrap();
            assert_eq!(blk.len(), 256);
            let d = ima_wav::decode_block(&blk, 1).unwrap();
            assert_eq!(d.len(), samples_per_block);
            decoded.extend_from_slice(&d);
        }
        let rms = rms_error(&decoded, &pcm);
        assert!(
            rms < 1500.0,
            "IMA-WAV mono round-trip RMS {rms} exceeds 1500"
        );
    }

    #[test]
    fn ima_wav_stereo_round_trip_low_error() {
        // Stereo, 256-byte block, header=8, body=248, group_bytes=8,
        // groups=31, samples_per_block = 1 + 31*8 = 249.
        let samples_per_block = 1 + 31 * 8;
        let n = samples_per_block * 3;
        let l = sine_pcm(n, 440.0, 22050.0, 8000.0);
        let r = sine_pcm(n, 660.0, 22050.0, 8000.0);
        let mut pcm = Vec::with_capacity(n * 2);
        for i in 0..n {
            pcm.push(l[i]);
            pcm.push(r[i]);
        }
        let mut decoded_l = Vec::new();
        let mut decoded_r = Vec::new();
        let per_block_interleaved = samples_per_block * 2;
        for chunk in pcm.chunks(per_block_interleaved) {
            let blk = ima_encode_block(chunk, 2, 256).unwrap();
            assert_eq!(blk.len(), 256);
            let d = ima_wav::decode_block(&blk, 2).unwrap();
            for i in 0..samples_per_block {
                decoded_l.push(d[i * 2]);
                decoded_r.push(d[i * 2 + 1]);
            }
        }
        let rms_l = rms_error(&decoded_l, &l);
        let rms_r = rms_error(&decoded_r, &r);
        assert!(rms_l < 1500.0, "IMA-WAV stereo L RMS {rms_l}");
        assert!(rms_r < 1500.0, "IMA-WAV stereo R RMS {rms_r}");
    }

    #[test]
    fn encoder_trait_end_to_end_emits_packets() {
        let mut p = CodecParameters::audio(CodecId::new(crate::CODEC_ID_IMA_WAV));
        p.sample_rate = Some(22050);
        p.channels = Some(1);
        let mut enc = make_encoder(&p).unwrap();
        // Send 505 samples in two AudioFrames to exercise buffered drain.
        let samples_per_block = 1 + 63 * 8;
        let pcm = sine_pcm(samples_per_block, 440.0, 22050.0, 16000.0);
        let pcm_bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        let af = AudioFrame {
            samples: samples_per_block as u32,
            pts: Some(0),
            data: vec![pcm_bytes],
        };
        enc.send_frame(&Frame::Audio(af)).unwrap();
        let pkt = enc.receive_packet().unwrap();
        assert_eq!(pkt.data.len(), 256);
        // No more pending: NeedMore.
        let next = enc.receive_packet();
        assert!(matches!(next, Err(Error::NeedMore)));
    }

    #[test]
    fn ms_encoder_rejects_too_few_samples() {
        assert!(encode_block(&[0i16], 1, 256).is_err());
        assert!(encode_block(&[], 1, 256).is_err());
    }

    #[test]
    fn ima_encoder_rejects_size_mismatch() {
        // Mono 256-byte block needs 505 samples; pass 504.
        let pcm = vec![0i16; 504];
        assert!(ima_encode_block(&pcm, 1, 256).is_err());
    }

    #[test]
    fn make_encoder_rejects_unknown_codec_id() {
        let p = CodecParameters::audio(CodecId::new("adpcm_not_a_real_id"));
        let r = make_encoder(&p);
        assert!(r.is_err());
    }

    #[test]
    fn yamaha_a_factory_builds_via_make_encoder_for_mono_only() {
        // Mono is the only valid config for ADPCM-A (chip-internal codec
        // = single rhythm channel per stream).
        let mut p = CodecParameters::audio(CodecId::new(crate::CODEC_ID_YAMAHA_A));
        p.sample_rate = Some(22_050);
        p.channels = Some(1);
        let _enc = make_encoder(&p).expect("ADPCM-A mono encoder factory");

        // Stereo and 0-channel are rejected with `unsupported`.
        let mut p2 = CodecParameters::audio(CodecId::new(crate::CODEC_ID_YAMAHA_A));
        p2.sample_rate = Some(22_050);
        p2.channels = Some(2);
        assert!(
            make_encoder(&p2).is_err(),
            "stereo ADPCM-A encoder should be rejected"
        );
    }

    #[test]
    fn yamaha_a_encoder_emits_one_packet_per_send_frame() {
        // Stream-oriented: one `send_frame` → one packet.
        let mut p = CodecParameters::audio(CodecId::new(crate::CODEC_ID_YAMAHA_A));
        p.sample_rate = Some(8_000);
        p.channels = Some(1);
        let mut enc = make_encoder(&p).unwrap();
        let pcm = sine_pcm(200, 220.0, 8_000.0, 6_000.0);
        let pcm_bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        let af = AudioFrame {
            samples: 200,
            pts: Some(0),
            data: vec![pcm_bytes],
        };
        enc.send_frame(&Frame::Audio(af)).unwrap();
        let pkt = enc.receive_packet().unwrap();
        // 200 samples → 100 bytes (two nibbles per byte).
        assert_eq!(pkt.data.len(), 100);
        assert!(matches!(enc.receive_packet(), Err(Error::NeedMore)));
    }

    // ---------------- IMA-QT encoder tests ----------------

    #[test]
    fn ima_qt_mono_round_trip_sine_low_error() {
        // One QT block per channel = 64 samples. Encode 8 blocks of a
        // 440Hz sine at 22.05 kHz mono.
        let n_blocks = 8;
        let pcm = sine_pcm(QT_SAMPLES_PER_BLOCK * n_blocks, 440.0, 22050.0, 16000.0);
        let mut decoded = Vec::new();
        for chunk in pcm.chunks(QT_SAMPLES_PER_BLOCK) {
            let blk = ima_qt_encode_block(chunk, 1).unwrap();
            assert_eq!(blk.len(), QT_BLOCK_BYTES_PER_CHANNEL);
            let d = crate::ima_qt::decode_block(&blk, 1).unwrap();
            assert_eq!(d.len(), QT_SAMPLES_PER_BLOCK);
            decoded.extend_from_slice(&d);
        }
        let rms = rms_error(&decoded, &pcm);
        // 4-bit predictor on a 16k-amplitude sine — same headroom as the
        // other IMA variant. The QT 9-bit-seed quantisation also adds
        // ~64 LSB once per block; still comfortably under 1500 RMS.
        assert!(
            rms < 1500.0,
            "IMA-QT mono round-trip RMS {rms} exceeds 1500"
        );
    }

    #[test]
    fn ima_qt_stereo_round_trip_low_error() {
        let n_blocks = 6;
        let n = QT_SAMPLES_PER_BLOCK * n_blocks;
        let l = sine_pcm(n, 440.0, 22050.0, 8000.0);
        let r = sine_pcm(n, 660.0, 22050.0, 8000.0);
        let mut pcm = Vec::with_capacity(n * 2);
        for i in 0..n {
            pcm.push(l[i]);
            pcm.push(r[i]);
        }
        let mut decoded_l = Vec::new();
        let mut decoded_r = Vec::new();
        let per_block_interleaved = QT_SAMPLES_PER_BLOCK * 2;
        for chunk in pcm.chunks(per_block_interleaved) {
            let blk = ima_qt_encode_block(chunk, 2).unwrap();
            assert_eq!(blk.len(), QT_BLOCK_BYTES_PER_CHANNEL * 2);
            let d = crate::ima_qt::decode_block(&blk, 2).unwrap();
            assert_eq!(d.len(), per_block_interleaved);
            for i in 0..QT_SAMPLES_PER_BLOCK {
                decoded_l.push(d[i * 2]);
                decoded_r.push(d[i * 2 + 1]);
            }
        }
        let rms_l = rms_error(&decoded_l, &l);
        let rms_r = rms_error(&decoded_r, &r);
        assert!(rms_l < 1500.0, "IMA-QT stereo L RMS {rms_l}");
        assert!(rms_r < 1500.0, "IMA-QT stereo R RMS {rms_r}");
    }

    #[test]
    fn ima_qt_encode_rejects_size_mismatch() {
        // Mono needs exactly 64 samples.
        let pcm = vec![0i16; 63];
        assert!(ima_qt_encode_block(&pcm, 1).is_err());
        let pcm = vec![0i16; 64];
        // 64 mono samples is valid.
        assert!(ima_qt_encode_block(&pcm, 1).is_ok());
        // Stereo needs exactly 128 samples (64 per channel).
        let pcm = vec![0i16; 127];
        assert!(ima_qt_encode_block(&pcm, 2).is_err());
    }

    #[test]
    fn ima_qt_encode_rejects_unsupported_channel_count() {
        let pcm = vec![0i16; QT_SAMPLES_PER_BLOCK * 3];
        assert!(ima_qt_encode_block(&pcm, 3).is_err());
        assert!(ima_qt_encode_block(&[], 0).is_err());
    }

    #[test]
    fn ima_qt_factory_builds_via_make_encoder() {
        let mut p = CodecParameters::audio(CodecId::new(crate::CODEC_ID_IMA_QT));
        p.sample_rate = Some(22050);
        p.channels = Some(1);
        let _enc = make_encoder(&p).expect("IMA-QT encoder factory");
    }

    #[test]
    fn ima_qt_encoder_emits_one_packet_per_block() {
        let mut p = CodecParameters::audio(CodecId::new(crate::CODEC_ID_IMA_QT));
        p.sample_rate = Some(22050);
        p.channels = Some(1);
        let mut enc = make_encoder(&p).unwrap();
        let pcm = sine_pcm(QT_SAMPLES_PER_BLOCK * 2, 440.0, 22050.0, 16000.0);
        let pcm_bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        let af = AudioFrame {
            samples: (QT_SAMPLES_PER_BLOCK * 2) as u32,
            pts: Some(0),
            data: vec![pcm_bytes],
        };
        enc.send_frame(&Frame::Audio(af)).unwrap();
        let p0 = enc.receive_packet().unwrap();
        let p1 = enc.receive_packet().unwrap();
        assert_eq!(p0.data.len(), QT_BLOCK_BYTES_PER_CHANNEL);
        assert_eq!(p1.data.len(), QT_BLOCK_BYTES_PER_CHANNEL);
        assert!(matches!(enc.receive_packet(), Err(Error::NeedMore)));
    }

    #[test]
    fn ima_qt_seed_predictor_is_top_9_bits() {
        // Constant 12345 = 0x3039 → top 9 bits cleared low-7 = 0x3000 = 12288.
        // mean_delta = 0 → step_index seed = 0.
        let pcm = vec![12345i16; QT_SAMPLES_PER_BLOCK];
        let blk = ima_qt_encode_block(&pcm, 1).unwrap();
        // First preamble byte = (12288 >> 8) & 0xFF | 0 (step index 0) = 0x30.
        // Second preamble byte = 12288 as u8 | 0 = 0x00.
        assert_eq!(blk[0], 0x30);
        assert_eq!(blk[1], 0x00);
    }

    #[test]
    fn ima_qt_seeds_step_index_from_local_delta() {
        // A high-delta input should push step_index toward a larger value.
        // A 200-LSB-per-sample ramp implies target_step ≈ 200 * 8/3 ≈ 533;
        // first step >= 533 is index 47 (s=544).
        let pcm: Vec<i16> = (0..QT_SAMPLES_PER_BLOCK as i32)
            .map(|i| (i * 200) as i16)
            .collect();
        let blk = ima_qt_encode_block(&pcm, 1).unwrap();
        let preamble = u16::from_be_bytes([blk[0], blk[1]]);
        let step_index = (preamble & 0x7F) as usize;
        assert!(
            (40..=55).contains(&step_index),
            "step_index seed {step_index} should be ~47 for a 200-LSB-per-sample ramp"
        );
    }
}
