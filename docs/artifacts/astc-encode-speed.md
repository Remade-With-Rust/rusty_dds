# ASTC encode speed vs astcenc (equal quality)

## SIMD round (branch `perf/astc-encoder-simd`, 2026-11-22)

The encoder gained SSE2/AVX2 twins of its hot kernels plus two redundancy cuts, every one
byte-identical to 0.11.1 (encoded-byte identity over 160 encodes: Kodak 01 03 05 08 13 15 19 23 x
all four presets x 4x4/6x6/8x8/12x12 under Ldr, and Kodak 01 08 15 23 x Fast/Medium under LdrSrgb;
`bench/astc/rig/enc_ident.sh`). Same machine as below (i7-14650HX, WSL2), single thread.

### ES1 against astcenc 5.7.1, same session, both arms

`taskset -c 2 nice -n -5 astc_harness es1 <preset> 4x4,6x6,8x8,12x12` on Kodak 01, 03, 08, 15,
harness under rusty_alloc with the `simd` feature (earlier harness builds lacked it and timed the
scalar fallbacks). Method as in the next section: astcenc `-quality` bisected to our PSNR, 4 ABBA
rounds, minimum per arm, CPU clocks. Quality is unchanged by construction (the dB columns are the
same in both arms).

| Preset | Footprint | ours dB | 0.11.1 / none | branch / none | 0.11.1 / avx2 | branch / avx2 |
|---|---|---|---|---|---|---|
| Fast | 4x4 | 44.582 | 0.83 | 0.94 | 0.31 | 0.36 |
| Fast | 6x6 | 37.402 | 0.76 | 1.08 | 0.23 | 0.34 |
| Fast | 8x8 | 33.291 | 0.90 | 1.38 | 0.26 | 0.41 |
| Fast | 12x12 | 29.356 | 0.73 | 1.18 | 0.25 | 0.42 |
| Medium | 4x4 | 44.824 | 0.65 | 0.78 | 0.26 | 0.32 |
| Medium | 6x6 | 37.593 | 0.61 | 0.99 | 0.18 | 0.30 |
| Medium | 8x8 | 33.477 | 0.62 | 1.16 | 0.18 | 0.34 |
| Medium | 12x12 | 29.520 | 0.53 | 1.05 | 0.19 | 0.39 |

Medium 12x12, which sat exactly at the 0.5 ES1 floor, is now at 1.05 against `astcenc-none` (about
twice the floor) and 0.39 against `astcenc-avx2`. ES2 (at least 0.5 against `astcenc-avx2`) is
still not met: 0.30 to 0.42.

### Branch against 0.11.1 directly

`bench/astc` `--time-encode <preset> 4x4,6x6,8x8,12x12 1` on the same four images: whole processes
ABBA-alternated for 10 rounds, `taskset -c 2 nice -5`, thread CPU clock, rusty_alloc in every arm,
identical bench source. Ratio = 0.11.1 ms / branch ms (above 1: the branch is faster).

| Preset | 4x4 | 6x6 | 8x8 | 12x12 |
|---|---|---|---|---|
| Medium | 1.112 (10/10) | 1.469 (10/10) | 1.702 (10/10) | 1.932 (10/10) |
| Fast | 1.104 (9/10) | 1.390 (10/10) | 1.526 (10/10) | 1.619 (10/10) |
| Null arm, Medium (0.11.1 vs a byte copy) | 1.037 (8/10) | 1.072 (7/10) | 1.043 (7/10) | 0.987 (5/10) |

ISA rung, one binary (`tuning` build, `RUSTY_DDS_ASTC_NO_AVX2` turns the AVX2 and POPCNT twins off;
the SSE2 twins stay): 4x4 0.938 (2/10, inside the null; 4x4 does not use the AVX2 twins),
6x6 1.278 (8/10), 8x8 1.372 (9/10), 12x12 1.486 (10/10).

Instruction counts (callgrind, `bench/astc/rig/ir.sh`, kodim23 256x256 crop + ldr-rgba-00, four
footprints): enc-fastest -43.8%, enc-fast -46.5%, enc-medium -50.1% against 0.11.1 (a gather counts
as one instruction, so these overstate the AVX2 bricks; the wall ratios above are the speed claim).

