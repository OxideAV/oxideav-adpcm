# oxideav-adpcm

Pure-Rust decoder family for the common **ADPCM** (Adaptive Differential Pulse
Code Modulation) audio formats found in WAV / AVI / QuickTime streams.

## Supported codec ids

| Codec id          | Variant                       | Origin                      |
|-------------------|-------------------------------|-----------------------------|
| `adpcm_ms`        | Microsoft ADPCM               | WAV tag `0x0002` / AVI      |
| `adpcm_ima_wav`   | IMA / DVI ADPCM — WAV variant | WAV tag `0x0011`            |
| `adpcm_ima_qt`    | IMA ADPCM — QuickTime variant | QuickTime / MOV (fourcc `ima4`) |
| `adpcm_yamaha`    | Yamaha ADPCM-B / DELTA-T (Y8950/YM2608-B/YMZ280B/AICA) | WAV tag `0x0020` |
| `adpcm_yamaha_a`  | Yamaha ADPCM-A (YM2608/YM2610 rhythm channels) | chip-internal; no WAV tag |
| `adpcm_dialogic`  | OKI / Dialogic VOX ADPCM      | `.vox` (headerless; no WAV tag) |

G.722 (WAV tag `0x0028`) and G.726/G.723.1/G.729 live in their own crates and
are NOT re-implemented here.

## Status

Decoders **and** encoders for all six supported codec ids:

| Codec id          | Decoder | Encoder |
|-------------------|---------|---------|
| `adpcm_ms`        | yes     | yes     |
| `adpcm_ima_wav`   | yes     | yes     |
| `adpcm_ima_qt`    | yes     | yes     |
| `adpcm_yamaha`    | yes     | yes     |
| `adpcm_yamaha_a`  | yes     | yes     |
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

## Robustness

`tests/decoder_fuzz.rs` enumerates structured-malformation coverage
across all five variants: every out-of-spec predictor / step-index
byte is rejected with `Err` (no panic); every prefix of a well-formed
block is fed to the decoder to assert clean rejection of truncated
inputs; a deterministic in-test LCG drives a few thousand
pseudo-random bytes through each variant's `decode_packet`; and the
trait-level `Decoder::send_packet` / `receive_frame` path is exercised
on every variant with random packets. Property-style assertions also
pin the spec-derived emitted-sample-count formulas (MS:
`2 + body_bytes·2`; IMA-WAV: `1 + groups·8`; IMA-QT: `64·channels`;
Yamaha / Dialogic: `2·packet_bytes`).

The fuzz layer surfaced (and fixed) an integer-overflow path in
`ms::decode_nibble`: an adversarial block header whose initial `delta`
field was a large signed-i16 value could overflow the
`MS_ADAPTATION[i] * delta` i32 multiplication after a handful of
iterations. The recurrence now runs in i64 with saturating
multiplication and a final clamp back to i32 — spec-compliant inputs
are bit-identical (the existing ffmpeg-oracle round-trip tests still
pass) and hostile ones emit bounded samples instead of panicking.

## Benchmarks

A Criterion harness lives at `benches/decode.rs` covering the
per-block / per-packet decode hot path across all six variants — 11
scenarios in total, spanning MS / IMA-WAV mono+stereo at 256-byte and
512-byte block sizes, the fixed 34-byte IMA-QT block mono+stereo, the
Yamaha ADPCM-B mono+stereo streaming path, the Yamaha ADPCM-A mono
12→16-bit widening path, and both Dialogic VOX nibble orders
(HiFirst/Wide16 + LoFirst/Native12). All inputs are synthesised
in-bench from a deterministic xorshift32 seed: block-oriented variants
build a valid encoded buffer via the crate's public encoder at setup
time so the timed loop measures only the decoder, while
stream-oriented variants feed the byte stream straight into
`decode_packet`. No `docs/` fixtures or external files are read. Run
with:

    cargo bench -p oxideav-adpcm --bench decode

The harness is intended as a stable A/B baseline for future
optimisation rounds (block-aligned SIMD, per-sample LUT, no-bounds-
check inner loops, predictor-fold rewrites) — the numbers themselves
aren't pinned to any specific microarchitecture, only their relative
ratios across crate versions.

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
- **Yamaha ADPCM-B / DELTA-T** (`adpcm_yamaha`) — step-adaptation rate
  table and `X(n+1) = X(n) + sign(L4) * (L3 + L2/2 + L1/4 + 1/8) * Δn`
  update rule from Yamaha's public *Y8950 (MSX-AUDIO) Application
  Manual*, section I-4 and Table I-2. See
  [Y8950 manual PDF](https://map.grauw.nl/resources/sound/yamaha_y8950.pdf).
- **Yamaha ADPCM-A** (`adpcm_yamaha_a`) — the YM2608 rhythm-ROM /
  YM2610 ADPCM-A channel codec. 4-bit nibble (1 sign + 3 magnitude),
  12-bit signed reconstructed acc clamped to `-2048..=2047`, 49-entry
  step-size table (`16 .. 1552`, identical geometry to OKI Table 2) and
  16-entry step-pointer adjustment `{-1,-1,-1,-1, 2, 5, 7, 9, ...}`.
  Tables transcribed from `docs/audio/adpcm/yamaha/yamaha-adpcm.md` §3
  (independent-RE consensus of the NeoGeo Development Wiki and the
  MAME/ymfm hardware-RE effort, verified against real YM2608/YM2610
  silicon — NOT from any general-purpose multimedia decoder). Single
  channel per stream by chip design; the registry decoder + encoder
  reject stereo with `Error::Unsupported`. 12→16-bit narrowing is
  handled internally so consumers always see i16-LE PCM on output.
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
