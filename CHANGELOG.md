# Changelog

All notable changes to `rusty_dds`. Dates are release dates; every performance
figure is reproducible from the repo with the command given beside it.

## 0.3.14 - 2026-08-19

**Vectorised palette gather for BC5.** An 8-entry palette looked up sixteen
times per channel is exactly what `pshufb` does in one instruction.

### The cost, isolated

0.3.13 left the gather untouched and unmeasured. Isolating it properly:

| probe | Mpx/s | implies |
|---|---:|---|
| full | ~371 | - |
| index math kept, **table lookup removed** | ~655 | lookup ~43% |
| both removed | ~789 | index math ~10% |

An earlier probe had suggested the reverse - that the index arithmetic dominated
- but it replaced the lookup index with a loop constant, so the compiler folded
the load away *and* the arithmetic. A probe that removes more than it means to
does not isolate anything.

### Performance

Eight samples per arm, alternating order: **378.2 -> 489.9 Mpx/s, +29.5%.**

Against Microsoft DirectXTex on a cooked 1024^2 pack: **BC5U 652.8 vs 61.2 Mpx/s
- 10.66x**, up from 7.62x; **7.16x** across all formats.

### Two things that did not work

- **Palette in a register instead of memory.** Holding the eight entries in a
  `u64` and selecting with `>> (8 * idx)` to avoid a load measured **9.8%
  slower**. An L1-resident table indexed by a computed value pipelines better
  than a dependent multiply-then-variable-shift chain.
- **Index bytes via a `[u8; 16]` array.** Building the sixteen index bytes into
  an array and loading it as a vector is a store-forwarding stall - sixteen
  narrow stores feeding one wide load - and gave only +11.6% with overlapping
  arms. Building the vector in registers with `pdep` gave +25% cleanly.

### Notes on the feature gate

The kernel needs SSSE3 (`pshufb`) **and** BMI2 (`pdep`), and additionally needs
`pdep` to be a real instruction rather than microcode.

**On AMD Zen 1 and Zen 2, `pdep` is microcoded at ~18 cycles** against 3 on Intel
Haswell-and-later and AMD Zen 3-and-later. The kernel issues four per block
against a block budget near 100 cycles, so enabling it there would be a large
*regression* on hardware that advertises BMI2. The gate therefore checks CPU
vendor and family via `cpuid` and refuses AMD families below 0x19 (Zen 3).

A portable register-only index unpack was measured as an alternative and is
**neutral against the scalar path**, so it is not shipped - the win genuinely
depends on fast `pdep`.

- Decode output is unchanged, bit for bit; the gather is verified against the
  scalar lookup over 5 000 randomised palette and index combinations.
- SIMD path 94 tests, scalar fallback 88 (`--no-default-features`).

## 0.3.13 - 2026-08-19

**BC5 writes a block row per store.** 0.3.12 left BC5 as the one BCn format that
did not improve. Decomposing where its time went, rather than guessing, found
the cause.

### Where BC5 time goes

Stubbing each stage in turn:

| probe | Mpx/s | implies |
|---|---:|---|
| full | 314.4 | - |
| palette build removed | 372.4 | palette ~16% |
| per-pixel index reads removed | 576.6 | index + palette gather ~45% |

BC5 performs two index extractions and two palette lookups per pixel where BC4
does one of each, which is why it sat at 314 against BC4's 588.

### The fix

Writing a whole block row in one store - four bytes x four pixels - instead of
four separately range-checked four-byte stores.

Ten samples per arm, alternating order:

| format | before | after | |
|---|---:|---:|---|
| **BC5U** | 291.4 Mpx/s | **392.4** | **+34.7%**, no overlap |
| BC1 | 582.7 | 626.9 | +7.6%, overlapping - not resolvable |
| BC4U | 581.6 | 578.5 | neutral |

Only BC5 improves decisively. Its two-channel word build evidently blocks a store
coalescing that LLVM already performs for the single-channel formats, so BC5 was
the only one still paying four range-checks per row. The change is applied
uniformly anyway - identical shape, no regression anywhere.

Against Microsoft DirectXTex on a cooked 1024^2 pack: **BC5U 451.2 vs 59.2 Mpx/s
(7.62x)**, up from 5.53x; 6.61x across all formats.

### Notes

- Decode output is unchanged, bit for bit; the BC4/BC5 oracle tests (30 000
  blocks each, signed and unsigned) pass unchanged.
- Both the SIMD path (93 tests) and the scalar fallback (88 tests,
  `--no-default-features`) pass.

## 0.3.12 - 2026-08-19

**BC1 through BC5 now decode in-house.** The same serial dependency chain that
dominated BC7 before 0.3.6 was in these formats too: the reference walks the
index word with `indices >>= 2` (BC1/BC2) or `>>= 3` (BC3/BC4/BC5) after every
pixel, so sixteen index reads cannot overlap. Reading each by computed offset
from an immutable word makes all sixteen independent.

BC4 and BC5 additionally decoded in **two passes** - sixteen single-channel bytes
first, then a second pass expanding them to RGBA. That is now one pass of packed
word stores.

### Ceiling first

Following 0.3.11, the headroom was measured before anything was written, by
stubbing the block decoders out entirely:

