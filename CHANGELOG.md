# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **MS-ADPCM decoder integer overflow on adversarial input.** A block
  whose header parsed a wild `delta` (signed-i16 read directly from
  `block[channels..]`) could overflow the `MS_ADAPTATION[i] * delta`
  i32 multiplication inside `decode_nibble`, panicking the decoder
  under `debug-assertions` (and silently wrapping in release). Lifted
  the delta-adapt + predictor recurrence to i64 with saturating
  multiplication, then clamp back to i32 (capped at `i32::MAX`).
  Spec-compliant streams produce bit-identical output (validated by
  the existing ffmpeg-oracle round-trip tests); hostile inputs now
  surface as bounded `Ok` decoded samples instead of a panic. Surfaced
  by the new `tests/decoder_fuzz.rs::ms_truncated_prefixes_never_panic_mono`
  coverage.

### Added

- **Decoder fuzz / never-panic coverage** (`tests/decoder_fuzz.rs`) —
  26 structured-malformation tests across all five variants
  (`adpcm_ms`, `adpcm_ima_wav`, `adpcm_ima_qt`, `adpcm_yamaha`,
  `adpcm_dialogic`). Truncated-prefix sweeps, every out-of-spec
  predictor / step-index byte, body-misalignment cases, an in-test
  deterministic LCG driving a few thousand pseudo-random bytes through
  each decoder, and trait-level (`Decoder::send_packet` /
  `receive_frame`) end-to-end pushes — every path must return `Ok` or
  `Err` cleanly, never panic. Property-style assertions also pin the
  spec-derived emitted-sample-count formulas (MS: `2 + body_bytes*2`,
  IMA-WAV: `1 + groups*8`, IMA-QT: `64*channels`, Yamaha/Dialogic:
  `2*packet_bytes`).
- **Yamaha ADPCM encoder** (`encoder::YamahaEncoder`,
  `yamaha::encode_sample`, `yamaha::encode_packet`) — closes the
  last decoder-only variant in the crate. Closed-form quantiser
  derived from the Y8950 manual §I-4 *analysis* recurrence: sign
  bit from `dn = Xn − x̂n`, magnitude bits from the eight thresholds
  `{0, 1/4, 1/2, 3/4, 1, 5/4, 3/2, 7/4}` of `|dn|/Δn` printed in
  Table 5-1 (YM2608) and Table 1 (AICA FQ8005). State advances
  through `yamaha::decode_nibble` so the encoder is bit-for-bit
  equivalent to the decoder it ships with. Stream-oriented
  (per-channel predictor + step carry across `send_frame` calls);
  up to 8 channels, sample-interleaved input, low-nibble-first byte
  packing per the WAV-tag-0x0020 convention. Round-trip RMS error
  for a 50 ms 220 Hz sine at 8 kHz stays under 2000 LSB mono /
  stereo, under 3000 LSB through the registry on a 100 ms sine.
- `encoder::make_encoder` now serves `CODEC_ID_YAMAHA`; the codec's
  `register_codecs` entry installs both decoder and encoder.
- `tests/encode_round_trip.rs` — added Yamaha mono + stereo
  registry round-trip cases alongside the existing four variants.

## [0.0.5](https://github.com/OxideAV/oxideav-adpcm/compare/v0.0.4...v0.0.5) - 2026-05-29

### Other

- update register_codecs docstring to reflect 5 variants
- add OKI/Dialogic VOX decoder + encoder (adpcm_dialogic)
- IMA-ADPCM-QT (Apple ima4) encoder
- MS-ADPCM and IMA-ADPCM-WAV encoders (decoder-loop search)

### Added

