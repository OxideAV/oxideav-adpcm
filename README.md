# oxideav-adpcm

Pure-Rust decoder family for the common **ADPCM** (Adaptive Differential Pulse
Code Modulation) audio formats found in WAV / AVI / QuickTime streams.

## Supported codec ids

| Codec id          | Variant                       | Origin                      |
|-------------------|-------------------------------|-----------------------------|
| `adpcm_ms`        | Microsoft ADPCM               | WAV tag `0x0002` / AVI      |
| `adpcm_ima_wav`   | IMA / DVI ADPCM — WAV variant | WAV tag `0x0011`            |
| `adpcm_ima_qt`    | IMA ADPCM — QuickTime variant | QuickTime / MOV (fourcc `ima4`) |
| `adpcm_yamaha`    | Yamaha ADPCM (Y8950/YM2608)   | WAV tag `0x0020` / `AICA`   |

G.722 (WAV tag `0x0028`) and G.726/G.723.1/G.729 live in their own crates and
are NOT re-implemented here.

## Status

Decoders for all four supported codec ids. Encoders for the two
block-oriented WAV variants:

| Codec id          | Decoder | Encoder |
|-------------------|---------|---------|
| `adpcm_ms`        | yes     | yes     |
| `adpcm_ima_wav`   | yes     | yes     |
| `adpcm_ima_qt`    | yes     | no      |
| `adpcm_yamaha`    | yes     | no      |

The encoders use the textbook decoder-loop search: for each input PCM
sample they evaluate all 16 candidate nibbles by simulating the
decoder's recurrence forward, then emit the nibble whose reconstructed
output minimises absolute error against the target. The algorithm is
derived from the decoder recurrence already in this crate — no
third-party encoder source was consulted. Round-trip RMS error for a
20 ms 440 Hz sine at 22.05 kHz is well under 1000 LSB for both
encoders.

Default block size is 256 bytes per channel for both encoders (matches
the default ffmpeg emits at 22050 Hz mono). Override via
`MsEncoder::set_block_size` / `ImaWavEncoder::set_block_size` before
the first `send_frame` call.

## Specs followed

Each variant was implemented from its **public normative spec**, not from any
implementation:

- **Microsoft ADPCM** — block header layout, `AdaptationTable`, `AdaptCoeff1`,
  `AdaptCoeff2`, and the `predictor + nibble*delta` update rule as documented
  on the [MultimediaWiki Microsoft ADPCM page](https://wiki.multimedia.cx/index.php/Microsoft_ADPCM)
  (transcribing Microsoft's publicly-documented WAVEFORMATEX tag `0x0002`).
- **IMA ADPCM** — the 89-entry step-size table and 16-entry index-adjust
  table from the original Interactive Multimedia Association "Recommended
  Practices for Digital Audio" (see
  [MultimediaWiki IMA ADPCM](https://wiki.multimedia.cx/index.php/IMA_ADPCM)
  for the spec transcription).
- **Microsoft IMA ADPCM (WAV)** — block header + per-channel interleave
  layout documented on
  [MultimediaWiki Microsoft IMA ADPCM](https://wiki.multimedia.cx/index.php/Microsoft_IMA_ADPCM).
- **Apple QuickTime IMA ADPCM** — 34-byte fixed block, big-endian preamble
  (9-bit predictor + 7-bit step index), block-level channel interleave, per
  [MultimediaWiki Apple QuickTime IMA ADPCM](https://wiki.multimedia.cx/index.php/Apple_QuickTime_IMA_ADPCM).
- **Yamaha ADPCM** — step-adaptation rate table and
  `X(n+1) = X(n) + sign(L4) * (L3 + L2/2 + L1/4 + 1/8) * Δn` update rule
  from Yamaha's public *Y8950 (MSX-AUDIO) Application Manual*, section I-4
  and Table I-2. See [Y8950 manual PDF](https://map.grauw.nl/resources/sound/yamaha_y8950.pdf).

The adaptation / step tables are normative constants (uncopyrightable facts);
the implementation was written from these spec descriptions without reading
any decoder source.

## License

MIT. See [LICENSE](LICENSE).
