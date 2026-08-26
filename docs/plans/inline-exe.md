# inline-execution — converting inline scalar work to SIMD/NEON/AVX2

**Opened 2026-08-22.** Mission: find every place in the codec where inline scalar
byte/element work could be replaced by a vector kernel, list the decoder and the
encoder separately, and rank what is actually worth building.

Five independent scans were run over the codec. Each scan's completion is logged in
§1 with what it covered and what it returned. §2/§3 are the candidate lists
(decoder / encoder), §4 the coverage-and-reachability status of the SIMD that
already exists, §5 the ranked build list, §6 what is ruled out. **No new timing was
taken for this survey** — value ranks come from trip counts, reach, and the
measurements already recorded in-tree; every brick in §5 still owes the
profile-first / revert-if-not-faster gate before it lands.

---

## 0. What qualifies a candidate (the discipline)

A scalar loop is NOT a candidate just because it is a loop. The bar, from the
campaign's own history:

1. **Reachability before anything.** The most expensive SIMD defect in this crate's
   history was not scalar-vs-vector — it was kernels that existed and were not
   wired (`pack_alpha_indices` shipped until 0.8.0 with a 677-instruction scalar
   double loop doing exactly what `alpha_select_avx2` already did; `try_bc7_mode4`
   was once justified by a fixture that called it **zero** times). Every candidate
   below records whether an existing twin covers it and whether the shipping path
   reaches that twin.
2. **A named reason auto-vec fails.** Plain mul/add/min/max/sqrt over contiguous
   data is SSE2-baseline and LLVM vectorizes it already — hand-SIMD there has
   been a revert everywhere it was tried. A candidate must name its blocker:
   gather/scatter, cross-lane reduction, shuffle/pack/interleave, saturating or
   clamped integer op, per-element LUT, bit-extraction chain, or loop-carried min.
3. **SSE2-baseline first.** The decode kernels won +52-93% per BC7 mode in pure
   SSE2 — no runtime detection, no second code path, one path the tests exercise.
   AVX2 only where the algorithm can actually use the width.
4. **Dispatch site is part of the design.** The same `pshufb` kernel measured
   47.8% slower dispatched per-block and +38.7% hoisted to surface scope — a
   `#[target_feature]` function cannot inline into a caller without the feature.
   The loop goes inside the kernel, not the kernel inside the loop.
5. **A fast-path GATE must state the kernel's actual requirement.** Four
   instances found in one day, in decode and encode, by different authors: the
   mip filter demanded even dimensions when any dimension ≥2 is clamp-free; the
   BC1-BC5 dispatch and then BC7's demanded `w%4==0 && h%4==0` when the kernels
   need only whole blocks (`1020x1023` — width perfectly aligned — paid the full
   penalty for one odd row). Each cost 2.4-6.5x on the excluded population and
   each passed every correctness gate, because a gate that is too strict is
   never *wrong*, only expensive. When writing one, state what the kernel
   actually requires and prove the gate equals it — do not restate an
   assumption from the first implementation.
6. **Gates.** Integer kernels: byte-identical (`assert_eq!` oracle over ≥60k random
   cases, the 0.8.0 convention). Float kernels: rel-err < 1e-5 vs scalar AND
   end-to-end PSNR unmoved. Every `cfg`-selected kernel commit runs
   `cargo test --no-default-features` (the scalar-fallback-doesn't-compile trap —
   which §4.4 shows this crate has already fallen into).

---

## 1. Scan log

| # | slice | files | status |
|---|-------|-------|--------|
| 1 | Decoder BCn block kernels | `decode/bcn.rs`, `decode/bc6h.rs`, `decode/reference.rs`, `decode/mod.rs` | DONE 2026-08-26 |
| 2 | Uncompressed decode + pixel-format/surface byte paths | `decode/uncompressed.rs`, `content.rs`, `surface.rs`, `upload.rs`, `format/*` | DONE 2026-08-26 |
| 3 | Encoder core block kernels | `encode/blocks/{bc1,bc7,m1,alpha,oracles}.rs`, `encode/blocks.rs` | DONE 2026-08-26 |
| 4 | Encoder BC6H + RDO + mips + driver | `encode/bc6h.rs`, `encode/blocks/rdo.rs`, `encode/mips.rs`, `encode/mod.rs` | DONE 2026-08-26 |
| 5 | Existing-SIMD coverage & reachability audit | `decode/simd.rs`, `encode/blocks/simd.rs`, all call sites, arch/feature gating, 8-config build matrix | DONE 2026-08-26 |

### 1.1 Scan 1 — decoder BCn block kernels

**Covered:** all of `decode/bcn.rs` (2565 lines), `decode/bc6h.rs`, `decode/mod.rs`,
`decode/reference.rs`; `decode/simd.rs` touched only to determine wiring.

