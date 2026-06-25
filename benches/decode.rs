//! Criterion benchmarks for the oxideav-adpcm decoder hot paths.
//!
//! Round 216 (depth-mode benchmarks): per the workspace
//! "saturated → fuzz/bench/profile" memo, all six ADPCM variants
//! (`adpcm_ms`, `adpcm_ima_wav`, `adpcm_ima_qt`, `adpcm_yamaha`,
//! `adpcm_yamaha_a`, `adpcm_dialogic`) have shipped feature-complete
//! decoder + encoder pairs. This file wires `criterion` so future
//! optimisation rounds (block-aligned SIMD, per-sample LUT, no-bounds-
//! check inner loops, predictor-fold rewrites) can A/B-test their
//! tweaks against a stable, deterministic baseline.
//!
//! Each scenario is self-contained — every byte input is synthesised
//! in-bench:
//!
//!   - **block-oriented variants** (MS / IMA-WAV / IMA-QT) use the
//!     crate's public encoder once at setup time to produce a buffer
//!     of valid blocks; the bench then times only the per-block
//!     decode loop. This is the realistic shape a player pays — the
//!     encoder cost is amortised over many decode passes.
//!   - **stream-oriented variants** (Yamaha-B / Yamaha-A / Dialogic
//!     VOX) feed a deterministic xorshift32 byte stream straight into
//!     `decode_packet` — the streaming codecs accept every byte
//!     pattern (no header validation), so synthetic bytes faithfully
//!     model the silicon hot path.
//!
//! No `docs/` fixtures or external files are read. Run with:
//!
//!     cargo bench -p oxideav-adpcm --bench decode

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use oxideav_adpcm::{dialogic, encoder as adpcm_encoder, ima_qt, ima_wav, ms, yamaha, yamaha_a};

/// Deterministic xorshift32 — synthesises evenly-distributed sample
/// values so per-block encoders exercise every adaptation rung and the
/// stream-oriented decoders touch every nibble bucket.
fn xorshift32(state: &mut u32) -> u32 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    *state
}

/// Build `n` pseudo-random i16 samples that look like reasonable PCM
/// audio (modest amplitude so the WAV encoders don't saturate on every
/// nibble). Output amplitude is clamped to roughly ±12_000 LSB which
/// keeps the encoder's adaptation curves in the spec-typical range.
fn build_pcm(n: usize, seed: u32) -> Vec<i16> {
    let mut state = seed;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        // Scale a u32 down to ~±12_000.
        let r = xorshift32(&mut state) as i32;
        let s = ((r % 24_001) - 12_000) as i16;
        out.push(s);
    }
    out
}

/// Build `n` pseudo-random nibble-carrier bytes for the streaming
/// decoders. xorshift32 covers all 256 values roughly uniformly so the
/// 4-bit nibble unpack visits every magnitude code, including the
/// step-pointer boundary cases.
fn build_bytes(n: usize, seed: u32) -> Vec<u8> {
    let mut state = seed;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push((xorshift32(&mut state) & 0xFF) as u8);
    }
    out
}

// ----------------------------------------------------------------------
// MS-ADPCM (WAV tag 0x0002) — block-oriented.
// ----------------------------------------------------------------------

fn bench_decode_ms_mono_256b_blocks_1s(c: &mut Criterion) {
    // ~1 second at 22050 Hz with the crate's default 256-byte block
    // emits 22050 / ((256 - 7) * 2 + 2) ≈ 44 blocks → 11_264 bytes of
    // encoded bitstream → 22_050 decoded i16 samples.
    let block_size = 256usize;
    let body_samples_per_block = (block_size - 7) * 2 + 2;
    let blocks = 22_050usize.div_ceil(body_samples_per_block);
    let pcm = build_pcm(blocks * body_samples_per_block, 0xCAFE_F00D);
    let mut encoded = Vec::with_capacity(blocks * block_size);
    for chunk in pcm.chunks_exact(body_samples_per_block) {
        let blk = adpcm_encoder::encode_block(chunk, 1, block_size)
            .expect("adpcm_ms encode_block must succeed on full chunks");
        encoded.extend_from_slice(&blk);
    }

    let mut g = c.benchmark_group("decode_ms_mono_256b_blocks_1s");
    g.throughput(Throughput::Bytes(encoded.len() as u64));
    g.bench_function(
        BenchmarkId::from_parameter("ms/mono/256B/22050Hz/1s"),
        |b| {
            b.iter(|| {
                let src = criterion::black_box(&encoded);
                let mut acc: i64 = 0;
                for chunk in src.chunks_exact(block_size) {
                    let pcm = ms::decode_block(chunk, 1).expect("decode_block");
                    for s in pcm {
                        acc = acc.wrapping_add(s as i64);
                    }
                }
                criterion::black_box(acc)
            });
        },
    );
    g.finish();
}

