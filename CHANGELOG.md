# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Encoder leading-edge transient reduced for MS-ADPCM and
  IMA-ADPCM-WAV.** Both block-oriented encoders now seed their
  per-block step state from the mean absolute first-difference of the
  first 16 samples in each block, instead of using a fixed cold-start
  value (`delta = 16` for MS, `step_index = 0` for IMA-WAV).
  * **IMA-ADPCM-WAV** — same mean-|Δ| heuristic the IMA-ADPCM-QT
    encoder already uses: `target_step ≈ mean_delta × 8 / 3`, then
    pick the first IMA step-table entry ≥ that target. For a 22.05 kHz
    440 Hz amplitude-12000 sine, round-trip RMS error against the
    source drops from ~413 (mono) / ~634 (stereo) to ~88 / ~78 — a
    79% / 88% reduction.
  * **MS-ADPCM** — with the default predictor index 0 (coef1=256,
    coef2=0) the decoder recurrence reduces to
    `reconstructed = sample1 + signed_nibble × delta`, so seeding
    `delta ≈ mean_|Δ| / 4` places typical-magnitude nibbles at the
    midrange of the 16-candidate sweep. RMS error on the same sine
    drops from ~271 / ~207 (mono / stereo) to ~100 / ~86 — a 63% / 59%
    reduction. The seed is clamped to the spec-mandated [16, 16384]
    range.
  * Encoder fuzz / round-trip tests already in place all still pass;
    the per-variant RMS bounds in `tests/encode_round_trip.rs` are
    tightened from < 1000-1500 to < 250 to pin the improvement.

### Added

- **Typed `Variant` accessor surface.** The decoder-dispatch enum
  `decoder::Variant` is now re-exported at the crate root and gains
  a small inspection API so callers that already know their codec
  do not have to round-trip through a `&str`:
  * `Variant::all()` — `&'static [Variant]` over every supported
    variant in declaration order.
  * `Variant::codec_id()` — canonical `adpcm_*` id string (matches
    the existing `CODEC_ID_*` constants).
  * `Variant::from_codec_id(&CodecId)` (newly public) — `Option<Variant>`
    parse of the id back to the typed enum.
  * `Variant::wave_format_tag()` — `Option<u16>` returning `0x0002`
    (MS), `0x0011` (IMA-WAV) or `0x0020` (Yamaha-B); `None` for the
    three tagless variants (IMA-QT addresses via FourCC; ADPCM-A and
    Dialogic VOX are chip-internal / headerless).
  * `Variant::fourcc()` — `Option<[u8;4]>` returning `b"ima4"` for
    ADPCM-IMA-QT and `None` for every other variant.
  Five unit tests pin the round-trip (`codec_id()` → `from_codec_id()`),
  rejection of non-ADPCM ids, exhaustiveness of `Variant::all()`
  against the `CODEC_ID_*` constants, and bit-for-bit agreement
  between the typed tag accessors and what `register_codecs` actually
  wires into the registry — so any future ADPCM variant addition has
  to update both surfaces in lockstep.
- **Encoder fuzz / never-panic coverage** (`tests/encoder_fuzz.rs` +
  4 new `fuzz/` libfuzzer targets) — symmetric counterpart to the
  existing decoder fuzz suite. The in-tree harness adds 17
  deterministic tests across every variant: adversarial PCM
  (`i16::MIN/MAX`, alternating ± clips, DC), randomised block-size
  + sample-count sweeps for the block-oriented variants, out-of-spec
  encoder-state seeds (negative `step_index`, out-of-range
  `predictor` / `acc`) for the stream-oriented variants, plus a
  registry-level pass covering zero-length frames + random-byte
  streams through `Encoder::send_frame` + `flush`. The cargo-fuzz
  side adds four new libfuzzer targets (`encode_packet_ms`,
  `encode_packet_ima_wav`, `encode_packet_ima_qt`,
  `encode_packet_stream`) so a long-running fuzz job can do
  coverage-guided exploration of the encoder hot path against
  arbitrary PCM input + (for the stream variants) arbitrary state
  seeds. Contract: every PCM + parameter tuple produces either
  `Ok(Vec<u8>)` or `Err(Error::Invalid | Error::Unsupported)`
  (block-oriented) or a finite `Vec<u8>` (stream-oriented); never
  panic, debug-overflow, OOM, or index out of bounds.

### Fixed

- **MS-ADPCM encoder integer overflow on adversarial PCM.** The
  encoder's simulate-then-advance search loop multiplied
  `MS_ADAPTATION[n] * delta` (and `sample1 * coef1 + sample2 *
  coef2`) in i32 — same shape as the decoder bug fixed in the prior
  round. On adversarial input (e.g. alternating `i16::MIN` /
  `i16::MAX`) the search can drive `delta` past 1 M after a handful
  of iterations, overflowing the i32 product. Lifted the recurrence
  to i64 with saturating multiplication and a final clamp back to
  i32 / i16 storage. Spec-compliant streams produce bit-identical
  output (the existing round-trip + oracle tests still pass);
  adversarial PCM emits bounded `Ok` blocks instead of panicking
  under `debug-assertions`. Surfaced by
  `tests/encoder_fuzz.rs::ms_encoder_extreme_pcm_never_panics`.
- **Yamaha ADPCM-A `step_index` index-out-of-bounds on adversarial
  encoder state.** `decode_nibble` and `encode_sample` indexed
  `YAMAHA_A_STEP_SIZE` with `state.step_index as usize` directly —
  a caller-supplied `Channel` (such as a long-stream resume) carrying
  a negative `step_index` wrapped to a huge unsigned index and
  panicked. Both functions now clamp `step_index` (and `acc`) to
  their tabulated spec ranges on entry, identical to the
  post-update clamp the same function already applies on the way
  out. Round-trip + bit-equivalence with the prior nibble
  trajectories is preserved (verified by all existing encoder /
  decoder tests). Surfaced by
  `tests/encoder_fuzz.rs::yamaha_a_encoder_extreme_state_seed_never_panics`.

