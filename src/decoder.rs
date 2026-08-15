//! `oxideav_core::Decoder` impls — a small state machine that dispatches
//! each incoming packet to the right variant.
//!
//! All variants share the same pattern: `send_packet` parses the block /
//! packet immediately and stores the decoded PCM in `pending`, then
//! `receive_frame` emits exactly one `AudioFrame` and drops the buffer.

use crate::{dialogic, g726, ima_qt, ima_wav, ms, yamaha, yamaha_a};
use oxideav_core::Decoder;
use oxideav_core::{AudioFrame, CodecId, CodecParameters, Error, Frame, Packet, Result};

/// Which of the supported variants this instance implements.
///
/// `Variant` is the typed counterpart of the `adpcm_*` codec-id string
/// table — callers that already know which variant they want (a fixture
/// loader pinning ADPCM-A; a unit test addressing one specific decoder;
/// a higher-level container demuxer that has already parsed the
/// WAVEFORMATEX `wFormatTag` or QuickTime fourcc) can construct it
/// directly and avoid round-tripping through a `&str`.
///
/// The accessors form a small inspection surface so the [`Variant`] enum
/// is interchangeable with the canonical id string + container tag without
/// re-typing the dispatch ladder in user code:
///
/// - [`Variant::codec_id`] — canonical `adpcm_*` id string (matches the
///   `CODEC_ID_*` constants in [`crate`]).
/// - [`Variant::from_codec_id`] — parse from a [`CodecId`] (None on
///   unknown ids).
/// - [`Variant::wave_format_tag`] — `WAVEFORMATEX::wFormatTag` for the
///   four variants that carry one; `None` for the three that don't
///   (QuickTime / ADPCM-A / VOX).
/// - [`Variant::fourcc`] — QuickTime / MP4 sample-entry FourCC for
///   ADPCM-IMA-QT; `None` for everything else.
/// - [`Variant::shape`] — `Shape::BlockOriented` for the WAV / AVI /
///   QuickTime per-block-reseed variants, `Shape::StreamOriented` for
///   the Yamaha / Dialogic per-channel-state-carries variants.
/// - [`Variant::max_channels`] — maximum channel count the factory
///   accepts for this variant (the [`Variant::Yamaha`] DELTA-T stream
///   has no upper bound and returns `None`; every other variant has a
///   hard cap derived from its container layer or chip topology).
/// - [`Variant::header_bytes`] — fixed per-block header byte count for
///   block-oriented variants (the spec-mandated value the decoder
///   parses and the encoder emits before the nibble body); `None` for
///   stream-oriented variants.
/// - [`Variant::samples_per_block`] — per-channel sample count produced
///   by one block of `block_bytes` for block-oriented variants;
///   `None` for stream-oriented variants and for inputs that don't
///   match the variant's framing constraints.
/// - [`Variant::block_size_bytes`] — inverse of
///   [`Variant::samples_per_block`]: the block byte count that encodes
///   exactly N samples per channel (the WAV `nBlockAlign` for a chosen
///   `nSamplesPerBlock`); `None` for stream-oriented variants and
///   off-boundary sample counts.
/// - [`Variant::all`] — slice of every supported variant, suitable for
///   `for v in Variant::all() { … }` iteration.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Variant {
    Ms,
    ImaWav,
    ImaQt,
    Yamaha,
    YamahaA,
    Dialogic,
    G726,
}

/// On-wire framing shape of an ADPCM variant.
///
/// The ADPCM family splits along a stark line in how it carries its
/// per-channel encoder state — predictor + step pointer — over the wire:
///
/// - **Block-oriented.** The header of each block re-seeds the predictor
///   and step pointer from explicit fields; subsequent blocks do not
///   depend on prior blocks. The decoder is *memoryless across blocks*
///   and `Decoder::reset` does not need to clear per-channel state.
///   Microsoft ADPCM (per-block 7-byte header per channel; Mc Gill §IV),
///   IMA-ADPCM-WAV (4-byte header per channel), and Apple QuickTime IMA
///   (2-byte big-endian preamble per channel — IMA spec §3.5.3 +
///   QuickTime ima4 sample-entry layout) all take this shape.
/// - **Stream-oriented.** No block framing — the byte stream is one
///   contiguous nibble run, and the predictor + step pointer carry
///   across packet boundaries indefinitely. `Decoder::reset` must clear
///   per-channel state. Yamaha ADPCM-B / DELTA-T (Y8950 manual §I-4,
///   AICA FQ8005 manual §I), Yamaha ADPCM-A (YM2608 / YM2610
///   rhythm-channel chip-internal stream — no header), and OKI /
///   Dialogic VOX (Dialogic 00-1366-001 §2 — `.vox` files are stored
///   as a continuous bit stream with no header) all carry state
///   forever.
///
/// The shape is observable in `Decoder::reset` semantics (block-oriented
/// variants are no-ops for state because there is no state to keep, and
/// stream-oriented variants must re-seed every per-channel `Channel`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Shape {
    /// Each block carries its own predictor / step-pointer header; the
    /// decoder re-seeds on every block boundary.
    BlockOriented,
    /// The byte stream is a continuous nibble run; predictor / step
    /// pointer state carry across packet boundaries indefinitely.
    StreamOriented,
}