| format | before | ceiling | block decode share |
|---|---:|---:|---:|
| BC1 | 621.6 Mpx/s | 1216.1 | 49% |
| BC5U | 404.4 | 671.9 | 40% |
| BC4U | 498.1 | 683.8 | 27% |

Unlike BC7 mode 6, where the equivalent measurement showed 2.5%, there was real
room here.

### Performance

Six samples per arm, alternating order:

| format | before | after | |
|---|---:|---:|---|
| **BC4U** | 441.4 Mpx/s | **669.3** | **+51.6%** (no overlap) |
| **BC1** | 554.3 | **660.6** | **+19.2%** (no overlap) |
| BC5U | 335.4 | 341.3 | +1.8%, neutral |

BC5 does not move. Its change is kept because it removes a pass and shares one
implementation with BC4 rather than because it is faster - stated here rather
than counted as a win.

Against Microsoft DirectXTex on a cooked 1024^2 pack: **BC4U 842.6 vs 102.0
Mpx/s (8.26x)**, up from 5.34x; 6.69x across all formats.

### Notes

- **BC3 alpha is not BC4 alpha.** BC4 interpolates with fixed-point weights
  (`>> 16`); BC3 alpha uses integer division by 7 and 5. They disagree - for
  `a0 = 60, a1 = 133` the four-interpolant entry is 74 by division and 75 by
  weights. The reference makes the same distinction, so both forms are kept. The
  oracle test caught this the moment BC3 was wired to the wrong one.
- Verified bit-identical to the general decoder: 40 000 BC1 blocks across both
  endpoint orderings and opaque mode, 30 000 each for BC2 and BC3, and 30 000
  each for BC4 and BC5 in both signed and unsigned form, plus the all-zero,
  all-ones, `c0 == c1`, `a0 == a1` and `-128`-clamp cases.
- BC2 and BC3 share the fixed colour block, and BC3 the independent index reads,
  but neither appears in our packs so neither has a real-content measurement.
- Decode output is unchanged, bit for bit.

## 0.3.11 - 2026-08-19

**Index fields read as `u64` instead of `u128`.** Every BC7 index region is at
most 47 bits, so one wide shift down to `u64` replaces sixteen `u128` shifts. On
x86_64 a `u128` shift with a runtime-variable amount is a multi-instruction
sequence; the `u64` equivalent is one instruction.

This matters exactly where the shift amount is *not* a compile-time constant -
the multi-subset modes, whose index offsets depend on the partition anchor.

### Performance

Six samples per arm, alternating order:

| mode | before | after | |
|---|---:|---:|---|
| 0 | 354.7 Mpx/s | **425.9** | **+20.1%** (no overlap) |
| 3 | 526.5 | **589.7** | +12.0% |
| 1 | 515.1 | **571.4** | +10.9% |

**Mode 6 is unchanged**, and that is expected: its shift amounts are
compile-time constants in an unrolled loop, which LLVM had already folded.

### Notes on what did not work

Mode 6 was the target of this round and it did not move. Two attempts, both
refuted by measurement and both recorded at the site:

- **Normalising the fix-up index away** so all sixteen indices are uniformly four
  bits, removing a per-pixel branch: eight samples per arm, 321.9 vs 326.8 Mpx/s.
  The branch was constant-folded by the unroller, so this cost three real
  operations to remove one that did not exist.
- **Attacking the weight lookup at all.** Replacing the entire per-pixel lookup
  with a constant - the absolute ceiling for any gather, vectorised or not -
  measures **345.7 Mpx/s against 336.9 with it. The whole weight extraction is
  worth ~2.5%.**

Mode 6 is at the limit of this approach. Since our own packs are 70-88% mode 6,
whole-content figures do not move in this release either; the gain is for content
using the multi-subset modes.

- Decode output is unchanged, bit for bit. Both the SIMD path (88 tests) and the
  scalar fallback (83 tests, `--no-default-features`) pass.

## 0.3.10 - 2026-08-19

**SIMD across the four channels.** Every BC7 mode now interpolates two pixels per
vector operation.

### Why 16-bit lanes, and why SSE2

0.3.9 rearranged interpolation to `base + w * delta`. That did more than halve
the multiply count - it also bounded every intermediate:

| term | range | fits `i16` |
|---|---|---|
| `base` = `e0 * 64 + 32` | `32 ..= 16_352` | yes |
| `delta` = `e1 - e0` | `-255 ..= 255` | yes |
| `w * delta` | `-16_320 ..= 16_320` | yes, so `mullo` is exact |
| `base + w * delta` | `32 ..= 16_352` | yes |

Sixteen-bit lanes therefore hold **eight channels per register** - two whole
pixels - instead of four. The same rearrangement that halved the multiplies also
doubled the lane count.

The kernel is **SSE2**, which is baseline on x86_64: no runtime detection, no
second code path, and the path that ships is the path that is tested. (The
encoder AVX2 kernels are runtime-detected because AVX2 is not guaranteed;
nothing here needs that.)

### Performance

Per mode, 256^2 serial:

| mode | 0.3.9 | 0.3.10 | |
|---|---:|---:|---|
| 5 | 356.7 Mpx/s | **688.5** | +93% |
| 0 | 218.8 | **387.1** | +77% |
| 2 | 220.2 | **387.3** | +76% |
| 4 | 312.0 | **541.6** | +74% |
| 3 | 349.7 | **541.7** | +55% |
| 7 | 313.9 | **488.1** | +55% |
| 1 | 347.1 | **526.5** | +52% |
| 6 | 280.9 | 336.9 | +20% |

