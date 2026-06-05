//! `oxideav_core::Decoder` impls — a small state machine that dispatches
//! each incoming packet to the right variant.
//!
//! All variants share the same pattern: `send_packet` parses the block /
//! packet immediately and stores the decoded PCM in `pending`, then
//! `receive_frame` emits exactly one `AudioFrame` and drops the buffer.

use crate::{dialogic, ima_qt, ima_wav, ms, yamaha, yamaha_a};
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
            _ => None,
        }
    }

    /// `WAVEFORMATEX::wFormatTag` for the variants that have a canonical
    /// WAV / AVI tag assignment:
    ///
    /// - `Ms` → `0x0002` (`WAVE_FORMAT_ADPCM`).
    /// - `ImaWav` → `0x0011` (`WAVE_FORMAT_DVI_ADPCM`).
    /// - `Yamaha` → `0x0020` (`WAVE_FORMAT_YAMAHA_ADPCM`).
    ///
    /// `None` for ADPCM-IMA-QT (QuickTime addresses it via a fourcc),
    /// ADPCM-A (chip-internal — no WAV assignment) and Dialogic VOX
    /// (headerless — no WAV assignment).
    pub const fn wave_format_tag(self) -> Option<u16> {
        match self {
            Variant::Ms => Some(0x0002),
            Variant::ImaWav => Some(0x0011),
            Variant::Yamaha => Some(0x0020),
            Variant::ImaQt | Variant::YamahaA | Variant::Dialogic => None,
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
        Variant::Ms | Variant::ImaQt => {
            if channels > 2 {
                return Err(Error::unsupported(format!(
                    "adpcm: {:?} variant supports 1 or 2 channels, got {channels}",
                    variant
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
    }
    Ok(Box::new(AdpcmDecoder {
        codec_id: params.codec_id.clone(),
        variant,
        channels,
        pending: None,
        yamaha_state: vec![yamaha::Channel::default(); channels as usize],
        yamaha_a_state: vec![yamaha_a::Channel::default(); channels as usize],
        dialogic_state: vec![dialogic::Channel::default(); channels as usize],
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
    pending: Option<PendingFrame>,
    // Yamaha carries state across packets; the other block-oriented
    // variants re-seed per block.
    yamaha_state: Vec<yamaha::Channel>,
    // Yamaha ADPCM-A is also stream-oriented (12-bit silicon).
    yamaha_a_state: Vec<yamaha_a::Channel>,
    // Dialogic / OKI VOX is also stream-oriented (state persists across
    // packets — no per-block resets).
    dialogic_state: Vec<dialogic::Channel>,
    eof: bool,
}

impl AdpcmDecoder {
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
            Variant::Ms => ms::decode_block(&pkt.data, channels)?,
            Variant::ImaWav => ima_wav::decode_block(&pkt.data, channels)?,
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
                dialogic::NibbleOrder::HiFirst,
                dialogic::Output::Wide16,
            ),
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
            *st = yamaha::Channel::default();
        }
        for st in &mut self.yamaha_a_state {
            *st = yamaha_a::Channel::default();
        }
        for st in &mut self.dialogic_state {
            *st = dialogic::Channel::default();
        }
        Ok(())
    }
}