impl Variant {
    /// Every supported [`Variant`] in declaration order — handy for
    /// table-driven registration tests, exhaustiveness audits, and
    /// container layers that probe codec parameters across the whole
    /// family.
    pub const fn all() -> &'static [Variant] {
        &[
            Variant::Ms,
            Variant::ImaWav,
            Variant::ImaQt,
            Variant::Yamaha,
            Variant::YamahaA,
            Variant::Dialogic,
            Variant::G726,
        ]
    }

    /// Canonical `adpcm_*` codec-id string for this variant. Always
    /// equal to one of the `CODEC_ID_*` constants in [`crate`].
    pub const fn codec_id(self) -> &'static str {
        match self {
            Variant::Ms => crate::CODEC_ID_MS,
            Variant::ImaWav => crate::CODEC_ID_IMA_WAV,
            Variant::ImaQt => crate::CODEC_ID_IMA_QT,
            Variant::Yamaha => crate::CODEC_ID_YAMAHA,
            Variant::YamahaA => crate::CODEC_ID_YAMAHA_A,
            Variant::Dialogic => crate::CODEC_ID_DIALOGIC,
            Variant::G726 => crate::CODEC_ID_G726,
        }
    }

    /// Parse a [`Variant`] from its canonical codec id. Returns `None`
    /// if `id` does not match a known `adpcm_*` codec.
    pub fn from_codec_id(id: &CodecId) -> Option<Self> {
        match id.as_str() {
            crate::CODEC_ID_MS => Some(Self::Ms),
            crate::CODEC_ID_IMA_WAV => Some(Self::ImaWav),
            crate::CODEC_ID_IMA_QT => Some(Self::ImaQt),
            crate::CODEC_ID_YAMAHA => Some(Self::Yamaha),
            crate::CODEC_ID_YAMAHA_A => Some(Self::YamahaA),
            crate::CODEC_ID_DIALOGIC => Some(Self::Dialogic),
            crate::CODEC_ID_G726 => Some(Self::G726),
            _ => None,
        }
    }

    /// `WAVEFORMATEX::wFormatTag` for the variants that have a canonical
    /// WAV / AVI tag assignment:
    ///
    /// - `Ms` → `0x0002` (`WAVE_FORMAT_ADPCM`).
    /// - `ImaWav` → `0x0011` (`WAVE_FORMAT_DVI_ADPCM`).
    /// - `Yamaha` → `0x0020` (`WAVE_FORMAT_YAMAHA_ADPCM`).
    /// - `Dialogic` → `0x0010` (`WAVE_FORMAT_OKI_ADPCM`) — the
    ///   WAV-container framing of the OKI ADPCM chip-set algorithm whose
    ///   headerless form is the `.vox` file. The 4-bit WAV-OKI body is
    ///   the canonical VOX layout (two samples per byte, high nibble
    ///   first), so the registry decoder handles it unchanged.
    /// - `G726` → `0x0040` (`WAVE_FORMAT_G721_ADPCM`) — the documented
    ///   WAV assignment for the 32 kbit/s (4-bit) rate, whose bit-exact
    ///   behaviour G.726 preserves (the 1990 Recommendation consolidates
    ///   G.721). The staged `WAVE_FORMAT_*` catalogue carries no tag for
    ///   the other three rates, so a demuxer resolving this tag should
    ///   run the decoder at its 4-bit default.
    ///
    /// `None` for ADPCM-IMA-QT (QuickTime addresses it via a fourcc) and
    /// ADPCM-A (chip-internal — no WAV assignment).
    ///
    /// The *canonical* tag is the first element of
    /// [`Variant::wave_format_tags`]; a couple of variants additionally
    /// answer to a documented alias tag (see that method).
    pub const fn wave_format_tag(self) -> Option<u16> {
        // `[T]::first()` is not const-stable on the pinned toolchain and
        // `is_empty()` trips `clippy::len_zero` in a const context, so
        // match the slice directly.
        match self.wave_format_tags() {
            [canonical, ..] => Some(*canonical),
            [] => None,
        }
    }

    /// Every `WAVEFORMATEX::wFormatTag` this variant answers to, in
    /// preference order — the canonical assignment first, then any
    /// documented alias tag that frames the *same* on-wire algorithm.
    /// Empty for the two tagless variants (IMA-QT is FourCC-routed,
    /// ADPCM-A is chip-internal).
    ///
    /// Most variants own exactly one tag. Two answer to a second,
    /// documented alias tag for the *same* ITU / OKI algorithm:
    ///
    /// - `Dialogic` also answers to `0x0203`
    ///   (`WAVE_FORMAT_DIALOGIC_OKI_ADPCM`) — the Dialogic WAV framing of
    ///   the same 4-bit OKI VOX body (mono, `nBlockAlign = 1`, no
    ///   extra-format-data) that `0x0010` (`WAVE_FORMAT_OKI_ADPCM`) also
    ///   carries. The staged catalogue records both as "created and read
    ///   by the OKI ADPCM chip set", so both decode byte-identically
    ///   through this variant's registry decoder.
    /// - `G726` also answers to `0x0014` (`WAVE_FORMAT_G723_ADPCM`) — the
    ///   older CCITT G.723 ADPCM, whose 3-bit (24 kbit/s) and 5-bit
    ///   (40 kbit/s) rates the 1990 G.726 Recommendation consolidates
    ///   alongside G.721 (`0x0040`, the 4-bit rate). The staged catalogue
    ///   notes the G.721 header format is "essentially the same as G.723";
    ///   the rate is selected from `wBitsPerSample` (the `bits_per_sample`
    ///   codec option on the registry decoder).
    ///
    /// This is the single source of truth for both
    /// [`Variant::wave_format_tag`] (which returns the first element) and
    /// the reverse [`Variant::from_wave_format_tag`]. Tag values mirror
    /// the `WAVE_FORMAT_*` enumeration staged in
    /// `docs/audio/adpcm/sdl_sound-wave-types.html`.
    pub const fn wave_format_tags(self) -> &'static [u16] {
        match self {
            Variant::Ms => &[0x0002],
            Variant::ImaWav => &[0x0011],
            Variant::Yamaha => &[0x0020],
            Variant::Dialogic => &[0x0010, 0x0203],
            Variant::G726 => &[0x0040, 0x0014],
            Variant::ImaQt | Variant::YamahaA => &[],
        }
    }

    /// QuickTime / MP4 sample-entry FourCC. Only ADPCM-IMA-QT carries a
    /// canonical FourCC (`ima4`); every other variant returns `None`.
    pub const fn fourcc(self) -> Option<[u8; 4]> {
        match self {
            Variant::ImaQt => Some(*b"ima4"),
            _ => None,
        }
    }

    /// Reverse of [`Variant::wave_format_tag`] — resolve a
    /// `WAVEFORMATEX::wFormatTag` to the ADPCM variant that owns it.
    ///
    /// This is the typed counterpart of the container's registry
    /// tag-resolution path: a WAV / AVI demuxer that has parsed the
    /// `fmt ` chunk's `wFormatTag` can map it straight to a [`Variant`]
    /// without round-tripping through a [`CodecId`] string. Only the five
    /// variants with a canonical WAV-format assignment resolve:
    ///
    /// - `0x0002` → `Variant::Ms` (`WAVE_FORMAT_ADPCM`).
    /// - `0x0010` / `0x0203` → `Variant::Dialogic` (`WAVE_FORMAT_OKI_ADPCM`
    ///   and its `WAVE_FORMAT_DIALOGIC_OKI_ADPCM` alias — the same 4-bit
    ///   OKI VOX body).
    /// - `0x0011` → `Variant::ImaWav` (`WAVE_FORMAT_DVI_ADPCM`).
    /// - `0x0020` → `Variant::Yamaha` (`WAVE_FORMAT_YAMAHA_ADPCM`).
    /// - `0x0040` / `0x0014` → `Variant::G726` (`WAVE_FORMAT_G721_ADPCM`
    ///   32 kbit/s 4-bit, and `WAVE_FORMAT_G723_ADPCM` 24/40 kbit/s
    ///   3-/5-bit; the 1990 G.726 Recommendation consolidates both older
    ///   G.721 and G.723 ADPCM standards).
    ///
    /// Every other `wFormatTag` — including tags that belong to *other*
    /// codec families (`0x0001` PCM, `0x0028` G.722, …) and the two
    /// tagless ADPCM variants (IMA-QT addressed by FourCC, ADPCM-A
    /// chip-internal) — returns `None`. The tag assignments mirror the
    /// `WAVE_FORMAT_*` enumeration staged in
    /// `docs/audio/adpcm/sdl_sound-wave-types.html`.
    ///
    /// `from_wave_format_tag(tag) == Some(v)` for every `tag` in every
    /// variant's [`Variant::wave_format_tags`], canonical or alias.
    pub const fn from_wave_format_tag(tag: u16) -> Option<Self> {
        match tag {
            0x0002 => Some(Variant::Ms),
            0x0010 | 0x0203 => Some(Variant::Dialogic),
            0x0011 => Some(Variant::ImaWav),
            0x0020 => Some(Variant::Yamaha),
            0x0040 | 0x0014 => Some(Variant::G726),
            _ => None,
        }
    }

    /// Reverse of [`Variant::fourcc`] — resolve a QuickTime / MP4
    /// sample-entry FourCC to the ADPCM variant that owns it.
    ///
    /// Only `*b"ima4"` resolves (to [`Variant::ImaQt`]); every other
    /// FourCC returns `None`. The typed counterpart of the container's
    /// FourCC routing for the one ADPCM variant QuickTime addresses by
    /// sample-entry code rather than `wFormatTag`.
    ///
    /// `from_fourcc(v.fourcc().unwrap()) == Some(v)` for every variant
    /// whose `fourcc()` is `Some`.
    pub const fn from_fourcc(fourcc: [u8; 4]) -> Option<Self> {
        // `match` on a byte array is not const-evaluable on all toolchains;
        // compare the packed big-endian word instead (FourCCs are stored
        // most-significant-byte-first, matching their textual order).
        let word = u32::from_be_bytes(fourcc);
        if word == u32::from_be_bytes(*b"ima4") {
            Some(Variant::ImaQt)
        } else {
            None
        }
    }

    /// On-wire framing shape — see [`Shape`].
    ///
    /// The three WAV / AVI / QuickTime variants (MS, IMA-WAV, IMA-QT)
    /// are [`Shape::BlockOriented`]: every block carries its own
    /// predictor + step-pointer header and the decoder re-seeds per
    /// block. The three chip-stream variants (Yamaha-B / DELTA-T,
    /// Yamaha-A, Dialogic VOX) are [`Shape::StreamOriented`]:
    /// predictor and step state persist across packet boundaries
    /// indefinitely (so `Decoder::reset` must clear every per-channel
    /// `Channel`).
    pub const fn shape(self) -> Shape {
        match self {
            Variant::Ms | Variant::ImaWav | Variant::ImaQt => Shape::BlockOriented,
            Variant::Yamaha | Variant::YamahaA | Variant::Dialogic | Variant::G726 => {
                Shape::StreamOriented
            }
        }
    }

    /// Maximum channel count the factory accepts for this variant.
    ///
    /// - `Variant::Ms` → `Some(2)` — Microsoft ADPCM is defined for
    ///   mono and stereo only (Mc Gill §IV).
    /// - `Variant::ImaWav` → `Some(8)` — IMA-WAV uses 4-byte-group
    ///   per-channel interleave; eight matches the WAVEFORMATEX 8-channel
    ///   speaker assignment ceiling the registry layer enforces.
    /// - `Variant::ImaQt` → `Some(8)` — QuickTime `ima4` carries one
    ///   independent 34-byte block per channel, round-robin (block-level
    ///   interleave). The layout has no intrinsic channel ceiling, so this
    ///   matches the multichannel surround layouts (mono / stereo / 4.0 /
    ///   5.1 / 7.1), capped at [`ima_qt::QT_MAX_CHANNELS`].
    /// - `Variant::Yamaha` → `None` — DELTA-T is a continuous nibble
    ///   stream with sample-level channel round-robin; the decoder
    ///   factory accepts any positive count.
    /// - `Variant::YamahaA` → `Some(1)` — YM2608 / YM2610 ADPCM-A
    ///   rhythm channels are individually single-channel streams (each
    ///   chip channel is its own decoder instance).
    /// - `Variant::Dialogic` → `Some(2)` — the VOX nibble interleave
    ///   convention (sample-level round-robin) is defined for mono and
    ///   stereo only.
    /// - `Variant::G726` → `Some(1)` — Recommendation G.726 is a
    ///   single-channel 8 kHz telephony codec; the headerless code
    ///   stream has no channel interleave convention.
    pub const fn max_channels(self) -> Option<u16> {
        match self {
            Variant::Ms => Some(2),
            Variant::ImaWav => Some(8),
            Variant::ImaQt => Some(ima_qt::QT_MAX_CHANNELS as u16),
            Variant::Yamaha => None,
            Variant::YamahaA => Some(1),
            Variant::Dialogic => Some(2),
            Variant::G726 => Some(1),
        }
    }

    /// Fixed per-block header byte count for [`Shape::BlockOriented`]
    /// variants, given a channel count.
    ///
    /// - `Variant::Ms` — `7 * channels` (per-channel predictor index
    ///   byte + signed-i16 initial delta + two signed-i16 history
    ///   samples).
    /// - `Variant::ImaWav` — `4 * channels` (per-channel signed-i16
    ///   predictor + u8 step index + reserved byte).
    /// - `Variant::ImaQt` — `2 * channels` (per-channel big-endian u16
    ///   preamble: 9-bit predictor + 7-bit step index).
    ///
    /// Returns `None` for [`Shape::StreamOriented`] variants
    /// ([`Variant::Yamaha`], [`Variant::YamahaA`], [`Variant::Dialogic`])
    /// which carry no block header — predictor + step state persist
    /// across packets indefinitely. Also returns `None` if `channels`
    /// is zero (no variant accepts a zero-channel layout).
    pub const fn header_bytes(self, channels: u16) -> Option<usize> {
        if channels == 0 {
            return None;
        }
        let ch = channels as usize;
        match self {
            Variant::Ms => Some(7 * ch),
            Variant::ImaWav => Some(4 * ch),
            Variant::ImaQt => Some(2 * ch),
            Variant::Yamaha | Variant::YamahaA | Variant::Dialogic | Variant::G726 => None,
        }
    }

    /// Per-channel sample count produced by a single block of
    /// `block_bytes` for [`Shape::BlockOriented`] variants.
    ///
    /// The formulas are spec-derived:
    ///
    /// - `Variant::Ms` — `2 + (body_bytes * 2) / channels`, where
    ///   `body_bytes = block_bytes - 7 * channels` (two history
    ///   samples seed the output from the header, then each body
    ///   nibble adds one sample). `block_bytes` must be ≥ the
    ///   `7 * channels` header; the body must be a whole number of
    ///   per-channel bytes.
    /// - `Variant::ImaWav` — `1 + groups * 8`, where
    ///   `groups = body_bytes / (4 * channels)` and the body length
    ///   must be a whole number of 4-byte-per-channel interleave
    ///   groups (one seed sample from the header predictor, then
    ///   8 samples per channel per group).
    /// - `Variant::ImaQt` — always [`ima_qt::QT_SAMPLES_PER_BLOCK`]
    ///   (64); `block_bytes` must equal `QT_BLOCK_SIZE * channels`
    ///   (34 * channels — the QuickTime `ima4` block layout is fixed).
    ///
    /// Returns `None` when:
    /// - the variant is [`Shape::StreamOriented`] (no block framing);
    /// - `channels` is zero or exceeds [`Self::max_channels`];
    /// - `block_bytes` is smaller than the per-channel header
    ///   ([`Self::header_bytes`]);
    /// - the body length doesn't match the variant's framing
    ///   constraint (per-channel multiple for MS, 4*channels group
    ///   multiple for IMA-WAV, exact `34*channels` for IMA-QT).
    ///
    /// This is the same formula the per-block decoders parse with —
    /// callers can size an output buffer up-front (`samples_per_block *
    /// channels * 2` bytes of i16-LE) without round-tripping through
    /// a probe-decode call.
    pub const fn samples_per_block(self, channels: u16, block_bytes: usize) -> Option<usize> {
        // Channel-count guard mirrors `make_decoder`.
        if channels == 0 {
            return None;
        }
        let ch = channels as usize;
        let max = match self.max_channels() {
            Some(m) => m as usize,
            None => return None, // stream-oriented (Yamaha-B) has no block
        };
        if ch > max {
            return None;
        }
        match self {
            Variant::Ms => {
                let header_len = 7 * ch;
                if block_bytes < header_len {
                    return None;
                }
                let body_len = block_bytes - header_len;
                // Body must be a whole number of per-channel bytes;
                // otherwise the per-channel nibble-pair count isn't an
                // integer.
                if body_len % ch != 0 {
                    return None;
                }
                Some(2 + (body_len * 2) / ch)
            }
            Variant::ImaWav => {
                let header_len = 4 * ch;
                if block_bytes < header_len {
                    return None;
                }
                let body_len = block_bytes - header_len;
                let group_bytes = 4 * ch;
                if body_len % group_bytes != 0 {
                    return None;
                }
                Some(1 + (body_len / group_bytes) * 8)
            }
            Variant::ImaQt => {
                // QT IMA: the block layout is fixed at 34 B per channel.
                if block_bytes != crate::ima_qt::QT_BLOCK_SIZE * ch {
                    return None;
                }
                Some(crate::ima_qt::QT_SAMPLES_PER_BLOCK)
            }
            Variant::Yamaha | Variant::YamahaA | Variant::Dialogic | Variant::G726 => None,
        }
    }

    /// Block byte count that encodes exactly `samples_per_channel` PCM
    /// samples per channel for a [`Shape::BlockOriented`] variant — the
    /// inverse of [`Self::samples_per_block`].
    ///
    /// A container layer that wants to emit fixed-duration blocks (the WAV
    /// `nSamplesPerBlock` field) can size each block's `nBlockAlign`
    /// up-front without re-deriving the per-variant framing formula. The
    /// returned size always round-trips: for any
    /// `Some(b) = block_size_bytes(ch, n)`,
    /// `samples_per_block(ch, b) == Some(n)`.
    ///
    /// The formulas invert the spec-derived layouts:
    ///
    /// - `Variant::Ms` — `7 * channels + ((n - 2) * channels) / 2`. The
    ///   header contributes the first 2 samples; every remaining sample
    ///   is one body nibble, two nibbles per byte, so the body spans
    ///   `(n - 2) / 2` bytes per channel. Requires `n >= 2` and
    ///   `(n - 2) * channels` even (a whole number of body bytes).
    /// - `Variant::ImaWav` — `4 * channels + groups * 4 * channels` with
    ///   `groups = (n - 1) / 8`. The header predictor seeds 1 sample;
    ///   each `4 * channels`-byte interleave group decodes 8 samples per
    ///   channel. Requires `n >= 1` and `(n - 1)` divisible by 8.
    /// - `Variant::ImaQt` — always `QT_BLOCK_SIZE * channels`
    ///   (`34 * channels`); the QuickTime `ima4` block decodes a fixed
    ///   [`ima_qt::QT_SAMPLES_PER_BLOCK`] (64) samples per channel, so
    ///   `n` must equal 64.
    ///
    /// Returns `None` when:
    /// - the variant is [`Shape::StreamOriented`] (no block framing);
    /// - `channels` is zero or exceeds [`Self::max_channels`];
    /// - `samples_per_channel` is below the variant's header-only minimum
    ///   (2 for MS, 1 for IMA-WAV, the fixed 64 for IMA-QT);
    /// - the requested sample count doesn't land on a whole-block
    ///   boundary (MS: `(n - 2) * channels` must be even; IMA-WAV:
    ///   `(n - 1)` must be a multiple of 8; IMA-QT: `n` must equal 64).
    pub const fn block_size_bytes(
        self,
        channels: u16,
        samples_per_channel: usize,
    ) -> Option<usize> {
        if channels == 0 {
            return None;
        }
        let ch = channels as usize;
        let max = match self.max_channels() {
            Some(m) => m as usize,
            None => return None, // stream-oriented (Yamaha-B) has no block
        };
        if ch > max {
            return None;
        }
        match self {
            Variant::Ms => {
                // Header always emits the first 2 samples.
                if samples_per_channel < 2 {
                    return None;
                }
                let body_samples = samples_per_channel - 2;
                // Body decodes 2 samples per byte per channel; the total
                // body byte run must be a whole number of bytes.
                let body_nibbles = body_samples * ch;
                if body_nibbles % 2 != 0 {
                    return None;
                }
                Some(7 * ch + body_nibbles / 2)
            }
            Variant::ImaWav => {
                // Header predictor seeds 1 sample.
                if samples_per_channel < 1 {
                    return None;
                }
                let body_samples = samples_per_channel - 1;
                // 8 samples per channel per 4*ch-byte group.
                if body_samples % 8 != 0 {
                    return None;
                }
                let groups = body_samples / 8;
                Some(4 * ch + groups * 4 * ch)
            }
            Variant::ImaQt => {
                // Fixed 34 B/channel block decodes exactly 64 samples.
                if samples_per_channel != crate::ima_qt::QT_SAMPLES_PER_BLOCK {
                    return None;
                }
                Some(crate::ima_qt::QT_BLOCK_SIZE * ch)
            }
            Variant::Yamaha | Variant::YamahaA | Variant::Dialogic | Variant::G726 => None,
        }
    }

    /// Build the codec `extradata` trailer a WAV muxer needs to frame this
    /// variant's blocks — the `WAVEFORMATEX` extension body (the bytes
    /// after the 16-byte `WAVEFORMATEX` base), **excluding** the leading
    /// `cbSize` word, matching this crate's `CodecParameters::extradata`
    /// convention. A muxer writing a `fmt ` chunk prepends
    /// `cbSize = returned.len()`.
    ///
    /// `block_align` is the WAV `nBlockAlign` (total block bytes across all
    /// channels) the muxer will use; the trailer's `wSamplesPerBlock` is
    /// derived from it via [`Self::samples_per_block`], so the value in the
    /// header is always consistent with the block geometry the decoder
    /// re-derives.
    ///
    /// Per variant:
    ///
    /// - `Variant::Ms` → the `ADPCMWAVEFORMAT` trailer
    ///   (`wSamplesPerBlock` + `wNumCoef = 7` + the seven preset
    ///   `aCoeff[]` pairs), `cbSize`-equivalent length 32. This is the
    ///   inverse of [`ms::parse_extradata_coeffs`], so the produced bytes
    ///   round-trip straight back through the decoder's `extradata` path.
    /// - `Variant::ImaWav` → `wSamplesPerBlock` only (`cbSize`-equivalent
    ///   length 2 — the IMA-WAV `fmt ` extension carries no coefficient
    ///   table).
    ///
    /// Returns `None` for:
    /// - [`Variant::ImaQt`] — QuickTime `ima4` is carried in an ISO-BMFF
    ///   sample entry, not a WAV `fmt ` chunk, so it has no `WAVEFORMATEX`
    ///   extension here;
    /// - the three [`Shape::StreamOriented`] variants (no block framing);
    /// - any `(channels, block_align)` pair whose
    ///   [`Self::samples_per_block`] is `None` (invalid block geometry —
    ///   bad channel count, too-small block, or off-boundary body length).
    pub fn build_wave_format_extra(self, channels: u16, block_align: usize) -> Option<Vec<u8>> {
        // G.726 in a WAV container (tags 0x0040 / 0x0014) is the one
        // stream-shaped variant with a `fmt ` extension: the single
        // `nAuxBlockSize` field. Only the aux-free form is derivable
        // from (channels, block_align) alone — `nBlockAlign` must then
        // be exactly 16 sub-blocks of `bits · channels` bytes at a
        // documented rate (48 / 96, 64 / 128, 80 / 160). A non-zero
        // aux prefix is ambiguous against a higher rate, so any other
        // geometry returns `None` (serialise it directly with
        // [`g726::wav_format_extra`] instead).
        if self == Variant::G726 {
            if channels == 0 || channels > 2 {
                return None;
            }
            let per = block_align / (16 * channels as usize);
            if per * 16 * channels as usize != block_align {
                return None;
            }
            let rate = g726::Rate::from_bits(u8::try_from(per).ok()?)?;
            if !g726::wav_rate_supported(rate) {
                return None;
            }
            return Some(g726::wav_format_extra(0).to_vec());
        }
        let spb = self.samples_per_block(channels, block_align)?;
        // wSamplesPerBlock is a u16 field on the wire; an out-of-range
        // block geometry has no valid header form.
        let spb_u16: u16 = spb.try_into().ok()?;
        match self {
            Variant::Ms => ms::build_extradata(spb_u16, &ms::STANDARD_COEFFS).ok(),
            Variant::ImaWav => Some(spb_u16.to_le_bytes().to_vec()),
            // QuickTime ima4 lives in an ISO-BMFF sample entry, not a WAV
            // fmt extension; stream variants carry no block header.
            Variant::ImaQt
            | Variant::Yamaha
            | Variant::YamahaA
            | Variant::Dialogic
            | Variant::G726 => None,
        }
    }
}

