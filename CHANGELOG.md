# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Three more WAV-container tag claims** — the registry (and
  `Variant::wave_format_tags` / `from_wave_format_tag`) now also route:
  `0x0017` (`WAVE_FORMAT_DIALOGIC_OKI_ADPCM` per the IANA WAVE registry,
  RFC 2361 §A.16 — the same 4-bit OKI chip-set body as `0x0010` /
  `0x0203`) → `adpcm_dialogic`; `0x0045` (the raw bit-continuous
  G.726-in-WAV tag common tools write — `nBlockAlign = 1`, MSB-first;
  established black-box against the opaque validator, whose own G.726
  WAV output carries this tag) and `0x0064` (`WAVE_FORMAT_G726_ADPCM`,
  RFC 2361 §A.54; decoded byte-identically to `0x0045` by the
  validator) → `adpcm_g726`. A WAV demuxer that resolves tags through
  the codec registry now reaches these decoders directly.
- **Container-derived G.726 framing default** — when the demuxer
  records the on-wire tag in `CodecParameters::tag`, the G.726
  factories default the `framing` option from it: the Antex sub-block
  tags (`0x0040` / `0x0014`, staged `docs/audio/adpcm/g72x-wav/` layout)
  default to `framing=wav`; the raw-stream tags (`0x0045` / `0x0064`)
  and tagless parameters keep `framing=raw`. An explicit `framing`
  option always wins (`decoder::g726_default_framing` exposes the rule
  to tests).
- **`OKIADPCMWAVEFORMAT` (`wPole`) extension support** — the staged
  catalogue's OKI-in-WAV entry defines a single `WORD wPole` ("high
  frequency emphasis value", `cbSize = 2`) after the `WAVEFORMATEX`
  base. `dialogic::wav_format_extra` / `wav_parse_format_extra`
  serialise and parse it; the registry decoder accepts the extension
  through `CodecParameters::extradata` (a one-byte body is rejected as
  malformed) and decodes the 4-bit code stream independently of the
  value — the catalogue specifies no emphasis transfer function, so the
  field is carried, not applied. `Variant::Dialogic.build_wave_format_extra`
  now emits the `wPole = 0` (no-emphasis) trailer for the catalogue's
  4-bit geometry (`nBlockAlign = 1`, mono or stereo) instead of `None`.
- **Block geometry from the documented `fmt ` trailers** — with no
  explicit `block_align` option, the MS and IMA-WAV decoders now derive
  `nBlockAlign` from the `wSamplesPerBlock` word that opens the
  `ADPCMWAVEFORMAT` / `DVIADPCMWAVEFORMAT` extensions when the demuxer
  passes the extension body through `CodecParameters::extradata`
  (4-bit via `Variant::block_size_bytes`, 3-bit via the 12-byte-group
  inverse), so multi-block packets split correctly with no out-of-band
  option. `wSamplesPerBlock = 0` is treated as absent; a non-zero value
  off the block-boundary lattice errors as a malformed header.

- **IMA reference ladder compressor** — the compression procedure the
  IMA "Recommended Practices" Rev 3.00 publishes in Appendix D §6.1
  (and the DVI Wave Type specification's matching 4-bit / 3-bit encode
  listings) joins the crate as an alternative to the decoder-loop
  search: `ima_wav::ima_quantize_nibble` / `ima_quantize_code3`
  quantize by successive threshold subtraction and advance the shared
  `(predictor, step_index)` state through the published expansion, so
  encode is the bit-exact inverse of decode by construction. The §6.1
  and §6.2 worked examples are pinned byte-for-byte.
- **Reference block encoders with cross-block index carry** —
  `encoder::ima_encode_block_reference` /
  `ima_encode_block_3bit_reference` / `ima_qt_encode_block_reference`
  follow the specification's stream shape exactly: header predictor is
  `Samp0` verbatim (top-9-bit form in the QT preamble), no heuristic
  seeding, and the new public `ima_wav::ImaCodecState` (one per
  channel, caller-held) carries the step index across blocks — cleared
  once before the first block, each header recording the previous
  block's end index — so the byte stream is fully determined by the
  input (interchange-exact). Hostile carried indices are clamped to
  the header-representable 0..=88 on entry.
- **`quantizer` codec option (IMA-WAV / IMA-QT encoders)** — `search`
  (default; unchanged) or `reference` selects the strategy on the
  registry path; `ImaWavEncoder::set_quantizer` /
  `ImaQtEncoder::set_quantizer` on the direct API. The option is
  encoder-side only, so the IMA decoders validate and ignore it (an
  encode→decode pair built from one `CodecParameters` keeps working);
  other variants reject it. Registry reference streams are pinned
  byte-identical to the direct block APIs under ragged frame chops.
- **Reference-compressor conformance suite**
  (`tests/ima_reference.rs`) — double-entry verification against
  independent in-test re-transcriptions of both staged listings: the
  Recommendation's worked examples, step-table boundary rows, dense
  predictor/sample grids at every step index, an exhaustive
  index × code × predictor-grid sweep for both widths, a 200k-sample
  continuously-carried lockstep walk, byte-exact oracle stream
  assembly (mono + stereo, DVI layout + index carry), QT preamble +
  body nibbles re-derived through the oracle, and the pin that the
  default search encoder is never materially worse than the reference
  (why `search` stays the default).
- **Reference-mode wire conformance + hostile-input coverage** — the
  opaque-validator harness decodes reference-ladder streams (mono
  256-byte-block broadband, stereo 1024, QT CAF) proving an
  independent decoder honours the cross-block index carry;
  `tests/encoder_fuzz.rs` adds hostile carried-state and
  random-frame-chop reference legs; the `encode_packet_ima_wav` /
  `encode_packet_ima_qt` fuzz targets grow search + reference legs
  (including the previously-unfuzzed 3-bit encoder) and ran bounded
  coverage-guided sessions (~0.3M / ~9M execs) with zero findings.