**Returned:** 25 sites, of which 7 rank. The headline class is **scalar work running
inside the vector kernels**: `bc4_palette_packed` (no SIMD twin, ~34% of the BC5
block by the crate's own ceiling probe, called from inside `bc4/bc5_blocks_ssse3`),
`bc1_palette` (scalar GPR arithmetic inlined into all three BC1/2/3 SSSE3 surface
kernels), and `bc3_alpha_palette_packed` (~22% of BC3 decode, still carrying the
8-deep serial OR chain its BC4 sibling was rewritten to remove). Second class:
**three dispatch gates that drop whole populations to scalar** — no aarch64/NEON
code exists under `decode/` at all; `has_ssse3()` requires fast `pdep` so Zen 1/
Zen 2 run BC4/BC5 fully scalar; the BC1/2/3 surface kernels require `w%4==0 &&
h%4==0` so NPOT surfaces and the bottom two mips of every chain take the scalar
block path. Third: the BC6H **non-mode-11 interleaved→planar transpose** (96
element moves per block re-reading bytes `bcdec_rs` just stored — every
third-party/signed BC6H block pays it). Recorded refutations honoured: BC7 weight
extraction ceiling-probed at ~2.5% (mode 6) — not a lever; the BC6H
conversion-into-scatter fusion was tried and lost.

### 1.2 Scan 2 — uncompressed decode + pixel-format/surface byte paths

**Covered:** `decode/uncompressed.rs`, `content.rs`, `surface.rs`, `upload.rs`,
`decode/mod.rs`, `format/*` (all four files, loop-grep exhaustive), `lib.rs`,
`decode/reference.rs`, plus the BGRA arm of `encode/mod.rs`.

**Returned:** one HIGH-value candidate pair — the BGRA8↔RGBA8 byte swizzle
(decode `uncompressed.rs:65-67`, encode `encode/mod.rs:409-414`; same self-inverse
`[2,1,0,3]` permutation, one `pshufb` kernel serves both; measured ~0.62 ns/px
scalar, decode of a bgra8 surface is 4.5x the cost of the identical rgba8 copy).
Everything else in the slice is memcpy/memset-shaped and already optimal.
**Negative finding:** the anticipated 565/4444/5551/10-10-10-2/luminance
bit-expansion loops do not exist — `DecodeContent` has exactly two uncompressed
variants (Rgba8, Bgra8); the `format/` masks are header metadata never applied to
pixels. That whole SIMD class is contingent on format-support expansion, not
present debt. **Bonus:** `reference.rs:78-80` is a ready byte-identical oracle for
the swizzle kernel; zero NEON anywhere in the crate (all SIMD cfg sites are
x86_64-only).

### 1.3 Scan 3 — encoder core block kernels

**Covered:** all of `encode/blocks/{bc1,bc7,m1,alpha}.rs` and `encode/blocks.rs`
in full; `oracles.rs` confirmed 100% test-only; `simd.rs` inventoried by symbol
list only (Scan 5's slice); `tuning.rs`/`harvest.rs` checked to establish what a
default build compiles (PCA seed and both alpha windows are const-false).

**Returned:** 43 sites. Four HIGH finds: (1) **`m1.rs` — BC7 mode 1 — has zero
SIMD dispatch in the entire file**: the partition-ranking accumulation (~512
MACs/block), the 8-entry RGB argmin fit (~4096 squared-diff ops/block — no
8-entry-RGB kernel exists anywhere; `alpha_fit_avx2` is 1-channel,
`bc1_fit_4color_avx2` is 4-entry), 48 hardware integer divides per block in the
cluster bound, and a `quantize6p` never table-ized although the 7-bit QTAB
precedent sits in `bc7.rs` next door. (2) **`channel_span` (`blocks.rs:250-264`)**
— whole-surface stride-4 min/max, W×H trips per BC4 call (×2 for BC5), no twin,
per-element bounds check — the largest uncovered trip count in the encoder.
(3) **`lattice_refine_bc1`** (~40% of BC1 encode, measured 10.7 fits/call): each
±1-lattice candidate re-crosses the `#[target_feature]` boundary and re-widens
the same 16 pixels; `alpha_nbhd_avx2` is the in-crate precedent for hoisting the
sweep inside the kernel and was never built for the 565 palette. (4) **BC4/BC5
neighborhood sweep**: `alpha_nbhd_avx2` implements exactly this sweep shape for
BC7 modes 4/5 but is not wired for BC4/BC5 (it hard-codes W2/W3; BC4 needs
W6/W4) — near-unwired-twin. Also: `pca_extremes_rgb` (~525 instr/block, live on
the BC7 path via `ColorSeeds`, no twin); mode-4/5 LS *accumulation* scalar while
its solve is vectorized; BC4/BC5 channel extraction ignoring the existing
`planar_avx2`/`alpha_channel_avx2`; `extrema_rgba`'s scalar arm missing
`#[cold] #[inline(never)]` unlike every sibling; modes 1/4/5 still on the serial
`BitWriter` where mode 6 got the u128 SWAR pack.

### 1.4 Scan 4 — encoder BC6H + RDO + mips + driver

**Covered:** all of `encode/bc6h.rs`, `encode/blocks/rdo.rs` (1762 lines),
`encode/mips.rs`, `encode/mod.rs`, `tuning.rs`, `harvest.rs`; call-target spots
in `blocks.rs`/`bc1.rs`/`simd.rs`. Confirmed `tuning`/`harvest` contribute zero
code to a default build; RDO findings marked opt-in-shipping (`lambda > 0`).

**Returned:** 24 sites. HIGH: (1) **`f32_to_half_uf16`** — 48 branchy scalar
f32→half conversions per BC6H block (fused min/max reduction) with no twin;
F16C `vcvtps2ph` does 8 lanes/instruction and the UF16 clamp is two extra vector
ops. (2) **the mip box filter (`downsample_rgba8`)** — a whole per-pixel pass
over every mip level, runtime-clamped inner trips so nothing auto-vectorizes, no
SIMD twin, and fully serial while the block encoders around it are
thread-scope-parallel. MEDIUM-HIGH: the BC6H block gather re-clamps x/y for
every pixel including interior blocks (the interior fast path `blocks.rs`'s
`gather_block` already got was never applied); `bc7_block_sse` (RDO) accumulates
in over-wide i64 with no twin in a file where every other SSE kernel has one;
**`Mode6Fixed::new`** re-gathers W6M scalar ~131×/block when the whole 16-lookup
is literally one `pshufb` against the already-existing `W6M_REP` table — the
highest instruction-per-line-changed ratio found. MEDIUM: BC6H `ls_endpoints`
has no twin (BC1's and mode 6's identical shape both do); `table_ls` builds its
512-byte weight spread scalar purely as input to an AVX2 kernel; the hand-rolled
3-plane transpose in `polish_endpoints_fixed_table` duplicates `planar_avx2`
370 lines below its existing call site (reachability defect); `parse_mode6`
re-reads a just-packed block with 119 single-bit extracts (fix is plumbing, not
SIMD); `psnr_rgba8` is a strict-FP f64 reduction that provably cannot
auto-vectorize (harness-facing). Recorded refutation honoured: the RDO dedup
scans' vectorized `contains` was tried and refuted in-source (`rdo.rs:651-655`)
— listed as closed, not missed.

### 1.5 Scan 5 — existing-SIMD coverage & reachability audit

**Covered:** all of `decode/simd.rs` (1237 lines) and `encode/blocks/simd.rs`
(2949 lines); every `simd::` call site in `src/` (140 hits, grepped exhaustively);
all 158 `target_arch`/`target_feature`/detection hits; both public entry chains;
the oracle-test inventory; and an **empirically-run 8-configuration build matrix**
including a real `cargo check --target aarch64-unknown-linux-gnu`.

**Returned:** the full kernel inventory (14 decode kernels + ~40 encode kernels,
§4.1), with exactly **one unwired kernel** (`ls_accum_sse`, `#[cfg(test)]`-only,
superseded by the fused `ls_accum_solve_565` — not a lost win, but the shipping
fused kernel is only oracle-tested transitively through the test-only copy).
Reachability is otherwise clean: every kernel has a production caller, no BC7
decode mode bypasses SIMD. The serious findings are structural: **three broken
build configurations** (§4.4 — aarch64 does not compile at all with default
features; `encode` with `simd` off violates the crate's own `forbid(unsafe_code)`;
`encode,simd` without `decode` has a dangling `BC1_SEL` reference), a malformed
attribute stack that leaves `bc1_ls_solve_impl` outside `#[target_feature]` —
**three real ABI call boundaries per solve on the hot x86 mode-6 LS path** —
plus `has_avx2` missing the `#[inline]` its own doc claims, **per-block dispatch
on three BC6H decode kernels** (the 86-point-swing pattern §0.4 names), ~30
encode wrappers that are safe fns with only a `debug_assert!` guarding an ISA
precondition (UB if a caller-side guard ever drifts), and **16 shipping kernels
with no `*_matches_scalar` oracle** (§4.5), `alpha_select_avx2` the most exposed.

---

## 2. Decoder candidates

Ordered by value. "Coverage today" states who already runs vector code at this
site; the residual population is the candidate.

| # | site | what / blocker | coverage today | value |
|---|------|----------------|----------------|-------|
| D1 | `bc4_palette_packed` — `bcn.rs:472-533` | 8-entry palette build as scalar mul/add/shift feeding an OR tree; pack/saturate class. Runs **inside** `bc4/bc5_blocks_ssse3` and the scalar block fns; 2 calls per BC5 block. Crate's own probe: **~34% of the BC5 block**; `__m128i` palette build named in-tree as the remaining lever (`bcn.rs:601-603`) | none — scalar on every path | **HIGH** |
| D2 | `bc1_palette` — `bcn.rs:328-376` | 565→888 4-entry palette build; 12 scalar channel extract/mul/pack ops per block, inlined into all three BC1/2/3 SSSE3 surface kernels. shuffle/pack class | none — scalar on every path | **HIGH** |
| D3 | `bc3_alpha_palette_packed` — `bcn.rs:763-784` | divide-form 8-entry alpha palette + **8-deep serial OR pack chain** (the defect its BC4 sibling was rewritten to remove). ~**22% of BC3 decode** (in-tree measurement). `pmulhuw`-class + balanced-tree fix | none — scalar on every path | **MED-HIGH** |
| D4 | BGRA8→RGBA8 swizzle — `uncompressed.rs:65-67` | `[2,1,0,3]` byte permutation per pixel; textbook `pshufb`, 4 px/instr SSSE3, 8 px AVX2. Measured: bgra8 decode = **4.5x** the identical-size rgba8 copy (~2 cycles/px scalar). Oracle ready-made at `reference.rs:78-80`. Pairs with E7 (self-inverse mask, one kernel) | none | **HIGH** (cheapest brick in the survey) |
| D5 | BC6H decode dispatch hoist — `bc6h.rs:75,98,222` | `bc6h_interp_avx2` / `half48_to_f32` / `bc6h_planar_to_rgba` all pay a `#[target_feature]` call boundary **per 4×4 block** — the exact pattern measured at −26.7..−47.8% on BC1 before those kernels were hoisted to surface scope. Restructure, not a new kernel | kernels exist and are wired; the boundary siting is the defect | **HIGH-class law**, cheap |
| D6 | BC6H non-mode-11 transpose — `bc6h.rs:47-53` | 48-element interleaved→planar `u16` de-interleave (stride-3 gather), re-reading the 96 bytes `bcdec_rs::bc6h_half` just stored. Paid by **every** third-party/signed BC6H block | none — scalar for all non-mode-11 content | **HIGH** for third-party content, LOW for own-encoded |
| D7 | BC6H mode-11 weight unpack — `bc6h.rs:214-219` | 16 4-bit index extracts + LUT per block; `vpsrlvd`+`vpshufb` shape. Un-probed (unlike mode 6's 2.5% refutation) | none | **MED** — ceiling-probe first |
| D8 | NPOT surfaces excluded from every BCn surface kernel — the `width % 4 == 0 && height % 4 == 0` dispatch gate | **LANDED 2026-08-26** (rusty-dds-28). The gate was not approximating the requirement, it was a DIFFERENT requirement: the kernels store whole 4×4 blocks, so they are safe on every block except a PARTIAL last column/row. New `decode_surface_peeled` runs the kernel over the interior and peels only the partial edge; the five kernels gained a `grid_x` stride so they can cover a sub-rectangle. **Measured penalty the gate was imposing** (same pixel count, 1024² vs 1023², best-of-60): **BC1 3.30x, BC2 2.73x, BC3 2.74x, BC4 5.85x, BC5 3.99x** — and `1020x1023`, width PERFECTLY aligned, paid the full penalty because one odd row disqualified the whole surface. **After** (interleaved HEAD-worktree ABBA): NPOT at parity with aligned — BC4 ~6.3x/6.5x faster, BC2 ~3.8x/4.0x, BC5 ~3.6x/3.1x, BC1 ~3.5x/3.3x, BC3 ~2.9x/3.1x (1023² / 1020×1023); every B round beat every A round with no overlap, aligned path neutral (BC1/BC2, the formats untouched by D1/D3, unmoved). All 15 probe hashes byte-identical; new standing oracle `peeled_surface_matches_scalar_for_every_edge_shape` covers every `(w%4, h%4)` pair plus sub-block sizes; full suite + aarch64 test-check + qemu 26/26 green. **Third instance of the over-strict-gate class** (with the mip filter's EVEN-dimension gate and its NPOT-level gate) — written by different people at different times, failing identically. See §5.2k for the remaining residual populations (Zen 1/2 via `pdep`, and **BC7, which has the same gate AND loses thread parallelism with it**) |
| D9 | BC7 scalar mode loops — `bcn.rs:1272-2492` (8 modes) | 16 px × 4 ch interpolate+store per block; `write2`/`write2_split` twins cover x86. The scalar arm is the shipping path on **every non-x86 build** | x86_64+`simd` covered; no NEON mirror exists | **HIGH on aarch64** → NEON track (§5.3) |

Not candidates (measured/structural): RGBA8 passthrough copies, depth-slice
extends, zero-fills (all memcpy/memset-shaped, §6); BC7 weight extraction
(ceiling-probed ~2.5%); `blit_rgba4`, rotation scatter, surface driver loops.

## 3. Encoder candidates

| # | site | what / blocker | coverage today | value |
|---|------|----------------|----------------|-------|
| E1 | `f32_to_half_uf16` — `encode/bc6h.rs:20-41`, loop `:313-320` | 48 branchy scalar f32→UF16-half conversions + fused min/max per BC6H block; data-dependent branches ×4 arms. F16C `vcvtps2ph` = 8 lanes/instr, UF16 clamps = 2 vector ops | none | **HIGH** |
| E2 | mip box filter `downsample_rgba8` — `mips.rs:19-48` | per-pixel 2×2(×2) box average over **every mip level**; runtime-clamped inner trips block auto-vec; integer divide by runtime `n`. Exact form: `pmaddubsw`+`psrlw` (note: 2-level `pavgb` is NOT bit-identical — rounds differently). Also fully serial while block encode is thread-parallel | none | **HIGH** |
| E3 | `channel_span` — `blocks.rs:250-264` | whole-surface stride-4 channel min/max, W×H trips, 1 call BC4 / 2 calls BC5, per-element bounds check. Fix: min/max all 4 channels in one 32-byte-lane pass, return the asked pair — also fuses BC5's two passes into one | none (`channel_minmax_avx2` is per-block, wrong shape) | **HIGH** |
| E4 | `lattice_refine_bc1` — `bc1.rs:424-462` | ~**40% of BC1 encode** (10.7 fits/call, 191 847 calls / 196 608 blocks measured in-tree). Each ±1-565 candidate re-crosses the kernel boundary and re-widens the same 16 px. Fix = batched multi-candidate kernel (the `alpha_nbhd_avx2` pattern: load/widen once, sweep inside) | per-candidate fit twinned (`bc1_fit_565_avx2`); the **sweep** is not | **HIGH** (largest single encoder win potential; medium effort) |
| E5 | BC7 mode 1 kernel set — `m1.rs:109-115, 264-282, 343-359, 123-127` | the whole file has zero SIMD: masked subset accumulation (cross-lane reduction + `P2` gather, ~512 MACs/block), 8-entry RGB argmin fit (~4096 sqr-diffs/block, loop-carried min — **no 8-entry-RGB kernel exists in the crate**), `quantize6p` (LUT fix — QTAB precedent next door), 48 integer divides/block (no SIMD divide exists; 16-entry reciprocal table) | none | **HIGH** (route the LUT + divide items through eliminate-redundancy first; the fit kernel is the SIMD brick) |
| E6 | BC4/BC5 neighborhood sweep — `alpha.rs:299-354, 847-906` | ±1/±2 endpoint window, each candidate a fresh palette build + kernel crossing. `alpha_nbhd_avx2<N>` implements **exactly this sweep** for BC7 modes 4/5 — it hard-codes W2/W3; a W6/W4 variant wires it for BC4/BC5 | inner fit twinned; sweep not batched | **HIGH** (near-existing kernel) |
| E7 | RGBA8→BGRA8 encode swizzle — `encode/mod.rs:409-414` | same self-inverse permutation as D4; one kernel serves both directions | none | **HIGH by adjacency** (lands with D4) |
| E8 | `Mode6Fixed::new` — `rdo.rs:1291-1296` (RDO, opt-in) | 16 W6M lookups ×~131 calls/block; the whole gather is **one `pshufb`** against the already-existing `W6M_REP` table + one `cvtepu8_epi16` | table exists, vectors exist, this site scalar | **MED-HIGH**, cheapest RDO brick |
| E9 | `bc7_block_sse` — `rdo.rs:1236-1243` (RDO) | 64-element SSE vs source with an over-wide i64 accumulator (max possible sum 4.16e6 → i32 suffices, doubling lanes); widen+`madd_epi16` shape; second pass over bytes the decoder just stored | none (every sibling SSE kernel in the file has a twin) | **MED-HIGH** |
| E10 | BC6H block gather — `bc6h.rs:436-450` | per-pixel x/y clamp recomputed for interior blocks; the interior `copy_from_slice` fast path `blocks.rs:221-229` already proved never applied here | n/a (redundancy fix, not new SIMD) | **MED-HIGH** |
| E11 | BC6H fit plumbing — `bc6h.rs:79-97, 161-166, 213-218` | palette build 3-wide inner loop (all intermediates fit i32 → 8 AVX2 lanes if channel-major); `fit_avx2`'s AoS→SoA prologue (48 scalar stores) and scalar reduction epilogue bracket the vector loop with two store-forwarding round-trips | `fit_avx2` core exists and is wired | **MED** |
| E12 | LS accumulation twins — `bc7.rs:558-582` (mode 4/5), `bc6h.rs:351-380` (BC6H), `alpha.rs:581-590, 982-991` (BC4/5) | three LS accumulators whose identical-shaped siblings (BC1, mode 6) both have wired AVX2 twins; LUT-gather + cross-lane reduction class. Mode-4/5 is half-wired (solve vectorized, accumulation not) | solve halves covered; accumulation scalar | **MED** |
| E13 | wire existing kernels — `alpha.rs:60-75` (BC4/5 channel extraction; `planar_avx2`/`alpha_channel_avx2` exist, 5 call sites use neither), `rdo.rs:901-906` (hand-rolled 3-plane transpose; `planar_avx2` is called 370 lines below) | reachability class — the fix is a call, not a kernel | kernels exist, unreferenced here | **MED**, near-free |
| E14 | `table_ls` builder — `rdo.rs:602-609` (RDO) | 16-iter LUT walk + 512 bytes of scalar stores built purely as input to an AVX2 kernel; 4-entry LUT by 2-bit index = one shuffle | consumer vectorized, builder not | **MED** |
| E15 | `pca_extremes_rgb` — `bc1.rs:558-621` | ~525 instr/block: covariance (6 cross-lane reductions), projection argmin/argmax. Dead on BC1 by default; **live on the BC7 path** via `ColorSeeds::pca` behind mode-4/5 early-outs | none | **MED-HIGH** on BC7-alpha content |
| E16 | signed-path conversions — `alpha.rs:88-96, 689-694` (UNORM→SNORM: integer divide, LUT fix), `alpha.rs:941, 1045, 1113` (SNORM→UNORM palette, 3 copies, per-candidate LUT gather) | none | | **MED** |
| E17 | `consider_unique_pairs_u/s` — `alpha.rs:207-218, 774-785` | ≤20 candidates × (palette build + 16×8 argmin) per block; same batched-sweep shape as E6 | inner fit twinned | **MED-HIGH** (shares E6's kernel) |
| E18 | SWAR packs — `m1.rs:387-399`, `bc7.rs:493-515, 692-713` (modes 1/4/5 on serial `BitWriter` where mode 6 got the u128 `nib8` form), `bc1.rs:738-746` (BC2 nibble pack), `rdo.rs:1517-1562` (`parse_mode6`: 119 single-bit extracts re-reading a just-packed block — better fix: return the parts from the packer) | n/a | **LOW-MED** (redundancy class, not SIMD) |
| E19 | `psnr_rgba8` — `encode/mod.rs:512-526` | strict-FP f64 single-accumulator reduction — provably cannot auto-vectorize; diffs ≤255 so integer squares are exact (`pmaddwd`). Harness-facing public API, not encode-path | none | **MED** (harness only) |
| E20 | `bc7_mode6_seeds_extra` — `bc7.rs:872-902` | 16×4 mean + gated 120-pair farthest-pair argmax (480 sqr-diffs) | none | **MED** |

## 4. Already covered / reachability status

### 4.1 Inventory

`decode/simd.rs`: 14 kernels — `write2`/`write2_split` (SSE2 baseline, all 8 BC7
modes), `bc1/bc2/bc3_blocks_ssse3` (whole-surface, hoisted), `bc4/bc5_blocks_ssse3`
(+ per-block `bc5_gather`, SSSE3+BMI2 with the fast-`pdep` CPUID gate),
`bc6h_interp_avx2`, `half48_to_f32` (F16C), `bc6h_planar_to_rgba` (F16C), plus
register packers and const-fn tables (`BC1_SEL`, `BC2_ALPHA`, `BC3_SEL`).
`encode/blocks/simd.rs`: ~40 AVX2 kernels, all runtime-detected via a cached
relaxed `AtomicU8`. Third site: `encode/bc6h.rs:157 fit_avx2`.

### 4.2 Reachability: clean, with one test-only kernel

Every kernel has a production caller **except `ls_accum_sse`** — `#[cfg(test)]`,
superseded by the fused `ls_accum_solve_565`. Not a lost win, but the fused
shipping kernel is oracle-tested only transitively through the test-only copy,
and `ls_accum_scalar`'s doc still names the dead kernel as its twin. No BC7
decode mode bypasses SIMD. Known bypass populations (by design or defect):
non-mode-11 BC6H blocks skip `bc6h_interp_avx2` (D6); NPOT surfaces skip the
BC1/2/3 surface kernels **with no per-block fallback**; Zen 1/2 skip BC4/BC5
entirely via the `pdep` gate (D8).

### 4.3 Dispatch sites

Decode BC1–BC5: hoisted to surface scope (correct). **Decode BC6H: all three
kernels dispatch per-block** — the exact anti-pattern measured at −26.7..−47.8%
on BC1 (D5). Encode: **per-call at every one of ~40 leaf sites**, some at
measured rates of 17.7–259 calls per block; `has_avx2()` is one relaxed atomic
load so the flag is cheap, but the boundary is not — and `has_avx2` carries **no
`#[inline]` attribute** while its own doc claims `#[inline(always)]`. Worse,
the malformed attribute stack at `simd.rs:1403-1408` leaves `bc1_ls_solve_impl`
outside `#[target_feature]`: its calls to `solve_pair` and `round_pack` (×2)
cannot inline — **three real ABI boundaries per `bc1_ls_solve`** on the hot
mode-6 LS path (`bc7.rs:600, 1229`).

### 4.4 Build matrix — three configurations are broken (measured, `cargo check`)

> **STATUS 2026-08-26: all three FIXED by the §5.1 repairs — the full 10-config
> matrix (including both aarch64 rows) now compiles.** The table below records
> the pre-repair state as found.
>
> **Gate upgraded same day:** the matrix's aarch64 legs were `cargo check`
> only, which skips `#[cfg(test)]` code — rusty-dds-28's qemu lane exposed
> that three encode oracle tests lacked arch cfgs, and two integration tests
> (`encode_determinism`, `parser_robustness`) had NO `required-features` and
> could never compile in any reduced-feature config. All fixed;
> `cargo check --tests --lib --target aarch64-unknown-linux-gnu` (all six
> feature legs) joins the commit checklist so the gap cannot reopen.

| features | target | result |
|---|---|---|
| default | x86_64 | OK |
| `decode` / `decode,simd` / none | x86_64 | OK |
| **`encode` (simd off, any combo)** | x86_64 | **FAIL** — `gather_block`'s unconditional `unsafe` transmute (`blocks.rs:229`) violates the crate's own `#![forbid(unsafe_code)]` under `not(simd)` (`lib.rs:42`). The documented "simd off ⇒ zero unsafe" contract is unbuildable, and it is the exact configuration `lib.rs:33-34` recommends to downstreams |
| **`encode,simd` without `decode`** | x86_64 | **FAIL** — `encode/blocks/simd.rs:1096` reads `crate::decode::simd::BC1_SEL` across the feature boundary (E0433) |
| **default** | **aarch64** | **FAIL — 6 hard errors.** Missing `#[cfg(target_arch = "x86_64")]` on: `has_avx2` (:126, collides with the :159 non-x86 stub), `detect_avx2` (:149), the orphaned attribute pair at :182-184, `bc1_fit_4color_avx2`/`_impl` (:456, :478), and `bc1_ls_solve_impl` (:1431, missing both cfg and target_feature). The `has_avx2() -> false` non-x86 stub the whole fallback story depends on **has never been compiled** |

Consequence: the aarch64 scalar fallback is malformed, not merely absent, and the
"same hashes with the feature on or off" claim in `tests/encode_determinism.rs`
**cannot currently be executed** for the off half.

### 4.5 Safety contracts and oracle debt

~30 encode wrappers are safe fns whose only ISA guard is `debug_assert!(has_avx2())`
— in release, calling one on a non-AVX2 CPU is UB with nothing to stop it;
correctness rests on ~40 caller-side `if has_avx2()` guards staying in lockstep.
(Decode is sound by construction: kernels check inside and return `bool`, or are
`unsafe fn` with discharged contracts.) **16 shipping kernels have no
`*_matches_scalar` oracle** *(2026-08-26: the top five are now covered — see
§5.1.4; the remainder below stays open)*: `alpha_select_avx2` ~~(live on
three production paths; the existing oracle tests the *scalar* selector and is
`#[ignore]`d)~~ **covered**, the two fused kernels (`palette_fit_mode6_avx2`,
`ls_accum_solve_565`) **covered**, `extrema_opaque/rgba_avx2`
(untested `15-i` tie-break trick) **covered — tie-break proven**,
`channel_minmax_avx2`, `planar_avx2`,
`alpha_minmax_avx2`, `mode6_chan_sse_pair_avx2`, `mode6_chan_errs_avx2`,
`bc1_ls_endpoints_avx2`, `bc1_psq_rgb_avx2`, `bc1_fit_565_avx2`,
`bc1_widen_palette`/`bc1_fit_4color_pre_avx2`, `bc1_palette_565_i16_avx2`,
`bc6h_interp_avx2` (end-to-end coverage only).

## 5. Ranked build list

### 5.1 Repairs first (P0 — not new SIMD; some are prerequisites for everything else)

**ALL FOUR LANDED 2026-08-26.** Gates run: 10-config build matrix green
(8 x86 feature configs + aarch64 default + aarch64 simd-off), full
`cargo test --release` green under default features AND `decode,encode`
(simd off — the previously-unbuildable half of the determinism claim now
actually executes), 24/24 byte-identity probe hashes unchanged
(probe_encbytes / probe_6hbytes / probe_rdo_bytes / probe_snorm_bytes),
and `--emit asm` confirms `callq solve_pair` / `callq round_pack` are gone.

1. ~~Fix the malformed cfg/attribute stacks~~ **DONE** — all five stacks fixed;
   `bc1_ls_solve_impl` is back under `#[cfg]+#[target_feature]` (asm-verified:
   zero residual call boundaries where there were three per solve). Also: the
   dead non-x86 `has_avx2` stub deleted (every dispatch site is
   target-arch-gated, so it had never been reachable — a comment now says why
   there is no stub), and `SEL1`/`build_sel1` gained their missing cfgs.
2. ~~Restore the `simd`-off contract~~ **DONE** — `gather_block`'s transmute is
   a cfg-split `flat_to_pixels` helper (transmute with `simd`, safe copy loop
   without); `BC1_SEL` moved to a new `crate::simd_tables` module gated on
   `simd`+x86_64+`any(decode,encode)`, decode re-exports it, encode references
   it directly. The newly-live `decode,encode` config compiles **warning-free**;
   `encode` without `decode` still carries 23 pre-existing dead-code warnings
   (BC7-RDO machinery wants the decode oracle) — deferred, not blocking.
3. ~~`#[inline]` / `#[cold]` consistency~~ **DONE** — `has_avx2` carries the
   `#[inline]` its doc claimed; `extrema_rgba_scalar` split out
   `#[cold] #[inline(never)]` like its siblings.
4. ~~Oracle debt~~ **DONE for the named four** — new oracles, all passing 60k+
   randomized cases with forced tie/extreme inputs: `alpha_select_matches_scalar`
   (the key-packed tie-break vs the scalar strict-`<` first-min),
   `palette_fit_mode6_fused_matches_scalar` (fusion vs all-scalar composition),
   `ls_accum_solve_565_matches_scalar` (bit-identical vs the replicated
   `refit_with_ls` scalar chain, singular systems skipped by the same `1e-4`
   guard `table_ls` uses), plus `extrema_avx2_match_scalar` in
   `blocks/oracles.rs` covering both extrema kernels — the feared `15-i`
   tie-break divergence is disproved and now guarded. The rest of the §4.5 list
   stays open, opportunistically as sites are touched.

### 5.2 New vector bricks — x86 (one brick = one commit, profile-first gate each)

| rank | brick | why this order |
|---|---|---|
| 1 | **D4+E7 BGRA↔RGBA `pshufb` pair** — **LANDED 2026-08-26.** New shared `crate::swizzle::swap_rb` (SSSE3, loop-inside-`target_feature`, scalar tail, cached probe), wired at both `decode_bgra8_into` and `encode_slice`'s BGRA arm. **Measured 3.14x** (best-of-31, 1024²: scalar 0.696 → pshufb 0.222 ns/px), byte-identical by 60k-case oracle + self-inverse round-trip. Full suite + matrix green | smallest effort in the survey, measured 4.5x gap, oracle pre-exists, byte-identical gate trivial |
| 2 | **D5 BC6H decode dispatch hoist** — **REFUTED 2026-08-26** (rusty-dds-28). The full hoist (`bc6h_blocks_avx2`: one `#[target_feature(avx2,f16c)]` surface loop, all three kernels inlined, mirroring BC1-BC5) was built, byte-identical, and **measured 1.5-2x SLOWER** — 2.05 → 2.9-4.5 ns/px at 1024², interleaved HEAD-worktree ABBA, replicated across seven rounds, two orderings, and a binary-location swap; boundaries confirmed gone in the asm. Mechanism: the restructured loop compiled to 2462 lines vs the incumbent's 1280 with 13 bounds-check panic paths vs 4 — this loop's codegen is precariously tuned, and for kernels this heavy the per-block boundaries act as register/scheduling barriers. The 26.7% BC1 law does NOT transfer. Everything reverted to the incumbent shape (verified back at parity, 1.57-1.63 ns/px both arms). **What landed from the dig:** (1) per-block slice bounds check eliminated via `chunks_exact` rows (asm: `slice_index_fail` 2→1, per-block check gone, fn 1301 vs 1280 lines — no cliff; byte-identical; wall parity); (2) `bc6h_interp_avx2`'s first DIRECT oracle, `mode11_interp_vector_matches_scalar` (60k domain-true cases incl. both saturation extremes) — closes its §4.5 row; (3) the refutation record at the top of `decode/simd.rs`'s BC6H section. Bonus law: an 88-byte tuple returned from a non-inlined helper cost more per block than all three call boundaries combined. | restructure of existing kernels; the 86-point-swing law says the per-block boundary may be eating the kernels' win today |
| 3 | **E1 BC6H f32→half F16C** | 48 branchy scalar ops/block on the shipping HDR encode path; F16C is a near-exact ISA fit |
| 4 | **E3 `channel_span` one-pass 4-channel min/max** — **LANDED 2026-08-26.** SSE2 kernel `channel_spans_sse2`: RGBA's period-4 layout means a running 16-byte `vmin`/`vmax` accumulates channel `j%4` in lane `j` with ZERO shuffles; SSE2 is baseline so no runtime detection (the baseline-ISA law); new `channel_span2` returns both BC5 spans from one walk. **Measured 2.35x per call** (best-of-31, 2048²: scalar 0.3505 → 0.1494 ns/px), **~4.7x effective on BC5 flat content** which paid the scalar walk twice. Byte-identical trivially (min/max exact); 60k-case oracle with past-the-surface sentinels proves no over-read. A/B lesson: the first probe read scalar = 0.0000 ns — a PURE walk gets hoisted out of a timing loop; `black_box` both input and result | O(pixels) pre-pass before any BC4/BC5 encode byte; fuses BC5's two passes |
| 5 | **E2 mip box filter** — **LANDED 2026-08-26.** SSSE3 kernel in `encode/mips.rs` (`pshufb` channel-pair regroup + `pmaddubsw`·1 + `(sum+2)>>2`, two out-px per iteration, scalar row tail) for the even-w/even-h/2D case — every POT-chain level until an axis hits 1; odd dims, volumes and non-SSSE3 keep the scalar loop as fallback+oracle. **Measured 12.36x** (best-of-31, 1024²→512²: scalar 18.406 → 1.489 ns/out-px), byte-identical over 4000 mixed-parity oracle cases + 512×384. Full suite + matrix green | whole-mip-chain per-pixel pass; `pmaddubsw` exact form keeps byte-identity; parallel split is a free rider |
| 6 | **D1 `bc4_palette_packed` in `__m128i`** — **LANDED 2026-08-26** (rusty-dds-28). New `bc4_palette_xmm` (`pmulld`/`pblendw`/`pshufb`-truncate; one formula for all eight entries via W[0]=0, W[1]=65536); palettes now build in-register INSIDE `bc5_gather_ssse3`, so the vector path runs no scalar build, no pack tree, no `movq` — and both BC5 channel palettes build in parallel chains. `has_ssse3` gate gains an SSE4.1 term (population unchanged: BMI2 hardware all post-dates SSE4.1 — documented). **Measured (interleaved HEAD-worktree ABBA, 4 rounds each way, 2048²): BC5U 0.84 → 0.51 ns/px ≈ 1.65x, BC5S ~1.6x, BC4U ~1.35x, BC4S ~1.5x — every B round beat every A round; all four hashes byte-identical throughout.** Gates: EXHAUSTIVE oracle (all 65 536 endpoint pairs × both signs vs the scalar build), surface-kernel and general-decoder oracles green, full x86 suite green, aarch64 test-check + qemu 25/25 green. Asm: `bc5_blocks_ssse3` is 268 lines, zero `imul`, zero `callq`, eight `pmulld` — the scalar chain is gone. (+D3, +D2 same class — see §5.2f) | in-tree probes: ~34% of BC5 block / ~22% of BC3; the crate names the lever itself |
| 7 | **E8 `Mode6Fixed::new` `pshufb`** | one-instruction fix, ~131 calls/block on the RDO path |
| 8 | **E4 BC1 lattice batched-candidate kernel** — **REFUTED 2026-08-26** (reverted; refutation note at `lattice_refine_bc1`). v1 (batch calling the fit per candidate through an internal `#[target_feature]` call): **0.85x** — the call did not inline (the file's `fit_one` law) and the candidate array added an ABI spill. v2 (fit body shared as a macro, pixels widened once per round, acceptance chain replayed in-kernel): byte-identical over 60k seeded blocks and **DEAD FLAT, 1.00x** (serial 323.7 vs batched 324.4 ns/block, best-of-15, 4096 gradient blocks). Mechanism: this fit core is ~109 instructions — heavy enough to amortize its own boundary — and the dispatch flag is one relaxed byte; there was nothing left to win. Confirms the D5 lesson from the OTHER direction: the 26.7% boundary law is a property of LIGHT kernels, bounded from both sides now. Keep: the `bc1_fit_core_body!` macro extraction (neutral, oracle-gated — the next candidate-sweep kernel wants it) | biggest single encoder number (~40% of BC1) but medium effort + quality-neutrality care; do after the cheap bricks bank |
| 9 | **E6/E17 `alpha_nbhd` W6/W4 variant for BC4/BC5** | clones a proven, shipped kernel shape |
| 10 | **E5 mode-1 8-entry-RGB fit kernel** | new kernel shape; precede with the free eliminate-redundancy moves (QTAB-style `quantize6p` LUT, reciprocal-multiply divides, const member tables) |
| 11 | **E9 `bc7_block_sse` i32 kernel; D6 BC6H transpose; E10 gather fast path; E11-E15 as touched** | solid but smaller or population-gated |

### 5.2b Three adjacent small wins (identified 2026-08-26, same area as rank 1)

All three sit in the exact files the rank-1 brick touched, all are
memory-copies class (they move allocations, not bytes — deterministic and
byte-identical by construction), and none needs new SIMD:

1. **`decode_rgba8`'s fresh allocation** — `uncompressed.rs:13`
   (`data[..expected].to_vec()`). The only board case behind DirectXTex
   (724 vs 472 ns at 64²), and the loss is the allocation + OS page zeroing,
   not the copy. The `_into` twin already avoids it; the win is routing the
   hot streaming callers onto `decode_rgba8_into` / a recycled buffer, not a
   kernel.
2. **`decode_surface_pixels`' per-slice allocate-copy-drop** —
   `decode/mod.rs:459-475`. The volume path calls `decode_slice` per z-slice,
   which allocates a fresh `Vec`, is copied into `out` by `extend`, and
   dropped — one full surface allocation + copy per depth slice. The
   allocation-free machinery already exists (`decode_slice_into`,
   `mod.rs:479`); resize `out` once and decode straight into
   `out[z*bytes..]` chunks.
3. ~~The encoder's unconditional per-layer copy~~ — **LANDED 2026-08-26**
   together with §5.2c.2 as the mip-chain plumbing brick: mip 0 encodes
   straight from `src0`, and the levels below ping-pong two recycled buffers
   through the new `downsample_rgba8_into`. **Measured 3.05x on the full
   1024²×11-level chain** (1292.0 → 423.4 µs, best-of-15) — the copy +
   per-level allocation/page-fault tax was two thirds of the chain. Byte-
   identical (400-case chain-equivalence oracle + the existing reference
   oracle re-gating the `_into` refactor).

### 5.2c Five adjacent small wins (identified 2026-08-26, same area as rank 5)

All five sit in the mip filter's own function area — `encode/mips.rs` and the
driver mip loop in `encode/mod.rs:252-285` that calls it. None was landed with
the brick; each is deterministic (byte-identical by construction):

1. **The serial downsample fence** — `encode/mod.rs:278-283`. Each mip's block
   encode is `thread::scope`-parallel, but the downsample BETWEEN mips runs on
   one thread, serializing the chain once per level per physical slice. The
   kernel shrank the fence ~12x; row-splitting `downsample_rgba8` across the
   same scope (rows are independent) or overlapping it with the previous mip's
   encode removes it.
2. ~~Per-level allocate + zero-fill~~ — **LANDED 2026-08-26** with §5.2b.3
   (see there for the 3.05x number). The `resize` memset remains but lands on
   warm recycled pages; the allocation and fault tax is gone.
2b. **`surface_mut` re-walks the mip chain per level** — the driver calls
   `dds.surface_mut(id)` once per mip, and each call re-derives the offset by
   walking the chain from the top (`mip_offset_and_size_in_chain` is
   O(levels) per query → O(levels²) per slice; `upload.rs` documents having
   already halved its own call count for exactly this reason). A running
   offset in the driver loop deletes the walk.
3. **Interior clamps recomputed per output pixel** — `mips.rs` scalar path
   (`(x*2).min(sw-1)` ×6 per pixel). For interior pixels every `min` is a
   no-op; peel the edge row/column/slice into their own loops and the interior
   loses six clamps per pixel. The x86 kernel made this moot for even-2D;
   odd dims, volumes and (until the NEON mirror) all of ARM still pay it.
4. **The tap count is ALWAYS a power of two** — `mips.rs` (`(acc + n/2) / n`).
   Each axis contributes 1 or 2 taps, so `n ∈ {1,2,4,8}` — the per-channel
   integer divide is a shift in disguise the compiler cannot see because `n`
   is a runtime accumulator. `acc >> n.trailing_zeros()` with `+ (n>>1)`
   rounding deletes four hardware divides per scalar output pixel.
5. **SWAR pair-summing for the scalar path** — `mips.rs` inner loops. The
   scalar accumulation reads sixteen bytes one at a time; reading each pixel
   pair as `u64` and summing channels masked SWAR-style
   (`(a & 0x00FF00FF..) + (b & 0x00FF00FF..)`) halves the per-quad ops with
   plain integer code — the cheap interim for ARM and odd-dimension levels
   until a NEON mirror lands.

### 5.2d Five adjacent small wins (identified 2026-08-26, same area as rank 4)

The BC4/BC5 surface pre-pass and the flat gate it feeds — `blocks.rs`
(`channel_span`/`gather_block`), `encode/mod.rs:459-511`, and the flat/signed
encoders in `alpha.rs`. All deterministic:

1. **The span pre-pass is serial pure overhead** — `encode_bc4/5_surface` walk
   the whole surface before a single encode byte, on one thread, while the
   encode itself is scope-parallel. Min/max are associative: compute per-band
   spans inside the same `thread::scope` and fold, and the extra pass
   disappears into the parallel section.
2. **The scalar walk keeps its per-byte bounds check** — `channel_span`'s
   fallback (`rgba[row + x*4 + channel]`) pays an indexed check per byte and
   resists auto-vec. Restructure over `chunks_exact(4)` and the checks
   vanish; LLVM can then auto-vectorize the u8 min/max reduction — the
   ARM/`simd`-off twin of the kernel, no `unsafe` needed.
3. **The signed-flat conversion divides per sample** — `alpha.rs:88-96` and
   `:689-694`: `(u*253+127)/254 - 127` per sample, an integer divide with no
   SIMD instruction, on every signed BC4/BC5 block both flat and presweep
   paths. The inverse table (`SNORM_TO_UNORM`) already exists in-tree; the
   forward 256-entry LUT deletes the divide.
4. **The non-flat arm ignores existing channel-extraction kernels** —
   `alpha.rs:60-75`: `pixels.map(|p| p[0])` / `p[1]` strided gathers per
   block, while `planar_avx2` (all four planes, one pass) and
   `alpha_channel_avx2` sit wired for OTHER call sites. Survey E13 — the fix
   is a call, not a kernel.
5. **`gather_block`'s edge path clamps per pixel** — `blocks.rs:270-284`,
   twenty lines above `channel_span`: the non-interior arm recomputes
   `min`/`saturating_sub` per pixel per block. Peel: interior rows of an edge
   block are still contiguous runs; only the clamped border column/row needs
   per-pixel work.

### 5.2e Three adjacent small wins (identified 2026-08-26, same area as rank 2 / BC6H decode)

Identified during the D5 dig, not landed; claim as usual. All three live in
`decode/bc6h.rs` or its kernels.

1. **D6, now cleanly isolated: the non-mode-11 transpose** — the general-decoder
   fallback re-reads the 96 bytes `bcdec_rs::bc6h_half` just stored and
   transposes them scalar (16×3 element moves), per block, for **every**
   third-party or signed BC6H block. Our encoder never emits these, so the
   corpus never sees it — but third-party HDR content pays it on every block.
   `punpck`-class shuffle work. CAUTION from the D5 refutation: this loop's
   codegen is precariously tuned — ABBA any change against a HEAD worktree.
2. **The alloc-form page tax, MEASURED once already** — `decode_bc6h`'s
   `vec![0f32; n]` (16 MiB zeroed at 1024²) looks like pure double-touch tax,
   and an uninitialised-buffer form was built and measured **NEUTRAL** under
   the system allocator: first-touch page faults dominate and the decode's own
   writes pay them either way (the in-source comment records this). The win
   only materialises under an allocator that recycles segments — re-evaluate
   under `rusty_alloc` (the deployment allocator; sim already runs it), where
   the memset becomes the dominant term. The buffer-recycling `_into` form
   remains the real answer for hot paths (3.4-6.5 vs 1.6 ns/px measured).
3. **D7, still unprobed: the mode-11 weight unpack** — 16 sequential 3-4-bit
   extracts + LUT per block feeding the interp kernel; `vpsrlvd`+`vpshufb`
   shape. Mode 6's BC7 twin ceiling-probed at ~2.5% and was refuted — probe
   BC6H's before building anything (the survey's own rule), and note the
   extraction currently compiles into the well-tuned incumbent loop the D5
   episode proved fragile.

### 5.2f Five adjacent small wins (identified 2026-08-26, same area as rank 6 / the BC4-BC7 palette-gather kernels)

Identified during the D1 brick, not landed; claim as usual.

1. ~~D3, the natural sibling~~ — **LANDED 2026-08-26** (rusty-dds-28). New
   `bc3_alpha_xmm` builds the division-form palette in-register inside
   `bc3_blocks_ssse3`: one formula for all eight lanes (`WA=7/WB=0` and
   `WA=0/WB=7` reproduce `a0`/`a1` exactly), the exact divisions become one
   `pmulhi_epu16` against a reciprocal (`x9363>>16 == /7` for N<1872, need
   1786; `x13108>>16 == /5` for N<3276, need 1276), arm B's lane 6 falls out
   of the formula and lane 7 is one `insert_epi16`. Pure SSE2 + the caller's
   existing `pshufb` — the `has_pshufb` gate is UNCHANGED, so no population
   moves. **Measured (interleaved HEAD-worktree ABBA, 4 rounds each way,
   2048²): BC3 1.21 → 0.82 ns/px ≈ 1.4x, every B round beat every A round;
   BC1/BC2 unmoved; hashes byte-identical.** EXHAUSTIVE oracle (all 65 536
   endpoint pairs vs the scalar build) + general-decoder oracle + full suite
   + aarch64 test-check green. NOTE the coexisting refutation: the SCALAR
   `base + k*delta` rewrite at this site measured -6..-7% twice and stays
   refuted — the vector build wins by a different mechanism (all lanes issue
   together; the reciprocal replaces the divide; the pack chain and `movq`
   vanish), not by reducing scalar op count.
2. **D2, same class:** `bc1_palette` — the 565→888 4-entry build is 12 scalar
   channel ops inlined into all three BC1/2/3 SSSE3 surface kernels;
   `pmulhuw`-class in-register form, palette stays in the register `pshufb`
   consumes.
3. **`bc4_indices` byte assembly** — `u64::from_le_bytes([blk[0]..blk[7]])`
   spells eight indexed reads; a `try_into::<[u8; 8]>` form is one length
   check (the chunks_exact lesson from the D5 dig, applied one function up).
4. **The scalar block fns' fallback rows** (`bc4/bc5_block_rgba` tails) still
   index the palette per pixel — they serve NPOT edges and pre-SSE4.1
   machines only, so probe the population before spending anything (D8's
   law).
5. **The `w%4==0 && h%4==0` surface-gate residue (D8):** NPOT surfaces and
   the bottom mips of every chain take the per-block path with its per-block
   wrapper checks; an edge-aware surface kernel (interior rows fast, border
   clamped) would carry the D1 palette win to the whole population — measure
   the NPOT share first.

### 5.2g Five adjacent small wins (identified 2026-08-26, same area as D3 / the BC1-BC3 surface kernels)

Identified during the D3 brick, not landed; claim as usual.

1. **D2, now the LAST scalar palette in any surface kernel:** `bc1_palette`'s
   565→888 build (12 scalar channel ops per block) runs inside all three
   BC1/2/3 SSSE3 kernels; the D1/D3 in-register recipe applies
   (`pmulhuw`-class expansion of the 5/6-bit fields), and the palette lands
   directly in the register `pshufb` consumes.
2. **BC2's alpha table pressure:** `bc2_blocks_ssse3` loads `BC2_ALPHA[byte]`
   twice per row from a 4 KiB table; the 4-bit→8-bit `x17` expansion is
   `(x << 4) | x` per nibble — buildable in-register from the raw alpha
   `u16`s, retiring the table and its L1 footprint.
3. **`aidx` byte assembly** (`bc3_blocks_ssse3`): `u64::from_le_bytes([blk[0]
   ..blk[7]]) >> 16` spells eight indexed reads — the `try_into::<[u8; 8]>`
   form is one length check (same fix as §5.2f.3, this site).
4. **`BC3_SEL` double lookup:** two 64-bit loads per row from a 6-bit-indexed
   table; a multiply-spread in-register selector could replace both — but the
   table is L1-resident, so CEILING-PROBE first (the D7 rule).
5. **The BC1/2/3 NPOT fallback (D8):** `w%4 || h%4` drops whole surfaces —
   including the bottom two mips of every chain — to the scalar block fns,
   which now miss BOTH the D1-class palette wins and the surface kernels
   entirely; an edge-aware surface variant carries every landed win to that
   population. Measure the NPOT share of real streaming loads first.

### 5.2h Mip-chain plumbing campaign — 10 wins, 2 refutations (2026-08-26)

A dedicated sweep of the mip/driver area after the tier-5 brick, on the premise
that more was hiding there. It was. Every win byte-identical (the 400-case
chain-equivalence oracle and the reference oracle re-gate the whole file after
each), each measured best-of-N in-process with `black_box` on input and result.

| # | win | measured |
|---|-----|---------:|
| 1 | Per-level zero-fill deleted — every output byte is overwritten, so `clear+resize(…,0)` was a discarded memset; a shrinking recycled buffer makes the steady state a pure `truncate` | **1.33x** on the 10-level buffer prep |
| 2 | Ping-pong buffers hoisted above the physical-slice loop — a cubemap's six faces share one warm pair instead of allocating and faulting a fresh pair per face | **1.13x** on a 6-face 256² chain |
| 3 | `n` is always a power of two (each axis contributes 1 or 2) — the four per-pixel rounding divides became shifts | 1.15x on the scalar path |
| 4 | Non-degenerate 2D peeled into a clamp-free loop — the general loop's runtime-clamped trip counts were what blocked auto-vec | 3.05x cumulative |
| 5 | SWAR pair-summing in that loop — two pixels as one `u64`, even/odd bytes to `u16` lanes | 5.55x cumulative |
| 6 | **The kernel gate was over-strict**: it demanded EVEN dimensions, but the clamp analysis holds for ANY dimension ≥ 2, so every NPOT level had been taking the scalar path for no arithmetic reason | **28.94x cumulative** on 1001²→500² |
| 7 | Volume kernel (`downsample_3d_ssse3`) — 2×2×2 in i16 lanes, `(sum+4)>>3` | **36.38x** on 256×256×8 |
| 8 | Degenerate-axis (1×N / N×1) path — the chain tail of every non-square texture — via the carry-safe byte-average identity `(a\|b) − (((a^b)>>1) & 0x7F…)`, portable, no SIMD gate | **10.67x** on 8192×1 |
| 9 | `subresource_chain_ranges` — one chain walk for every level's byte range, replacing per-level `surface_mut` (each of which re-walked from the top: O(levels²) per slice) | **6.54x** on a 13-level chain |
| 10 | AVX2 twin of the row kernel — four output pixels per iteration; all three ops are in-lane so one permute restores order | **1.20x** over SSSE3 |

**Refuted, both reverted with the mechanism in-source:**
- **Row-banding the downsample across threads: 0.38x** (2.6x slower). The
  kernel is memory-bound at ~18 GB/s single-thread; extra threads buy
  bandwidth contention plus spawn cost.
- **Overlapping each level's encode with the next level's downsample: 0.50x.**
  Byte-identical by construction (disjoint writes, joined before the next
  level) and twice as slow — per-level spawn/join latency plus two
  memory-bound passes contending.
- Together these say the mip chain is **bandwidth-bound, not
  parallelism-starved** — the same law as the rs_h264 SAD record. Do not
  re-attempt threading here without a bandwidth measurement first.

### 5.2i BC7 seed area — E15 landed, and the cost model it disproved (2026-08-26)

**Reachability probe first** (the campaign's own rule), with test-only counters
now permanently in `bc7.rs` (`probe_counters` + `reach::bc7_seed_reach`,
`#[cfg(test)]`, zero shipping cost). Over 16384 blocks of alpha-structured
content:

| site | fires | per block |
|---|---:|---:|
| `pca_extremes_rgb` builds | 19083 | **1.165** |
| `ls_endpoints_mode5` | 32981 | **2.01** |
| `bc7_mode6_seeds_extra` (span>16) | 2861 | 17.5% |
| farthest-pair (span>48) | 2823 | 17.2% |

**The finding: `ColorSeeds::pca`'s documented cost model was wrong.** Its
comment claimed the mode-5 (89%) and mode-4 (69%) early-outs keep the
525-instruction PCA seed off most blocks; it actually builds **1.165× per
block**. Mode 4 reaches its colour search — and so the seed — *before* its
early-out, so its 69% never protected the seed at all; and each rotation
constructs its own seed set, so the `OnceCell` dedupes rotation 0 only. Same
disease as the mip filter's over-strict gate and D1's stale estimate: **a
documented cost model that nobody re-measured after the surrounding control
flow moved.** Comment corrected in-source with the measured number.

**LANDED — E15 `pca_extremes_avx2`: 1.52x** (198.4 → 130.4 ns/block,
best-of-15 over 8192 gradient blocks), byte-identical over 60k cases spanning
gradients, exactly-flat and near-flat blocks (both `None` degeneracies),
single-channel ramps and noise. Bit-identity is *proved per stage*, not hoped:
the mean is exact (integer sum ≤ 4080, `/16` a power of two, so any order);
the covariance is NOT exact, so it is vectorised **across the six terms**
rather than across pixels — each lane sees the scalar accumulator's exact
sequence; each projection `t` is its own expression in the scalar's
association; and `min`/`max` are exact and associative, with the FIRST
attaining index recovered by an ordered scan to match the scalar's strict
`<`/`>`.

### 5.2k.1 BC7 NPOT gate — LANDED 2026-08-26 (the over-strict-gate class, 4th instance)

Handed over by rusty-dds-28 after D8: `decode_bc7_into` computed
`aligned = w%4==0 && h%4==0` and used it **twice** — as the direct-write gate
*and* as a term in `parallel`. One odd row or column therefore cost an NPOT
BC7 surface the direct path **and** the thread-parallel path, dropping the
crate's most expensive format onto serial scratch-and-blit: two multipliers
compounding. As with D8 and the mip filter, the gate stated a *different*
requirement than the kernel's — the direct write needs WHOLE BLOCKS, not
aligned dimensions.

`decode_bc7_rows` is now one peeled walker serving both arms: interior blocks
(`bx < out_w/4`, full 4-row group) write straight to the output pitch; the
partial last column/row go through the existing 64-byte scratch and clamping
blit. The interior loop is *peeled, not branched*, so an aligned surface
executes what the old direct loop did. The `parallel` gate keeps only its
work terms.

| surface | pre-peel | shipping | ratio |
|---|---:|---:|---:|
| 1023×1023 | 3.43–3.56 ns/px | 1.14–1.46 | **2.4–3.1x** |
| 1020×1023 (width aligned, one odd row) | 3.28–3.76 | 1.21–1.38 | **2.4–2.9x** |
| 1024×1024 aligned — **neutrality** | 3.13–3.17 | 3.05–3.31 | 1.02 / 0.95 / 1.02 (noise) |

NPOT is now at parity with aligned (1.14–1.46 vs 1.42 ns/px). Byte-identical
against `decode_bc7_scratch` — kept as a `#[cfg(test)]` oracle precisely
because it is correct at *every* size, so the peel is proved over the whole
sweep rather than half of it — across every `(w%4, h%4)` pair, sub-block
sizes, and sizes past the parallel gate where a band can receive the partial
row.

**Measurement note worth keeping:** the first neutrality arm compared a serial
replica against the *parallel* dispatch and read 2.35x — thread count, not the
peel. Fixed to serial-vs-serial, like-for-like walkers. Arm-duration/work
parity is not a formality; it nearly banked a fabricated number.

### 5.2j Five adjacent small wins (identified 2026-08-26, same area)

1. **The farthest-pair scan** (`bc7.rs:886-903`) — O(16²): 120 pairs × 4
   channels = 480 squared diffs, **measured at 17.2% of blocks**. The 15
   shifted-vector passes form (`pixels[i]` against all `j>i` at once) replaces
   the scalar double loop; or bound it first with a cheap axis-extreme
   prefilter, since the pair is only a seed.
2. **`ls_endpoints_mode5`** (`bc7.rs:562-617`) — **2.01 builds/block**, the
   highest-frequency site in the area, and a *half-wired twin*: its solve goes
   through `simd::bc1_ls_solve` while its 16×11-flop accumulation stays
   scalar. Two levers, both provably exact: the `WF` weights are multiples of
   2⁻⁶ so every product and partial sum is exact in f32 (**reassociation is
   free here — vectorise without a tolerance argument**), and since `u+w=1`,
   **`b1 = Σx − b0`** halves the accumulation even in scalar.
3. **Per-rotation PCA covariance is partly redundant** — a rotation swaps
   exactly one colour channel with alpha, so for rotation `c` the two
   untouched channels' three covariance terms are bit-identical to rotation
   0's. Carry them instead of recomputing; combined with the 1.165 rate above
   this is the structural half of the same finding.
4. **The rotation channel swap** (`bc7.rs:95-98`) — 16 `p.swap(c, 3)` per
   rotation, up to 3 rotations per block: one `pshufb` with a
   rotation-selected constant mask.
5. **Three separate passes over the same sixteen pixels** —
   `ColorSeeds::new` builds `extrema` (2r+3g+b argmin/argmax) and `cminmax`
   (per-channel min/max) in two passes, and `channel_minmax_rgba` walks them
   a third time at `bc7.rs:84` for the rotation spans. One fused pass feeds
   all three consumers.

### 5.2k Five adjacent small wins (identified 2026-08-26, same area as D8 / the decode dispatch gates)

Identified while landing D8, not landed; claim as usual. The first is the
biggest single decode item left in the plan.

1. **BC7 carries the SAME gate — and pays double for it.**
   `decode_bc7_into` (`bcn.rs:319-330`) computes `aligned = width % 4 == 0 &&
   height % 4 == 0`, and `parallel` requires `aligned` as well. So an NPOT BC7
   surface loses the direct path **and** the thread-parallel path (measured at
   2.8x on large surfaces), falling to a single-threaded scratch-and-blit —
   on the crate's most expensive format. The D8 peel transfers directly:
   interior blocks are whole-block writes exactly as in BC1-BC5, and the
   parallel strip loop can take the same `run_y` bound. Expect the
   compounding of two multipliers.
2. ~~Zen 1 / Zen 2 still get NO vector BC4/BC5 at all~~ — **LANDED
   2026-08-26** (rusty-dds-28). New `idx_spread_ssse3` unpacks the sixteen
   3-bit indices with `pshufb` + `mullo_epi16` + one uniform `srli_epi16(13)`:
   the multiply lands each field at bits `[13,16)` so the shift both extracts
   and masks it, which is how you get a per-lane variable shift out of SSE2,
   which has none. `has_fast_pdep` and the BMI2 term are **deleted** — the
   dispatch is now plain SSSE3+SSE4.1, so Zen 1/2 stop being excluded and go
   from the scalar block path to the vector kernel (the D8 probe measures that
   gap at ~6.3x for BC4, ~3.6x for BC5; not re-measured here, and no Zen
   timing is claimed — none was taken). Selector entries all point at real
   index bytes, single-byte lanes taking `-1` so `pshufb` zeroes them, so the
   constants imply nothing about data past the 48 bits that exist. Gated by
   `idx_spread_matches_scalar` (every field position × every value, against an
   all-ones background, plus 200k random words), byte-identical on all decode
   probes, full suite + aarch64 + qemu 26/26 green.
   **On fast-`pdep` hardware it is NOT a regression: BC4 ~1.17x FASTER**
   (0.369 → 0.316 ns/px, no overlap over six interleaved rounds), BC5 neutral.
   ★ **MEASUREMENT LESSON — an isolated instruction A/B got the SIGN wrong.**
   `probe_idx_spread` compares the two sequences alone in a loop and reports
   `pshufb` **2.19x slower**, predicting a clear end-to-end loss. In situ the
   opposite happened, because an isolated loop measures a sequence's own
   throughput on a dependent chain, while a real kernel measures its MARGINAL
   cost against port pressure: `pdep` is single-ported and was contending with
   the palette build, the shuffles and the stores, whereas
   `pshufb`/`pmullw`/`psrlw` used ports that had slack. Marginal cost is not
   proportional to standalone cost and can invert — take the number IN the
   kernel. The probe is kept as the record of the inversion, not as a gate.
3. **`decode_rgba_blocks_into`'s aligned branch is now nearly dead.** With all
   five formats routed through `decode_surface_peeled` whenever SSSE3 is
   present, its `width % 4 == 0 && height % 4 == 0` fast branch serves only
   pre-SSSE3 x86 and non-x86. Fold it into the peel helper (which already
   handles both) and one of the two block loops disappears.
4. **The peeled edge blocks re-probe the ISA per block.** `bc4_block_rgba` /
   `bc5_block_rgba` call into `bc5_gather`, which calls `has_ssse3()` — so
   every edge block pays a probe the surface dispatch already made. Pass the
   decision down, or give the peel its own `#[target_feature]` edge kernel.
   Small (edges are O(w+h) blocks) but it is the per-block-probe pattern this
   campaign keeps finding.
5. **BC6H's interior test is per-block, not per-surface.** `decode_bc6h_into`
   re-evaluates `py0 + 4 <= h && px0 + 4 <= w` for every block to pick the
   planar scatter — correct, but it is the D8 shape one level down. CAUTION:
   the D5 refutation proves that loop's codegen is fragile; hoist the
   *predicate* only, keep the call structure, and ABBA it.

### 5.2m Five adjacent small wins (identified 2026-08-26, same area as §5.2k.2 / the BC4-BC5 gather)

Identified while landing k.2, not landed; claim as usual.

1. **§5.3b.5 is now unblocked and half-written.** The NEON `bc5_gather` port
   was blocked on `pdep` having no ARM equivalent — that dependency is gone,
   and `idx_spread_ssse3` is a near-1:1 NEON port (`vqtbl1q_u8` for the
   window gather, `vmulq_u16` + `vshrq_n_u16::<13>` for the extract, all
   baseline AdvSIMD). Doing it makes BC4/BC5 the first BCn formats fully
   vectorised on both architectures.
2. **BC4 spreads a word it knows is zero.** `bc4_block_rgba` and
   `bc4_blocks_ssse3` pass `ig = 0` and `green = None`, so the second
   `idx_spread` call unpacks a constant zero to produce a palette lookup of
   an all-zero palette. LLVM may fold it; the asm has not been checked.
   Confirm, and if it survives, give the kernel a `green.is_none()` fast path
   that skips the second gather entirely.
3. **The `mult` and selector constants rematerialise per call.** They are
   built with `_mm_setr_*` inside `idx_spread_ssse3`, which is called twice
   per block; whether LLVM hoists them out of the surface loop is unverified.
   If not, they are three registers' worth of loop-invariant constants —
   check the asm before assuming.
4. **`has_ssse3`'s name now lies.** It asserts SSSE3 **and** SSE4.1 and no
   longer has anything to do with `pdep`; the doc still discusses BMI2 at the
   call sites. Rename to `has_bc45_isa` (or similar) and fix the stale prose —
   a predicate whose name understates what it checks is how the wrong CPU
   eventually gets let in.
5. **The 3-bit unpack is duplicated in the scalar paths.** `bc4_block_rgba`,
   `bc5_block_rgba` and the BC3 alpha loop each re-derive `(idx >> 3p) & 7`
   inline. One shared scalar helper, oracle-shared with `idx_spread_ssse3`,
   would make the vector/scalar pair explicit — and the D3 alpha path could
   then reuse the same vector unpack it currently does without.

### 5.3 NEON track (UNBLOCKED by §5.1; stage 1 LANDED 2026-08-26)

Port order (near-1:1 mirrors first):
1. `write2`/`write2_split` → all eight BC7 modes at once — **LANDED
   2026-08-26** (rusty-dds-28). New `decode/neon.rs` mirrors; the register
   pre-packing and the 16-bit-lane proof moved to a shared arch-neutral
   `decode/interp_pack.rs` so the two mirrors cannot drift; the eight mode
   loops dispatch through one `interp` alias
   (`simd` on x86-64, `neon` on aarch64 — NEON is baseline, so the aarch64
   side has NO runtime detection and no scalar arm). **Executed on aarch64
   under qemu-user 10.2.1: 25/25 decode tests pass, including both new
   `*_matches_scalar` sweeps AND all eight BC7
   `*_matches_the_general_decoder` bcdec oracles, which route through the
   NEON kernels on this arch.** Codegen (aarch64 release asm): the
   interpolation compiles to `mla v.8h` + `sqshrun #6` — LLVM fused the
   multiply-add AND the shift-plus-saturating-narrow, two vector
   instructions per two pixels against the scalar arm's ~26 scalar ops.
   NOT yet timed on ARM hardware — emulation proves correctness, never
   speed. Repro lane (now installed in WSL):
   `cargo test --lib --no-run --target aarch64-unknown-linux-musl
   --no-default-features --features decode,simd` with
   `CARGO_TARGET_..._LINKER=rust-lld`, then `qemu-aarch64 <test-exe>`.
   **Matrix gap found on the way:** the aarch64 legs only ever ran
   `cargo check`, which does NOT compile `#[cfg(test)]` code — the encode
   oracle mod has ≥3 tests calling x86-only kernels without an x86_64 cfg
   (encode/blocks/simd.rs ~:2499-2574), so `cargo test --no-run --target
   aarch64` fails for the full feature set. Fix is a few cfg attrs +
   adding a test-compile leg to the matrix (owner: whoever holds
   encode/blocks/simd.rs).
2. `bc1/bc2/bc3_blocks` (`pshufb`→`vqtbl1q_u8`; the selector tables port unchanged;
   dispatch already hoisted; `crate::simd_tables`' gate is ALREADY widened to
   `any(x86_64, aarch64)` in anticipation).
3. BC6H half conversion — FP16 conversion is **baseline** on ARMv8-A, no detection
   needed; `bc6h_planar_to_rgba`'s transpose collapses into `vst4q_f32`.
4. `planar_avx2` (`vld4q_u8` does the whole deinterleave in one instruction) and
   the min/max reductions (`vminvq_u8`/`vmaxvq_u8` are single instructions).
5. `bc5_gather` last — the `pdep` unpack has no NEON equivalent and needs a
   shift/`tbl` rewrite (which also retires the Zen-class CPUID hazard pattern).
Encoder mirrors follow decode by value. Every port lands under the same
`*_matches_scalar` oracle — and now actually EXECUTED via the qemu lane
above, not merely cross-checked.

### 5.3b Five adjacent small wins (identified 2026-08-26, same area as stage 1 / the NEON track)

Identified during the stage-1 port, not landed; claim as usual.

1. **Stage 2, ready to go:** `bc1/bc2/bc3_blocks` NEON mirrors — `pshufb` is
   `vqtbl1q_u8` near-verbatim, `BC1_SEL`/`BC2_ALPHA`/`BC3_SEL` port
   unchanged, dispatch is already hoisted to surface scope, and the shared
   table module's cfg is already widened. The biggest remaining aarch64
   population win (whole-surface LDR decode).
2. **BC6H half conversion with ZERO detection:** FP16 converts are baseline
   ARMv8-A — `half48_to_f32`'s mirror needs no probe at all, and
   `bc6h_planar_to_rgba`'s four-shuffle transpose collapses into one
   `vst4q_f32` interleaving store. (Respect the D5 lesson: wire it with the
   per-block call structure, ABBA before restructuring the loop.)
3. **`vld4q_u8` deinterleave class:** the AoS→SoA plane split that costs
   `planar_avx2` a shuffle cascade is a single instruction on NEON — one
   kernel serves the future encode-side ports (E13's wiring class) and the
   BC4/5 channel extraction.
4. **Single-instruction horizontal reductions:** `vminvq_u8`/`vmaxvq_u8`
   replace the x86 fold-shuffle ladders in the extrema/channel_minmax
   family — the encode-side mirrors get SIMPLER than their x86 originals.
5. **`bc5_gather` without `pdep`:** the shift/`tbl` index unpack rewrite —
   required on NEON anyway (no `pdep` exists) — also retires the whole
   Zen-microcode CPUID hazard pattern (`has_fast_pdep`) if back-ported to
   x86, collapsing two dispatch predicates into one.

## 6. Ruled out

- **BC7 index/weight bit extraction (decode)** — mode 6 ceiling-probed at ~2.5%
  (`bcn.rs:2441-2445`). Do not re-attempt without a per-mode probe. (BC6H's
  unprobed twin D7 stays a candidate until probed.)
- **Batching the BC1 lattice's candidate sweep into one kernel crossing** —
  tried both ways 2026-08-26 and refuted (see the §5.2 rank-8 row for the
  numbers and mechanism): the fit core amortizes its own boundary. The macro
  extraction it produced is kept; the batching is not. Do not re-attempt on
  any kernel of this weight without a per-fit cost model.
- **RDO dedup-scan vectorized `contains`** — tried and refuted in-source
  (`rdo.rs:651-655`); the Bloom filter already mitigates. `score_bc1`/`score_bc7`
  window scans are the same shape at lower frequency — re-read that refutation
  before attempting.
- **BC6H conversion-fused-into-scatter** — tried and lost (`bc6h.rs:87-96`); only
  the vectorized-scatter form (`bc6h_planar_to_rgba`) won.
- **memcpy/memset-shaped loops** — RGBA8 passthrough, depth-slice extends,
  buffer zero-fills: already lower to vectorized intrinsics. (The rgba8 `Vec`
  allocation loss vs DirectXTex is a memory-copies item, not SIMD.)
- **Bit-depth expansion (565/4444/5551/10-10-10-2/luminance)** — the loops do not
  exist; the crate decodes only Rgba8/Bgra8 uncompressed. Future work if format
  support expands.
- **Small fixed-trip argmins** — `quantize10_for_half` (5 candidates → LUT),
  `quantize6p` (LUT), 3×3 power iteration, `bc7_bd3/bd4` (auto-vec or trivial):
  below the boundary-cost threshold this crate has repeatedly measured
  (`rdo.rs:414-420`).
- **Driver/edge loops** — `blit_rgba4`, `gather_block` edge path, mode-dispatch
  surface loops (data-dependent branch, nothing to vectorize), mode 4/5 rotation
  scatter (8 elements).
- **Oracle/test-only code** — `reference.rs`, `oracles.rs`, `#[cfg(test)]`
  helpers: excluded throughout (though `reference.rs:78-80` serves as D4's oracle).
