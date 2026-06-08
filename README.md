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
250 LSB across the block-oriented encoders, and under 3000 LSB for the
stream-oriented encoders (where step state has to converge from cold
start).

Default block size is 256 bytes per channel for the MS and IMA-WAV
encoders (matches the default ffmpeg emits at 22050 Hz mono); override
via `MsEncoder::set_block_size` / `ImaWavEncoder::set_block_size`
before the first `send_frame` call. The IMA-QT encoder uses the
spec-mandated 34-byte-per-channel block — there is no `set_block_size`
because the on-wire layout is fixed.

To minimise the high-amplitude leading-edge transient inherent to
per-block re-seeding, all three block-oriented encoders pick their
initial step state from the mean absolute first-difference of the
first 16 samples in each block (rather than always cold-starting):

- **IMA-ADPCM-QT** / **IMA-ADPCM-WAV** — `target_step ≈ mean_|Δ| × 8 / 3`
  (a magnitude-4 nibble produces `diff = step/8 + step/4 = 0.375 × step`,
  so this seed places typical magnitudes near the midrange of the 16
  candidates), then pick the first IMA step-table entry ≥ that target.
- **MS-ADPCM** — with the default predictor index 0 (coef1=256, coef2=0)
  the decoder recurrence reduces to `sample1 + signed_nibble × delta`
  with signed_nibble ∈ -8..=7, so seeding `delta ≈ mean_|Δ| / 4` places
  typical magnitudes near the middle of the sweep. The seed is clamped
  to `[16, 16384]` to honour the spec minimum and avoid runaway.

On a 22.05 kHz 440 Hz amplitude-12000 sine these heuristics drop
round-trip RMS by **63–88%** versus the cold-start seeds (MS mono
271 → 100; MS stereo 207 → 86; IMA-WAV mono 413 → 88; IMA-WAV stereo
634 → 78).

## Typed variant accessor

For callers that already know which ADPCM variant they want — a fixture
loader pinning ADPCM-A; a WAV demuxer that has already parsed
`WAVEFORMATEX::wFormatTag`; a unit test addressing exactly one decoder
— the crate re-exports the dispatch enum at the crate root as
[`oxideav_adpcm::Variant`] together with a small inspection surface:

```rust
use oxideav_adpcm::{Variant, CODEC_ID_MS};
use oxideav_core::CodecId;

// Round-trip a codec id through the typed enum.
let v = Variant::from_codec_id(&CodecId::new(CODEC_ID_MS)).unwrap();
assert_eq!(v.codec_id(), CODEC_ID_MS);

// Container-layer tag inspection without re-typing the dispatch ladder.
assert_eq!(Variant::Ms.wave_format_tag(),     Some(0x0002));
assert_eq!(Variant::ImaWav.wave_format_tag(), Some(0x0011));
assert_eq!(Variant::Yamaha.wave_format_tag(), Some(0x0020));
assert_eq!(Variant::ImaQt.fourcc(),           Some(*b"ima4"));

// Iterate every supported variant — exhaustiveness audits, table-
// driven registration tests, configuration UIs.
for &v in Variant::all() {
    println!("{} ({:?})", v.codec_id(), v);
}
```

A unit-test in `src/lib.rs` pins bit-for-bit agreement between
`Variant::wave_format_tag()` / `Variant::fourcc()` and the tags that
`register_codecs` actually wires into the registry, so a future
ADPCM variant addition has to update both surfaces in lockstep.

The same surface also classifies the on-wire framing shape and the
channel-count ceiling each variant accepts:

```rust
use oxideav_adpcm::{Shape, Variant};

// Three block-oriented (WAV / AVI / QuickTime — per-block header re-seed)
// vs three stream-oriented (Yamaha-A/B + Dialogic VOX — predictor and
// step pointer carry across packets indefinitely) variants.
assert_eq!(Variant::Ms.shape(),       Shape::BlockOriented);
assert_eq!(Variant::ImaWav.shape(),   Shape::BlockOriented);
assert_eq!(Variant::ImaQt.shape(),    Shape::BlockOriented);
assert_eq!(Variant::Yamaha.shape(),   Shape::StreamOriented);
assert_eq!(Variant::YamahaA.shape(),  Shape::StreamOriented);
assert_eq!(Variant::Dialogic.shape(), Shape::StreamOriented);

// Maximum channel count the factory accepts. None == unbounded
// (Yamaha DELTA-T is sample-level round-robin).
assert_eq!(Variant::Ms.max_channels(),       Some(2));
assert_eq!(Variant::ImaWav.max_channels(),   Some(8));
assert_eq!(Variant::ImaQt.max_channels(),    Some(2));
assert_eq!(Variant::Yamaha.max_channels(),   None);
assert_eq!(Variant::YamahaA.max_channels(),  Some(1));
assert_eq!(Variant::Dialogic.max_channels(), Some(2));
```