- **G.726 VBR conformance — staged demo reference reproduced
  bit-exactly** (`tests/g726_vbr_conformance.rs`). The staged VBR demo
  reference set (`docs/audio/adpcm/g726/conformance/voice*`, schedule
  recovered in `docs/audio/adpcm/g726/vbr-demo-rate-schedule.md`) is
  now a black-box gate on `State::set_rate`: the linear leg
  (`voicevbr.lrf`) reproduces byte-for-byte over all 52 736 samples —
  A-law compand front end, §4.2.1/§4.2.8 law interface, and 3 295
  mid-stream rate switches through the `16-24-32-40-32-24` kbit/s
  cycle with full Table 6 state carriage. The run also settles the
  schedule note's one flagged unknown **empirically**: the demo's
  frame period is **16 samples, not the note's 256-sample inference**
  (a 256-period run diverges; pinned by
  `frame_period_is_16_samples_not_256`), and a codec pair that resets
  at each switch falls off the reference trajectory (pinned by
  `reference_run_carries_state_across_switches` — the reference itself
  carries state). The vectors are non-normative demo data and stay in
  the docs staging area: the tests read them through the
  `OXIDEAV_G726_VBR_DIR` environment variable and skip cleanly when it
  is unset. The two log-PCM legs (`voicevbr.arf` / `voicevbr.urf`) are
  deliberately unpinned: exhaustive black-box search shows they are
  not reproducible from the staged `voice.src` under any byte-level
  input model (log-PCM passthrough matches both legs bit-exactly
  through sample 82 — up to the file's embedded ASCII annotation —
  then provably admits no shared continuation; every companding model
  fails from sample 0), so their reconstruction is a documented
  upstream-docs gap, not a codec gap (the law interfaces are already
  bit-exact per the Appendix II corpus). `tests/g726_vbr.rs` header
  corrected to cite the established 16-sample period while keeping its
  256-sample property-test blocks for SNR-floor stability, and gains a
  **per-sample pseudo-random rate walk** test — Appendix I.1's
  strongest form, the rate free to change at every sample at arbitrary
  non-block-aligned positions — pinning encoder/decoder lockstep and
  word-exact SYNC tandem transparency on both law interfaces under
  thousands of random switches.

- **G.723/G.721-in-WAV sub-block bit-cell codec** — the intra-sub-block
  bit grid is now staged (`docs/audio/adpcm/g72x-wav/`, reconstructed
  from the packing convention plus the surviving stereo-3-bit "Byte 3"
  row and pinned by byte-exact packing vectors), unblocking the
  sub-block decoder this crate previously could not offer. New
  `g726::wav_pack_codes` / `wav_unpack_codes` implement the grid (codes
  MSB-first into a big-endian bitstream, time-major channel-minor
  stereo interleave, whole 8-sample-per-channel sub-blocks);
  `g726::wav_strip_aux` removes the per-block `nAuxBlockSize` prefix;
  `g726::wav_decode_packet` / `wav_encode_packet` run the unpacked
  codes through per-channel §4.2 codec states (mono or stereo — the
  container defines a two-channel interleave, unlike the raw
  bit-continuous telephony stream); `g726::wav_rate_supported` gates
  the layer to the documented 3-/4-/5-bit rates (2-bit / 16 kbit/s has
  no tag in the archived catalogue). `tests/g726_wav_framing.rs` pins
  all four staged packing vectors byte-for-byte, re-derives the
  surviving catalogue bit row independently of the packer, and covers
  stereo lane independence, cross-packet state carriage and
  failed-call state-purity.
- **`framing` / `aux_block_size` codec options (G.726 registry
  decoder)** — `framing=wav` switches the `adpcm_g726` decoder from the
  raw bit-continuous telephony stream onto the G.723/G.721-in-WAV
  sub-block layout: MSB-first bit-cell unpacking (an explicit
  `bit_order=lsb` is rejected — the grid fixes the order), 1..=2
  channels (the container defines a stereo interleave; each lane is an
  independent codec state), only the documented 3-/4-/5-bit rates, and
  a per-block `aux_block_size` (`nAuxBlockSize`) auxiliary prefix
  stripped incrementally — block position, sub-byte bits *and* a
  lane-alignment code carry all persist across packets, so a demuxer
  may split blocks anywhere (including mid-prefix and mid-frame).
  `reset` re-seeds every lane plus the framing cursors.
- **`framing=wav` on the G.726 registry encoder** — the encode side of
  the same container layout: whole 8-sample-per-channel sub-blocks
  (partial frames buffer lane-aligned across `send_frame` calls;
  `flush` pads the final sub-block with silence so the stream stays the
  shape every G.723/G.721-in-WAV reader expects), stereo per-lane
  states, aux-free blocks (`aux_block_size` must be 0 on encode), and
  the same rate / bit-order gates as the decoder. The emitted bytes are
  chop-invariant and byte-identical to `g726::wav_encode_packet`; the
  G.711 log-PCM `law` interface composes with the WAV framing on both
  sides.