**On a real 192-texture pack**, four ABBA samples per arm with no overlap between
arms: **256^2 254.3 -> 324.6 Mpx/s (+27.6%)** and **128^2 251.2 -> 315.8
(+25.7%)**.

Against Microsoft DirectXTex on a cooked 1024^2 pack: **BC7 735.6 vs 70.6 Mpx/s -
10.42x**, and 6.21x across all formats.

### Notes

- Modes 4 and 5 carry two index sets, so colour and alpha take different weights.
  Their kernel builds the weight vector with the alpha-weighted value in the lane
  the rotation names, and the rotation itself is resolved into the packed
  base/delta before the vector op rather than per pixel.
- Mode 6 gains least. It is the only mode with 4-bit indices, so sixteen wider
  weight extractions now dominate what is left.
- **This corrects 0.3.9.** Modes 0 and 2 were described there as
  partition-lookup bound, on the evidence that two optimisations had failed to
  move them. They gained 77% and 76% here, so they were interpolation bound all
  along - the scalar work simply had not moved enough throughput to show it.
- Decode output is unchanged, bit for bit. The vector kernel is checked against
  the scalar expression across the full endpoint and weight domain, and every
  per-mode oracle test against the general decoder passed unchanged.
- Non-x86_64 targets keep the scalar path, which is compiled and tested via
  `--no-default-features`.

## 0.3.9 - 2026-08-19

**One multiply per channel instead of two.** The BC7 spec writes interpolation
as `(e0 * (64 - w) + e1 * w + 32) >> 6` - two multiplies per channel, both
depending on the per-pixel weight. It is exactly equal to:

```text
(e0 * 64 + 32 + w * (e1 - e0)) >> 6
```

where `base = e0 * 64 + 32` and `delta = e1 - e0` are constant for the whole
block. Sixteen pixels times four channels means **128 multiplies become 64**,
and the base/delta pair is computed once per endpoint pair.

### Performance

Per mode, 256^2 serial:

| mode | 0.3.8 | 0.3.9 | |
|---|---:|---:|---|
| 5 | 262.4 Mpx/s | **356.7** | +36% |
| 6 | 216.8 | **280.9** | +30% |
| 4 | 243.3 | 312.0 | +28% |
| 1 | 275.8 | **347.1** | +26% |
| 3 | 279.4 | **349.7** | +25% |
| 7 | 252.8 | 313.9 | +24% |
| 2 | 213.9 | 220.2 | +3% |
| 0 | 215.0 | 218.8 | +2% |

The three-subset modes barely move: their cost is the per-pixel partition lookup
and six endpoint pairs, not the interpolation.

**On a real 192-texture pack**, four ABBA samples per arm with no overlap between
arms: **256^2 240.6 -> 273.7 Mpx/s (+13.8%)** and **128^2 242.3 -> 274.6
(+13.3%)**. That is nearly double the whole-content gain 0.3.8 reported, and it
comes almost entirely from mode 6, which is 88% of that pack.

Against Microsoft DirectXTex on a cooked 1024^2 pack: **BC7 676.7 vs 79.3 Mpx/s
(8.53x)**, 5.83x across all formats.

### Notes

- Identical arithmetic, so decode output is unchanged bit for bit. The per-mode
  oracle tests, which compare every mode against the general decoder, passed
  unchanged.

## 0.3.8 — 2026-08-19

**Every BC7 mode now has a specialised decoder.** Modes 0, 2, 4 and 5 join 1, 3,
6 and 7; `bcdec_rs::bc7` is now reached only for the reserved encoding, which it
zero-fills per spec.

### Performance

Per mode, isolated on all-mode-N surfaces, alternating-order ABBA:

| mode | general | specialised | |
|---|---:|---:|---|
| 4 | 146.7 Mpx/s | 253.8 | **+73%** |
| 5 | 158.2 | 261.0 | **+65%** |
| 7 | 164.6 | 250.4 | +52% |
| 3 | 180.3 | 248.7 | +38% |
| 1 | 187.9 | 245.7 | +31% |
| 2 | 163.4 | 202.1 | +24% |
| 0 | 158.4 | 194.1 | +22% |
| 6 | 205.9 | ~244 | +18% |

On a real 192-texture pack at 256^2: **207.8 -> 223.0 Mpx/s, +7.3%**, four ABBA
samples per arm with no overlap. That is the honest whole-content figure; the
pack is 88% mode 6, which was already specialised, so most of the gain here comes
from mode 5 at 9.4% share.

Against Microsoft DirectXTex on a cooked 1024^2 pack: **BC7 503.5 vs 58.4 Mpx/s
(8.62x)**, and **6.30x** across all formats.

### Fixed

- **Dispatch.** Chaining eight per-mode probes with `||`, each `#[inline]`,
  measured **8-10% slower on real content** than before modes 0/2/4/5 existed —
  a net regression despite every isolated mode being faster. Eight inlined
  decoders blow the block loop's instruction footprint, and a mode-5 block paid
  seven failed probes before being claimed. Now one `trailing_zeros` and a
  `match` (a jump table), with the decoders out of line.

  The per-mode benchmarks could not see this: each exercises one decoder and
  never pays for the other seven being resident.