### Added (prior depth-mode work)

- **Coverage-guided fuzz harness** (`fuzz/`) — depth-mode complement
  to the existing in-tree deterministic `tests/decoder_fuzz.rs`
  structured-malformation suite. New cargo-fuzz crate at
  `crates/oxideav-adpcm/fuzz/` with four libfuzzer targets:
  `decode_packet_ms` (drives `ms::decode_block` with a fuzz-picked
  channel count + arbitrary header/body bytes), `decode_packet_ima_wav`
  (same shape for `ima_wav::decode_block`, 1..=8 channels),
  `decode_packet_ima_qt` (the 34-byte/channel Apple QuickTime path
  through `ima_qt::decode_block`), and `decode_packet_stream` (one
  fuzz byte picks the variant, the next picks the channel count, the
  next 8 seed the predictor + step-index — exercising Yamaha
  ADPCM-A / ADPCM-B and Dialogic VOX in both `HiFirst`/`LoFirst`
  nibble orders and `Wide16`/`Native12` output widths). Contract is
  "every byte slice returns `Ok` or `Err`, never panics / debug-
  overflows / OOMs". The fuzz crate is a self-contained workspace
  member (`[workspace] members = ["."]`) so libfuzzer's nightly
  requirement doesn't leak into the umbrella resolver; `fuzz/target`,
  `fuzz/corpus/*/`, and `fuzz/artifacts` are gitignored while the
  target sources under `fuzz/fuzz_targets/` are committed. Run with
  `cd crates/oxideav-adpcm/fuzz && cargo +nightly fuzz run <target>`.
  Closes the "saturated → fuzz/bench/profile" memo's coverage-guided
  exploration slot — every variant has shipped feature-complete
  decoder + encoder pairs (README "Status" table all `yes/yes`),
  Criterion benches landed last round, and structured-malformation
  in-tree fuzz already covers hand-enumerated cases; this adds the
  long-running coverage-guided exploration layer on top.

- **Criterion bench harness** (`benches/decode.rs`) — depth-mode
  benchmark coverage for the per-block / per-packet decode hot path
  across all six ADPCM variants. 11 scenarios: MS-ADPCM mono
  (256-byte blocks, ~1 s @ 22050 Hz) + stereo (512-byte blocks,
  ~500 ms); IMA-ADPCM-WAV mono + stereo at the same shapes; IMA-ADPCM
  QuickTime mono + stereo at the spec-mandated 34-byte block; Yamaha
  ADPCM-B mono + stereo streaming at 8 kHz; Yamaha ADPCM-A mono with
  the 12→16-bit `Wide16` output; Dialogic VOX mono in both nibble
  orders (HiFirst/Wide16 — canonical `.vox`/MSM6295 — and
  LoFirst/Native12 — MSM6258). Block-oriented variants build a valid
  encoded buffer via the crate's public encoder at setup time, so the
  timed loop measures only the decoder. Stream-oriented variants feed
  a deterministic xorshift32 byte stream straight into
  `decode_packet`. New `criterion = "0.5"` dev-dep, new
  `[[bench]] name = "decode"` harness; no library-API change. Run
  with `cargo bench -p oxideav-adpcm --bench decode`. Per the
  workspace "saturated → fuzz/bench/profile" memo — every variant has
  shipped feature-complete decoder + encoder pairs (README "Status"
  table all `yes/yes`), so the next observable improvement is making
  the existing implementation faster against a stable, fixture-free
  A/B baseline.

- **Yamaha ADPCM-A** (`adpcm_yamaha_a`) — second Yamaha 4-bit ADPCM
  flavour, the YM2608 rhythm-ROM / YM2610 ADPCM-A channel codec.
  Distinct from the existing ADPCM-B / DELTA-T (`adpcm_yamaha`)
  variant: ADPCM-A uses a 49-entry step-size table (`16 .. 1552`,
  numerically identical to OKI/Dialogic Table 2) and a 16-entry
  step-pointer adjustment `{-1,-1,-1,-1, 2, 5, 7, 9, ...}` (versus
  OKI's `{2, 4, 6, 8}` upper half — the magnitude-7 growth differs).
  Output is 12-bit signed (`-2048 ..= 2047`) clamped on the silicon;
  the registry-resolved decoder shifts to 16-bit so consumers see
  uniform i16-LE PCM. New module `src/yamaha_a.rs` (decoder + encoder
  + Native12/Wide16 output enum); new tables `YAMAHA_A_STEP_SIZE` +
  `YAMAHA_A_INDEX_ADJUST` + `YAMAHA_A_PREDICTOR_{MIN,MAX}` in
  `tables.rs` transcribed directly from
  `docs/audio/adpcm/yamaha/yamaha-adpcm.md` §3 (independent-RE consensus
  of the NeoGeo Development Wiki + MAME/ymfm hardware-RE effort against
  real YM2608/YM2610 silicon — NOT from any general-purpose multimedia
  decoder source). Single channel by chip design; the factory rejects
  stereo with `Error::Unsupported`. Stream-oriented (per-byte state
  carries across `send_frame` calls). Round-trip RMS for a 50 ms
  220 Hz sine at 8 kHz wide-16 amp 6000 stays under 4500 LSB; through
  the registry on a 100 ms 440 Hz sine at amp 12000, under 7000 LSB.
  5 new fuzz-style coverage tests + 1 registry round-trip + 2 factory
  unit tests; existing factory sweeps extended to cover the 6th
  variant.

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