/// Parse the `chip` codec option into a [`yamaha::Chip`].
///
/// Accepted only for [`Variant::Yamaha`] (ADPCM-B / DELTA-T): `"aica"`
/// (default) or `"opna"`. Absent ⇒ `Chip::Aica`. For any other variant a
/// present `chip` option is rejected (the option is meaningless there).
pub(crate) fn parse_yamaha_chip_option(
    variant: Variant,
    params: &CodecParameters,
) -> Result<yamaha::Chip> {
    match params.options.get("chip") {
        None => Ok(yamaha::Chip::Aica),
        Some(v) => {
            if variant != Variant::Yamaha {
                return Err(Error::unsupported(format!(
                    "adpcm: chip option {v:?} is only valid for adpcm_yamaha, not {variant:?}"
                )));
            }
            match v {
                "aica" => Ok(yamaha::Chip::Aica),
                "opna" => Ok(yamaha::Chip::Opna),
                other => Err(Error::unsupported(format!(
                    "adpcm_yamaha: chip option {other:?} not supported (aica or opna)"
                ))),
            }
        }
    }
}

/// Parse the `quantizer` codec option into a
/// [`crate::encoder::Quantizer`].
///
/// Accepted only for the IMA variants ([`Variant::ImaWav`] /
/// [`Variant::ImaQt`]): `"search"` (default; the decoder-loop search)
/// or `"reference"` (the IMA Recommended Practices Appendix D §6.1 /
/// DVI reference ladder compressor). The option selects encoder
/// behaviour only — the emitted stream decodes identically either way —
/// so the decoder factory validates and ignores it, keeping an
/// encode→decode pair built from one `CodecParameters` working. For any
/// other variant a present option is rejected.
pub(crate) fn parse_ima_quantizer_option(
    variant: Variant,
    params: &CodecParameters,
) -> Result<crate::encoder::Quantizer> {
    match params.options.get("quantizer") {
        None => Ok(crate::encoder::Quantizer::Search),
        Some(v) => {
            if !matches!(variant, Variant::ImaWav | Variant::ImaQt) {
                return Err(Error::unsupported(format!(
                    "adpcm: quantizer option {v:?} is only valid for adpcm_ima_wav / adpcm_ima_qt, not {variant:?}"
                )));
            }
            match v {
                "search" => Ok(crate::encoder::Quantizer::Search),
                "reference" => Ok(crate::encoder::Quantizer::Reference),
                other => Err(Error::unsupported(format!(
                    "adpcm_ima: quantizer option {other:?} not supported (search or reference)"
                ))),
            }
        }
    }
}