### Notes

- Corrects 0.3.6's note that mode 5 does not benefit from specialisation. It
  does — by 65%. The earlier attempt resolved the rotation with a conditional
  swap **inside** the per-pixel loop; hoisting it into a channel map computed
  once is the whole difference.
- Verified bit-identical to the general decoder for every mode: all partitions
  each mode can address (16 for mode 0, 64 for modes 1/2/3/7), every rotation and
  index-selection combination for modes 4 and 5, plus all-zero and all-ones
  payloads. Partition tables asserted against the spec constants.
- Decode output is unchanged, bit for bit.

## 0.3.7 — 2026-08-19

**Specialised BC7 decoder for mode 7**, the last two-subset mode and the first
fast path carrying real alpha (RGBA 5.5.5.5, four unique p-bits, one 2-bit index
set). Isolated, alternating-order ABBA, four samples per arm:

| mode | general | specialised | |
|---|---:|---:|---|
| 7 | 162.5-165.9 Mpx/s | **237.7-257.7** | **+52%** |

The largest single-mode gain of the campaign, with no overlap between arms.

### Where BC7 decode now stands

Per-mode, 256^2, serial:

| mode | Mpx/s | |
|---|---:|---|
| 3 | 301.9 | specialised |
| 1 | 289.0 | specialised |
| 7 | 257.8 | specialised |
| 6 | 236.2 | specialised |
| 2 | 162.2 | general |
| 5 | 160.5 | general |
| 4 | 152.7 | general |
| 0 | 152.5 | general |

The split is bimodal, and that is the finding: **every specialised mode lands at
236-302 Mpx/s regardless of how fast it was before.** Mode 6 started fastest
(205.9) and gained least; mode 7 started slowest (164.6) and gained most. The
general decoder's whole 152-206 spread was per-pixel bitstream and dispatch
overhead, not the intrinsic cost of the mode.

### Notes

- Verified bit-identical to the general decoder across all **64 partitions x 200
  randomised blocks**, plus all-zero and all-ones payloads. Mode 7 is the only
  two-subset fast path with alpha, so a wrong p-bit or component offset would
  surface in the alpha channel alone — the oracle covers it.
- As with mode 3, **packs cooked by this crate contain no mode 7 at all**; our
  encoder emits modes 1, 5 and 6. This pays on content from compressors that use
  it. Shipped on the isolated measurement, stated plainly here.
- Decode output is unchanged, bit for bit.

## 0.3.6 — 2026-08-18

**Specialised BC7 decoders for the two-subset modes 1 and 3.** Profiling the
general decoder per mode found the real cost, and it is not the partition
lookup: `bcdec_rs` reads pixel indices through a stateful bitstream where every
read mutates the cursor, so sixteen index reads form a **sixteen-deep serial
dependency chain**. Reading each index by computed offset from an immutable
`u128` makes all sixteen independent.

### Performance

Isolated on all-mode-N surfaces, alternating-order ABBA, four samples per arm:

| mode | general | specialised | |
|---|---:|---:|---|
| 1 | 185-191 Mpx/s | **242-253** | **+31%** |
| 3 | 171-189 | **245-252** | **+38%** |

Larger than mode 6 got in 0.3.5 (+18%), because the two-subset modes carried
more of that serial read overhead to begin with.

### Notes

- **On packs cooked by this crate the change is not measurable**, because our own
  encoder emits ~88% mode 6 and no mode 3 at all. The gain applies to content
  whose encoder favours the two-subset modes; how much you see depends entirely
  on what compressed your textures.
- Verified bit-identical to the general decoder across **all 64 partitions x 200
  randomised blocks x 2 modes**, plus the all-zero and all-ones payloads, with
  every non-matching mode asserted declined rather than mis-decoded.
- The partition tables are asserted against the spec values (0xCCCC, 0x8888,
  0xEEEE), not merely against themselves: subset 0 must own pixel 0, every
  partition must use both subsets, and each anchor must belong to subset 1.
- Decode output is unchanged, bit for bit.

## 0.3.5 — 2026-08-18

**A specialised BC7 mode-6 block decoder.** Mode 6 is **87.8%** of the blocks in
a real 192-texture pack: one subset, so no partition-table lookup and no
per-pixel subset branch; RGBA 7.7.7.7 endpoints with one p-bit each; sixteen
contiguous 4-bit indices. The general decoder pays a bitstream reader, a
partition lookup and an index-width branch *per pixel* to stay general across
all eight modes. For mode 6 all of that is loop-invariant.

Anything that is not mode 6 falls through to the general decoder untouched.

### Performance

ABAB against the previous code, serial, into a recycled buffer:

| surface | general | mode-6 path | |
|---|---:|---:|---|
| 1024^2 | 707-771 Mpx/s | 727-811 Mpx/s | no change |
| 256^2 | 201-206 | 235-242 | **+17%** |
| 128^2 | 200-203 | 242-258 | **+24%** |
| 64^2 | 196-220 | 254-261 | **+23%** |

At 1024^2 BC7 decode is **memory-bandwidth bound** — it scales only 3.7x on 24
cores — so no amount of saved ALU work shows up there. The gain is real once the
surface fits in cache, which is where a streamer decoding full mip chains spends
most of its decode time.

