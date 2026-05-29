# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