/// Parse the `nibble_order` codec option into a [`dialogic::NibbleOrder`].
///
/// Accepted only for [`Variant::Dialogic`] (OKI / Dialogic VOX): `"hi"`
/// (default; Dialogic VOX / MSM6295) or `"lo"` (MSM6258). Absent ⇒
/// `NibbleOrder::HiFirst`. For any other variant a present option is
/// rejected.
pub(crate) fn parse_dialogic_order_option(
    variant: Variant,
    params: &CodecParameters,
) -> Result<dialogic::NibbleOrder> {
    match params.options.get("nibble_order") {
        None => Ok(dialogic::NibbleOrder::HiFirst),
        Some(v) => {
            if variant != Variant::Dialogic {
                return Err(Error::unsupported(format!(
                    "adpcm: nibble_order option {v:?} is only valid for adpcm_dialogic, not {variant:?}"
                )));
            }
            match v {
                "hi" => Ok(dialogic::NibbleOrder::HiFirst),
                "lo" => Ok(dialogic::NibbleOrder::LoFirst),
                other => Err(Error::unsupported(format!(
                    "adpcm_dialogic: nibble_order option {other:?} not supported (hi or lo)"
                ))),
            }
        }
    }
}

/// Parse the `bit_order` codec option into a [`g726::BitOrder`].
///
/// Accepted only for [`Variant::G726`]: `"msb"` (default; each code
/// packed from the byte's most-significant end — the network / RTP
/// convention) or `"lsb"` (least-significant end first). Absent ⇒
/// `BitOrder::MsbFirst`. For any other variant a present option is
/// rejected.
pub(crate) fn parse_g726_order_option(
    variant: Variant,
    params: &CodecParameters,
) -> Result<g726::BitOrder> {
    match params.options.get("bit_order") {
        None => Ok(g726::BitOrder::MsbFirst),
        Some(v) => {
            if variant != Variant::G726 {
                return Err(Error::unsupported(format!(
                    "adpcm: bit_order option {v:?} is only valid for adpcm_g726, not {variant:?}"
                )));
            }
            match v {
                "msb" => Ok(g726::BitOrder::MsbFirst),
                "lsb" => Ok(g726::BitOrder::LsbFirst),
                other => Err(Error::unsupported(format!(
                    "adpcm_g726: bit_order option {other:?} not supported (msb or lsb)"
                ))),
            }
        }
    }
}

