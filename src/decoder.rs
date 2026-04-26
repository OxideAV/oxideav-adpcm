//! `oxideav_core::Decoder` impls — a small state machine that dispatches
//! each incoming packet to the right variant.
//!
//! All variants share the same pattern: `send_packet` parses the block /
//! packet immediately and stores the decoded PCM in `pending`, then
//! `receive_frame` emits exactly one `AudioFrame` and drops the buffer.

use crate::{ima_qt, ima_wav, ms, yamaha};
use oxideav_core::Decoder;
use oxideav_core::{
    AudioFrame, CodecId, CodecParameters, Error, Frame, Packet, Result, SampleFormat, TimeBase,
};

/// Which of the four variants this instance implements.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Variant {
    Ms,
    ImaWav,
    ImaQt,
    Yamaha,
}

impl Variant {
    fn from_codec_id(id: &CodecId) -> Option<Self> {
        match id.as_str() {
            crate::CODEC_ID_MS => Some(Self::Ms),
            crate::CODEC_ID_IMA_WAV => Some(Self::ImaWav),
            crate::CODEC_ID_IMA_QT => Some(Self::ImaQt),
            crate::CODEC_ID_YAMAHA => Some(Self::Yamaha),
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
    }
    let sample_rate = params.sample_rate.unwrap_or(8_000);
    let time_base = TimeBase::new(1, sample_rate as i64);
    Ok(Box::new(AdpcmDecoder {
        codec_id: params.codec_id.clone(),
        variant,
        channels,
        sample_rate,
        time_base,
        pending: None,
        yamaha_state: vec![yamaha::Channel::default(); channels as usize],
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
    sample_rate: u32,
    time_base: TimeBase,
    pending: Option<PendingFrame>,
    // Yamaha carries state across packets; the other variants re-seed per
    // block.
    yamaha_state: Vec<yamaha::Channel>,
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
        // Re-seed per-channel Yamaha state. MS / IMA are memoryless per
        // block so no further action needed.
        for st in &mut self.yamaha_state {
            *st = yamaha::Channel::default();
        }
        Ok(())
    }
}
