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
| `adpcm_dialogic`  | OKI / Dialogic VOX ADPCM      | `.vox` (headerless; no WAV tag) |

G.722 (WAV tag `0x0028`) and G.726/G.723.1/G.729 live in their own crates and
are NOT re-implemented here.

## Status

Decoders **and** encoders for all five supported codec ids:

| Codec id          | Decoder | Encoder |
|-------------------|---------|---------|
| `adpcm_ms`        | yes     | yes     |
| `adpcm_ima_wav`   | yes     | yes     |
| `adpcm_ima_qt`    | yes     | yes     |
| `adpcm_yamaha`    | yes     | yes     |
| `adpcm_dialogic`  | yes     | yes     |

The block-oriented WAV encoders (MS, IMA-WAV, IMA-QT) use the textbook
decoder-loop search: for each input PCM sample they evaluate all 16
candidate nibbles by simulating the decoder's recurrence forward, then
emit the nibble whose reconstructed output minimises absolute error
against the target. The stream-oriented encoders (Yamaha, Dialogic VOX)
use closed-form quantisers derived directly from the spec's analysis
recurrence — `sign(dn) | mag(|dn|/Δn)` against the 7-threshold ladder
the manuals print. Both shapes are derived from the decoder recurrence
already in this crate — no third-party encoder source was consulted.
Round-trip RMS error for a 20 ms 440 Hz sine at 22.05 kHz stays below
1500 LSB across the block-oriented encoders, and under 3000 LSB for the
stream-oriented encoders (where step state has to converge from cold
start).

Default block size is 256 bytes per channel for the MS and IMA-WAV
encoders (matches the default ffmpeg emits at 22050 Hz mono); override
via `MsEncoder::set_block_size` / `ImaWavEncoder::set_block_size`
before the first `send_frame` call. The IMA-QT encoder uses the
spec-mandated 34-byte-per-channel block — there is no `set_block_size`
because the on-wire layout is fixed. To minimise the high-amplitude
leading-edge transient inherent to per-block re-seeding, the IMA-QT
encoder picks the initial step index from the mean |Δ| of the first
samples in each block (rather than always seeding at 0).

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
- **OKI / Dialogic VOX ADPCM** — 49-entry calculated step-size table
  (Table 2) and 8-entry magnitude-indexed step-pointer adjustment (the
  row-collapsed form of Table 1) from Dialogic Corporation's *Dialogic
  ADPCM Algorithm* application note, doc 00-1366-001 (1988); the same
  decoder/encoder pseudocode (§2–§3) is transcribed directly from the
  app note. Reconstructed predictor is 12-bit signed (`-2048..=2047`);
  the registry-resolved decoder shifts to a full-range i16 on output,
  while the raw 12-bit value remains available via `dialogic::Output::Native12`.
  Dialogic VOX is **headerless** — `.vox` files carry no sample rate;
  callers supply it out of band (commonly 6 kHz or 8 kHz for telephony).
  The MSM6258's LSB-first nibble order is reachable via the lower-level
  `dialogic::decode_packet(.., NibbleOrder::LoFirst, ..)` API.

The adaptation / step tables are normative constants (uncopyrightable facts);
the implementation was written from these spec descriptions without reading
any decoder source.

## License

MIT. See [LICENSE](LICENSE).
