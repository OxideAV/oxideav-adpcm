# oxideav-adpcm

Pure-Rust decoder + encoder family for the common **ADPCM** (Adaptive
Differential Pulse Code Modulation) audio formats found in WAV / AVI /
QuickTime / VOX / FM-synth streams.

## Supported codec ids

| Codec id          | Variant                       | Origin                      |
|-------------------|-------------------------------|-----------------------------|
| `adpcm_ms`        | Microsoft ADPCM               | WAV tag `0x0002` / AVI      |
| `adpcm_ima_wav`   | IMA / DVI ADPCM — WAV variant (4-bit **and** 3-bit) | WAV tag `0x0011` |
| `adpcm_ima_qt`    | IMA ADPCM — QuickTime variant | QuickTime / MOV (fourcc `ima4`) |
| `adpcm_yamaha`    | Yamaha ADPCM-B / DELTA-T (Y8950/YM2608-B/YMZ280B/AICA) | WAV tag `0x0020` |
| `adpcm_yamaha_a`  | Yamaha ADPCM-A (YM2608/YM2610 rhythm channels) | chip-internal; no WAV tag |
| `adpcm_dialogic`  | OKI / Dialogic VOX ADPCM      | `.vox` (headerless) **and** WAV tag `0x0010` (`WAVE_FORMAT_OKI_ADPCM`) |

G.722 (WAV tag `0x0028`) and G.726 / G.723.1 / G.729 live in their own
crates and are not re-implemented here.

## Status

**Decoders and encoders for all six codec ids.** Output is i16-LE PCM.

The block-oriented WAV encoders (MS, IMA-WAV, IMA-QT) use the
decoder-loop search — each input sample evaluates all candidate nibbles
by simulating the decoder forward and emits the one minimising absolute
error. The MS-ADPCM encoder additionally trial-encodes each block under
all seven spec predictor coefficient pairs and writes the
lowest-error index (a pure quality gain — the index travels in the block
header so the decode is unchanged). All three block encoders seed their
initial step state from the mean absolute first-difference of the first
16 samples to suppress the per-block leading-edge transient. The
stream-oriented encoders (Yamaha, Dialogic VOX) use closed-form
quantisers derived from each spec's analysis recurrence.

Default block size is 256 bytes per channel for the MS and IMA-WAV
encoders (override via `set_block_size`); IMA-QT uses the spec-mandated
34-byte-per-channel block (fixed layout, no override).

### Notable format details

- **Multi-block packets (`block_align` decode option)** — the
  block-oriented MS and IMA-WAV (4-bit and 3-bit) decoders split a packet
  that carries several concatenated blocks — a whole WAV `data` chunk, an
  AVI audio chunk, a large demuxer read buffer — into its constituent
  blocks, each re-seeding its predictor from its own header. The decoder
  learns the WAV `nBlockAlign` (bytes per block, all channels) from the
  `block_align` codec option; pass it through `CodecParameters::options`.
  Without the option a packet is taken as a single block (back-compatible
  with producers that already frame one block per packet). IMA-QT derives
  its own fixed 34-byte block and ignores the option.
- **MS-ADPCM custom predictor sets** — the decoder reads the
  `ADPCMWAVEFORMAT` trailer (`wSamplesPerBlock`, `wNumCoef`, variable
  `aCoeff[]`) from `CodecParameters::extradata`, so a block's
  `bPredictor` byte can index custom coefficient pairs beyond the seven
  mandatory presets. Block-level entry points: `ms::decode_block` and
  `ms::decode_block_with_coeffs`, with `ms::parse_extradata_coeffs` /
  `ms::STANDARD_COEFFS` exposed.
- **3-bit IMA / DVI ADPCM** — WAV tag `0x0011` defines both 4-bit (the
  default) and 3-bit code widths. The 3-bit mode shares the block header
  and 89-entry step table but uses a 1-sign + 2-magnitude code, its own
  8-entry index-adjust table, and a 12-byte-group channel interleave.
  Reachable via `ima_wav::decode_block_3bit` /
  `encoder::ima_encode_block_3bit`, `set_bits_per_sample(3)`, or the
  `bits_per_sample` codec option.
- **Yamaha ADPCM-B chip selection** — the `yamaha::Chip` selector on
  `yamaha::Channel` picks the exact quantization-width-change-rate
  constants: `Chip::Aica` (default, the WAV-tag-`0x0020` convention,
  also Y8950 / YMZ280B) vs `Chip::Opna` (YM2608 OPNA Table 5-1). The two
  tables live in `tables::YAMAHA_INDEX_SCALE` /
  `tables::YAMAHA_INDEX_SCALE_OPNA`.

### Typed variant accessor

`oxideav_adpcm::Variant` is the dispatch enum re-exported at the crate
root, with a const inspection surface for container layers:

```rust
use oxideav_adpcm::{Shape, Variant};

assert_eq!(Variant::Ms.wave_format_tag(),  Some(0x0002));
assert_eq!(Variant::ImaQt.fourcc(),        Some(*b"ima4"));
assert_eq!(Variant::Ms.shape(),            Shape::BlockOriented);
assert_eq!(Variant::Yamaha.shape(),        Shape::StreamOriented);
assert_eq!(Variant::Ms.max_channels(),     Some(2));

// Block framing helpers (None for stream-oriented variants):
assert_eq!(Variant::Ms.header_bytes(2),            Some(14));
assert_eq!(Variant::Ms.samples_per_block(1, 256),  Some(500));
assert_eq!(Variant::Ms.block_size_bytes(1, 500),   Some(256)); // inverse
```