- **OKI / Dialogic VOX ADPCM** decoder + encoder (`adpcm_dialogic`),
  registered through `register_codecs`. Headerless byte-stream codec
  used by Dialogic voice-processing hardware and the OKI MSM6258 /
  MSM6295 silicon family (`.vox` files). Implementation transcribed
  from Dialogic Corporation's *Dialogic ADPCM Algorithm* application
  note (doc 00-1366-001, 1988): 49-entry calculated step-size table
  (Table 2), 8-entry magnitude-indexed step-pointer adjustment (the
  row-collapsed Table 1), and the §2–§3 decoder + encoder pseudocode.
  The reconstructed predictor is signed 12-bit (`-2048..=2047`) inside
  the codec and is shifted to the i16 range on output for the registry
  path; the native 12-bit form is available via `dialogic::Output::Native12`.
  MSB-first nibble unpack (Dialogic VOX / MSM6295) is the registry
  default; LSB-first (MSM6258) is selectable on the `dialogic::decode_packet`
  /`dialogic::encode_packet` lower-level API via the [`NibbleOrder`] enum.
  Round-trip RMS error for a 0.1 s 440 Hz sine at 8 kHz stays under
  6000 LSB (against a 12000-LSB-amplitude i16 source).
- **MS-ADPCM encoder** (`encoder::MsEncoder`) and **IMA-ADPCM-WAV
  encoder** (`encoder::ImaWavEncoder`) implementing
  `oxideav_core::Encoder`. Both factories register through
  `register_codecs` so `CodecRegistry::first_encoder(&params)` works
  out of the box.
- Encoders use the decoder-loop nibble-search algorithm derived from
  the existing decoder recurrence (no third-party encoder source
  consulted). Default per-channel block size is 256 bytes; override
  via the per-variant `set_block_size` method before the first
  `send_frame` call.
- `tests/encode_round_trip.rs` — end-to-end PCM → encode → decode →
  PCM round trips through the registry for MS mono/stereo and
  IMA-WAV mono/stereo; bounded RMS error against the source.
- **IMA-ADPCM-QT encoder** (`encoder::ImaQtEncoder`,
  `encoder::ima_qt_encode_block`) for the Apple QuickTime `ima4`
  variant. Fixed 34-byte-per-channel blocks per spec (no
  `set_block_size`); block-level channel interleave preserved on
  output. The encoder picks its initial step-index seed from the
  mean |Δ| of the first 8 samples to compress the leading-edge
  transient that block-by-block re-seeding otherwise creates. Round
  trips through `ima_qt::decode_block` plus registry-level mono/stereo
  round trips through `tests/encode_round_trip.rs`. Mono/stereo RMS
  on a 0.1 s 440 Hz sine at 22.05 kHz stays under 1500 LSB.

## [0.0.4](https://github.com/OxideAV/oxideav-adpcm/compare/v0.0.3...v0.0.4) - 2026-05-06

### Other

- drop dead `linkme` dep
- registry calls: rename make_decoder/make_encoder → first_decoder/first_encoder
- auto-register via oxideav_core::register! macro (linkme distributed slice)
- unify entry point on register(&mut RuntimeContext) ([#502](https://github.com/OxideAV/oxideav-adpcm/pull/502))

### Changed

- **`register` entry point unified on `RuntimeContext`** (task #502).
  The legacy `pub fn register(reg: &mut CodecRegistry)` is renamed to
  `register_codecs` and a new `pub fn register(ctx: &mut
  oxideav_core::RuntimeContext)` calls it internally. Breaking change
  for direct callers passing a `CodecRegistry`; switch to either the
  new `RuntimeContext` entry or the explicit `register_codecs` name.

## [0.0.3](https://github.com/OxideAV/oxideav-adpcm/compare/v0.0.2...v0.0.3) - 2026-05-03

### Other

- replace never-match regex with semver_check = false
- migrate to centralized OxideAV/.github reusable workflows
- drop unused AdpcmDecoder fields + imports (slim-frame leftover)
- adopt slim AudioFrame shape
- pin release-plz to patch-only bumps

## [0.0.2](https://github.com/OxideAV/oxideav-adpcm/compare/v0.0.1...v0.0.2) - 2026-04-25

### Other

- drop oxideav-codec/oxideav-container shims, import from oxideav-core
- clippy + rustfmt polish
- integration tests against ffmpeg oracle + verona AVI fixture