/// Parse the `law` codec option into an optional [`g726::Law`].
///
/// Accepted only for [`Variant::G726`]: `"linear"` (default; the
/// 16-bit linear PCM interface), `"alaw"` or `"ulaw"` (the
/// Recommendation's G.711 log-PCM interface — §4.2.1 EXPAND on
/// encode, the §4.2.8 COMPRESS + SYNC output chain on decode — with
/// the law words carried as 16-bit linear frames on the law lattice).
/// For any other variant a present option is rejected.
pub(crate) fn parse_g726_law_option(
    variant: Variant,
    params: &CodecParameters,
) -> Result<Option<g726::Law>> {
    match params.options.get("law") {
        None => Ok(None),
        Some(v) => {
            if variant != Variant::G726 {
                return Err(Error::unsupported(format!(
                    "adpcm: law option {v:?} is only valid for adpcm_g726, not {variant:?}"
                )));
            }
            match v {
                "linear" => Ok(None),
                "alaw" => Ok(Some(g726::Law::ALaw)),
                "ulaw" => Ok(Some(g726::Law::ULaw)),
                other => Err(Error::unsupported(format!(
                    "adpcm_g726: law option {other:?} not supported (linear, alaw or ulaw)"
                ))),
            }
        }
    }
}

/// G.726 stream framing selected by the `framing` codec option.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub(crate) enum G726Framing {
    /// The raw bit-continuous telephony stream (default): one code
    /// after another, straddling byte boundaries, packed per the
    /// `bit_order` option.
    #[default]
    Raw,
    /// The G.723/G.721-in-WAV container layout (tags 0x0014 / 0x0040):
    /// whole-byte 8-sample sub-blocks per the staged bit-cell grid
    /// (fixed MSB-first), optionally prefixed per block by
    /// `aux_block_size` auxiliary bytes, mono or stereo.
    Wav,
}

/// Parse the `framing` codec option. Accepted only for
/// [`Variant::G726`]: `"raw"` (default) or `"wav"`.
pub(crate) fn parse_g726_framing_option(
    variant: Variant,
    params: &CodecParameters,
) -> Result<G726Framing> {
    match params.options.get("framing") {
        None => Ok(G726Framing::Raw),
        Some(v) => {
            if variant != Variant::G726 {
                return Err(Error::unsupported(format!(
                    "adpcm: framing option {v:?} is only valid for adpcm_g726, not {variant:?}"
                )));
            }
            match v {
                "raw" => Ok(G726Framing::Raw),
                "wav" => Ok(G726Framing::Wav),
                other => Err(Error::unsupported(format!(
                    "adpcm_g726: framing option {other:?} not supported (raw or wav)"
                ))),
            }
        }
    }
}