`Variant::all()` iterates every variant; `from_codec_id` / `codec_id`
round-trip a codec id; `Shape` (block- vs stream-oriented) is also
re-exported. Lib-side tests pin these accessors against what
`register_codecs` and the per-block decoders actually do, so a new
variant must update both surfaces in lockstep.

## Robustness

`tests/decoder_fuzz.rs` and `tests/encoder_fuzz.rs` enumerate
structured-malformation coverage across all six variants: out-of-spec
predictor / step-index bytes, truncated-block prefixes, and
pseudo-random byte streams through both the block-level and
`Decoder` / `Encoder` trait paths — every input returns `Ok` or `Err`,
never panics or overflows in a debug build. The MS decode/encode
recurrences run in i64 with saturating multiplication + final clamp, and
the Yamaha ADPCM-A path clamps `step_index` / `acc` to spec range on
entry, so adversarial state emits bounded samples instead of panicking.

`tests/wav_decode.rs` additionally runs each WAV-tagged variant (MS,
IMA-WAV, Yamaha) and the QuickTime `ima4` variant end-to-end against an
opaque validator: a sine fixture is encoded by the validator, decoded by
our decoder, and cross-correlated (> 0.98) against the validator's own
PCM dump. The `ima4` path has no WAV tag, so its fixture is a CAF
container and the harness pulls the raw 34-byte `ima4` blocks straight
out of the CAF `data` chunk before feeding the decoder. Fixtures are
generated on demand and skipped when the validator binary is absent.

A coverage-guided [`cargo-fuzz`](https://rust-fuzz.github.io/book/cargo-fuzz.html)
harness under `fuzz/` exposes per-variant decode and encode targets:

    cd crates/oxideav-adpcm/fuzz
    cargo +nightly fuzz run decode_packet_ms

## Benchmarks

A Criterion harness at `benches/decode.rs` covers the per-block /
per-packet decode hot path across all six variants (11 scenarios). All
inputs are synthesised in-bench from a deterministic seed — block
variants build a valid buffer via the public encoder so the timed loop
measures only the decoder. No fixtures are read.

    cargo bench -p oxideav-adpcm --bench decode

## Specs followed

Each variant was implemented from its **public normative spec**, not
from any implementation. The adaptation / step tables are normative
constants (uncopyrightable facts).

- **Microsoft ADPCM** — block header, `AdaptationTable`, `AdaptCoeff1/2`,
  and the `predictor + nibble*delta` update rule per the publicly
  documented WAVEFORMATEX tag `0x0002`. The `ADPCMWAVEFORMAT` trailer
  layout is transcribed from the archived WAVE-format-type enumeration
  staged at `docs/audio/adpcm/sdl_sound-wave-types.html`.
- **IMA ADPCM** — the 89-entry step-size and 16-entry index-adjust
  tables from the Interactive Multimedia Association "Recommended
  Practices for Digital Audio".
- **3-bit IMA / DVI ADPCM** — the *DVI ADPCM Wave Type* specification
  (Intel, 1992) preserved at `docs/audio/adpcm/sdl_sound-wave-types.html`.
- **Apple QuickTime IMA ADPCM** — 34-byte fixed block, big-endian 9-bit
  predictor + 7-bit step-index preamble, block-level channel interleave.
- **Yamaha ADPCM-B / DELTA-T** — step-adaptation rate table and the
  `X(n+1) = X(n) + sign(L4)·(L3 + L2/2 + L1/4 + 1/8)·Δn` update rule from
  Yamaha's public *Y8950 (MSX-AUDIO) Application Manual*, §I-4 / Table I-2.
- **Yamaha ADPCM-A** — the YM2608 / YM2610 rhythm channel codec (4-bit
  1-sign + 3-magnitude, 12-bit signed acc, 49-entry step table)
  transcribed from `docs/audio/adpcm/yamaha/yamaha-adpcm.md` §3
  (independent hardware-RE consensus verified against real silicon).
  Single channel per stream by chip design; 12→16-bit narrowing handled
  internally.
- **OKI / Dialogic VOX ADPCM** — 49-entry step table and 8-entry
  step-pointer adjustment from Dialogic Corporation's *Dialogic ADPCM
  Algorithm* application note (doc 00-1366-001, 1988). Headerless `.vox`
  (caller supplies sample rate) plus the `WAVE_FORMAT_OKI_ADPCM`
  (`0x0010`) WAV framing, which decodes byte-identically. The MSM6258's
  LSB-first nibble order is reachable via
  `dialogic::decode_packet(.., NibbleOrder::LoFirst, ..)`; the raw 12-bit
  value is available via `dialogic::Output::Native12`. The app note's
  §5 stream-reset sequence — 24 bytes / 48 samples of alternating ±zero
  codes that walk the step pointer to its floor without introducing a DC
  offset — is produced by `dialogic::reset_preamble`. The 3-bit OKI mode
  is not implemented (the app note specifies only the 4-bit algorithm).

## License

MIT. See [LICENSE](LICENSE).