fn bench_decode_ms_stereo_512b_blocks_500ms(c: &mut Criterion) {
    // 512-byte stereo blocks — header is 14 bytes, body 498 bytes;
    // samples/channel = 2 + (498 * 2)/2 = 500. ~0.5 s at 22050 Hz
    // wants 11_025 samples/channel → 23 blocks.
    let block_size = 512usize;
    let body = block_size - 14;
    let samples_per_channel = 2 + body; // (body * 2)/2 = body
    let blocks = 23usize;
    let total_samples = blocks * samples_per_channel * 2;
    let pcm = build_pcm(total_samples, 0xBEEF_BABE);
    let mut encoded = Vec::with_capacity(blocks * block_size);
    for chunk in pcm.chunks_exact(samples_per_channel * 2) {
        let blk = adpcm_encoder::encode_block(chunk, 2, block_size)
            .expect("adpcm_ms stereo encode_block must succeed");
        encoded.extend_from_slice(&blk);
    }

    let mut g = c.benchmark_group("decode_ms_stereo_512b_blocks_500ms");
    g.throughput(Throughput::Bytes(encoded.len() as u64));
    g.bench_function(
        BenchmarkId::from_parameter("ms/stereo/512B/22050Hz/500ms"),
        |b| {
            b.iter(|| {
                let src = criterion::black_box(&encoded);
                let mut acc: i64 = 0;
                for chunk in src.chunks_exact(block_size) {
                    let pcm = ms::decode_block(chunk, 2).expect("decode_block");
                    for s in pcm {
                        acc = acc.wrapping_add(s as i64);
                    }
                }
                criterion::black_box(acc)
            });
        },
    );
    g.finish();
}

// ----------------------------------------------------------------------
// IMA-ADPCM-WAV (WAV tag 0x0011) — block-oriented.
// ----------------------------------------------------------------------

fn bench_decode_ima_wav_mono_256b_blocks_1s(c: &mut Criterion) {
    // 256-byte mono block: header 4 bytes, body 252 bytes = 63 groups
    // of 4 bytes → 1 + 63·8 = 505 samples/block. 22_050 samples / 505
    // ≈ 44 blocks.
    let block_size = 256usize;
    let body = block_size - 4;
    let groups = body / 4;
    let samples_per_block = 1 + groups * 8;
    let blocks = 22_050usize.div_ceil(samples_per_block);
    let pcm = build_pcm(blocks * samples_per_block, 0xDEAD_BEEF);
    let mut encoded = Vec::with_capacity(blocks * block_size);
    for chunk in pcm.chunks_exact(samples_per_block) {
        let blk = adpcm_encoder::ima_encode_block(chunk, 1, block_size)
            .expect("adpcm_ima_wav encode_block must succeed");
        encoded.extend_from_slice(&blk);
    }

    let mut g = c.benchmark_group("decode_ima_wav_mono_256b_blocks_1s");
    g.throughput(Throughput::Bytes(encoded.len() as u64));
    g.bench_function(
        BenchmarkId::from_parameter("ima_wav/mono/256B/22050Hz/1s"),
        |b| {
            b.iter(|| {
                let src = criterion::black_box(&encoded);
                let mut acc: i64 = 0;
                for chunk in src.chunks_exact(block_size) {
                    let pcm = ima_wav::decode_block(chunk, 1).expect("decode_block");
                    for s in pcm {
                        acc = acc.wrapping_add(s as i64);
                    }
                }
                criterion::black_box(acc)
            });
        },
    );
    g.finish();
}