Two more lib-side tests pin `Variant::shape()` against the
block-vs-stream partition (3 in each bucket — fails loudly if a new
variant lands without being slotted), and `Variant::max_channels()`
against what the registry's `make_decoder` actually accepts / rejects
at the boundary (`max` works, `max + 1` is `Err`; zero is `Err` for
every variant). The `Shape` enum is re-exported at the crate root so
container layers can branch on framing without round-tripping through
the `Variant` enum.

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
are bit-identical (the existing oracle round-trip tests still pass)
and hostile ones emit bounded samples instead of panicking.

Encoder-side robustness is exercised by a sibling
`tests/encoder_fuzz.rs` suite (17 deterministic never-panic tests
across all six variants — adversarial PCM, off-size sample counts,
out-of-spec encoder-state seeds, plus a registry-level pass covering
zero-length frames + random-byte streams through `send_frame` +
`flush`). That harness surfaced two latent panics carried over from
the original encoder shape:

- **MS-ADPCM encoder i32 overflow** in `ms_advance` /
  `ms_simulate_nibble`. The `MS_ADAPTATION[n] * delta` product (mirror
  of the decoder bug already fixed in the prior round) and the
  `sample1 * coef1 + sample2 * coef2` predictor sum could overflow
  i32 when the encoder's search loop drove `delta` into the millions
  on adversarial PCM. Both products now run in i64 with saturating
  multiplication and a final clamp back to i16 / i32 storage — the
  encoder's existing round-trip tests are bit-identical, and the new
  fuzz tests no longer panic.
- **Yamaha ADPCM-A index-out-of-bounds** in `decode_nibble` /
  `encode_sample`. A negative `step_index` field on a caller-supplied
  `Channel` (the fuzz harness threads adversarial state directly into
  this struct, mimicking long-stream resumption) was used as `usize`
  to index `YAMAHA_A_STEP_SIZE`, wrapping to a huge index and
  panicking. Both functions now clamp `step_index` + `acc` to their
  spec ranges on entry — identical to the post-update clamp the same
  function applies on the way out, so the recurrence is unaffected
  for any well-shaped stream.

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

## Coverage-guided fuzzing

In addition to the deterministic, hand-enumerated `tests/decoder_fuzz.rs`
suite (run on every `cargo test`), a [`cargo-fuzz`][cargo-fuzz]
harness under `fuzz/` exposes four libfuzzer targets so a long-running
fuzz job can do coverage-guided exploration of the per-variant decode
hot paths:

| Target | Entry point under test |
|--------|------------------------|
| `decode_packet_ms` | `oxideav_adpcm::ms::decode_block` |
| `decode_packet_ima_wav` | `oxideav_adpcm::ima_wav::decode_block` |
| `decode_packet_ima_qt` | `oxideav_adpcm::ima_qt::decode_block` |
| `decode_packet_stream` | Yamaha-A / Yamaha-B / Dialogic-VOX `decode_packet` (variant + state seed picked from the first 10 fuzz bytes) |
| `encode_packet_ms` | `oxideav_adpcm::encoder::encode_block` (PCM in, MS-ADPCM block out; block size derived from the first 3 fuzz bytes) |
| `encode_packet_ima_wav` | `oxideav_adpcm::encoder::ima_encode_block` (1..=8 channels; block size derived from the first 3 fuzz bytes) |
| `encode_packet_ima_qt` | `oxideav_adpcm::encoder::ima_qt_encode_block` (fixed 64-samples-per-channel block; exercises the mean-\|Δ\| step-index heuristic against adversarial PCM) |
| `encode_packet_stream` | Yamaha-A / Yamaha-B / Dialogic-VOX `encode_packet` (both nibble orders for Dialogic; both 12-bit widenings for ADPCM-A; state seed picked from the first 10 fuzz bytes) |

Each target's contract is the same as the in-tree fuzz tests' — every
byte slice must produce either `Ok(samples)` or `Err(Error::…)` (or, for
the stream encoders, a finite `Vec<u8>`); never panic, debug-overflow,
OOM, or index out of bounds. The stream-oriented targets additionally
seed out-of-spec predictor + step-index values so the input-clamp paths
fire on cold start.

Run an individual target with a nightly toolchain:

    cd crates/oxideav-adpcm/fuzz
    cargo +nightly fuzz run decode_packet_ms

[cargo-fuzz]: https://rust-fuzz.github.io/book/cargo-fuzz.html

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