Against Microsoft DirectXTex at 1024^2, cooked pack: BC7 **392.0 vs 59.1 Mpx/s**,
and **5.35x** across all formats.

### Notes

- Verified bit-identical to the general decoder on **20 000** randomised mode-6
  blocks plus the all-zero and all-ones payloads, and every non-mode-6 encoding
  is declined rather than mis-decoded.
- Decode output is unchanged, bit for bit.

## 0.3.4 — 2026-08-18

**The full decode matrix against DirectXTex.** 0.3.3 compared HDR decode 1:1 and
found 3.75x. The LDR half of that comparison had never been run — both providers
implemented it and nothing called them. Running it produced a competitive
picture and one migration hazard worth documenting.

| format, mip 0 | rusty_dds | DirectXTex | ratio |
|---|---:|---:|---:|
| BC1 | 684.7 Mpx/s | 107.8 Mpx/s | **6.35x** |
| BC5U | 421.6 Mpx/s | 72.8 Mpx/s | **5.79x** |
| BC4U | 543.4 Mpx/s | 98.2 Mpx/s | **5.53x** |
| BC6H | 114.8 Mpx/s | 31.3 Mpx/s | **3.67x** |
| BC7 | 263.2 Mpx/s | 72.6 Mpx/s | **3.63x** |
| **all** | | | **4.82x** |

### Documentation

- **BC4 and BC5 channel conventions are now documented on `decode_rgba8`.** We
  decode BC4 to `(R, 0, 0, 255)` — what a GPU returns when sampling it.
  DirectXTex **replicates** the single channel to `(R, R, R, 255)`, a
  greyscale-viewer convention. Over a 512^2 surface the two agree on R and A for
  all 262 144 pixels and disagree on G and B for all of them.

  Neither is wrong, but nothing warned about it: porting from
  `DirectXTex::Decompress` turns every roughness and height map red. DirectXTex
  does not replicate for BC5, so only BC4 is affected. Behaviour is unchanged —
  this documents what was always true.

## 0.3.3 — 2026-08-18

**The BC6H conversion tail.** With buffer restructuring exhausted, the only
remaining cost in HDR decode was inside the block decoder. Splitting it apart
showed `bcdec_rs::bc6h_float` is `bc6h_half` plus 48 half-to-float conversions
carrying two branches each — 15.5% of the call. Taking the halves directly and
converting them branchlessly recovers most of it.

### Performance

1024^2 BC6H_UF16, measured ABAB against the previous code, not against a
remembered number:

| | 0.3.2 | 0.3.3 |
|---|---:|---:|
| serial | 12.103 / 11.455 ms | **10.780 / 10.629 ms** |
| 24-thread caller split | 1.839 / 1.888 ms | **1.605 / 1.607 ms** |
| throughput, split | 555.5 / 570.3 Mpx/s | **653.2 / 652.6 Mpx/s** |

Against Microsoft DirectXTex on a cooked HDR pack, all mips: **108.4 Mpx/s
against 28.9 — 3.75x**, up from 3.30x, pixels verified equal first.

Cumulative since 0.3.0, 1024^2 BC6H: **26.428 ms to 1.605 ms, 16.5x.**

### Changed

- `decode_rgba_f32` now calls `bcdec_rs::bc6h_half` and converts to `f32`
  itself, with a branchless IEEE binary16 to binary32 conversion verified
  exhaustively against the reference for **all 65 536 input bit patterns**,
  including Inf, NaN, denormals and negative zero.

### Notes

- The conversion runs as its own tight pass, **not** folded into the strided
  RGBA scatter. Fusing them is one pass instead of two and measured *slower*
  (1.72 ms against 1.61 ms): 48 independent conversions vectorise, a strided
  read/write with the conversion inline does not. Recorded in a code comment so
  it is not retried.
- Decode output is unchanged, bit for bit.

## 0.3.2 — 2026-08-18

**BC6H could not be uploaded to a GPU.** Wiring HDR content into the streaming
simulator immediately failed on `open`, and the cause was in this crate: BC6H
had no entry in the GPU format table at all. A format rusty_dds can decode *and*
encode could not be handed to a renderer — `gpu_format`, and therefore
`upload_plan_compressed`, failed closed with `UnsupportedFormat` on every HDR
texture.

This is the same blind spot 0.3.1 fixed one layer down: nothing in the harness
cooked BC6H, so nothing ever asked to upload it.

### Fixed

- **`BC6H_UF16` / `BC6H_SF16` added to `gpu_format`** (`Bc6hRgbUfloat` /
  `Bc6hRgbFloat`, `VK_FORMAT_BC6H_UFLOAT_BLOCK` / `..._SFLOAT_BLOCK`, 16-byte
  blocks). HDR textures now plan uploads like any other compressed format.

### Documentation

- `decode_block_rows_f32_into` now states the split threshold, with the numbers
  behind it. Splitting a **whole mip chain** across 24 threads measured **0.53x
  — slower than serial** on a cooked 512^2 pack, because a ten-level chain is
  mostly small mips and entering `std::thread::scope` costs ~50 us even for one
  worker. Splitting only above ~16 384 blocks (512x512) turns the same pack into
  **1.35x**. The 6.8x figure from 0.3.1 is mip 0 at 1024^2; both are true, and a
  caller needs to know which one applies.

