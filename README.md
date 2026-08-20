# oxideav-adpcm

[![CI](https://github.com/OxideAV/oxideav-adpcm/actions/workflows/ci.yml/badge.svg)](https://github.com/OxideAV/oxideav-adpcm/actions/workflows/ci.yml) [![crates.io](https://img.shields.io/crates/v/oxideav-adpcm.svg)](https://crates.io/crates/oxideav-adpcm) [![docs.rs](https://docs.rs/oxideav-adpcm/badge.svg)](https://docs.rs/oxideav-adpcm) [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Pure-Rust decoder + encoder family for the common **ADPCM** (Adaptive
Differential Pulse Code Modulation) audio formats found in WAV / AVI /
QuickTime / VOX / FM-synth streams.

## Supported codec ids

| Codec id          | Variant                       | Origin                      |
|-------------------|-------------------------------|-----------------------------|
| `adpcm_ms`        | Microsoft ADPCM               | WAV tag `0x0002` / AVI      |
| `adpcm_ima_wav`   | IMA / DVI ADPCM — WAV variant (4-bit **and** 3-bit) | WAV tag `0x0011` |
| `adpcm_ima_qt`    | IMA ADPCM — QuickTime variant (mono…7.1, block-interleaved) | QuickTime / MOV (fourcc `ima4`) |
| `adpcm_yamaha`    | Yamaha ADPCM-B / DELTA-T (Y8950/YM2608-B/YMZ280B/AICA) | WAV tag `0x0020` |
| `adpcm_yamaha_a`  | Yamaha ADPCM-A (YM2608/YM2610 rhythm channels) | chip-internal; no WAV tag |
| `adpcm_dialogic`  | OKI / Dialogic VOX ADPCM      | `.vox` (headerless) **and** WAV tags `0x0010` (`WAVE_FORMAT_OKI_ADPCM`) / `0x0203` + `0x0017` (the catalogue's and RFC 2361 §A.16's `WAVE_FORMAT_DIALOGIC_OKI_ADPCM` assignments) |
| `adpcm_g726`      | ITU-T G.726 narrowband ADPCM (40/32/24/16 kbit/s) | telephony / RTP; WAV tags `0x0040` (`WAVE_FORMAT_G721_ADPCM`, 4-bit) / `0x0014` (`WAVE_FORMAT_G723_ADPCM`, the older 3-/5-bit G.723 rates) — the Antex sub-block WAV layout — plus the raw bit-continuous WAV tags `0x0045` (the tag common tools write; black-box established) / `0x0064` (`WAVE_FORMAT_G726_ADPCM`, RFC 2361 §A.54) |

G.722 (WAV tag `0x0028`) and G.723.1 / G.729 live in their own crates
and are not re-implemented here.

## Status

**Decoders and encoders for all seven codec ids.** Output is i16-LE PCM.

The block-oriented WAV encoders (MS, IMA-WAV, IMA-QT) use the
decoder-loop search — each input sample evaluates all candidate nibbles
by simulating the decoder forward and emits the one minimising absolute
error. The two IMA encoders alternatively speak the **published
reference compressor** via the `quantizer` codec option (see below).
The MS-ADPCM encoder additionally trial-encodes each block under
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

- **End-to-end WAV tag claiming** — every WAV-carried variant claims
  its `wFormatTag`s in `register_codecs`, so a demuxer that resolves
  tags through `oxideav_core::CodecResolver` reaches these decoders
  directly; `WAVE_FORMAT_EXTENSIBLE` streams need nothing extra here,
  because a `DEFINE_WAVEFORMATEX_GUID`-template SubFormat is documented
  as equivalent to its embedded legacy tag (the demuxer folds it back
  to the same 16-bit claim — the core `CodecTag` has no GUID form by
  design). The factories reconstruct everything else from what a
  demuxer can actually pass in `CodecParameters`: the `wSamplesPerBlock`
  word of the documented `ADPCMWAVEFORMAT` / `DVIADPCMWAVEFORMAT`
  trailers (via `extradata`) re-derives `nBlockAlign` for multi-block
  packet splitting, and the on-wire tag (via `CodecParameters::tag`)
  picks the G.726 framing default — Antex sub-block layout for
  `0x0040` / `0x0014`, raw bit-continuous stream for `0x0045` /
  `0x0064` (an explicit `framing` option always wins). The encoders
  advertise the same fields outward: `output_params` carries the
  canonical wire tag (caller-supplied alias tags round-trip untouched)
  and the variant's `fmt ` extension as `extradata`, so a WAV muxer
  needs no codec-specific knowledge. `tests/wav_tag_e2e.rs` pins the
  whole chain on real validator-generated WAVs (resolve → build →
  multi-block decode → cross-correlate), including the EXTENSIBLE fold
  and both raw G.726 tags; `tests/encoder_container_fields.rs` closes
  the loop by decoding a concatenated stream through a decoder built
  straight from `output_params`.
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
- **IMA-QT multichannel block interleave** — the QuickTime `ima4` layout
  is one independent 34-byte block per channel, round-robin, each with its
  own preamble and predictor/step state. The decoder and encoder accept
  1..=8 channels (mono / stereo / 4.0 / 5.1 / 7.1) — the layout has no
  intrinsic channel ceiling, so the extra channels are simply more
  per-channel blocks in the packet, with no new framing. `Variant::ImaQt`
  now reports `max_channels() == Some(8)` (was 2);
  `ima_qt::QT_MAX_CHANNELS` is exposed as the cap.
- **IMA-WAV 4-bit multichannel encode** — the 4-bit IMA-WAV decoder,
  `ima_encode_block` and the encoder factory all support 1..=8 channels
  (the body interleaves channels in 4-byte groups). The frame-based
  encoder now sizes its default block per channel count so the
  4-byte-group framing always holds — the previous fixed 256-byte default
  errored at `flush` for layouts where `256 - 4·channels` wasn't a
  multiple of `4·channels` (e.g. 5.1). Mono/stereo defaults are unchanged
  (still 256 bytes). Multichannel (4.0 / 5.1) encode→decode round-trips
  are pinned with per-lane RMS bounds plus a direct block-API lane
  assignment check.
- **MS-ADPCM custom predictor sets** — the decoder reads the
  `ADPCMWAVEFORMAT` trailer (`wSamplesPerBlock`, `wNumCoef`, variable
  `aCoeff[]`) from `CodecParameters::extradata`, so a block's
  `bPredictor` byte can index custom coefficient pairs beyond the seven
  mandatory presets. Block-level entry points: `ms::decode_block` and
  `ms::decode_block_with_coeffs`, with `ms::parse_extradata_coeffs` /
  `ms::STANDARD_COEFFS` exposed.
- **WAVEFORMATEX trailer builders** — the inverse serialisation path for
  WAV muxers. `ms::build_extradata(samples_per_block, coeffs)` produces
  the MS `ADPCMWAVEFORMAT` body (the inverse of `parse_extradata_coeffs`),
  and `Variant::build_wave_format_extra(channels, block_align)` is the
  per-variant convenience: it derives `wSamplesPerBlock` from the block
  geometry and emits the codec-specific `fmt `-chunk extension (the full
  MS trailer for `adpcm_ms`, just `wSamplesPerBlock` for `adpcm_ima_wav`),
  `None` for the FourCC-routed IMA-QT and the headerless stream variants.
  Both exclude the leading `cbSize` word (the crate's `extradata`
  convention — the muxer prepends `cbSize = len`), and the MS output
  round-trips straight back through `parse_extradata_coeffs`.
- **IMA reference ladder compressor (`quantizer` option)** — the IMA
  "Recommended Practices" Rev 3.00 publishes its reference compression
  procedure in full (Appendix D §6.1, with worked examples; the DVI
  Wave Type specification lists the matching 4-bit and 3-bit encode
  procedures), and the IMA-WAV / IMA-QT encoders now implement it as an
  alternative to the default search: `quantizer=reference` on the
  registry path, `ima_wav::ima_quantize_nibble` / `ima_quantize_code3`
  per-sample and `encoder::ima_encode_block_reference` /
  `ima_encode_block_3bit_reference` / `ima_qt_encode_block_reference`
  at block level (per-channel `ima_wav::ImaCodecState` carried by the
  caller). The ladder quantizes by successive threshold subtraction and
  advances state through the published expansion, so encode is the
  bit-exact inverse of decode by construction; the step index is
  cleared once before the first block and **carried across block
  boundaries** (each header records the previous block's end index),
  and there is no heuristic seeding — the byte stream is fully
  determined by the input, i.e. interchange-exact with any other
  conforming reference compressor. `tests/ima_reference.rs` pins the
  implementation against independent re-transcriptions of both
  listings (worked examples, dense state grids, an exhaustive
  index × code sweep, a 200k-sample lockstep walk, and byte-exact
  block/stream assembly for all three shapes); the opaque-validator
  harness proves an independent decoder reconstructs reference-mode
  streams, cross-block index carry included. The search stays the
  default because it is never worse and usually better on error; the
  reference mode is for interchange determinism and conformance work.
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
  `tables::YAMAHA_INDEX_SCALE_OPNA`. The registry decoder **and** encoder
  honour a `chip` codec option (`"aica"` default / `"opna"`) passed
  through `CodecParameters::options`, so a YM2608/OPNA stream resolves to
  the correct step constants without dropping to the block-level API; the
  encoder seeds its analysis state with the same chip so its bytes decode
  bit-exactly under the matching option. A `chip` option on any other
  variant is rejected.
- **OKI / Dialogic nibble order** — the registry decoder and encoder
  honour a `nibble_order` codec option (`"hi"` default — Dialogic VOX /
  MSM6295, high nibble = first sample — vs `"lo"` — MSM6258, low nibble =
  first sample) passed through `CodecParameters::options`. The arithmetic
  is identical between the two chips; only the in-byte unpack order
  differs, so an MSM6258 stream is now reachable through the registry
  rather than only the explicit `dialogic::decode_packet(.., LoFirst, ..)`
  entry point. A `nibble_order` option on any other variant is rejected.
- **OKI / Dialogic stereo encode** — VOX is mono in practice, but its
  nibble-interleave layout (nibble 0 → ch 0, nibble 1 → ch 1, …)
  generalises to stereo, and the decoder already accepted 1..=2 channels.
  The registry encoder now matches: it accepts 1..=2 channels, and
  `dialogic::encode_packet_multi` / `encode_packet_multi_wide16` are the
  exact per-channel inverses of `decode_packet`. Mono output is
  byte-identical to the single-channel `dialogic::encode_packet`; a
  registry-path stereo encode→decode round-trip is pinned with per-lane
  RMS bounds under both nibble orders. Three or more channels are
  rejected on both the encode and decode paths.

- **ITU-T G.726 (Rec. G.726, 12/1990)** — bit-exact §4.2 state machine
  shared by encode and decode, all four rates (5/4/3/2 bits per sample =
  40/32/24/16 kbit/s) selected via the `bits_per_sample` codec option
  (default 4). The stream is headerless and **bit-continuous**: codec
  state *and* partial code words carry across packet boundaries (3- and
  5-bit codes straddle bytes), so the decoder holds a bit-level
  unpacker and the encoder a bit-level packer whose sub-byte residue
  crosses `send_frame` calls (`flush` emits the zero-padded tail). The
  G.726-specific `bit_order` option picks the in-byte packing: `msb`
  (default — the network/RTP convention and the WAV `0x0045` framing
  produced by common tools) or `lsb`. Mono only. Two PCM interfaces:
  by default the registry path speaks 16-bit linear mapped onto the
  spec's 14-bit uniform words (`>> 2` in, clamp + `<< 2` out;
  standalone G.711 streams remain the `oxideav-g711` codec tags'
  domain), and the G.726-specific `law` option (`linear` default /
  `alaw` / `ulaw`) switches both factories onto the Recommendation's
  log-PCM interface with the law words carried as 16-bit linear frames
  on the law lattice. The direct API exposes that interface fully —
  §4.2.1 EXPAND on the encoder side and the full §4.2.8 output chain
  (COMPRESS → re-EXPAND → re-quantization → SYNC synchronous coding
  adjustment, Tables 15-20) on the decoder side
  (`State::encode_law` / `State::decode_law`, `Law::{ALaw, ULaw}`,
  `expand` / `compress`). **Proven bit-exact against the official ITU
  Appendix II conformance test sequences** — every reset and homing
  vector, both directions, all four rates, both laws and both
  cross-law decode legs reproduce byte-for-byte (see
  `tests/g726_conformance.rs`). Direct API under `g726::`
  (`State`, `Rate`, `Law`, `BitOrder`, `BitPacker`/`BitUnpacker`,
  `encode_packet`/`decode_packet`, `pack_codes`/`unpack_codes`).
  The registry also answers to the `0x0014` (`WAVE_FORMAT_G723_ADPCM`)
  tag — the older CCITT G.723 ADPCM (3-bit / 24 kbit/s and 5-bit /
  40 kbit/s) that the 1990 Recommendation consolidates alongside G.721 —
  with the rate taken from `wBitsPerSample`. Mid-stream **rate
  switching with state carriage** (`State::set_rate` — the Appendix I.1
  DCME property; only the quantizer tables are rate-scoped) is proven
  **bit-exact against the staged VBR demo reference**: the linear leg
  (`voicevbr.lrf`) reproduces byte-for-byte over all 52 736 samples —
  3 295 scheduled switches through the `16-24-32-40-32-24` kbit/s
  cycle at the demo's true 16-sample frame period (the staged note's
  256-sample inference is corrected and pinned), A-law compand front
  end and §4.2.8 output chain included — plus SYNC tandem transparency
  held across switches.
