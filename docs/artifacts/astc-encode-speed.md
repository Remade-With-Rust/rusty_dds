# ASTC encode speed vs astcenc (equal quality)

Encoder under test: `rusty_dds::astc::encode_rgba8` (scalar), branch `feat/astc` at `f0ed691`.
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
| Fast | 4x4 | 44.582 | 1.185 | 6.25 | 44.557 | 1.532 | 3.918 | **0.77** | 0.30 |
| Fast | 6x6 | 37.402 | 0.869 | 12.50 | 37.370 | 1.176 | 3.878 | **0.74** | 0.22 |
| Fast | 8x8 | 33.291 | 1.021 | 12.50 | 33.278 | 1.204 | 4.106 | **0.85** | 0.25 |
| Fast | 12x12 | 29.356 | 1.040 | 18.75 | 29.313 | 1.684 | 4.535 | **0.62** | 0.23 |
| Medium | 4x4 | 44.841 | 0.588 | 25.00 | 44.856 | 1.017 | 2.609 | **0.58** | 0.23 |
| Medium | 6x6 | 37.598 | 0.509 | 25.00 | 37.569 | 1.045 | 3.375 | **0.49** | 0.15 |
| Medium | 8x8 | 33.478 | 0.521 | 21.88 | 33.490 | 1.012 | 3.469 | **0.51** | 0.15 |
| Medium | 12x12 | 29.528 | 0.572 | 28.12 | 29.503 | 1.277 | 3.576 | **0.45** | 0.16 |

A repeat of the Medium run gave 0.58, 0.52, 0.53 and 0.46. The ratios repeat to within 0.03,
although the absolute rates moved by up to 15% with load on the hyperthread sibling.

## Gates

| Gate | Criterion | Result |
|---|---|---|
| ES1 | ours / `astcenc-none` at least 0.5 at `Fast` and `Medium` | Fast **pass** (0.62 to 0.85). Medium **fails at 12x12** (0.45 to 0.46) and is borderline at 6x6 (0.49 to 0.52); 4x4 and 8x8 pass |
| ES2 (performance phase) | at least 1.0 vs `astcenc-none`, and at least 0.5 vs `astcenc-avx2` | not met (0.15 to 0.30 vs avx2; the encoder has no SIMD yet) |

The encoder speed claim is therefore limited to Fast. Closing Medium at 12x12 is open work:
most of the cost is the per-grid mode ranking, and the planned lever is a block-mode shortlist
per footprint harvested from our own encodes (`docs/plans/astc-ledger.md`).