## 0.3.1 — 2026-08-18

**BC6H, the last unoptimised decode.** Profiling the whole format matrix found
HDR decode running at 39.7 Mpx/s against BC1's 337 and BC7's ~400 — a 10x gap,
on the most expensive format we ship. **1024^2 BC6H: 26.428 ms to 2.743 ms, 9.6x.**
Output is bit-identical; a test asserts that at every split point.

### Added

- **`decode_rgba_f32_into`** — decode HDR into a buffer you own and recycle. This
  output is 16 bytes a pixel, four times RGBA8, so the buffer the OS zeroes for
  you and the decoder immediately overwrites is 16 MiB on a 1024^2 surface.
- **`decode_block_rows_f32_into` / `block_rows_f32`** — the caller-parallel seam,
  the HDR twin of `decode_block_rows_into`. BC6H has no internal thread pool and
  deliberately does not grow one: a texture library that seizes cores inside a
  frame is a library an engine has to work around.

### Performance

1024^2 BC6H_UF16, 24 cores:

| | time | throughput |
|---|---:|---:|
| 0.3.0 | 26.428 ms | 39.7 Mpx/s |
| fused single pass | 18.691 ms | 56.1 Mpx/s |
| `decode_rgba_f32_into` | 11.941 ms | 87.8 Mpx/s |
| **`decode_block_rows_f32_into`, 24 threads** | **2.743 ms** | **382.3 Mpx/s** |

### Fixed

- `decode_bc6h` built a full-surface RGB plane and then made a **second pass**
  over it to widen to RGBA. At 1024^2 that was 12 MiB written, 12 MiB read back
  and 16 MiB written again, for a 16 MiB result. Now one fused pass through a
  192-byte block scratch that never leaves L1. The tell was throughput *falling*
  with surface size — 56.8 / 49.2 / 39.7 Mpx/s at 256/512/1024 — which is a cache
  cliff, not decode cost. It now flattens: 76.9 / 62.7 / 56.1.

### Notes

- Purely additive; every existing call is unchanged. MSRV remains 1.73.
- Splitting is the **caller's** call: at 256^2 a 24-thread split is 0.56x, because
  spawn cost dominates. The seam exists so your scheduler decides, not ours.

## 0.3.0 — 2026-08-18

**The runtime streaming path.** A texture-streaming simulator
([`sim/`](sim/)) measured this crate against Microsoft DirectXTex on D3D11 and
Vulkan and found rusty_dds *behind* on the profile a running game actually
exercises. This release closes that gap. Encoder output is unchanged.

### Added

- **`DdsView<'a>` — zero-copy parse.** `Dds` is now `DdsBase<Vec<u8>>` and
  `DdsView<'a>` is `DdsBase<&'a [u8]>`, sharing one implementation. Every
  existing call is unchanged. `DdsView::parse(&bytes)` allocates **nothing**.
- **`DdsView::read_into` / `read_into_limited`** — read from any reader into a
  buffer you recycle, for callers that cannot borrow (archive, network).
- **`decode_rgba8_into`** — decode into your buffer instead of a fresh one.
- **`decode_block_rows_into` / `block_rows`** — decode a range of block rows, so
  your job system parallelises the work and the library owns no threads.

### Performance

Measured pinned, ABBA-interleaved, N=7, 192 textures, 10 500 frames; every run
gated on byte-identical uploaded data.

| | before | after |
|---|---:|---:|
| Container parse, total | 433.4 ms | **1.5 ms** |
| Allocations per run | 263 112 | **45 162** (DirectXTex: 45 162) |
| `decode_rgba8` (1024² BC7) | 2.184 ms | **1.158 ms** via `_into` |
| Payload copy | 1 per open | **0** with `DdsView` |

Root cause, in one line: `Dds::read` allocated a fresh payload buffer per open,
and ~87% of that call was the operating system faulting in and zeroing pages the
copy then overwrote. `DdsView` does not copy; `read_into` reuses warm pages.

### Fixed

- **BC7 parallel decode threshold** was 4 096 blocks — precisely the size where
  spawning a thread per core is a **net 2.26× loss**. Raised to 16 384, the
  smallest size where parallelism is measured to win. At 4 096 blocks the call
  drops from 75 allocations to 1.
- `std::thread::available_parallelism()` was a syscall on every decode; cached.
- Internal format queries allocated a `Box<dyn DataFormat>` — **12 per
  `upload_plan_compressed`**, now zero, via an allocation-free `FormatOf`.
- `upload_plan_compressed` computed the subresource range twice.
- `decode_rgba_f32` (BC6H HDR) built the whole surface a second time even for a
  single-slice 2D texture, the only shape anyone decodes. The LDR path had always
  short-circuited `depth == 1`; this one had not. On 256^2: **3 allocations and
  2.75 MiB down to 2 and 1.75 MiB** for a 1.00 MiB output.

### Notes

- No behaviour change: the simulator's whole-run trace hash is identical before
  and after, and the decode/encode matrices are unchanged.
- MSRV remains 1.73. `Dds` keeps its name and its `data: Vec<u8>` field.

### Also in 0.3.0 — the API and hardening pass

Landed before the runtime campaign and released here for the first time. The
encoder's output is unchanged — byte-identical on
all 22 payload hashes in the new `tests/encode_determinism.rs`, verified
against the 0.2.0 tree — but how it is *configured*, and how the parser behaves
on bytes it did not create, both changed.