- **G.723/G.721-in-WAV sub-block framing (`framing=wav`)** — the WAV
  carriage of the same codes (tags `0x0014` / `0x0040`) groups them
  into whole-byte 8-sample-per-channel **sub-blocks** instead of the
  raw bit-continuous telephony stream. The intra-sub-block **bit-cell
  grid** is implemented from the staged reconstruction
  (`docs/audio/adpcm/g72x-wav/` — codes MSB-first into a big-endian
  bitstream, time-major channel-minor stereo interleave, anchored by
  the archived catalogue's surviving stereo-3-bit "Byte 3" row) and
  pinned byte-exactly against all four staged packing vectors.
  `g726::wav_subblock_bytes` / `wav_block_align` compute the geometry
  (`bits·channels`-byte sub-blocks, 16 to a block plus `nAuxBlockSize`,
  reproducing the catalogue's tabulated 48 / 96 / 80 / 160 rows);
  `wav_pack_codes` / `wav_unpack_codes` implement the grid;
  `wav_strip_aux` removes per-block auxiliary prefixes;
  `wav_decode_packet` / `wav_encode_packet` run per-channel §4.2 codec
  states (the container defines a stereo interleave, so 1..=2 channels
  — unlike the mono-only raw stream); `wav_format_extra` /
  `wav_parse_format_extra` round-trip the one-field `fmt ` extension
  (`nAuxBlockSize`, `cbSize = 2`), which
  `Variant::G726.build_wave_format_extra` serialises for the six
  aux-free documented geometries. On the registry path the `framing`
  option (`raw` default / `wav`) selects the layout for **both**
  factories; `aux_block_size` (or the extension via
  `CodecParameters::extradata`; the option wins) locates the per-block
  prefix, and block position, sub-byte bits and a lane-alignment code
  carry all persist across packets, so a demuxer may split blocks
  anywhere. Only the documented 3-/4-/5-bit rates are carried
  (2-bit / 16 kbit/s has no WAV tag) and the grid fixes MSB-first
  packing (`bit_order=lsb` is rejected). The encoder emits aux-free
  whole sub-blocks, padding the final partial one with silence at
  `flush`.

### Typed variant accessor

`oxideav_adpcm::Variant` is the dispatch enum re-exported at the crate
root, with a const inspection surface for container layers:

```rust
use oxideav_adpcm::{Shape, Variant};

assert_eq!(Variant::Ms.wave_format_tag(),  Some(0x0002));
assert_eq!(Variant::ImaQt.fourcc(),        Some(*b"ima4"));
// Reverse routing for container demuxers (the inverse of the two above):
assert_eq!(Variant::from_wave_format_tag(0x0011), Some(Variant::ImaWav));
assert_eq!(Variant::from_fourcc(*b"ima4"),        Some(Variant::ImaQt));
assert_eq!(Variant::from_wave_format_tag(0x0001), None); // PCM — not ours
assert_eq!(Variant::Ms.shape(),            Shape::BlockOriented);
assert_eq!(Variant::Yamaha.shape(),        Shape::StreamOriented);
assert_eq!(Variant::Ms.max_channels(),     Some(2));

// Block framing helpers (None for stream-oriented variants):
assert_eq!(Variant::Ms.header_bytes(2),            Some(14));
assert_eq!(Variant::Ms.samples_per_block(1, 256),  Some(500));
assert_eq!(Variant::Ms.block_size_bytes(1, 500),   Some(256)); // inverse
```

`Variant::all()` iterates every variant; `from_codec_id` / `codec_id`
round-trip a codec id; `from_wave_format_tag` / `from_fourcc` invert
`wave_format_tag` / `fourcc` so a WAV / AVI / QuickTime demuxer that has
parsed a `wFormatTag` or sample-entry FourCC can map it straight to a
typed `Variant` without round-tripping through a codec-id string (tags
owned by other families — PCM, G.722 — and the two tagless variants
resolve to `None`); `Shape` (block- vs stream-oriented) is also
re-exported. `Variant::wave_format_tags()` returns *every* tag a variant
answers to (canonical first, then the documented aliases) and backs both
`wave_format_tag()` and `from_wave_format_tag()`: `Variant::Dialogic`
answers to `0x0010` (`WAVE_FORMAT_OKI_ADPCM`) plus both documented
`WAVE_FORMAT_DIALOGIC_OKI_ADPCM` assignments — `0x0203` (the archived
catalogue) and `0x0017` (RFC 2361 §A.16) — all the same 4-bit OKI VOX
body; and `Variant::G726` answers to `0x0040` (`WAVE_FORMAT_G721_ADPCM`,
the 4-bit 32 kbit/s rate) and `0x0014` (`WAVE_FORMAT_G723_ADPCM`, the
older CCITT G.723 ADPCM at 3-bit / 24 kbit/s and 5-bit / 40 kbit/s — the
1990 Recommendation consolidates both G.721 and G.723, so the tag routes
the demuxer to this decoder at the rate `wBitsPerSample` selects; both
carry the Antex sub-block WAV layout and default the decoder to
`framing=wav`), plus the two raw bit-continuous G.726-in-WAV tags
`0x0045` (the tag common tools write — established black-box against
the opaque validator and pinned in `tests/wav_tag_e2e.rs`) and `0x0064`
(`WAVE_FORMAT_G726_ADPCM`, RFC 2361 §A.54; the validator decodes it
byte-identically to `0x0045`). Each
alias is registered on the codec so `from_wave_format_tag` and the
container registry stay in lockstep. Lib-side tests pin these accessors against what
`register_codecs` and the per-block decoders actually do, so a new
variant must update both surfaces in lockstep.

## Robustness

`tests/decoder_fuzz.rs` and `tests/encoder_fuzz.rs` enumerate
structured-malformation coverage across all seven variants: out-of-spec
predictor / step-index bytes, truncated-block prefixes, and
pseudo-random byte streams through both the block-level and
`Decoder` / `Encoder` trait paths — every input returns `Ok` or `Err`,
never panics or overflows in a debug build. The IMA reference-quantizer
legs add hostile carried-state seeds (arbitrary predictor, far
out-of-range step index — clamped on entry) across 1..=8 channels with
every output re-parsed by the matching block decoder, plus
random-frame-chop runs through the `quantizer=reference` registry
encoders pinned byte-identical to the one-shot stream; the
`encode_packet_ima_wav` / `encode_packet_ima_qt` coverage-guided
targets carry matching search + reference legs (4-bit and 3-bit) and
ran bounded sessions (~0.3M / ~9M execs) with zero findings. The MS decode/encode
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

`tests/g726_wav_framing.rs` pins the G.723/G.721-in-WAV container
layer: all four staged bit-cell packing vectors byte-for-byte, the
surviving catalogue bit row re-derived independently of the packer,
stereo lane independence, cross-packet state carriage, and the full
registry matrix (byte-split invariance against the direct API,
aux stripping at hostile split points, option validation, per-lane
reset). `tests/g726_vbr.rs` pins mid-stream rate switching properties
(lockstep SNR floors, SYNC tandem transparency, state carriage being
load-bearing) under the staged demo schedule shape **and** under a
per-sample pseudo-random rate walk — Appendix I.1's strongest form,
switching at arbitrary non-block-aligned positions with tandem
transparency held on both law interfaces;
`tests/g726_vbr_conformance.rs` is the black-box gate on top: the
staged VBR demo reference's linear leg reproduced bit-exactly
(52 736/52 736 words, 3 295 mid-stream rate switches at the
empirically established 16-sample frame period), with negative pins
proving the reference itself carries state across switches and that
the staged note's 256-sample period inference does not reproduce.
The vectors stay in the docs staging area — the test reads them via
the `OXIDEAV_G726_VBR_DIR` environment variable and skips cleanly
when unset. The two log-PCM legs (`voicevbr.arf`/`.urf`) are not
reproducible from the staged `voice.src` under any byte-level input
model and are left unpinned as a documented docs gap (the codec's law
interfaces are already bit-exact per Appendix II).
The structured-malformation suites add
`framing=wav` legs — arbitrary bytes and random packet/frame chops
(mid-prefix, mid-code, mid-frame) match whole-buffer references
exactly and never panic — and the `decode_packet_g726` /
`encode_packet_g726` fuzz targets carry matching sub-block legs.

`tests/g726_validate.rs` runs the G.726 conformance pair: the decode
direction feeds the validator's own G.726-in-WAV output (tag `0x0045`,
all four rates) to our decoder and cross-correlates > 0.97 against the
validator's PCM; the encode direction wraps our bytes in a WAV whose
`fmt ` geometry mirrors the validator's (including `nBlockAlign` = 3 / 5
at the odd code widths so readers that packetize on block boundaries
never split a code) and requires the validator's decode to correlate
> 0.97 with the input. `tests/g726_registry.rs` covers the registry
path: per-rate round trips under both bit orders, packetization
invariance, option validation, tag routing and reset semantics.