fn bench_decode_ima_wav_stereo_512b_blocks_500ms(c: &mut Criterion) {
    // 512-byte stereo block: header 8 bytes (4 per channel), body 504
    // bytes = 63 groups of 8 → 1 + 63·8 = 505 samples/channel.
    let block_size = 512usize;
    let body = block_size - 8;
    let groups = body / 8;
    let samples_per_channel = 1 + groups * 8;
    let blocks = 22usize;
    let total_samples = blocks * samples_per_channel * 2;
    let pcm = build_pcm(total_samples, 0xFEED_FACE);
    let mut encoded = Vec::with_capacity(blocks * block_size);
    for chunk in pcm.chunks_exact(samples_per_channel * 2) {
        let blk = adpcm_encoder::ima_encode_block(chunk, 2, block_size)
            .expect("adpcm_ima_wav stereo encode_block must succeed");
        encoded.extend_from_slice(&blk);
    }

    let mut g = c.benchmark_group("decode_ima_wav_stereo_512b_blocks_500ms");
    g.throughput(Throughput::Bytes(encoded.len() as u64));
    g.bench_function(
        BenchmarkId::from_parameter("ima_wav/stereo/512B/22050Hz/500ms"),
        |b| {
            b.iter(|| {
                let src = criterion::black_box(&encoded);
                let mut acc: i64 = 0;
                for chunk in src.chunks_exact(block_size) {
                    let pcm = ima_wav::decode_block(chunk, 2).expect("decode_block");
                    for s in pcm {
                        acc = acc.wrapping_add(s as i64);
                    }
                }
                criterion::black_box(acc)
            });
        },
    );
    g.finish();
}

// ----------------------------------------------------------------------
// IMA-ADPCM-QT (QuickTime fourcc `ima4`) — fixed 34-byte block.
// ----------------------------------------------------------------------

fn bench_decode_ima_qt_mono_500ms(c: &mut Criterion) {
    // QT-IMA block is fixed at 34 bytes / 64 samples per channel. At
    // 22_050 Hz mono, 500 ms → 11_025 samples → ceil(11_025 / 64) = 173
    // blocks → 5_882 encoded bytes.
    let blocks = 173usize;
    let samples_per_block = ima_qt::QT_SAMPLES_PER_BLOCK; // 64
    let pcm = build_pcm(blocks * samples_per_block, 0x1234_5678);
    let mut encoded = Vec::with_capacity(blocks * ima_qt::QT_BLOCK_SIZE);
    for chunk in pcm.chunks_exact(samples_per_block) {
        let blk = adpcm_encoder::ima_qt_encode_block(chunk, 1)
            .expect("adpcm_ima_qt encode_block must succeed");
        encoded.extend_from_slice(&blk);
    }

    let mut g = c.benchmark_group("decode_ima_qt_mono_500ms");
    g.throughput(Throughput::Bytes(encoded.len() as u64));
    g.bench_function(
        BenchmarkId::from_parameter("ima_qt/mono/34B/22050Hz/500ms"),
        |b| {
            b.iter(|| {
                let src = criterion::black_box(&encoded);
                let mut acc: i64 = 0;
                for chunk in src.chunks_exact(ima_qt::QT_BLOCK_SIZE) {
                    let pcm = ima_qt::decode_block(chunk, 1).expect("decode_block");
                    for s in pcm {
                        acc = acc.wrapping_add(s as i64);
                    }
                }
                criterion::black_box(acc)
            });
        },
    );
    g.finish();
}

