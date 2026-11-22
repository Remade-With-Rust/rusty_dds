# ASTC build ledger

One line per brick: state, gate result, commit. Refuted ideas go in the second table with their measurement. The plan is `docs/plans/astc.md`.

| Brick | State | Gate result | Commit |
|---|---|---|---|
| plan | done | decisions recorded (plan section 13) | 66fc0ca |
| release-plz | done | `features_always_increment_minor = true` | 229c4d9 |
| 0.1 oracle workspace | done | astcenc 5.7.1 (`0affb51`) built in WSL; `-version` prints 5.7.1 | (rig) |
| 0.2 oracle tool | done | `tools/astc-oracle`: decode/info/ise/modes/partitions; decodes equal astcenc CLI output on corpus files | 42d15fb |
| 0.3 corpus + census | done | 956 natural files (corpus.sh); census script in the harness | 42d15fb, fd15e76 |
| 0.4 reference vs itself | done | avx2 == none in all six cells; 5.7.1 == main@22ba8a0 bytes (main returns BAD_BLOCK for error strips) | 85f6f06 |
| 0.5 bench harness | done | bench/astc under rusty_alloc, ABBA + null arm; baselines reproduce the scout pattern (27-37% pixels off by 1) | fd15e76 |
| D1 block modes | done | DC3 49,152/49,152 | 42d15fb |
| D2 ISE | done | 21 ranges x lengths 1..=64, 348,000 sequences equal | 42d15fb |
| D3 parse | done | per-field equal on 24 x 30,000 legal-biased blocks + DC2 | 42d15fb |
| D4 unquantisation | done | colour values and weights equal (per-field sweep) | 42d15fb |
| D5a/b endpoints | done | decoded endpoints + HDR flags equal in `ldr` and `hdr` profiles | 42d15fb |
| D6 partitions + infill | done | DC4 73,728/73,728; per-texel weights equal | 42d15fb |
| D7 texels + error colours | done | DC1/DC2 0 diffs, six cells; sNaN quieting of HDR void constants matched | 42d15fb |
| D8 surface driver | done | 127x128, 1x1, 3D cropping tests; rows compose the surface | 92689cb |
| D9 3D | done | DC1-DC4 on the 10 3D footprints | 42d15fb |
| D10 robustness | done | `arbitrary_blocks_never_panic` (10^6/footprint in release); fuzz target `astc_decode` | 92689cb, fd15e76 |
| D11 goldens + census | done | golden_v1.bin (432 KiB, oracle output hashes); census 2,888/2,888 cells >= 32 | 92689cb |
| C1 .astc | done | astcenc negatives rejected; read/write round trip; astcenc reads ours | 099d88e |
| C2 KTX 1.0 | done | astcenc .ktx read; ours read by astcenc; KTX-Software array_7 | 099d88e |
| C3 DDS | done | layout snapshot (4,063 lines) unchanged across the refactor; image-dds 14/14; dds 0.2 reads ours | 433b27a, c0c372d, c54dba9 |
| C4a KTX 2.0 read | done | KTX-Software files; zstd via `ktx2-zstd` (rusty_zstd 0.5) | 099d88e |
| C4b KTX 2.0 write | done | KTX-Software 4.4.2 `ktx validate` accepts all five written files (6x6 UNORM 9 mips, 8x5 sRGB, 4x4 SFLOAT, 12x12 sRGB 9 mips, 4x4x4 3D) | 958d069 |
| E1 pack | done | parse(pack(parse(b))) == parse(b) on > 50,000 legal blocks; ISE encode round trip all ranges/lengths | ef446cf |
| E2/E3 encoder | first cut | EC1 0 bad blocks; EC2 standing test; harvest below | ef446cf |
| E4 presets | first cut | Fastest (2 partitions, 1 pattern), Fast, Medium; early out per preset; quality validation in progress | 08acc3a, 4526a22 |
| E5 Thorough | first cut | 3 and 4 partitions by k-means + confusion-matrix pattern ranking; kodim03 4x4 +0.67 dB over Medium | 4526a22 |
| E6 Exhaustive | first cut | 16/8/4 patterns, 12 exact candidates, no early out; skip-gate harvest not done yet | 4526a22 |
| E7 HDR encode | v1 | modes 11/14/15, one partition, LNS domain; legal in both HDR profiles (unit test); EC1 + quality vs astcenc in harness `ec1x` | 3056661 |
| E8 3D encode | done | `encode_volume_rgba8`; legal (unit test); EC1 in harness `ec1x` | 2696e37 |
| EC3 determinism | done | 96 frozen hashes identical on x86_64 (simd on/off, debug/release) and wasm32 (wasmtime); aarch64 CI job | 6ec2a71 |
| DC6 fuzz | done | `astc_decode` 3,380,934 execs / 1 CPU-hour, 0 crashes; `astc_container` 30 min, 0 crashes | d90afb0, d582d3b |
| P1 decoder scalar | done | fast kernel byte-identical to reference (all golden blocks + 50,000/footprint, six cells); claim-grade timing pending | f8dd16c, 1ef2333, faec493, f9dfb0c |
| P2 decoder SIMD | step 1 | SSE2/NEON 8-bit output twin, matches scalar on 20,000 random cases | 7ebde45 |
| I1 API + CLI | done | `inspect_block`, `Dds::encode_astc_from_rgba8`, CLI round trip test (.astc/.ktx/.ktx2/.dds) | 97a4ec7, 42a9921 |