## Earlier: scalar encoder (`feat/astc-perf` 687b7d3)

Encoder under test: `rusty_dds::astc::encode_rgba8` (scalar), branch `feat/astc-perf` at `687b7d3`
(Medium with the block-mode shortlist; the `feat/astc` numbers at `f0ed691` are kept below).
Reference: ARM astcenc 5.7.1, `astcenc-none` and `astcenc-avx2`, with `-j 1`. Machine:
Intel Core i7-14650HX, WSL2, single thread pinned with `taskset -c 2`. Date: 2026-11-21.

## Method (ES1, plan section 2.5)

- Images: Kodak 01, 03, 08 and 15 (1.57 Mpx in total).
- Equal quality: astcenc's float `-quality` is bisected until its mean PSNR is within 0.05 dB
  of ours at the preset under test. Both are then timed at that point.
- Clocks: our encoder uses the thread CPU clock. astcenc uses its child CPU time times its own
  coding share of wall time ("Coding time" over "Total time"), which leaves out PNG I/O. CPU
  clocks are used because the host was shared with other jobs and wall samples swung 4x
  between runs.
- Arms: 4 ABBA rounds with the order reversed every round; the minimum per arm is kept.
- Command: `astc_harness es1 <preset> 4x4,6x6,8x8,12x12 <images>`.

## Results

| Preset | Footprint | ours dB | ours MT/s | astcenc `-quality` | astcenc dB | none MT/s | avx2 MT/s | ours / none | ours / avx2 |
|---|---|---|---|---|---|---|---|---|---|
| Fast | 4x4 | 44.582 | 1.103 | 6.25 | 44.557 | 1.421 | 3.605 | **0.78** | 0.31 |
| Fast | 6x6 | 37.402 | 0.926 | 12.50 | 37.370 | 1.244 | 4.077 | **0.74** | 0.23 |
| Fast | 8x8 | 33.291 | 0.820 | 12.50 | 33.278 | 1.008 | 3.232 | **0.81** | 0.25 |
| Fast | 12x12 | 29.356 | 0.879 | 18.75 | 29.313 | 1.261 | 3.532 | **0.70** | 0.25 |
| Medium | 4x4 | 44.824 | 0.594 | 25.00 | 44.856 | 0.953 | 2.396 | **0.62** | 0.25 |
| Medium | 6x6 | 37.593 | 0.514 | 25.00 | 37.569 | 0.825 | 2.671 | **0.62** | 0.19 |
| Medium | 8x8 | 33.477 | 0.535 | 21.88 | 33.490 | 0.865 | 3.000 | **0.62** | 0.18 |
| Medium | 12x12 | 29.520 | 0.533 | 28.12 | 29.503 | 1.064 | 3.071 | **0.50** | 0.17 |

A second run gave Fast 0.74 / 0.73 / 0.86 / 0.65 and Medium 0.64 / 0.57 / 0.57 / 0.50.
The ratios repeat to within 0.05, although the absolute rates moved by up to 15% with load on
the hyperthread sibling.

At `feat/astc` `f0ed691`, before the shortlist and the float-contribution change, Medium
measured 0.58 / 0.49 / 0.51 / 0.45 (12x12 below the floor). Fast measured 0.77 / 0.74 / 0.85 / 0.62.

## Gates

| Gate | Criterion | Result |
|---|---|---|
| ES1 | ours / `astcenc-none` at least 0.5 at `Fast` and `Medium` | **pass**. Fast 0.65 to 0.86. Medium 0.57 to 0.64, and **0.50 at 12x12**, exactly at the floor in both runs |
| ES2 (performance phase) | at least 1.0 vs `astcenc-none`, and at least 0.5 vs `astcenc-avx2` | not met (0.15 to 0.30 vs avx2; the encoder has no SIMD yet) |

Medium at 12x12 has no margin. Most of its remaining cost is the per-grid mode ranking
(decimation plus residual) and the exact candidate evaluation (`docs/plans/astc-ledger.md`).