`tests/g726_conformance.rs` is the strongest gate in the crate: the
official ITU-T G.726 Appendix II digital test sequences (staged under
`tests/fixtures/g726/`, 16-bit LE word form) run byte-exactly through
both directions — 16 reset + 16 homing encoder legs (normal + overload
inputs, A-law + µ-law), 32 reset + 32 homing decoder legs including the
cross-law `fx`/`fc` paths, and 16 full-codeword decoder sweeps. The
homing legs first drive the codec to the homed state with the Appendix
II initialization sequences (`pcm_init.*` through the encoder,
`i_ini_<rate>.*` through the decoder; the binary files carry an 88-word
ASCII annotation trailer after the 3496-word payload, which the harness
pins and strips). One shipped vector (`hn16fc.o`) was generated from
the reset state rather than the homed state; the test reproduces it as
shipped and documents the quirk. A final rig re-encodes the verified
decoder outputs through two further synchronous stages and requires
PCM-identical results — the §4.2.8 SYNC tandem-transparency guarantee.

`tests/encode_validate.rs` runs the *opposite* direction — it proves our
**encoder** emits spec-conformant bytes, not merely bytes our own decoder
accepts. A PCM sine is encoded by our block encoder, wrapped in a
container the harness assembles itself (a RIFF/WAVE `fmt `+`data` for MS
and IMA-WAV — including the MS `wSamplesPerBlock`/`wNumCoef`/`aCoeff[]`
trailer — and a minimal CAF `desc`+`data` for the WAV-tag-less QuickTime
`ima4`), then handed to the opaque validator to decode back to PCM and
cross-correlated (> 0.97) against the original input, per channel. Nine
cases cover MS, IMA-WAV and IMA-QT in mono and stereo (so the stereo
block-interleave wire layout is validated in both encode and decode
directions), plus three broadband cases — a four-partial signal that
forces the MS per-block coefficient search and IMA step adaptation to
track a moving spectrum — where MS / IMA-WAV use a 256-byte block so the
encoder must emit a non-default `wSamplesPerBlock` for the validator to
frame the stream. Skipped when the validator binary is absent.