fn bench_decode_ima_qt_stereo_500ms(c: &mut Criterion) {
    // Stereo packet = 68 bytes (two consecutive 34-byte blocks).
    let blocks = 173usize;
    let pkt_bytes = ima_qt::QT_BLOCK_SIZE * 2; // 68
    let samples_per_packet = ima_qt::QT_SAMPLES_PER_BLOCK * 2; // 128 (interleaved)
    let pcm = build_pcm(blocks * samples_per_packet, 0xABAD_1DEA);
    let mut encoded = Vec::with_capacity(blocks * pkt_bytes);
    for chunk in pcm.chunks_exact(samples_per_packet) {
        let blk = adpcm_encoder::ima_qt_encode_block(chunk, 2)
            .expect("adpcm_ima_qt stereo encode_block must succeed");
        encoded.extend_from_slice(&blk);
    }

    let mut g = c.benchmark_group("decode_ima_qt_stereo_500ms");
    g.throughput(Throughput::Bytes(encoded.len() as u64));
    g.bench_function(
        BenchmarkId::from_parameter("ima_qt/stereo/68B/22050Hz/500ms"),
        |b| {
            b.iter(|| {
                let src = criterion::black_box(&encoded);
                let mut acc: i64 = 0;
                for chunk in src.chunks_exact(pkt_bytes) {
                    let pcm = ima_qt::decode_block(chunk, 2).expect("decode_block");
                    for s in pcm {
                        acc = acc.wrapping_add(s as i64);
                    }
                }
                criterion::black_box(acc)
            });
        },
    );
    g.finish();
}

// ----------------------------------------------------------------------
// Yamaha ADPCM-B / DELTA-T (WAV tag 0x0020) — stream-oriented.
// ----------------------------------------------------------------------

fn bench_decode_yamaha_b_mono_1s(c: &mut Criterion) {
    // 8 kHz mono, 1 s of ADPCM-B nibbles. Each byte = 2 samples, so
    // 4 000 bytes → 8 000 samples.
    let bytes = build_bytes(4_000, 0xBADC_0FFE);
    let mut g = c.benchmark_group("decode_yamaha_b_mono_1s");
    g.throughput(Throughput::Bytes(bytes.len() as u64));
    g.bench_function(BenchmarkId::from_parameter("yamaha_b/mono/8kHz/1s"), |b| {
        b.iter(|| {
            let mut state = [yamaha::Channel::default()];
            let src = criterion::black_box(&bytes);
            let pcm = yamaha::decode_packet(src, &mut state);
            criterion::black_box(pcm)
        });
    });
    g.finish();
}

fn bench_decode_yamaha_b_stereo_1s(c: &mut Criterion) {
    // 8 kHz stereo, 1 s. Sample-interleaved at nibble level so the
    // packet is 8 000 bytes → 16 000 samples (interleaved).
    let bytes = build_bytes(8_000, 0xC0FE_BABE);
    let mut g = c.benchmark_group("decode_yamaha_b_stereo_1s");
    g.throughput(Throughput::Bytes(bytes.len() as u64));
    g.bench_function(
        BenchmarkId::from_parameter("yamaha_b/stereo/8kHz/1s"),
        |b| {
            b.iter(|| {
                let mut state = [yamaha::Channel::default(), yamaha::Channel::default()];
                let src = criterion::black_box(&bytes);
                let pcm = yamaha::decode_packet(src, &mut state);
                criterion::black_box(pcm)
            });
        },
    );
    g.finish();
}

// ----------------------------------------------------------------------
// Yamaha ADPCM-A — 12-bit silicon, mono only.
// ----------------------------------------------------------------------

fn bench_decode_yamaha_a_mono_1s_wide16(c: &mut Criterion) {
    // YM2608 / YM2610 rhythm channels — 1 s of 8 kHz nibbles = 4 000
    // bytes. `Wide16` widens the 12-bit native predictor to i16 so the
    // result drops straight into a PCM pipeline.
    let bytes = build_bytes(4_000, 0x5EED_C0DE);
    let mut g = c.benchmark_group("decode_yamaha_a_mono_1s_wide16");
    g.throughput(Throughput::Bytes(bytes.len() as u64));
    g.bench_function(
        BenchmarkId::from_parameter("yamaha_a/mono/8kHz/1s/wide16"),
        |b| {
            b.iter(|| {
                let mut state = [yamaha_a::Channel::default()];
                let src = criterion::black_box(&bytes);
                let pcm = yamaha_a::decode_packet(src, &mut state, yamaha_a::Output::Wide16);
                criterion::black_box(pcm)
            });
        },
    );
    g.finish();
}

// ----------------------------------------------------------------------
// OKI / Dialogic VOX — headerless stream, mono.
// ----------------------------------------------------------------------

