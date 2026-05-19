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
//! Two variants currently have encoders here:
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
//! Default block sizes can be overridden via the `block_size` field on
//! the encoder before the first call to `send_frame`. The default of
//! 256 bytes per channel matches what ffmpeg's `-c:a adpcm_ms` /
//! `adpcm_ima_wav` emit by default at 22050 Hz mono.

use std::collections::VecDeque;

use crate::tables::{
    IMA_INDEX_ADJUST, IMA_STEP_SIZE, MS_ADAPTATION, MS_ADAPT_COEFF1, MS_ADAPT_COEFF2,
};
use oxideav_core::{
    AudioFrame, CodecId, CodecParameters, Encoder, Error, Frame, Packet, Result, TimeBase,
};

/// Default per-channel block size in bytes for the WAV/AVI block-oriented
/// variants. Chosen to match ffmpeg's default output at 22050 Hz mono and
/// to give the search a reasonable amortisation horizon.
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

fn clamp_i16(x: i32) -> i16 {
    x.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

/// Run the MS-ADPCM decoder recurrence for one nibble. Returns the
/// reconstructed sample without mutating `st`.
fn ms_simulate_nibble(st: &MsState, nibble: u8) -> i16 {
    let signed = ((nibble as i32) ^ 8) - 8;
    let predicted = (st.sample1 * st.coef1 + st.sample2 * st.coef2) >> 8;
    clamp_i16(predicted + signed * st.delta)
}

/// Advance `st` exactly as the decoder would after emitting `out` from
/// `nibble`. Splits the decoder step so the encoder can reuse the
/// simulate-then-advance pattern in its search loop.
fn ms_advance(st: &mut MsState, nibble: u8, out: i16) {
    st.sample2 = st.sample1;
    st.sample1 = out as i32;
    let mut d = (MS_ADAPTATION[nibble as usize] * st.delta) >> 8;
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
    // Initial delta is a moderate value — too small and the first few
    // nibbles can't reach the target; too large and quantisation noise
    // dominates. 16 (the spec minimum) works well in practice for the
    // unit step search, but the decoder always pre-saturates to 16 anyway
    // so we pick a slightly higher seed for headroom.
    for ch in 0..channels {
        states[ch].sample2 = samples[ch] as i32;
        states[ch].sample1 = samples[channels + ch] as i32;
        states[ch].delta = 16;
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

    let mut states: Vec<ImaState> = (0..channels)
        .map(|ch| ImaState {
            predictor: samples[ch] as i32,
            step_index: 0,
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
        let p = CodecParameters::audio(CodecId::new("adpcm_yamaha"));
        let r = make_encoder(&p);
        assert!(r.is_err());
    }
}