/// Parse the `aux_block_size` codec option (`nAuxBlockSize` — bytes of
/// auxiliary data at the start of every WAV data block). Accepted only
/// for [`Variant::G726`] with `framing=wav`; defaults to 0.
pub(crate) fn parse_g726_aux_option(
    variant: Variant,
    framing: G726Framing,
    params: &CodecParameters,
) -> Result<usize> {
    match params.options.get("aux_block_size") {
        None => Ok(0),
        Some(v) => {
            if variant != Variant::G726 || framing != G726Framing::Wav {
                return Err(Error::unsupported(format!(
                    "adpcm: aux_block_size option {v:?} is only valid for adpcm_g726 \
                     with framing=wav"
                )));
            }
            v.parse().map_err(|_| {
                Error::invalid(format!(
                    "adpcm_g726: aux_block_size option {v:?} is not a number"
                ))
            })
        }
    }
}

pub(crate) fn make_decoder(params: &CodecParameters) -> Result<Box<dyn Decoder>> {
    let variant = Variant::from_codec_id(&params.codec_id).ok_or_else(|| {
        Error::unsupported(format!(
            "adpcm: codec id {:?} not supported by oxideav-adpcm",
            params.codec_id
        ))
    })?;
    let channels = params.channels.unwrap_or(1);
    if channels == 0 {
        return Err(Error::unsupported("adpcm: channel count must be >= 1"));
    }
    match variant {
        Variant::Ms => {
            if channels > 2 {
                return Err(Error::unsupported(format!(
                    "adpcm: {:?} variant supports 1 or 2 channels, got {channels}",
                    variant
                )));
            }
        }
        Variant::ImaQt => {
            if channels as usize > ima_qt::QT_MAX_CHANNELS {
                return Err(Error::unsupported(format!(
                    "adpcm_ima_qt: supports up to {} channels, got {channels}",
                    ima_qt::QT_MAX_CHANNELS
                )));
            }
        }
        Variant::ImaWav => {
            if channels > 8 {
                return Err(Error::unsupported(format!(
                    "adpcm_ima_wav: supports up to 8 channels, got {channels}"
                )));
            }
        }
        Variant::Yamaha => {}
        Variant::YamahaA => {
            // YM2608 / YM2610 rhythm channels are individually single-
            // channel ADPCM-A streams; we reject anything other than
            // mono to keep the wire contract obvious.
            if channels != 1 {
                return Err(Error::unsupported(format!(
                    "adpcm_yamaha_a: only mono supported (got {channels} channels)"
                )));
            }
        }
        Variant::Dialogic => {
            dialogic::validate_channels(channels)?;
        }
        Variant::G726 => {
            // Recommendation G.726 is a single-channel 8 kHz telephony
            // codec; the raw headerless code stream defines no channel
            // interleave. The G.723/G.721-in-WAV container layout
            // (`framing=wav`) does define a two-channel sub-block
            // interleave, so stereo is accepted there.
            let framing = parse_g726_framing_option(variant, params)?;
            let max = if framing == G726Framing::Wav { 2 } else { 1 };
            if channels as usize > max {
                return Err(Error::unsupported(format!(
                    "adpcm_g726: {channels} channels not supported ({} with framing={})",
                    if max == 2 { "1 or 2" } else { "mono only" },
                    if framing == G726Framing::Wav {
                        "wav"
                    } else {
                        "raw"
                    },
                )));
            }
        }
    }
    // `bits_per_sample` codec option — IMA-WAV (tag 0x0011) defines both
    // 4-bit (default) and 3-bit coding via WAVEFORMATEX::wBitsPerSample,
    // and G.726 selects its operating rate by code width (2/3/4/5 bits =
    // 16/24/32/40 kbit/s; 4-bit / 32 kbit/s is the default and the
    // WAV-tag-0x0040 convention). Other variants have a fixed code width
    // and reject overrides.
    let mut ima_bits: u8 = 4;
    let mut g726_rate = g726::Rate::R32;
    if let Some(v) = params.options.get("bits_per_sample") {
        let bits: u8 = v.parse().map_err(|_| {
            Error::invalid(format!(
                "adpcm: bits_per_sample option {v:?} is not a number"
            ))
        })?;
        match (variant, bits) {
            (Variant::ImaWav, 3 | 4) => ima_bits = bits,
            (Variant::ImaWav, other) => {
                return Err(Error::unsupported(format!(
                    "adpcm_ima_wav: bits_per_sample {other} not supported (3 or 4)"
                )));
            }
            (Variant::G726, 2..=5) => {
                g726_rate = g726::Rate::from_bits(bits).expect("2..=5 maps to a rate");
            }
            (Variant::G726, other) => {
                return Err(Error::unsupported(format!(
                    "adpcm_g726: bits_per_sample {other} not supported (2, 3, 4 or 5)"
                )));
            }
            (_, 4) => {} // every other variant is natively 4-bit
            (v, other) => {
                return Err(Error::unsupported(format!(
                    "adpcm: bits_per_sample {other} not supported for {v:?} (fixed 4-bit)"
                )));
            }
        }
    }
    // Microsoft ADPCM may declare custom predictor coefficient sets in its
    // WAVEFORMATEX trailer (`wNumCoef` / `aCoeff[]`); a per-block predictor
    // index can then point at a custom set (>= 7). Resolve the table once
    // from `extradata`; an empty trailer keeps the 7 standard presets.
    let ms_coeffs = if variant == Variant::Ms {
        ms::parse_extradata_coeffs(&params.extradata)?
    } else {
        None
    };
    // `block_align` codec option — the WAV `nBlockAlign` (bytes per block,
    // summed over all channels). When the demuxer passes it, the
    // block-oriented MS / IMA-WAV decoders split a multi-block packet into
    // its constituent blocks; without it the packet is decoded as one
    // block (the prior behaviour). Stream-oriented variants ignore it.
    let block_align: Option<usize> = match params.options.get("block_align") {
        Some(v) => {
            let n: usize = v.parse().map_err(|_| {
                Error::invalid(format!("adpcm: block_align option {v:?} is not a number"))
            })?;
            if n == 0 {
                return Err(Error::invalid("adpcm: block_align option must be non-zero"));
            }
            Some(n)
        }
        None => None,
    };
    // `chip` codec option — Yamaha ADPCM-B emulates one of two
    // documented chip families whose step-adaptation constants differ:
    // `aica` (default; AICA FQ8005 / Y8950 / YMZ280B, the WAV-tag-0x0020
    // convention) or `opna` (YM2608 OPNA Application Manual Table 5-1).
    // The synthesis recurrence is identical; only the per-magnitude step
    // multiplier table differs, so a long stream diverges when decoded
    // against the wrong constants. Other variants reject the option.
    let yamaha_chip = parse_yamaha_chip_option(variant, params)?;
    // `quantizer` codec option — encoder-side nibble-selection strategy
    // for the IMA variants. The stream decodes identically either way,
    // so the decoder validates the option (foreign variants reject it,
    // bad values error) and otherwise ignores it — an encode→decode
    // pair built from one `CodecParameters` keeps working.
    let _ = parse_ima_quantizer_option(variant, params)?;
    // `nibble_order` codec option — OKI / Dialogic chips read the two
    // nibbles in a byte in opposite orders: `hi` (default; Dialogic VOX /
    // MSM6295, high nibble = first sample) or `lo` (MSM6258, low nibble =
    // first sample). The arithmetic is identical; only the unpack order
    // differs. Other variants reject the option.
    let dialogic_order = parse_dialogic_order_option(variant, params)?;
    // `bit_order` codec option — G.726 streams are packed either from
    // the byte's most-significant end (`msb`, default — the network /
    // RTP convention) or from the least-significant end (`lsb`). The
    // codes are identical; only the in-byte placement differs. Other
    // variants reject the option.
    let g726_order = parse_g726_order_option(variant, params)?;
    // `law` codec option — G.726 log-PCM (A-law / µ-law) interface
    // per §4.2.1 / §4.2.8; `None` keeps the linear interface.
    let g726_law = parse_g726_law_option(variant, params)?;
    // `framing` / `aux_block_size` codec options — the G.723/G.721-in-
    // WAV container layout vs the raw bit-continuous telephony stream.
    // The WAV layout fixes the bit order (MSB-first per the staged
    // bit-cell grid) and carries only the documented 3-/4-/5-bit rates.
    let g726_framing = parse_g726_framing_option(variant, params)?;
    let mut g726_aux = parse_g726_aux_option(variant, g726_framing, params)?;
    // With no explicit `aux_block_size` option, a WAV demuxer may hand
    // the `fmt ` extension through `extradata` instead — the
    // catalogue's one-field `nAuxBlockSize` form. An explicit option
    // wins over extradata.
    if g726_framing == G726Framing::Wav && params.options.get("aux_block_size").is_none() {
        if let Some(aux) = g726::wav_parse_format_extra(&params.extradata) {
            g726_aux = aux as usize;
        }
    }
    if g726_framing == G726Framing::Wav {
        if !g726::wav_rate_supported(g726_rate) {
            return Err(Error::unsupported(
                "adpcm_g726: framing=wav carries only the 3-, 4- and 5-bit rates \
                 (bits_per_sample 2 has no WAV tag)",
            ));
        }
        if g726_order == g726::BitOrder::LsbFirst {
            return Err(Error::unsupported(
                "adpcm_g726: framing=wav fixes the sub-block bit order (MSB-first \
                 per the bit-cell grid); bit_order=lsb is not applicable",
            ));
        }
    }
    let g726_lanes = if g726_framing == G726Framing::Wav {
        channels as usize
    } else {
        1
    };
    Ok(Box::new(AdpcmDecoder {
        codec_id: params.codec_id.clone(),
        variant,
        channels,
        ima_bits,
        ms_coeffs,
        block_align,
        dialogic_order,
        pending: None,
        yamaha_chip,
        yamaha_state: vec![yamaha::Channel::for_chip(yamaha_chip); channels as usize],
        yamaha_a_state: vec![yamaha_a::Channel::default(); channels as usize],
        dialogic_state: vec![dialogic::Channel::default(); channels as usize],
        g726_rate,
        g726_states: vec![g726::State::new(g726_rate); g726_lanes],
        g726_unpacker: g726::BitUnpacker::new(g726_order),
        g726_order,
        g726_law,
        g726_framing,
        g726_aux,
        g726_block_pos: 0,
        g726_code_carry: Vec::new(),
        eof: false,
    }))
}