### Added

- **`Rdo` — a typed RDO API.** `EncodeLayout::with_rdo(Rdo::lambda(4.0))`
  replaces the `RUSTY_DDS_RDO_LAMBDA` environment variable. The old design was
  not merely undiscoverable: it was **racy**. Lambda was read from process-global
  environment on every encode call, so two threads encoding at different
  strengths silently overwrote each other's setting — reproduced by running the
  determinism suite multi-threaded against 0.2.0, where a λ=4 encode produced a
  λ=0 payload. Lambda now travels in the layout, so the race is structurally
  impossible. `Rdo::Off` is the default and is byte-identical to the plain
  encoder.
- **`Dds::read_limited(r, max_data_len)`** and `Error::SizeLimitExceeded`.
  `Dds::read` reads to end-of-stream uncapped, which is right for a trusted file
  and wrong for a network or mod-archive source; the limited form fails closed
  without buffering the overrun.
- **`tests/encode_determinism.rs`** — a standing byte-identical gate. Payload
  hashes for every format × both quality tiers × RDO, plus repeatability and
  strip-parallel determinism. An output-preserving refactor must leave every
  hash untouched; a deliberate change updates the table in the same commit.
- **`tests/parser_robustness.rs`** — always-on structured fuzzing of the
  untrusted-input surface. Pure Rust, stable toolchain, deterministic, no new
  dependencies. Deep sweep: 150k mutations across every fixture plus 150k
  arbitrary inputs, clean.
- **`fuzz/`** — opt-in cargo-fuzz targets (`parse`, `read_limited`,
  `encode_roundtrip`). A standalone workspace, listed in the package `exclude`,
  so `libfuzzer-sys` and its LLVM C++ runtime can never reach a shipped
  dependency graph. Shares `tests/common/driver.rs` with the stable harness so
  the two cannot drift.
- **`tests/fixtures/regressions/`** — every crashing input, replayed on every
  `cargo test`.
- **`tuning` feature (off by default)** — the only way to reach the `RUSTY_DDS_*`
  encoder overrides. Development only.

### Fixed

- **Four unchecked-arithmetic defects on the untrusted path**, all found by the
  new harness on its first runs, all previously *silent* in release builds
  (a wrapped size goes on to slice the payload):
  - `get_texture_size` — `pitch * row_height * depth` overflowed on hostile
    header dimensions, and `pitch_height == 0` divided by zero.
  - `DxgiFormat::get_pitch` / `D3DFormat::get_pitch` — the same class, one layer
    down, in all three pitch formulas.
  - `get_min_mipmap_size_in_bytes` — `bpp + 7` overflowed on a raw
    `rgb_bit_count` header field.
  - `Dds::get_offset_and_size`, `get_data`, `get_mut_data`, `get_pitch` —
    unchecked `*` and `+` on header-derived values.
- **A header-driven hang.** `get_array_stride` looped `mip_map_count` times with
  no bound, so a file claiming `mip_map_count = 0xFFFF_FFFF` spun for billions of
  iterations on *every* metadata query — reachable from `get_data`,
  `subresource_range`, `surface` and every upload plan. The tail is now closed
  form once the mip size bottoms out.
- **The `rdo` module doctest**, which had never compiled: an indented block in
  the module header was parsed as Rust, so `cargo test` was red on a clean tree.
- Two `unwrap()` calls on a user-reachable encode path replaced with the
  infallible spelling.

### Changed

- **`src/encode/blocks.rs` split** (3188 lines → a 325-line root plus `bc1`,
  `alpha`, `bc7` and a `#[cfg(test)] oracles` module holding the campaign
  scaffolding that used to sit in the encoder core). Byte-identical, proven by
  the determinism gate.
- **Encoder tuning constants are frozen.** `RUSTY_DDS_BC7_M1_T`,
  `BC45U_WINDOW`, `ALPHA_SEL`, `BC1_LATTICE_ROUNDS`, `BC1_LATTICE_T` and the
  BC4/5 refine harvest were live environment reads in shipped builds, so a stray
  variable in a user's shell could silently change a cook's output. They are now
  compile-time constants in `src/encode/tuning.rs`, re-openable only under the
  non-default `tuning` feature.
- **`#[non_exhaustive]`** on the types the crate *produces* or whose variant set
  is owned by an outside authority: `Error`, `DxgiFormat`, `D3DFormat`,
  `DecodeContent`, `HdrDecodeContent`, `EncodeQuality`, `Rdo`, `GpuFormat`,
  `UploadPath`, `UploadPlan`, `SurfaceView`, `SurfaceViewMut`, `EncodeLayout`.
  Deliberately **not** applied to the wire-format mirrors (`Header`, `Header10`,
  `PixelFormat`, `Dds`), whose field sets are fixed by the DDS format itself, nor
  to `CubemapFace` (exactly six faces, forever), nor to the argument bags
  `NewD3dParams` / `NewDxgiParams` and the plain data carriers `ImageRgba8` /
  `ImageRgbaF32`, which callers must construct and which have no builder.
  **Breaking:** build `EncodeLayout` through `flat_2d` + the `with_*` builders,
  and add a `_` arm when matching the marked enums. `EncodeLayout` also loses
  `Eq` (it now carries an `f32`).