- **G.723/G.721-in-WAV `fmt ` extension serialisation** —
  `g726::wav_format_extra` / `wav_parse_format_extra` round-trip the
  catalogue's one-field extension (`nAuxBlockSize`, `cbSize = 2`;
  the leading `cbSize` word excluded per the crate's extradata
  convention). `Variant::G726.build_wave_format_extra(channels,
  block_align)` now serialises the aux-free form when `nBlockAlign` is
  exactly 16 documented-rate sub-blocks (48 / 96, 64 / 128, 80 / 160)
  instead of always `None`; ambiguous geometries still return `None`.
  The `framing=wav` registry decoder also accepts the extension through
  `CodecParameters::extradata` (an explicit `aux_block_size` option
  wins over extradata).
- **Variable-bit-rate switching pinned under the documented demo
  schedule** — `tests/g726_vbr.rs` exercises `State::set_rate` under
  the staged block-cyclic schedule (`16-24-32-40-32-24` kbit/s at a
  256-sample period, applied cyclically and stopping mid-cycle):
  encoder/decoder lockstep with per-block SNR floors across every
  switch direction, §4.2.8 SYNC tandem transparency held across rate
  switches (three synchronous stages, both laws, word-identical from
  stage two on), and a proof that carrying the Table 6 state through a
  switch is load-bearing (a receiver that resets at the boundary falls
  off the encoder's trajectory).
- **WAV-framing hostile-input coverage** — the structured-malformation
  suites gain `framing=wav` legs: arbitrary bytes through the registry
  decoder under every rate × channel × aux combination with random
  packet chops (mid-prefix, mid-code, mid-frame) match the
  whole-buffer reference exactly and never panic; full-range noise
  through the registry encoder under random frame chops stays
  byte-identical to the direct API including the silence-padded flush
  tail. The coverage-guided `decode_packet_g726` / `encode_packet_g726`
  fuzz targets grew matching sub-block legs (split transparency at
  sub-block boundaries, ragged-tail rejects, aux-stripping bounds);
  both ran bounded coverage-guided sessions (~1.4M / ~2.6M execs) with
  zero findings.
- **WAV-framing bench scenario** — `decode_g726_40kbit_wav_stereo_1s`
  times the bit-cell unpack plus two independent codec lanes over 1 s
  of 40 kbit/s stereo stream; at ~343 µs vs ~179 µs for the mono raw
  stream (reference machine) the container layer + second lane cost
  ~1.91× mono, i.e. the per-sample state machine still dominates and
  the sub-block layer itself is effectively free.

- **`WAVE_FORMAT_G723_ADPCM` (`0x0014`) alias tag for G.726** — the older
  CCITT G.723 ADPCM (3-bit / 24 kbit/s and 5-bit / 40 kbit/s rates) that
  the 1990 G.726 Recommendation consolidates alongside G.721 now routes
  to the `adpcm_g726` decoder: the tag is registered on the codec and
  `Variant::from_wave_format_tag(0x0014)` returns `Variant::G726`. The
  rate is taken from `wBitsPerSample` (the `bits_per_sample` codec
  option); the canonical G.726 tag stays `0x0040`
  (`WAVE_FORMAT_G721_ADPCM`, the 4-bit rate). The staged catalogue notes
  "the G.721 header format is essentially the same as G.723". New
  `g726::wav_subblock_bytes` / `wav_block_align` const helpers (plus
  `WAV_SUBBLOCKS_PER_BLOCK` / `WAV_SAMPLES_PER_BLOCK`) compute the
  WAV block-alignment geometry for these tags — a sub-block is
  `bits_per_sample * channels` bytes (8 samples/channel) and `nBlockAlign`
  is 16 sub-blocks plus `nAuxBlockSize`, reproducing the catalogue's
  tabulated 48 / 96 / 80 / 160 rows exactly at `aux = 0`. (The intra-byte
  bit ordering of a sub-block did not survive in the archived document, so
  no sub-block decoder is offered — only the byte geometry the tabulated
  values pin.)

- **`WAVE_FORMAT_DIALOGIC_OKI_ADPCM` (`0x0203`) alias tag** — the staged
  `WAVE_FORMAT_*` catalogue assigns a second WAV tag to the OKI VOX body
  the `adpcm_dialogic` variant already decodes: `0x0203`
  ("Dialogic OKI ADPCM": mono, `nBlockAlign = 1`, no extra-format-data,
  4 bits/sample) frames the identical 4-bit high-nibble-first body as
  `WAVE_FORMAT_OKI_ADPCM` (`0x0010`). A WAV demuxer that parses `0x0203`
  now resolves straight to the Dialogic decoder: the tag is registered
  on the codec and `Variant::from_wave_format_tag(0x0203)` returns
  `Variant::Dialogic`. New `Variant::wave_format_tags()` accessor returns
  every tag a variant answers to (canonical first, then aliases) and is
  the single source of truth for `wave_format_tag()` and
  `from_wave_format_tag()`; the canonical tag (`0x0010`) is unchanged.

- **G.726 log-PCM (G.711) interface + proven bit-exactness** — the
  Recommendation's A-law/µ-law PCM interfaces are now implemented on
  the direct API: §4.2.1 EXPAND on the encoder side
  (`g726::State::encode_law`) and the full §4.2.8 output chain —
  COMPRESS, re-EXPAND, difference re-quantization and the SYNC
  synchronous coding adjustment (Tables 16-19 via the QUAN ladders,
  SP+/SP− neighbours per the Table 20 examples, µ-law dual-zero skip
  included) — on the decoder side (`g726::State::decode_law`), with
  `g726::Law` and the `expand` / `compress` conversions public. The
  official ITU-T G.726 Appendix II digital test sequences are staged
  under `tests/fixtures/g726/` and pinned as a byte-exact CI gate in
  `tests/g726_conformance.rs`: all reset and homing legs, encoder and
  decoder, normal + overload + full-codeword-sweep inputs, A-law +
  µ-law + both cross-law decode paths (112 sequence comparisons), plus
  a synchronous-tandem rig that re-encodes the verified decoder
  outputs through two further stages and requires PCM-identical
  results. The homing legs reproduce the Appendix II initialization
  procedure (`pcm_init.*` / `i_ini_<rate>.*`, with the files'
  88-word ASCII annotation trailer pinned and stripped); the one
  shipped vector generated from the reset state (`hn16fc.o`) is
  reproduced as shipped and documented. Law-path property tests cover
  the EXPAND front-end equivalence and the no-panic domain of
  `decode_law`.

- `adpcm_g726` registry `law` codec option (`linear` default / `alaw` /
  `ulaw`): both factories switch onto the log-PCM interface — the
  encoder compands each 16-bit sample to a law word before the §4.2.1
  EXPAND front-end, the decoder runs the §4.2.8 COMPRESS + SYNC chain
  and expands the adjusted law word back to 16-bit linear (the frames
  sit on the G.711 lattice). The registry decoder reproduces the ITU
  reset conformance vectors end to end through the packed wire format
  (`tests/g726_registry.rs`); `g726::expand_i16` / `g726::compress_i16`
  are public.

- **ITU-T G.726 narrowband ADPCM** (`adpcm_g726`) — decoder + encoder
  for all four rates (40/32/24/16 kbit/s; 5/4/3/2 bits per sample) as a
  bit-exact transcription of Recommendation G.726 (12/1990) §4.2: the
  full sub-block set with the spec's masked integer arithmetic, one
  state machine shared by encode and decode (the decoder reproduces the
  encoder's reconstruction trajectory exactly), 16-bit signed-magnitude
  `DQ` (Table 6 note b), and the Table 6 optional-reset seeds. The
  per-rate RECONST / W(I) / F(I) tables live in `tables::G726_*`; the
  quantizer decision ladders (Tables 7-10, verified against the
  synchronous-coding Tables 16-19) sit next to the QUAN block.
  Registry: mono only; `bits_per_sample` option (2/3/4/5, default 4 =
  32 kbit/s) selects the rate and the G.726-specific `bit_order` option
  (`msb` default / `lsb`) the in-byte packing; WAV tag `0x0040`
  (`WAVE_FORMAT_G721_ADPCM`) routes to the decoder at its 4-bit
  default. The stream is bit-continuous: `BitPacker` / `BitUnpacker`
  carry partial code words across packet boundaries (3- and 5-bit codes
  straddle bytes) and `Decoder::reset` / `Encoder::flush` handle the
  residue. Direct API under `g726::` (`State`, `Rate`, `BitOrder`,
  `encode_packet` / `decode_packet`, `pack_codes` / `unpack_codes`).
  `Variant::G726` extends the whole typed accessor surface
  (`from_wave_format_tag(0x0040)`, `Shape::StreamOriented`,
  `max_channels = 1`).

- G.726 conformance + robustness rigs: `tests/g726_validate.rs`
  (opaque-validator cross-checks in both directions at every rate — the
  validator's G.726-in-WAV decodes on our side > 0.97 correlation, our
  bytes wrapped in validator-geometry WAVs decode on its side > 0.97),
  `tests/g726_registry.rs` (per-rate round trips under both bit orders,
  packetization invariance, option validation, tag routing, reset
  re-seed), in-tree never-panic/invariant sweeps in
  `tests/decoder_fuzz.rs` + `tests/encoder_fuzz.rs` (one sample per
  whole code, split-invariant wire bytes, LIMB-rail DC convergence,
  registry/direct byte equality), two coverage-guided cargo-fuzz
  targets (`decode_packet_g726`, `encode_packet_g726`), and four
  Criterion scenarios (~171 µs per decoded second at every rate).

- `Variant::from_wave_format_tag(u16)` and `Variant::from_fourcc([u8;4])`
  — the reverse of `wave_format_tag()` / `fourcc()`. A WAV / AVI /
  QuickTime demuxer that has parsed a `WAVEFORMATEX::wFormatTag`
  (`0x0002` MS, `0x0010` OKI/Dialogic, `0x0011` IMA-WAV, `0x0020`
  Yamaha-B) or a sample-entry FourCC (`ima4`) can now map it straight to
  a typed `Variant` without round-tripping through a codec-id string.
  Tags owned by other codec families (PCM `0x0001`, G.722 `0x0028`, …)
  and the two tagless variants (IMA-QT addressed by FourCC, ADPCM-A
  chip-internal) resolve to `None`. Both are `const fn`. Round-trip
  inversion is pinned for every tagged / fourcc'd variant, and foreign /
  unknown tags + a case-sensitive `ima4`-only FourCC check are pinned
  against drift.

### Other

- `ms::build_extradata` — the inverse of `parse_extradata_coeffs`; emits
  the MS `ADPCMWAVEFORMAT` trailer body (`wSamplesPerBlock` + `wNumCoef` +
  `aCoeff[]`, no `cbSize`) for a chosen `wSamplesPerBlock` and coefficient
  table. `Variant::build_wave_format_extra(channels, block_align)` wraps
  it as the per-variant WAV-muxer convenience (MS full trailer, IMA-WAV
  spb-only word, `None` for IMA-QT + stream variants). The
  `encode_validate.rs` harness now drives its `fmt `-chunk extensions
  through these helpers, so the opaque-validator decode round-trip proves
  the produced trailers are wire-conformant.

- Property-style trailer-builder coverage in `tests/encoder_fuzz.rs`: a
  512-iteration `(variant, channels, block_align)` sweep asserting
  `build_wave_format_extra` is total and self-consistent
  (`Some` ⇒ embedded `wSamplesPerBlock` matches `samples_per_block` and
  MS bytes parse back to the standard table; `None` ⇒ the geometry is
  also rejected by `samples_per_block` or the variant has no WAV
  extension), plus an arbitrary-`wSamplesPerBlock` / custom-coefficient
  `build_extradata` ↔ `parse_extradata_coeffs` strict-inverse check.

- Dialogic **stereo** decode benchmark (`benches/decode.rs`,
  `decode_dialogic_stereo_1s_hifirst_wide16`) mirroring the Yamaha-B
  stereo scenario, so the multi-channel nibble-interleave cursor-advance
  arm is on the timed hot path (12 scenarios total).

- Decoder-side coverage for the OKI / Dialogic **stereo** path
  (`tests/decoder_fuzz.rs`): a two-channel never-panic / state-bounds
  sweep across both nibble orders and several packet lengths, plus a
  byte-exact stereo encode→decode self-consistency check — the
  multi-channel encoder advances per-channel state through the same
  `decode_nibble` the decoder uses, so a fresh decoder fed the encoder's
  bytes must reproduce the encoder's reconstructed trajectory per lane
  with **no error budget** (exact equality, both nibble orders).

- Encoder fuzz coverage for the new OKI / Dialogic multi-channel encode
  path (`tests/encoder_fuzz.rs`): random interleaved PCM (odd + even
  lengths) under both nibble orders for 1..=2 channels with exact
  output-size assertions, plus an adversarial per-channel state-seed
  case (out-of-range predictor / step index) exercising the shared
  clamp-on-advance path. The cross-variant registry fuzz test now drives
  the Dialogic stereo path as well (previously skipped as mono-only).

- OKI / Dialogic VOX **stereo encode** symmetry. The decoder already
  accepted 1..=2 channels (sample-interleaved at the nibble level), but
  the registry encoder hard-rejected anything but mono and the only
  encode entry point (`dialogic::encode_packet`) was single-channel.
  Added `dialogic::encode_packet_multi` / `encode_packet_multi_wide16` —
  the exact inverse of `decode_packet` for the same channel count, packing
  nibbles in the same channel round-robin (nibble 0 → ch 0, nibble 1 →
  ch 1, …, two nibbles per byte) — and plumbed it through the
  `DialogicEncoder`, which now accepts 1..=2 channels via the shared
  `validate_channels` guard. Mono output is byte-identical to the prior
  `encode_packet`. A registry-path stereo encode→decode round-trip pins
  per-lane RMS, and dialogic-level tests confirm mono equivalence, the
  wide16 narrowing wrapper, and stereo per-lane tracking under both
  nibble orders.

- Encoder-output wire-conformance validation (`tests/encode_validate.rs`).
  The existing end-to-end coverage proved our *decoder* tracks an opaque
  validator's decode, and the self round-trip tests proved our encoder
  and decoder agree — but nothing proved our *encoder* emits bytes an
  independent decoder reconstructs faithfully (i.e. that the blocks we
  write are spec-conformant on the wire, not merely self-consistent).
  The new harness closes that direction: it encodes a PCM sine with our
  encoder, wraps the bytes in a spec-correct container assembled in-test
  (a RIFF/WAVE `fmt `+`data` for MS / IMA-WAV including the MS coefficient
  trailer; a minimal CAF `desc`+`data` for the WAV-tag-less QuickTime
  `ima4`), hands the container to the opaque validator to decode back to
  PCM, and cross-correlates the reconstruction against the original
  per channel (> 0.97). Nine cases: MS mono/stereo, IMA-WAV mono/stereo,
  IMA-QT mono/stereo — covering the stereo block-interleave wire layout
  in both directions — plus three broadband cases (a four-partial signal
  instead of a pure tone) where MS / IMA-WAV additionally use a 256-byte
  block so the encoder must write a non-default `wSamplesPerBlock` the
  validator honours to frame the stream. Skips cleanly when the validator
  binary is absent.
- Yamaha ADPCM-A (`adpcm_yamaha_a`) decode-level fidelity fix. The
  per-sample reconstruction used `delta = step·(2·mmm+1)/8`, exactly
  double the staged trace doc §3 rule `delta = (step·mmm)/8 + step/16 =
  step·(2·mmm+1)/16`. The encoder mirrored the same doubled levels, so
  self round-trips were unaffected, but decoding a real YM2610-encoded
  ADPCM-A stream produced twice the correct amplitude. Both decode and
  encode now use the documented `>> 4` shift; a new unit test pins the
  `{1,3,5,…,15}` level ladder at the minimum step against doc §3.
- Registry `chip` codec option for Yamaha ADPCM-B (`adpcm_yamaha`) and
  `nibble_order` codec option for OKI / Dialogic (`adpcm_dialogic`). The
  `yamaha::Chip` (AICA default / OPNA) and `dialogic::NibbleOrder`
  (HiFirst default / LoFirst — MSM6258) selections were previously only
  reachable through the block-level APIs; they are now wired through
  `CodecParameters::options` on both the decoder and encoder factories so
  a YM2608/OPNA stream or an MSM6258 low-nibble-first stream resolves
  correctly via the registry. The encoders seed their analysis state with
  the matching chip/order so emitted bytes decode bit-exactly under the
  same option. Unknown values, and either option on a variant that has no
  such selection, are rejected. New `encode_round_trip` tests: OPNA
  round-trip, AICA-vs-OPNA divergence proof, LoFirst round-trip, and an
  option-rejection matrix.
- IMA-WAV (`adpcm_ima_wav`, tag `0x0011`) 4-bit multichannel encode fix +
  end-to-end coverage — the 4-bit encoder's default block size was a fixed
  256 bytes, which only satisfies the 4-byte-group-per-channel framing
  constraint for channel counts that divide `256 - 4*channels`. For
  layouts like 5.1 (6ch: `256 - 24 = 232`, not a multiple of 24) the
  encoder errored at the first `flush`. The default block size is now
  channel-aware (`default_block_size_4bit`, rounding the body down to a
  whole number of per-channel groups), so the 1..=8 channel range the
  decoder and `ima_encode_block` already supported is reachable through
  the trait/factory path. Mono/stereo defaults stay at 256 bytes
  (unchanged fixtures/bounds). New tests: 4.0 + 5.1 registry round-trips
  with per-lane RMS bounds, a direct block-API six-lane assignment check,
  a factory 6ch send/flush drain, and an invariant test pinning the
  default block size valid for every channel count.
- IMA-QT (`adpcm_ima_qt`, QuickTime `ima4`) multichannel block interleave
  — the decoder, encoder and factory now accept 1..=8 channels (mono /
  stereo / 4.0 / 5.1 / 7.1) instead of the previous mono/stereo cap. The
  QuickTime layout is one independent 34-byte block per channel,
  round-robin, each with its own preamble + predictor/step state, so the
  extra channels require no new framing — only the channel-count guards
  were lifted. `Variant::ImaQt::max_channels()` now reports `Some(8)`
  (was 2); `ima_qt::QT_MAX_CHANNELS` exposes the cap. New tests cover a
  6-channel decode lane-assignment check and a 6-channel encode→decode
  round-trip (per-lane RMS bounded).
- IMA-QT (`adpcm_ima_qt`, QuickTime `ima4`) end-to-end validator
  coverage — a new integration test decodes a CAF-carried `ima4` sine
  (raw blocks pulled from the CAF `data` chunk) and cross-correlates the
  result against the oracle's own PCM decode (> 0.98), closing the last
  decoder variant that had only hand-block unit coverage. The oracle is
  used purely as an opaque byte source.
- Multi-block packet decode for MS / IMA-WAV (4-bit + 3-bit) — a packet
  carrying several concatenated blocks (whole WAV `data` chunk, AVI audio
  chunk, large read buffer) is now split into its constituent blocks via
  the new `block_align` decode option (WAV `nBlockAlign`); each block
  re-seeds its predictor from its own header. Without the option a packet
  is decoded as a single block (back-compatible). Previously only the
  first block of such a packet was decoded.
- Dialogic/OKI VOX §5 reset preamble (`dialogic::reset_preamble`) — the
  spec-mandated 24-byte / 48-sample alternating ±zero-code sequence that
  resets a stream to its initial conditions (step floor, no DC offset)

## [0.0.6](https://github.com/OxideAV/oxideav-adpcm/compare/v0.0.5...v0.0.6) - 2026-06-15

### Other

- MS-ADPCM custom predictor coefficient sets (wNumCoef / aCoeff[])
- route WAVE_FORMAT_OKI_ADPCM (0x0010) to the Dialogic/OKI decoder
- clarify MS-ADPCM delta seed is shared across the predictor search
- MS-ADPCM encoder per-block predictor coefficient search
- add ADPCM-B chip-multiplier selection (AICA default / YM2608 OPNA)
- 3-bit IMA/DVI ADPCM (WAV tag 0x0011, wBitsPerSample=3) decode + encode
- Variant::block_size_bytes — inverse of samples_per_block (nBlockAlign sizing)
- typed header_bytes + samples_per_block accessors on Variant
- typed Shape + max_channels accessors on Variant
- drop release-plz.toml — use release-plz defaults across the workspace
- mean-|Δ| step seeding for MS / IMA-WAV block-oriented encoders
- typed Variant accessor surface (codec_id / wave_format_tag / fourcc / all)
- encoder fuzz / never-panic harness + 2 latent encoder panics fixed
- cargo-fuzz harness — 4 libfuzzer targets for coverage-guided decode exploration
- criterion bench harness for the per-block / per-packet decode hot path
- add Yamaha ADPCM-A (YM2608 rhythm / YM2610 ADPCM-A channel)
- decoder fuzz coverage + MS-ADPCM overflow fix

### Changed

- **MS-ADPCM encoder: per-block predictor coefficient search.** The
  encoder previously hard-wrote predictor index 0 (`coef1=256, coef2=0`,
  plain first-order delta) into every block. It now trial-encodes each
  block under all seven spec predictor coefficient pairs (`AdaptCoeff1` /
  `AdaptCoeff2` rows 0..=6) and writes the index that minimises total
  absolute reconstruction error into the per-channel header byte. Because
  the chosen index travels in the block header, the decode is byte-for-
  byte unaffected for any decoder — this is a pure encoder quality gain
  with no wire-format change. On the reference 22.05 kHz 440 Hz
  amplitude-12000 sine (one 256-byte block) single-block round-trip RMS
  drops from ~100 (index-0 only) to ~14 (an 86% reduction); a clean tone
  is modelled far better by the second-order pair than by first-order
  delta, while transient blocks fall back to index 0 automatically.
  Derived from the MS-ADPCM decode recurrence already in `crate::ms`; no
  external encoder consulted.

### Added

- **MS-ADPCM custom predictor coefficient sets (`wNumCoef` / `aCoeff[]`).**
  The Microsoft ADPCM `WAVEFORMATEX` trailer (`ADPCMWAVEFORMAT`) declares
  the predictor coefficient table, and a block's `bPredictor` byte indexes
  into it. The decoder previously hard-coded the seven standard presets and
  rejected any index ≥ 7; it now parses the trailer from
  `CodecParameters::extradata` (`wSamplesPerBlock` + `wNumCoef` +
  `wNumCoef` × two i16-LE coefficients) and decodes blocks that address
  custom coefficient sets. An empty trailer keeps the seven presets; a
  trailer declaring fewer than seven sets, truncating the table, or
  altering a mandatory preset is rejected at decoder construction. New
  public surface: `ms::decode_block_with_coeffs`,
  `ms::parse_extradata_coeffs`, `ms::STANDARD_COEFFS`, `ms::CoefPair`.
  Derived from the Microsoft ADPCM `ADPCMWAVEFORMAT` spec; no external
  decoder consulted.
- **OKI ADPCM WAV-tag routing (`WAVE_FORMAT_OKI_ADPCM` = `0x0010`).**
  The `adpcm_dialogic` registration now also claims wave-format tag
  `0x0010` and `Variant::Dialogic.wave_format_tag()` returns
  `Some(0x0010)`. The OKI MSM6258/6585/6295 chip-set algorithm (the
  `.vox` codec) has a WAV-container framing under this tag whose 4-bit
  body is the canonical VOX layout (two samples per byte, high nibble
  first), so a WAV demuxer that has parsed `WAVEFORMATEX::wFormatTag =
  0x0010` resolves to this decoder by tag and decodes byte-identically to
  the headerless `.vox` path. A new `tests/oki_wav_tag.rs` integration
  suite pins the registry tag resolution and the byte-for-byte agreement
  with the typed `dialogic::decode_packet` path; a new lib test
  (`registry_resolves_each_wave_format_tag_to_its_variant`) pins every
  accessor tag against the actual `register_codecs` wiring so the two
  surfaces can't drift. Tag + framing sourced from the *OKI ADPCM Wave
  Types* entry in the archived WAVE-format enumeration
  (`docs/audio/adpcm/sdl_sound-wave-types.html`); the 4-bit recurrence is
  the already-implemented Dialogic app-note algorithm. The 3-bit WAV-OKI
  mode the same table advertises is left unimplemented (no normative
  3-bit OKI recurrence is staged).

- **Yamaha ADPCM-B chip-multiplier selection (AICA vs YM2608 OPNA).**
  The `adpcm_yamaha` family covers chips that round the
  quantization-width change rate `f(L3,L2,L1)` differently. The crate
  previously hard-wired the AICA / Y8950 rounding (`integer/256`,
  update `>> 8`); it now also exposes the **YM2608 (OPNA) Application
  Manual Table 5-1** rounding (`{57,77,102,128,153}/64`, update `>> 6`).
  New surface:
  * `yamaha::Chip` enum (`Aica` default / `Opna`) and
    `yamaha::Channel::for_chip` constructor; `Channel` carries a `chip`
    field so `decode_nibble` / `decode_packet` / `encode_packet` apply
    the right step-update constants per channel.
  * `tables::YAMAHA_INDEX_SCALE_OPNA` — the Table 5-1 ×64 numerators.
  The registry-resolved `adpcm_yamaha` decoder/encoder keeps the AICA
  default (the WAV-tag-`0x0020` convention); the OPNA constants are
  reached by constructing channel state with `Channel::for_chip`.
  Source: `docs/audio/adpcm/yamaha/yamaha-adpcm.md` §1
  (`ym2608-opna-application-manual.pdf` Table 5-1 +
  `aica-fq8005-sound-block-manual.pdf` Table 2).

- **3-bit IMA / DVI ADPCM (WAV tag `0x0011`, `wBitsPerSample = 3`).**
  The DVI ADPCM wave type defines two code widths; the crate previously
  implemented only the 4-bit mode. The 3-bit mode shares the 4-byte
  per-channel block header and the 89-entry step table but uses a
  1-sign + 2-magnitude code (`diff = step/4 + (c&1 ? step/2 : 0) +
  (c&2 ? step : 0)`), the 8-entry `tables::IMA3_INDEX_ADJUST` table
  (`{-1, -1, 1, 2}`, sign-mirrored), and a body that interleaves
  channels in 12-byte groups (three 32-bit words = 32 codes per
  channel — the smallest whole-code unit), packed low-bits-first into
  the little-endian 96-bit group value. New surface:
  * `ima_wav::ima_expand_code3` + `ima_wav::decode_block_3bit` (+
    `GROUP_BYTES_3BIT` / `GROUP_SAMPLES_3BIT` framing constants) —
    decode; emits `1 + groups * 32` samples per channel.
  * `encoder::ima_encode_block_3bit` — decoder-loop-search encode over
    the 8 candidate codes, with the mean-|Δ| step-index seed retuned
    for the 3-bit candidate ladder (`target_step ≈ mean|Δ| × 4/3`).
  * `ImaWavEncoder::set_bits_per_sample(3 | 4)` — selects the code
    width and re-derives a framing-valid default block size.
  * Registry path: a `bits_per_sample` codec option (`"3"` / `"4"`) on
    `CodecParameters::options` for both `make_decoder` and
    `make_encoder`; unset keeps the 4-bit default, and out-of-spec
    widths (or a 3-bit request on any fixed-width variant) are
    rejected with `Error::Unsupported`.
  * 12 new integration tests (`tests/ima_wav_3bit.rs`): mono + stereo
    round-trip RMS bounds, the emitted-sample-count formula across
    1–8 channels, registry option accept/reject, truncation sweep,
    random-byte + adversarial-PCM never-panic passes; plus per-code
    unit tests (sign mirror, index saturation, predictor clamp,
    low-bits-first extraction order).

- **`Variant::block_size_bytes()` typed accessor — inverse of
  `samples_per_block()`.** Given a desired per-channel sample count it
  returns the block byte size (`nBlockAlign`) whose block decodes to
  exactly that many samples per channel, so a muxer can choose a block
  size for a target `nSamplesPerBlock` without re-deriving the framing
  formula:
  * `Variant::block_size_bytes(channels: u16, samples_per_channel:
    usize) -> Option<usize>` — `Some(7 * ch + ((n - 2) * ch) / 2)` for
    MS (header emits the first 2 samples; body adds 2 per byte per
    channel), `Some(4 * ch + groups * 4 * ch)` with
    `groups = (n - 1) / 8` for IMA-WAV (header predictor seeds 1
    sample; 8 per channel per 4·ch-byte group), and the fixed
    `34 * ch` for IMA-QT (the `ima4` block decodes a fixed 64 samples
    per channel). `None` for the three stream-oriented variants, zero /
    over-cap channels, sample counts below the header-only minimum, and
    off-boundary counts that don't land on a whole-block edge (MS:
    `(n - 2) * ch` must be even; IMA-WAV: `(n - 1)` a multiple of 8;
    IMA-QT: `n` must equal 64). The accessor is `const` and exactly
    inverts `samples_per_block` — two new tests pin the round-trip
    (`block_size_bytes` → `samples_per_block` → same `n`, and the
    derived size through the per-block decoder → same decoded length)
    across mono + stereo, plus a rejection-path enumeration.

- **`Variant::header_bytes()` + `Variant::samples_per_block()` typed
  accessors.** Extends the typed `Variant` inspection surface with two
  more spec-derived primitives so container and pipeline layers can
  size block buffers without round-tripping through a probe-decode
  call.
  * `Variant::header_bytes(channels: u16) -> Option<usize>` —
    `Some(7 * ch)` for MS (per-channel predictor index byte +
    signed-i16 initial delta + two signed-i16 history samples),
    `Some(4 * ch)` for IMA-WAV (per-channel signed-i16 predictor +
    u8 step index + reserved byte), `Some(2 * ch)` for IMA-QT
    (per-channel big-endian preamble: 9-bit predictor + 7-bit step
    index). `None` for the three stream-oriented variants (Yamaha-B /
    Yamaha-A / Dialogic VOX — no per-block header) and for zero
    channels.
  * `Variant::samples_per_block(channels: u16, block_bytes: usize)
    -> Option<usize>` — the per-channel sample count one block of
    `block_bytes` produces, using each variant's spec-derived
    formula: MS → `2 + (body_bytes * 2) / channels` after subtracting
    the `7 * channels` header; IMA-WAV → `1 + groups * 8` with
    `groups = body_bytes / (4 * channels)`; IMA-QT → always 64 (the
    `34 * channels` block layout is fixed). Returns `None` for
    stream-oriented variants, zero / over-cap channels, blocks
    shorter than the per-channel header, and bodies that don't match
    the variant's per-channel / per-group / fixed-size framing
    constraint. The accessor is `const` and exactly mirrors what the
    per-block decoders (`ms::decode_block`, `ima_wav::decode_block`,
    `ima_qt::decode_block`) parse — three new tests pin
    bit-for-bit agreement against the actual decoded sample counts
    across mono + stereo at minimum / single-group / multi-group
    block sizes, plus a separate test enumerates every rejection
    path (stream variants, zero / over-cap channels, short blocks,
    body-misalignment, off-spec QT block sizes).

- **`Variant::shape()` + `Variant::max_channels()` typed accessors.**
  Extends the existing typed `Variant` surface (`codec_id()` /
  `from_codec_id()` / `wave_format_tag()` / `fourcc()` / `all()`)
  with two more inspection points so container layers and
  configuration UIs can branch on framing shape and channel-count
  ceiling without re-typing the dispatch ladder in `make_decoder`:
  * `Variant::shape() -> Shape` — `Shape::BlockOriented` for the
    three WAV / AVI / QuickTime variants (MS, IMA-WAV, IMA-QT —
    per-block header re-seeds predictor + step pointer; decoder is
    memoryless across blocks and `Decoder::reset` does not need to
    clear per-channel state), `Shape::StreamOriented` for the three
    chip-stream variants (Yamaha-B / DELTA-T, Yamaha-A, Dialogic
    VOX — no block framing, predictor and step pointer carry across
    packet boundaries indefinitely so `Decoder::reset` must clear
    per-channel state). The `Shape` enum is re-exported at the
    crate root alongside `Variant`.
  * `Variant::max_channels() -> Option<u16>` — `Some(2)` for MS /
    IMA-QT / Dialogic, `Some(8)` for IMA-WAV (matches the
    WAVEFORMATEX 8-channel speaker ceiling the `make_decoder`
    factory already enforces), `Some(1)` for Yamaha-A
    (YM2608/YM2610 rhythm channels are individually single-channel
    streams), `None` for Yamaha-B (sample-level channel round-robin
    over a contiguous nibble stream — no upper bound). The accessor
    is the typed counterpart of the scattered `if channels > N`
    branches in `decoder::make_decoder` so future channel-count
    changes have to update both surfaces in lockstep.
  Three new lib-side tests pin the partition (`variant_shape_*`:
  3 + 3 partition fails loudly if a new variant lands without being
  classified), the factory-accept boundary
  (`variant_max_channels_matches_factory_accept_reject`: `max` ok,
  `max + 1` `Err`, unbounded variants accept 16 channels), and the
  zero-channel reject (`variant_max_channels_rejects_zero_for_every_variant`:
  every variant rejects 0 channels regardless of upper bound).

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
  the existing oracle round-trip tests); hostile inputs now
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