A coverage-guided [`cargo-fuzz`](https://rust-fuzz.github.io/book/cargo-fuzz.html)
harness under `fuzz/` exposes per-variant decode and encode targets:

    cd crates/oxideav-adpcm/fuzz
    cargo +nightly fuzz run decode_packet_ms

## Benchmarks

A Criterion harness at `benches/decode.rs` covers the per-block /
per-packet decode hot path across all seven variants (18 scenarios,
including the Dialogic stereo nibble-interleave path, the four
G.726 rates — ~171 µs per decoded second at 8 kHz on the reference
machine, so the per-sample state machine, not the code width,
dominates — and the G.726 WAV-framing stereo path, whose two lanes
cost ~1.91× the mono raw stream: the sub-block container layer itself
is effectively free). All
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
  tables, plus the Appendix D reference algorithms (§6.1 compression /
  §6.2 decompression, pinned by the document's own worked examples),
  from the Interactive Multimedia Association "Recommended Practices
  for Enhancing Digital Audio Compatibility" Rev 3.00, staged at
  `docs/audio/adpcm/ima/IMA_ADPCM.pdf`.
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
  internally. The per-sample reconstruction follows the doc §3 rule
  `delta = (step·mmm)/8 + step/16 = step·(2·mmm+1)/16`, so at the minimum
  step (16) the eight magnitude levels are the documented `{1,3,5,…,15}`
  ladder; encode and decode share the recurrence bit-for-bit.
- **ITU-T G.726** — the complete §4.2 sub-block specification
  (EXPAND-less linear interface, LOG/SUBTB/QUAN, RECONST/ADDA/ANTILOG,
  FILTA-FILTE, FUNCTW/FUNCTF, LIMA-LIMD, FMULT/ACCUM/ADDB/ADDC,
  FLOAT A/B, UPA1/UPA2/UPB/XOR/TONE/TRANS/TRIGA/TRIGB) from
  Recommendation G.726 (12/1990), staged at
  `docs/audio/adpcm/g726/T-REC-G.726-199012-I.pdf`, with the per-rate
  RECONST / W(I) / F(I) tables cross-checked against the extracted
  normative CSVs under `docs/audio/adpcm/g726/tables/`. `DQ` uses the
  16-bit signed-magnitude form (Table 6 note b, mandatory at
  40 kbit/s). The quantizer decision ladders (Tables 7-10) were
  verified against the synchronous-coding `ID` tables (Tables 16-19).
  The G.711 log-PCM interface (§4.2.1 EXPAND, §4.2.8 COMPRESS/SYNC)
  is transcribed from the same Recommendation, with the per-sign
  quantization conventions and the SP+/SP− neighbour rules pinned by
  the Table 15 and Table 20 worked examples. Bit-exactness is proven
  by the ITU Appendix II digital test sequences (black-box
  input→output data staged from `docs/audio/adpcm/g726/conformance/`
  into `tests/fixtures/g726/`); no reference implementation of any
  kind was consulted. The G.723/G.721-in-WAV sub-block bit-cell grid
  is transcribed from `docs/audio/adpcm/g72x-wav/` — the packing
  convention reconstructed from the archived catalogue's surviving
  stereo-3-bit "Byte 3" row and validated there by byte-exact packing
  vectors, which the test suite pins in full.
- **OKI / Dialogic VOX ADPCM** — 49-entry step table and 8-entry
  step-pointer adjustment from Dialogic Corporation's *Dialogic ADPCM
  Algorithm* application note (doc 00-1366-001, 1988). Headerless `.vox`
  (caller supplies sample rate) plus the `WAVE_FORMAT_OKI_ADPCM`
  (`0x0010`) WAV framing, which decodes byte-identically. The
  catalogue's `OKIADPCMWAVEFORMAT` `fmt ` extension — a single
  `WORD wPole` ("high frequency emphasis value", `cbSize = 2`) — is
  serialised/parsed by `dialogic::wav_format_extra` /
  `wav_parse_format_extra` and accepted through
  `CodecParameters::extradata`; no emphasis transfer function is
  specified, so the field is carried, not applied (the code stream
  decodes independently of it). The MSM6258's
  LSB-first nibble order is reachable via
  `dialogic::decode_packet(.., NibbleOrder::LoFirst, ..)`; the raw 12-bit
  value is available via `dialogic::Output::Native12`. The app note's
  §5 stream-reset sequence — 24 bytes / 48 samples of alternating ±zero
  codes that walk the step pointer to its floor without introducing a DC
  offset — is produced by `dialogic::reset_preamble`. The 3-bit OKI mode
  is not implemented: the archived catalogue documents its WAV framing
  (`wBitsPerSample = 3`, `nBlockAlign` 3 mono / 6 stereo) but the app
  note specifies only the 4-bit algorithm — the 3-bit quantiser /
  reconstruction rule is a staged-docs gap.
- **IANA WAVE registry (RFC 2361)** — the `wFormatTag` assignments
  `0x0017` (`WAVE_FORMAT_DIALOGIC_OKI_ADPCM`, §A.16) and `0x0064`
  (`WAVE_FORMAT_G726_ADPCM`, §A.54) are transcribed from the RFC 2361
  registry staged at `docs/container/riff/rfc2361-wav.txt`. The raw
  G.726-in-WAV tag `0x0045` is not in any staged catalogue; it is
  claimed on black-box evidence alone (the opaque validator's own
  G.726 WAV output carries it, and decodes `0x0064` byte-identically),
  with the basis pinned in `tests/wav_tag_e2e.rs`.

## License

MIT. See [LICENSE](LICENSE).