- `Cargo.lock` is committed — the crate ships three binaries and the performance
  claims want a pinned graph.
- Docs refreshed: `docs/formats.md` claimed BC6H was deferred and BC7 encode was
  mode 6 only, both untrue since 0.2.0; the plan file said Phase 6 was in flight.

### Verified

- MSRV 1.73 still builds, against that toolchain.
- `wasm32-unknown-unknown`, decode-only, still builds.
- Full suite green: 14 test binaries, including the doctests.

## 0.2.0 — 2026-08-13

The encoder campaign. Against 0.1.2 on a 102-case real-content corpus
(ambientCG PBR + 16 CryTIF from CRYTEK GameSDK + 10 USC-SIPI TIFF):
**89 cases higher PSNR, 0 regressed, ~1.17× less encode CPU** —
`cargo run --release --example bench_encode_corpus`.

### Added

- **BC6H HDR path.** `Dds::decode_rgba_f32` → `ImageRgbaF32` for
  `BC6H_UF16`/`SF16`/`Typeless` across every context (2D / NPOT / mips /
  arrays / volume), and `Dds::encode_bc6h_uf16` (mode 11: single subset,
  10-bit endpoints, 4-bit indices). Polyhaven CC0 HDRIs round-trip at
  48.0–56.6 dB log-PSNR. New public items: `ImageRgbaF32`,
  `HdrDecodeContent`.
- **Rate-distortion optimization (opt-in).** `RUSTY_DDS_RDO_LAMBDA` re-chooses
  blocks among LZ-friendlier candidates under `J = SSE − λ·bytes_saved`, so the
  payload gets smaller *inside the shipping archive*. Candidates are always
  legal BCn, so conformance is free. Measured by deflating the payload:
  BC1 −10.4% at **+0.11 dB**, BC7 −3.9% at **+0.02 dB**; aggressive dials reach
  −15%. `λ=0` (the default) is byte-identical to the normal encoder, verified by
  payload hash on all 102 cases — `--example harvest_rdo`.
- **BC7 modes 1, 4 and 5** alongside mode 6, with rotations. Mode 5/4 decouple
  colour and alpha indices; mode 1 adds two-subset partitioning with a
  harvest-chosen 8-shape shortlist. Largest single-case gain **+13.53 dB**.
- **`simd` feature (default on).** AVX2 twins of the hot index-fit kernels,
  runtime-detected with scalar fallback and proven bit-exact against the scalar
  twins over 200k random cases each — output is identical on every CPU.
- `bench/ab_encode.ps1`, a pinned ABBA A/B harness, and
  `examples/bench_encode_corpus` / `examples/harvest_rdo`.
- `THIRD-PARTY-NOTICES.md` and `docs/commercial-model.md`.

### Changed

- **BC1** gained a PCA-axis seed, iterated least-squares refinement, and a
  565-lattice contract refine: +0.5…+1.6 dB on albedo. Every quality loss the
  0.1 README named against DirectXTex (Bricks/Rock BC1, Wood BC5S) is erased;
  the board now reads 22 higher / 2 tie / 0 lower.
- **BC3 alpha** now runs the full BC4-grade search instead of min/max only:
  +1.8…+3.2 dB on alpha-gradient UI content.
- **BC4/BC5 signed and unsigned** gained a windowed endpoint sweep with a
  provably-safe range-bound prune.
- **BC7 encode ~2× faster** than 0.1.2 (palette precompute, fused SSE, seed
  dedup) despite the added modes.
- The three signed cases that now trail DirectXTex by ~1.10× are named in the
  README rather than omitted; each buys +0.5…+0.7 dB.

### Fixed

- **BC1 inverted-565 mode.** When 565 quantization inverted the endpoint order,
  the packer fitted indices against a 3-colour palette that no decoder
  reconstructs. Now fits the decode-true 4-colour palette.
- **MSRV.** The crate declared `rust-version = "1.73"` but used
  `is_multiple_of` (Rust 1.87) and inline `const {}` blocks (1.79), so it could
  not build on its own stated minimum. Both replaced; the library now builds on
  1.73 for real, verified against that toolchain.
- **Attribution.** The BC7 two-subset partition table is copied verbatim from
  `bcdec_rs` (MIT); the required copyright and permission notice now travels
  with the source in `THIRD-PARTY-NOTICES.md`.
- `harvest_encode_quality_vs_dxtex` scored SNORM reconstructions against a UNORM
  source, under-reporting our own signed formats by ~35 dB.

### Safety

- `#![forbid(unsafe_code)]` is now applied automatically whenever the `simd`
  feature is off, so "no unsafe" is enforced by the compiler rather than
  asserted. With `simd` on, `unsafe` is confined to the `#[target_feature]`
  AVX2 kernels, each behind a runtime CPU check with a scalar oracle in-tree.

## 0.1.2 — 2026-08-11

- Fix docs.rs build.

## 0.1.1 — 2026-08-11

- README cross-links for the Remade With Rust family.

## 0.1.0 — 2026-08-11

- First release: DDS container read/write (ddsfile lineage), LDR decode and
  encode matrix (BC1–BC5 U/S, BC7, RGBA/BGRA × 2D/mips/array/cube/NPOT/volume),
  API-agnostic GPU upload plans, and the DirectXTex corpus boards.