## E3 harvest (2026-11-21)

Kodak 01, 03, 08, 15; 4x4 / 6x6 / 8x8 / 12x12; mean PSNR gap ours - astcenc 5.7.1 at the same preset name, PSNR on our decode (bit-exact with astcenc's).

| Comparison | 4x4 | 6x6 | 8x8 | 12x12 |
|---|---|---|---|---|
| Fastest vs astcenc `-fastest -partitioncountlimit 1 -2planelimitcorrelation 0` (like for like) | +0.07 | -0.03 | +0.11 | +0.26 |
| Fastest vs astcenc `-fastest` | -1.71 | -0.44 | -0.22 | +0.23 |
| Fast vs `-fast` | -0.37 | +0.21 | +0.34 | +0.60 |
| Medium vs `-medium` | -0.50 | -0.20 | -0.28 | -0.12 |

Decision: the section 2.3 quality targets stand; Medium is within reach, so the fallback cut (decoder-only 0.11.0) is not taken. The open problem is speed (Fast 6x6 0.10 MT/s single-thread vs astcenc-none ~3.6).

## Preset tuning (2026-11-21)

Before tuning, 4x4 failed its target at every preset (Fastest -0.484, Fast -0.425, Medium -0.575). Swept with the dev-only `tuning` env overrides, Kodak 01/03/05/08/13/15/19/23, 4x4 mean gap vs astcenc 5.7.1 at the same preset:

| Medium 4x4 knob (from the shipped Medium) | mean gap | MT/s |
|---|---|---|
| baseline (2 partitions, dual gate 0.85) | -0.575 | 0.27 |
| 3 partitions, 2 three-partition candidates | -0.490 | 0.19 |
| + 4 partitions | -0.479 | 0.17 |
| early-out 0.75 -> 0.3 | -0.535 | 0.23 |
| 3 partitions + 8 exact candidates | -0.450 | 0.16 |
| 3 partitions + dual gate 0.95 | -0.361 | 0.16 |
| 3 partitions (1 candidate) + dual gate 0.99 | **-0.326** | 0.20 |

The dual-plane correlation gate is the main lever at small blocks (Fastest with dual plane on at 0.9: -0.484 -> +0.080; Fast 0.65 -> 0.95: -0.425 -> -0.031). Shipped: Fastest dual on 0.9; Fast 0.95; Medium 3 partitions `[4, 1, 0]` + 0.99; Thorough 1.0. Frozen hashes regenerated.

Full sweep after tuning (8 Kodak images, mean / worst gap in dB, ours single-thread MT/s in WSL):

| Preset | 4x4 | 5x5 | 6x6 | 8x8 | 10x10 | 12x12 | worst | MT/s |
|---|---|---|---|---|---|---|---|---|
| Fastest | +0.080 | +0.095 | -0.121 | -0.141 | -0.156 | +0.057 | -0.741 | 1.31-2.17 |
| Fast | -0.031 | +0.089 | +0.043 | +0.027 | +0.034 | +0.185 | -0.541 | 0.68-0.98 |
| Medium | -0.326 | -0.290 | -0.250 | -0.387 | -0.358 | -0.277 | -0.657 | 0.25-0.29 |
| Thorough | -0.384 | -0.293 | -0.313 | -0.425 | -0.458 | -0.460 | -0.658 | 0.04-0.06 |

Targets (section 2.3: Fastest/Fast mean >= -0.30, Medium >= -0.50, worst >= -1.5): all met.

## Encoder speed, ES1 (2026-11-21)

ES1 = our MT/s over `astcenc-none` MT/s at equal PSNR (astcenc `-quality` bisected to within 0.05 dB of ours), Kodak 01/03/08/15, single thread pinned to one core in WSL, CPU-time clocks (the host is shared; wall samples swung 4x), 4 ABBA rounds, minimum per arm.

| Step | Medium 6x6 kodim03 Ir | Medium 12x12 kodim03 Ir | Note |
|---|---|---|---|
| after preset tuning | 39.6 G | 26.34 G* | *measured after the Medium retune below |
| Medium retune (refine 2, partition pre-ranking `part_search [2, 1, 0]`, early-out 1.5) | | | quality -0.31..-0.46 dB mean, 0.28 -> 0.53 MT/s |
| hoists: opaque alpha skip, per-grid feed sums, padded taps, per-texel residual weights | | 24.76 G | byte-identical |
| exact top-k residual pruning | 23.87 G | 22.33 G | byte-identical |
| one-pass partition refit | 22.93 G | | byte-identical |

ES1 result (two repeats agree within 0.03):

| Preset | 4x4 | 6x6 | 8x8 | 12x12 | floor |
|---|---|---|---|---|---|
| Fast | 0.77 | 0.74 | 0.85 | 0.62 | 0.5: pass |
| Medium | 0.58 | 0.49 / 0.52 | 0.51 / 0.53 | 0.45 / 0.46 | 0.5: 12x12 fails, 6x6 borderline |

Against `astcenc-avx2` (ES2 territory, not a release gate): 0.14-0.28.

Open: Medium at 12x12. The cost there is the per-grid ranking (decimation + residual for every grid of every fit). The planned lever is a block-mode shortlist harvested from our own encodes per footprint (plan E6), which changes output and needs its own quality gate.

## Refuted or deferred ideas

| Idea | Measurement | Verdict |
|---|---|---|
| Rank every mode by its own quantised weights | 0.041 MT/s Fast 6x6; per-grid residual + analytic noise term gives 0.111 MT/s at -0.04 dB | replaced |
| Endpoint class chosen by estimate alone | base+scale picked on lines far from the origin: kodim03 4x4 Fastest 36.3 dB vs 42.4 after adding the class model error and evaluating each kept mode in every class | fixed |
| Precomputed per-mode colour range table | byte-identical, no measurable speed change (mode ranking no longer the bottleneck) | kept (simpler), not a win |
| "First pure-Rust ASTC encoder" claim | crates.io survey 2026-11-21: ktx-rs 4.4.4 (astcenc port, encode + decode) and oxideav-dds 0.0.7 (LDR encoder) exist | claim dropped |
| Lower early-out threshold for small blocks | Medium 4x4 0.75 -> 0.3: +0.04 dB for -15% speed; dual-plane gate gives +0.25 | not taken |
| Partition candidates visited by ascending lower bound (sorted) | pruned more residuals, but the sort cost more: 24.76 -> 25.30 G Ir | replaced by index-order pruning (22.33 G) |
| Channel-major gather for the exact error loop | did not vectorise on the SSE2 baseline; buffer memset 1.46 G Ir | reverted |
| Medium `grid_iters` 3 -> 2 for speed | ES1 6x6 0.59 but 12x12 0.42 (astcenc's matched point gets faster too) | not taken |