struct PendingFrame {
    pts: Option<i64>,
    samples: u32,
    data: Vec<u8>,
}

pub struct AdpcmDecoder {
    codec_id: CodecId,
    variant: Variant,
    channels: u16,
    // IMA-WAV code width: 4 (default) or 3, from the `bits_per_sample`
    // codec option. Unused by the other variants.
    ima_bits: u8,
    // Microsoft-ADPCM resolved `aCoeff` table. `None` ⇒ the 7 standard
    // presets ([`ms::STANDARD_COEFFS`]); `Some` ⇒ a custom table parsed
    // from the WAVEFORMATEX trailer. Unused by the other variants.
    ms_coeffs: Option<Vec<ms::CoefPair>>,
    // WAV `nBlockAlign` (bytes per block, all channels) from the
    // `block_align` codec option. When set, a packet carrying several
    // concatenated MS / IMA-WAV blocks is split into that many per-block
    // decodes (each block re-seeds the predictor from its own header).
    // `None` keeps the historical behaviour: the whole packet is treated
    // as a single block, correct only when the producer already split the
    // stream one-block-per-packet. The QuickTime IMA path derives its own
    // fixed 34-byte block and ignores this field.
    block_align: Option<usize>,
    // OKI / Dialogic nibble unpack order from the `nibble_order` codec
    // option: `HiFirst` (default; Dialogic VOX / MSM6295) or `LoFirst`
    // (MSM6258). Unused by the other variants.
    dialogic_order: dialogic::NibbleOrder,
    pending: Option<PendingFrame>,
    // Selected Yamaha ADPCM-B chip (AICA vs OPNA) from the `chip` codec
    // option; retained so `reset` re-seeds the per-channel state with the
    // same constants instead of falling back to the default.
    yamaha_chip: yamaha::Chip,
    // Yamaha carries state across packets; the other block-oriented
    // variants re-seed per block. The per-channel `chip` field (set from
    // the `chip` codec option) selects the AICA vs OPNA step constants.
    yamaha_state: Vec<yamaha::Channel>,
    // Yamaha ADPCM-A is also stream-oriented (12-bit silicon).
    yamaha_a_state: Vec<yamaha_a::Channel>,
    // Dialogic / OKI VOX is also stream-oriented (state persists across
    // packets — no per-block resets).
    dialogic_state: Vec<dialogic::Channel>,
    // G.726 is stream-oriented at the *bit* level: codec state carries
    // across packets and, at the 3- and 5-bit rates, so does a partial
    // code word (the unpacker's residual bits). The rate and in-byte
    // order are retained so `reset` re-seeds with the same options.
    // One state per lane: a single entry for the raw telephony stream,
    // one per channel under the WAV sub-block framing (which defines a
    // stereo interleave; each lane is an independent codec).
    g726_rate: g726::Rate,
    g726_states: Vec<g726::State>,
    g726_unpacker: g726::BitUnpacker,
    g726_order: g726::BitOrder,
    // `framing` codec option: raw bit-continuous stream (default) vs
    // the G.723/G.721-in-WAV sub-block layout, with `aux_block_size`
    // auxiliary bytes at the start of every `nBlockAlign` block. The
    // byte position within the current block persists across packets so
    // a demuxer may split blocks arbitrarily.
    g726_framing: G726Framing,
    g726_aux: usize,
    g726_block_pos: usize,
    // Codes unpacked but not yet decoded because they do not complete a
    // whole interleave frame (stereo WAV framing only; 0..lanes-1
    // entries). Keeps every lane's state fed in order even when a
    // packet split lands mid-frame.
    g726_code_carry: Vec<u8>,
    // `Some` ⇒ the G.711 log-PCM interface: each code is decoded
    // through the §4.2.8 COMPRESS + SYNC chain and the resulting law
    // word expanded back to 16-bit linear (the output PCM sits on the
    // law lattice). `None` ⇒ the linear interface.
    g726_law: Option<g726::Law>,
    eof: bool,
}

impl AdpcmDecoder {
    /// Remove the per-block `aux_block_size` auxiliary prefix from a
    /// G.726 `framing=wav` payload, tracking the byte position within
    /// the current `nBlockAlign` block across packets (a demuxer may
    /// split blocks arbitrarily — including mid-prefix).
    fn g726_strip_aux_incremental(&mut self, data: &[u8]) -> Vec<u8> {
        let block = g726::wav_block_align(self.g726_rate, self.channels, self.g726_aux);
        let mut out = Vec::with_capacity(data.len());
        for &b in data {
            if self.g726_block_pos >= self.g726_aux {
                out.push(b);
            }
            self.g726_block_pos += 1;
            if self.g726_block_pos == block {
                self.g726_block_pos = 0;
            }
        }
        out
    }