fn bench_decode_dialogic_mono_1s_hifirst_wide16(c: &mut Criterion) {
    // 8 kHz mono VOX, 1 s = 4 000 bytes. Default `.vox` nibble order
    // is HiFirst (Dialogic / MSM6295).
    let bytes = build_bytes(4_000, 0x1A2B_3C4D);
    let mut g = c.benchmark_group("decode_dialogic_mono_1s_hifirst_wide16");
    g.throughput(Throughput::Bytes(bytes.len() as u64));
    g.bench_function(
        BenchmarkId::from_parameter("dialogic/mono/8kHz/1s/hifirst/wide16"),
        |b| {
            b.iter(|| {
                let mut state = [dialogic::Channel::default()];
                let src = criterion::black_box(&bytes);
                let pcm = dialogic::decode_packet(
                    src,
                    &mut state,
                    dialogic::NibbleOrder::HiFirst,
                    dialogic::Output::Wide16,
                );
                criterion::black_box(pcm)
            });
        },
    );
    g.finish();
}

fn bench_decode_dialogic_mono_1s_lofirst_native12(c: &mut Criterion) {
    // Same packet, LoFirst nibble order (MSM6258) + Native12 output —
    // exercises the alternate dispatch arms in both `decode_nibble`
    // and `decode_packet`.
    let bytes = build_bytes(4_000, 0x7E7E_F00D);
    let mut g = c.benchmark_group("decode_dialogic_mono_1s_lofirst_native12");
    g.throughput(Throughput::Bytes(bytes.len() as u64));
    g.bench_function(
        BenchmarkId::from_parameter("dialogic/mono/8kHz/1s/lofirst/native12"),
        |b| {
            b.iter(|| {
                let mut state = [dialogic::Channel::default()];
                let src = criterion::black_box(&bytes);
                let pcm = dialogic::decode_packet(
                    src,
                    &mut state,
                    dialogic::NibbleOrder::LoFirst,
                    dialogic::Output::Native12,
                );
                criterion::black_box(pcm)
            });
        },
    );
    g.finish();
}

fn bench_decode_dialogic_stereo_1s_hifirst_wide16(c: &mut Criterion) {
    // 8 kHz stereo VOX, 1 s. Nibbles round-robin across the two channels
    // (nibble 0 -> L, nibble 1 -> R, …), so the same 4 000-byte packet
    // decodes 4 000 samples per channel. Exercises the multi-channel
    // cursor-advance arm of `decode_packet`.
    let bytes = build_bytes(4_000, 0x5715_2A0E);
    let mut g = c.benchmark_group("decode_dialogic_stereo_1s_hifirst_wide16");
    g.throughput(Throughput::Bytes(bytes.len() as u64));
    g.bench_function(
        BenchmarkId::from_parameter("dialogic/stereo/8kHz/1s/hifirst/wide16"),
        |b| {
            b.iter(|| {
                let mut state = [dialogic::Channel::default(), dialogic::Channel::default()];
                let src = criterion::black_box(&bytes);
                let pcm = dialogic::decode_packet(
                    src,
                    &mut state,
                    dialogic::NibbleOrder::HiFirst,
                    dialogic::Output::Wide16,
                );
                criterion::black_box(pcm)
            });
        },
    );
    g.finish();
}

criterion_group!(
    benches,
    bench_decode_ms_mono_256b_blocks_1s,
    bench_decode_ms_stereo_512b_blocks_500ms,
    bench_decode_ima_wav_mono_256b_blocks_1s,
    bench_decode_ima_wav_stereo_512b_blocks_500ms,
    bench_decode_ima_qt_mono_500ms,
    bench_decode_ima_qt_stereo_500ms,
    bench_decode_yamaha_b_mono_1s,
    bench_decode_yamaha_b_stereo_1s,
    bench_decode_yamaha_a_mono_1s_wide16,
    bench_decode_dialogic_mono_1s_hifirst_wide16,
    bench_decode_dialogic_mono_1s_lofirst_native12,
    bench_decode_dialogic_stereo_1s_hifirst_wide16,
);
criterion_main!(benches);