    /// Decode a (possibly multi-block) packet of a block-oriented variant.
    ///
    /// `decode_one` decodes exactly one block (`bytes`, `channels`) into
    /// interleaved i16. When `self.block_align` is `Some(n)` and the packet
    /// is longer than one block, the packet is split into `n`-byte blocks
    /// (a shorter trailing remainder is decoded as a final block) and the
    /// per-block PCM is concatenated. When `block_align` is `None`, the
    /// whole packet is handed to `decode_one` as a single block — the
    /// historical behaviour, exact when the producer already framed one
    /// block per packet.
    fn decode_blocked<F>(&self, data: &[u8], channels: usize, decode_one: F) -> Result<Vec<i16>>
    where
        F: Fn(&[u8], usize) -> Result<Vec<i16>>,
    {
        match self.block_align {
            Some(blk) if data.len() > blk => {
                let mut out = Vec::new();
                let mut off = 0;
                while off < data.len() {
                    let end = (off + blk).min(data.len());
                    out.extend_from_slice(&decode_one(&data[off..end], channels)?);
                    off = end;
                }
                Ok(out)
            }
            _ => decode_one(data, channels),
        }
    }

    fn decode_packet(&mut self, pkt: &Packet) -> Result<()> {
        if pkt.data.is_empty() {
            self.pending = Some(PendingFrame {
                pts: pkt.pts,
                samples: 0,
                data: Vec::new(),
            });
            return Ok(());
        }
        let channels = self.channels as usize;
        let pcm = match self.variant {
            // MS / IMA-WAV are block-oriented: each block re-seeds the
            // predictor from its own header. A producer that hands the
            // decoder a packet spanning several blocks (a whole WAV `data`
            // chunk, an AVI audio chunk, a large read buffer) is decoded
            // block-by-block when `block_align` is known; otherwise the
            // packet is taken as a single block (back-compatible — the
            // common one-block-per-packet path is unchanged).
            Variant::Ms => {
                self.decode_blocked(&pkt.data, channels, |data, ch| match &self.ms_coeffs {
                    Some(coeffs) => ms::decode_block_with_coeffs(data, ch, coeffs),
                    None => ms::decode_block(data, ch),
                })?
            }
            Variant::ImaWav if self.ima_bits == 3 => {
                self.decode_blocked(&pkt.data, channels, |data, ch| {
                    ima_wav::decode_block_3bit(data, ch)
                })?
            }
            Variant::ImaWav => self.decode_blocked(&pkt.data, channels, ima_wav::decode_block)?,
            Variant::ImaQt => {
                // Packet may contain N·(channels·34) bytes — iterate.
                let blk = ima_qt::QT_BLOCK_SIZE * channels;
                if pkt.data.len() % blk != 0 {
                    return Err(Error::invalid(format!(
                        "adpcm_ima_qt: packet {} bytes is not a multiple of {blk} ({channels}ch × 34B)",
                        pkt.data.len()
                    )));
                }
                let mut out = Vec::with_capacity(
                    (pkt.data.len() / blk) * ima_qt::QT_SAMPLES_PER_BLOCK * channels,
                );
                for chunk in pkt.data.chunks_exact(blk) {
                    let block_pcm = ima_qt::decode_block(chunk, channels)?;
                    out.extend_from_slice(&block_pcm);
                }
                out
            }
            Variant::Yamaha => yamaha::decode_packet(&pkt.data, &mut self.yamaha_state),
            Variant::YamahaA => yamaha_a::decode_packet(
                &pkt.data,
                &mut self.yamaha_a_state,
                yamaha_a::Output::Wide16,
            ),
            Variant::Dialogic => dialogic::decode_packet(
                &pkt.data,
                &mut self.dialogic_state,
                self.dialogic_order,
                dialogic::Output::Wide16,
            ),
            Variant::G726 => {
                // Under the WAV sub-block framing, strip the per-block
                // auxiliary prefix first (position persists across
                // packets — a demuxer may split blocks arbitrarily).
                // The remaining bytes are one continuous MSB-first
                // code stream in both framings, round-robin across the
                // per-lane states (a single lane for raw / mono).
                let stripped;
                let payload: &[u8] = if self.g726_framing == G726Framing::Wav && self.g726_aux > 0 {
                    stripped = self.g726_strip_aux_incremental(&pkt.data);
                    &stripped
                } else {
                    &pkt.data
                };
                let mut codes = std::mem::take(&mut self.g726_code_carry);
                self.g726_unpacker
                    .feed(payload, self.g726_rate.bits(), &mut codes);
                let lanes = self.g726_states.len();
                // Hold back codes that do not complete a whole
                // interleave frame (possible when a stereo WAV-framing
                // packet split lands mid-frame) so lane order survives
                // the packet boundary.
                let rem = codes.len() % lanes;
                self.g726_code_carry = codes.split_off(codes.len() - rem);
                match self.g726_law {
                    None => codes
                        .iter()
                        .enumerate()
                        .map(|(n, &c)| self.g726_states[n % lanes].decode_i16(c))
                        .collect(),
                    Some(law) => {
                        // Log-PCM interface: §4.2.8 COMPRESS + SYNC per
                        // code, then the law word expanded to 16-bit
                        // linear (the PCM sits on the law lattice).
                        codes
                            .iter()
                            .enumerate()
                            .map(|(n, &c)| {
                                g726::expand_i16(
                                    self.g726_states[n % lanes].decode_law(c, law),
                                    law,
                                )
                            })
                            .collect()
                    }
                }
            }
        };

        // Interleaved i16 → little-endian bytes.
        let samples_total = pcm.len();
        if samples_total % channels != 0 {
            return Err(Error::invalid(format!(
                "adpcm: decoded {samples_total} samples not divisible by {channels} channels"
            )));
        }
        let samples_per_channel = (samples_total / channels) as u32;
        let mut bytes = Vec::with_capacity(samples_total * 2);
        for s in pcm {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        self.pending = Some(PendingFrame {
            pts: pkt.pts,
            samples: samples_per_channel,
            data: bytes,
        });
        Ok(())
    }
}

impl Decoder for AdpcmDecoder {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        if self.pending.is_some() {
            return Err(Error::other(
                "adpcm decoder: call receive_frame before sending another packet",
            ));
        }
        self.decode_packet(packet)
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        let Some(pf) = self.pending.take() else {
            return if self.eof {
                Err(Error::Eof)
            } else {
                Err(Error::NeedMore)
            };
        };
        Ok(Frame::Audio(AudioFrame {
            samples: pf.samples,
            pts: pf.pts,
            data: vec![pf.data],
        }))
    }

    fn flush(&mut self) -> Result<()> {
        self.eof = true;
        Ok(())
    }

    fn reset(&mut self) -> Result<()> {
        self.pending = None;
        self.eof = false;
        // Re-seed per-channel Yamaha (A + B) + Dialogic state. MS /
        // IMA are memoryless per block so no further action needed.
        for st in &mut self.yamaha_state {
            *st = yamaha::Channel::for_chip(self.yamaha_chip);
        }
        for st in &mut self.yamaha_a_state {
            *st = yamaha_a::Channel::default();
        }
        for st in &mut self.dialogic_state {
            *st = dialogic::Channel::default();
        }
        for st in &mut self.g726_states {
            *st = g726::State::new(self.g726_rate);
        }
        self.g726_unpacker = g726::BitUnpacker::new(self.g726_order);
        self.g726_block_pos = 0;
        self.g726_code_carry.clear();
        Ok(())
    }
}
