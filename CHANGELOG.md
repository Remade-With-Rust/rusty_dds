# Changelog

All notable changes to `rusty_dds`. Dates are release dates; every performance
figure is reproducible from the repo with the command given beside it.

## 0.3.35 - 2026-08-19

**BC4 decode +40.4%, BC5 +22.0% — same kernel, moved dispatch.** The last two
decoders still crossing the SIMD boundary per block now cross it once per
surface.

BC1, BC2 and BC3 were hoisted to surface scope in 0.3.28-0.3.30. BC4 and BC5
were not, and it went unnoticed because they **won anyway** (+22.2% and +32% in
0.3.28) — their gathers are heavy enough to carry a per-block
`#[target_feature]` call plus its `OnceLock` check, where BC1's was not (that
boundary measured **26.7% of BC1 decode**, enough to invert the sign of the whole
result). The two were only caught by listing every dispatch site side by side
while answering an unrelated question about a dead one-line wrapper.

Nothing about the gather changed. `bc5_gather_ssse3` now takes a raw destination
instead of a slice, so the surface loop calls it without reborrowing per block,
and because that kernel is itself a `#[target_feature]` function with the same
features it **inlines into the loop** rather than being called.

512^2, pinned, 16 paired CPU samples each, alternating leading arm:

| format | before | after | verdict |
|---|---|---|---|
| BC4 | 0.1077 ms | **0.0642 ms** | 16/16, z = +4.00, **+40.4%** |
| BC5 | 0.1540 ms | **0.1201 ms** | 16/16, z = +4.00, **+22.0%** |

BC1 re-measured flat (z = +0.71). Byte-identical: the surface loops are
oracle-tested against the scalar block decoders over 20 000 random surfaces per
format **in both signedness conventions**, since the signed palette takes the
other `unquantize` branch over `-127..=127` endpoints.

Every BCn decoder now dispatches once per surface.

## 0.3.34 - 2026-08-19

**BC7 encode +17.7%: two exact early-outs, found by asking how often these modes
actually win.** They almost never do.

Both BC7 mode 4 and mode 5 are built from two halves whose squared errors are
each non-negative and sum to the mode's total. So the moment *either half alone*
reaches the incumbent error, the mode cannot win, and abandoning it is exactly
equivalent to finishing it and losing the `<` comparison at the call site.

Counters, per block on alpha-structured content:

| | mode 4 | mode 5 |
|---|---|---|
| attempts that lose | **96%** | **95%** |
| already provably beaten after the *first* half | **69%** | **89%** |

The two modes search their halves in **opposite orders** — mode 4 does colour
then alpha, mode 5 does alpha then colour — so each one gets to skip the half
the other one starts with. Mode 5 now abandons before its entire colour search
(4 fits plus the LS refit) on 89% of blocks; mode 4 abandons before its entire
alpha search (seed, 24-candidate neighbourhood, re-score) on 69%.

512^2, forced serial, pinned, paired CPU samples:

| fixture | before | after | verdict |
|---|---|---|---|
| alpha-structured | 35.4817 ms | **29.2155 ms** | 16/16, z = +4.00, **+17.7%** |
| default | 20.8333 ms | 21.0503 ms | 4/12, z = +0.38, flat |

The default fixture is unmoved, and that is again the mechanism confirming
itself: mode 4 never runs there at all, and mode 5's alpha-side early-out fires
on only 1% of its blocks. **Byte-identical on both fixtures.**

### The general shape

This is not a kernel and not a heuristic. It is the observation that a search
which almost always loses should be asked to *prove it can still win* at the
first point where that is decidable. The cost is one `i64` comparison; the
saving is half a mode. Worth checking wherever a codec tries several modes and
keeps the best.

## 0.3.33 - 2026-08-19

**BC7 encode +10.1%, by deleting work rather than vectorising it.** The next
target was the colour fits' 7.95 kernel crossings per block. Counters found two
redundancies first, and eliminating redundancy comes before vectorising.

### Modes 4 and 5 were computing the same seeds twice

`extrema_opaque`, `channel_minmax_rgb` and `pca_extremes_rgb` are pure functions
of `pixels`, and at rotation 0 modes 4 and 5 run on the *same* pixels. Counters,
per block:

```
extrema=2.00  chan_minmax=2.00  pca=2.00  ls=2.00
```

Every one computed twice. They are now computed once into a `ColorSeeds` and
passed to both modes. Rotations get their own set, because rotated pixels are
different pixels.

(The first attempt to measure this reported `pca=0.00`, which is impossible
against two visible call sites — `pca_extremes_rgb` lives in `bc1.rs`, so the
patch that was supposed to instrument it never matched anything. A counter
reading zero for work that must be happening is a stale instrument, not a
finding.)

### One fit in eight was re-fitting a seed already fitted

The three seeds frequently coincide — measured **0.78 blocks per block** where
the PCA seed equals the extrema seed, and 0.26 where the channel min/max seed
does. Fitting an endpoint pair a second time cannot change anything: the same
endpoints give the same palette, the same indices and the same error, and the
guard is a strict `<`. Those **1.04 fits per block** are now skipped, exactly.

512^2, forced serial, pinned, paired CPU samples:

| fixture | before | after | verdict |
|---|---|---|---|
| alpha-structured | 39.3880 ms | **35.4004 ms** | 16/16, z = +4.00, **+10.1%** |
| default | 21.5929 ms | 21.0503 ms | 7/12, z = +1.26, +2.5% (not significant) |

The default fixture barely moves, and that is the mechanism confirming itself:
mode 4 never runs there, so there is no cross-mode duplication to remove and
only the duplicate-seed skip applies. **Byte-identical on both fixtures.**

### Still open

The colour fits still cross the SIMD boundary once each. That hoist - the
original next step - is unchanged and unattempted.

## 0.3.32 - 2026-08-19

**BC7 encode: +18.5%, by crossing the SIMD boundary 4.2x less often.** And a
correction to 0.3.31, which was measured on content that never ran the code half
of it changed.

### The fixture was not exercising mode 4 at all

Call counters - deterministic, no stopwatch - over 16 384 blocks of the encode
probe's content:

```
m5=13053  m4=0  rgb4=51881  alpha8=321165  alpha4=321165
```

`try_bc7_mode4` was called **exactly zero times**. The probe's alpha is
`0.6 + 0.4xy`, which varies by under one code across a 4-pixel span, so the
`a_hi - a_lo > 2` gate fails on every block; mode 5 only ever reached its
*rotation* path. 0.3.31's +18.7% was real and byte-identical, but it came
entirely from mode 5's rotations - **the mode-4 half of that change was never
executed by the measurement that justified it.**

The probes now carry a `PROBE_ALPHA=1` fixture with genuine per-block alpha
structure. On it both modes run on every block, and the picture is different:

| | default fixture | alpha-structured |
|---|---|---|
| `try_bc7_mode5` per block | 0.797 | 1.000 |
| `try_bc7_mode4` per block | **0.000** | 1.000 |
| kernel crossings per block | 22.8 | **57.95** |

### 50 of those 58 crossings were alpha

Modes 4 and 5 each run a 5x5-minus-centre endpoint search - 25 scans apiece -
and every scan was its own `#[target_feature]` call plus its own `OnceLock`
check. A `#[target_feature]` function cannot be inlined into a caller that lacks
the feature, so those were 50 real calls per block. §49 measured that same
boundary at 26.7% of BC1 decode.

`alpha_nbhd_avx2` now runs a whole neighbourhood inside **one** call. Two things
fall out of hoisting it:

- the sixteen samples are loaded and widened **once**, not twenty-five times;
- the search no longer tracks indices at all. The scalar twin adds
  `(pal[best] - a)^2`, which equals `(min_j |pal[j] - a|)^2` — the error depends
  only on the *minimum distance*, never on which entry achieved it. So the sweep
  needs `_mm256_min_epi16` and no index blending; the winner's indices come from
  one ordinary scan afterwards, which is also what keeps the lowest-index
  tie-break exactly the scalar one.

Mode 5 also stops padding its 4-entry palette out to 8, which had been doubling
the vector work on 86% of all crossings.

Crossings per block, measured: **57.95 -> 13.81**, with the alpha half going
**50.00 -> 5.86** (8.5x).

512^2, forced serial, pinned, paired CPU samples, alternating leading arm:

| fixture | before | after | verdict |
|---|---|---|---|
| alpha-structured | 56.2337 ms | **45.8170 ms** | 16/16, z = +4.00, **+18.5%** |
| default | 28.6458 ms | **24.6311 ms** | 12/12, z = +3.46, **+14.0%** |

**Byte-identical on both fixtures.** The neighbourhood kernel is oracle-tested
against the scalar loop over 60 000 cases per palette width, including flat
alpha (every candidate ties, so the seed must survive) and seeds at the clamp
edges where the offsets saturate.

### Still open

`rgb4` is unchanged at 7.95 crossings per block - the colour fits, four per mode
per block, still cross once each. Same hoist applies and is not attempted here.

## 0.3.31 - 2026-08-19

**BC7 encode modes 4 and 5: +18.7%, by adding no new kernel at all.**

Modes 4 and 5 carried four nearest-palette scans of their own - two colour
(16 pixels x 4 RGB entries), two alpha (16 samples x 8 and x 4 single-channel
entries). All four are the shape this campaign has now paid for five times.

The colour scan turned out to be **character-for-character**
`bc1_fit_4color_scalar`: same `sqr_rgb`, same strict `<`, same lowest-index
tie-break. So both colour fits route through `bc1_fit_4color_avx2` and inherit
its 200 000-case oracle rather than growing a third copy; only the output form
differs, and unpacking sixteen 2-bit fields is cheaper than the scan it
replaces. Mode 4's alpha scan is exactly BC4/BC5's, so it routes through
`alpha_fit_avx2`.

Mode 5's alpha palette has four entries where that kernel wants eight, so
entries 4..8 are filled with entry 0: **under a strict `<` tie-break a later
duplicate can never win**, so scanning eight is exactly scanning four.

512^2, forced serial, pinned, 16 paired CPU samples: **30.6803 ms against
37.7604, 16/16 wins, z = +4.00, +18.7%** (confirmed at +19.9% on a second run).
**Byte-identical** - payload hashes unchanged across BC7, BC1, BC3 and BC5U.
BC1 and BC3 encode re-measured flat (z = +0.00 over 24 pairs and z = +0.45).

### Two probes disagreed, and the reason is the finding

A stub probe - replacing the four scans with pixel-dependent junk - read **42%
of BC7 encode**. That number is inadmissible: `best_err` gates mode 4, the
mode-1 64-shape ranking and the rotation loop, so junk errors change how much
*downstream* work runs. It was a work-parity break, not a measurement.

A doubling probe, which runs each scan a second time into a discarded
accumulator and therefore leaves every downstream decision untouched, read
**~24%**.

The realized win is larger than that 24% predicts, and the gap is itself
informative: the scalar scan is a loop-carried dependency chain
(`if e < be { be = e; bi = j }`), so it is **latency-bound**, and a doubling
probe measures the *marginal* cost of a second copy - which is nearly free when
idle slots exist. Against latency-bound code the doubling probe is a **lower
bound**, not an estimate.

### Residual

Doubled again on the vectorised code, the four fits still cost >=22% of BC7
encode. The likely remainder is the `#[target_feature]` call boundary, paid four
to eight times per block by the candidate loops in `try_bc7_mode5` and
`try_bc7_mode4` - the same boundary that decided the sign of the BC1 decode
result in 0.3.28. Hoisting it above those loops is the next step, and is not
attempted here.

## 0.3.30 - 2026-08-19

**BC6H block decode gets SIMD: +30.4%.** And the measurement that found it
invalidated every BC6H decode figure this campaign has published.

### The instrument was measuring the allocator

The per-format decode probe called `decode_rgba_f32` for BC6H while calling
`decode_rgba8_into` - with a **reused** buffer - for every LDR format. The f32
entry point allocates and zeroes a fresh 4 MiB `Vec` per call. At 512^2:

| | CPU ms |
|---|---|
| `decode_rgba_f32` (allocates per call) | 1.5234 |
| `decode_rgba_f32_into` (buffer reused) | **0.6497** |

**59% of what was being reported as BC6H decode was allocation and zeroing.**
The probe now reuses a buffer, as the LDR arms always did. No shipped code was
wrong; the instrument was, and every BC6H share derived from it was too - the
interpolation loop had read as 14% of decode and is in fact **~37%**, the
largest share left in the format.

### The kernel

Sixteen weights against three channels, eight lanes at a time. `base` reaches
4 194 336 and `w * delta` spans +/-4 194 240, so the lanes must be 32-bit; the
sum is the original `a * (64 - w) + c * w + 32`, so `>> 6` lands in `0..=65535`
and `(v * 31) >> 6` in `0..=31743`, and `packus_epi32` never saturates.

The block now decodes **planar** - sixteen reds, then greens, then blues - which
is what lets the kernel avoid interleaving entirely: three broadcasts, six
store-ready vectors, no cross-lane shuffling. The f32 conversion downstream is
layout-agnostic, and the RGBA widen after it was already a strided read (a
ceiling probe puts it at ~8%, and reading three planes costs it nothing). The
general-decoder fallback still writes interleaved and is transposed by its
caller; this crate's own encoder emits only mode 11, so that path is cold.

512^2, pinned, 16 paired CPU samples, alternating leading arm: **0.4688 ms
against 0.6738, 16/16 wins, z = +4.00, +30.4%.** Byte-identical to the general
decoder across 40 000 randomised blocks including both saturating `unquantize`
branches.

### Ceiling probes, on the corrected instrument

| stage | share of BC6H decode |
|---|---|
| mode-11 interpolation | ~37% (now vectorised) |
| half-to-f32 conversion | ~14% (already F16C) |
| RGB-to-RGBA widen and scatter | ~8% |

## 0.3.29 - 2026-08-19

**BC2 and BC3 decode get SIMD: 2.43x and 1.90x.** The last two scalar LDR block
decoders, and the same defect in both.

Each decoded colour, stored four RGBA words per row, and then performed
**sixteen single-byte read-modify-writes back into those same words** to lay in
alpha - a store-forwarding hazard per pixel on top of a doubled store stream.
Ceiling probes: **37% of BC2 decode** (0.2305 ms against 0.1445 with the alpha
pass stubbed) and **26% of BC3** (0.2734 against 0.2031).

Both now merge alpha into the colour vector before a single store per row, on
the surface-scope dispatch 0.3.28 established:

- **BC2** looks its 4-bit alpha up two pixels at a time from a 2 KiB table laid
  out at the alpha byte positions with the colour bytes zeroed, so a row is two
  loads, an `unpacklo_epi64` and an `or`.
- **BC3** gathers its interpolated alpha with a second `pshufb`. The palette is
  eight bytes, so it rides in the low half of a register; `bc3_alpha_palette` now
  has a packed `u64` twin so it arrives by one `movq` rather than through a
  spilled array - the stall that cost BC1 13.9%. Twelve index bits per row split
  into two six-bit lookups, keeping the selector table at 512 bytes rather than
  the 64 KiB a twelve-bit table would need.

512^2, pinned, 16 paired CPU samples each, alternating leading arm:

| format | before | after | verdict |
|---|---|---|---|
| BC2 | 0.2329 ms | **0.0957 ms** | 16/16, z = +4.00, **+58.9%** |
| BC3 | 0.2712 ms | **0.1430 ms** | 16/16, z = +4.00, **+47.3%** |

BC1 re-measured flat (z = -0.58) confirming the shared colour path was not
disturbed. Byte-identical; both loops are oracle-tested against their scalar
block decoders over 20 000 random surfaces including both alpha-palette
branches and the degenerate `c0 == c1` colour case.

Every LDR block decoder - BC1, BC2, BC3, BC4, BC5, BC7 - is now vectorised.

## 0.3.28 - 2026-08-19

**BC1 and BC4 decode get SIMD.** Two of the four remaining scalar decoders, and
the first case in this campaign where the *same kernel* measured both a heavy
loss and a solid win depending on nothing but where the dispatch sat.

### BC4 decode: +22.2%

A ceiling probe put the palette gather at **~77% of BC4 decode** (0.1536 ms
against 0.0352 stubbed). BC4 is BC5 with a zero second channel, so it needs no
new kernel at all: the existing `bc5_gather`, with an all-zero green palette and
a zero index word, yields `(v, 0, 0, 255)` per pixel. It reuses that kernel's
oracle too, rather than duplicating either.

512^2, pinned, 16 paired CPU samples: **0.1094 ms against 0.1406, 16/16 wins,
z = +4.00, +22.2%.** BC5 re-measured flat (z = +0.33) confirming the shared
kernel was not disturbed. Byte-identical.

### BC1 decode: +38.7%, after a 47.8% loss

A BC1 palette is four RGBA entries - exactly sixteen bytes, exactly one register
- so one `pshufb` expands four pixels from a 256-entry compile-time selector
table. A ceiling probe put the gather at **~78% of BC1 decode**.

Written the obvious way, as a per-block gather called from the shared block
loop, it measured **0/16 wins, z = -4.00, 47.8% SLOWER than scalar**.

The refutation decomposed into two costs, both from the ABI boundary that a
`#[target_feature]` function cannot be inlined across:

| arm | 512^2 CPU ms | against |
|---|---|---|
| scalar, inline | 0.1354 | - |
| scalar body, behind the SIMD call | 0.1716 | **-26.7%** - the call and its `OnceLock` alone |
| `pshufb` body, same boundary | 0.1959 | **-13.9%** further, from the palette spill |
| `pshufb`, boundary hoisted to surface scope | **0.0805** | **+38.7%** |

The second cost is this crate's fifth store-forwarding stall: a `[u32; 4]` is
passed by value through a caller-allocated stack copy on the Windows x64 ABI, so
the callee rebuilt the vector element-wise from four loads and three `pinsrd` on
the shuffle port. One sixteen-byte load recovered 0.041 ms of it.

The fix for both is the same - dispatch **once per surface** instead of once per
block, with the whole block loop inside the `#[target_feature]` function. The
palette build is split into a plain `#[inline] bc1_palette`, which inlines *into*
that loop, so the palette is built in registers and never reaches memory. Same
kernel, same `pshufb`, an 86-point swing.

A store-floor probe - identical stores, no palette indexing - reads 0.041 ms, so
BC1 decode at 0.0805 is now within 2x of its own store stream.

512^2, pinned, 16 paired CPU samples, alternating leading arm: **0.0805 ms
against 0.1313, 16/16 wins, z = +4.00, +38.7%.** Byte-identical; the SSSE3
surface loop is oracle-tested against the scalar block decoder over 20 000
random surfaces including both endpoint orderings and the degenerate `c0 == c1`
three-colour case.

### Also

- Removed `bc4_palette`, a one-line wrapper left dead by the BC4 change.
- Non-multiple-of-four surfaces keep the scalar path, which is unchanged.

## 0.3.27 - 2026-08-19

**BC6H encode gets SIMD, and doubles.** It had none - `grep` for any intrinsic in
`src/encode/bc6h.rs` returned zero. Its index fit is the exact twin of the BC7
mode-6 one: sixteen pixels against a sixteen-entry palette, three channels,
entirely scalar.

A ceiling probe put that search at **~73% of BC6H encode** (4.0 ms against 1.1
with it stubbed) - the largest single share measured anywhere in this campaign.

BC6H encode, 256^2, pinned, 16 paired CPU samples: **median 1.9531 ms against
3.9062, 16/16 wins, z = +4.00, +50.00%.** A clean 2x, **byte-identical**.

### Why 32-bit lanes suffice

The values are half bits, so a channel difference reaches +/-31 775 and its square
1.01e9 - inside `i32`. The sum of three reaches **3.03e9**, which overflows `i32`
but fits **`u32`** (4.29e9). The sums are kept as `u32` bit patterns and compared
with a sign-bias, which is exact; only the final accumulation over sixteen pixels
needs `i64`, once, after extraction.

A new oracle checks 60 000 cases against the scalar twin, including the widest
possible separation (where a signed 32-bit sum would overflow) and duplicate
palette entries (where the lowest-index tie-break is the whole question).

### Refuted: a SIMD `palette_mode6`

Written and measured **neutral** - 7/12 paired wins, z = +0.58, 0.00% median, 8
ties. LLVM already auto-vectorises that loop (sixteen independent iterations over
four channels, no carried dependency), and a ceiling probe had put the whole
function inside the noise. Reverted, with the numbers recorded at the site.

- 100 tests pass.

## 0.3.26 - 2026-08-19

**A vectorised nearest-palette scan for BC4/BC5 alpha.** The alpha path had no
SIMD kernel at all - unlike BC1 and BC7, there was nothing in `simd.rs` for it.
Sixteen samples against an eight-entry palette, scalar, ~2.4 candidate fits per
alpha block.

Now sixteen samples live in one `__m256i` as `i16` lanes and the eight entries
are scanned with compare-and-blend, exactly as the BC1 and BC7 kernels do.

BC5U 512^2 x 10 mips, pinned, forced serial, 16 paired CPU samples: **median
7.812 ms against 10.417, 12/12 wins, z = +3.46, +19.64%.** BC4 shares the path.
BC3 measures neutral - its alpha is a smaller share of the block now that 0.3.25
gave its colour half +26%.

### Notes

- **Byte-identical.** The scalar scan uses a strict `<`, so the lowest index wins
  a tie; `_mm256_cmpgt_epi16(best, cur)` is exactly `cur < best` and preserves
  that. A new oracle checks 200 000 cases against the scalar scan, including
  palettes with duplicate values where the tie-break is the whole question.
- The caller's early abort now applies to the completed total. Error only
  accumulates, so a prefix that would have tripped the limit leaves a total that
  trips it too; acceptance is unchanged.
- `AlphaSelect` remains for non-AVX2 targets. It exists to make the scalar scan
  cheap by turning it into a threshold lookup; vectorised, the plain scan is
  cheaper still and needs no selector built per candidate.
- 99 tests pass.

## 0.3.25 - 2026-08-19

**The BC1 AVX2 kernel too.** `bc1_fit_4color_avx2` had the identical defect
0.3.24 fixed one function over: distances computed in vector registers, **stored
to a `[i32; 16]`**, then a **scalar sixteen-iteration loop** reading them back to
track the minimum - once per colour, four colours per call.

Now register-resident, with compare-and-blend. `sse16_rgba_noalpha` is gone; the
extraction loop stays scalar on purpose, because it carries the order-dependent
early abort and runs once per call rather than once per colour.

### Performance

512^2 x 10 mips, pinned, forced serial, paired CPU:

| format | register | array | wins | z | improvement |
|---|---:|---:|---:|---:|---:|
| **BC1** | **6.510 ms** | 10.417 ms | **20/20** | **+4.47** | **+37.50%** |
| **BC3** | **9.115 ms** | 13.021 ms | **12/12** | **+3.46** | **+26.11%** |

Larger than BC7's +9.23% from the same fix, and for a clear reason: BC1 evaluates
only **four** colours, so the fixed 64 scalar compare-branches and two
store-forwarding stalls were a far larger share of a much shorter kernel. BC3
gains through the same colour path.

### Notes

- **Byte-identical**, verified by the existing `bc1_avx2_matches_scalar` oracle,
  the frozen-payload tests, and an explicit payload hash across BC7, BC1, BC3
  and BC5U.
- `sim/examples/probe_encode_serial.rs` now takes `PROBE_FMT` so the same
  instrument serves every format.
- 98 tests pass.

## 0.3.24 - 2026-08-19

**The AVX2 index-fit kernel now stays in registers.** With `best_index_pal`
shown to be dead (0.3.23), `fit_indices_mode6_avx2` *is* the BC7 index fit - and
it was round-tripping through memory twice per palette entry.

The vector code computed sixteen per-pixel distances, **stored them to a
`[i32; 16]`**, and then a **scalar sixteen-iteration loop** read them back to
track the running minimum. Once per palette entry, sixteen entries per fit:
**256 scalar compare-branches per fit**, plus two store-forwarding stalls per
entry - the vector unit writing a stack array that scalar code immediately reads.
At 3.168 fits per block that is the dominant shape in the encoder hot path.

Now the distances stay in two `__m256i` and the minimum is tracked with
compare-and-blend. Nothing touches memory until the two results are extracted
once, at the end.

BC7 512^2 x 10 mips, pinned, forced serial, 20 paired CPU samples:

| | |
|---|---|
| register median | **41.667 ms** |
| array median | 46.224 ms |
| **wins** | **16 / 18** (2 ties) |
| **z** | **+3.30** |
| **improvement** | **median +9.23%**, mean +7.83% |

### Notes

- **Byte-identical.** `_mm256_cmpgt_epi32(best, cur)` is exactly `cur < best`,
  which keeps the lowest index on ties as the scalar twin does. Verified by the
  existing AVX2-vs-scalar oracle, the frozen-payload tests, and an explicit
  before/after payload hash across BC7, BC1, BC3 and BC5U.
- `_mm256_hadd_epi32` folds within 128-bit lanes, so the pair sums arrive as
  `[p0,p1,p4,p5,p2,p3,p6,p7]`; a `permutevar8x32` puts them back in pixel order.
- 98 tests pass.

## 0.3.23 - 2026-08-19

**Block statistics computed once each.** The BC7 mode-6 search walked the same
sixteen pixels repeatedly for values it already had. Counted per block:

| helper | calls | needed |
|---|---:|---:|
| `extrema_rgba` | **2.245** | 1 |
| `channel_minmax_rgba` | **3.245** | 2 |
| `rgba_span_sum` | 1.245 | 0 |

`extrema_rgba` ran once to seed `best_seed` and again inside the seed builder
that produces the same pair. `rgba_span_sum` is a sum over `channel_minmax_rgba`,
which the seed builder had already computed for seed 1. Each of these walks 16
pixels x 4 channels.

Now each statistic is computed once at the top of the search and threaded
through; `rgba_span_sum` is gone entirely, its value derived from the min/max
already in hand.

BC7 512^2 x 10 mips, pinned, forced serial, 20 paired CPU samples: **median
46.224 ms against 47.526, 11/13 wins (7 ties), z = +2.50, +2.67%.**
**Byte-identical.**

### Two non-targets, measured rather than assumed

- **`best_index_pal` is called ZERO times.** `fit_indices_mode6` dispatches to the
  AVX2 kernel when available, so the scalar nearest-palette search survives only
  as the fallback and the test oracle. It looks like an obvious hot spot - 16
  entries x 4 channels per pixel - and on this machine it never runs.
- **`palette_mode6` is already minimal** at 3.168 calls per block, one per index
  fit, and its interpolation was factored to one multiply per component in
  0.3.20. No further scalar win found.

### Notes

- Byte-identity verified by the frozen-payload tests and an explicit
  before/after payload hash across BC7, BC1, BC3 and BC5U.
- 98 tests pass.

## 0.3.22 - 2026-08-19

**`quantize_7p` becomes a compile-time table.** It was ~11% of BC7 encode by
ceiling probe and had never been opened.

`unquantize_7p_chan(q, p)` is `(q << 1) | p`, which makes the inner search a pure
function of `(channel_value, p_bit)` - **512 possible inputs**. The direct form
re-derived one of those 512 answers **24 times per call** (2 p-bits x 4 channels
x a 3-wide candidate window), and `quantize_7p` runs roughly six times per block.

Now: two p-bits x four channels = **8 table lookups**. The table is built by a
`const fn` running the *identical* search, so equivalence is by construction
rather than by argument. 768 bytes, permanently L1-resident.

### Performance

BC7 512^2 x 10 mips, pinned, forced serial, 20 paired CPU samples:

| | |
|---|---|
| table median | **57.943 ms** |
| direct median | 62.500 ms |
| **table wins** | **18 / 19** (1 tie) |
| **z** | **+3.90** |
| **paired improvement** | **median +7.26%**, mean +8.08% |

### Notes

- **Byte-identical.** Verified by the frozen-payload tests and by an explicit
  before/after payload hash across BC7, BC1, BC3 and BC5U.
- The table is checked against the direct search **exhaustively** for the
  per-channel primitive (all 512 inputs) and over 200 000 random colours for the
  four-channel p-bit selection - not merely on values an encoder happens to
  produce.
- 98 tests pass.

## 0.3.21 - 2026-08-19

**Encoder: the expensive BC7 mode-6 seeds are gated on residual error.** The
search tries up to five endpoint seeds. Counted over 21 847 blocks, the two cheap
ones win **93.6%** between them (74.3% + 19.3%); the three expensive extras - a
mean-split pair and an **O(16^2) farthest-pair scan** - win **6.4%**.

Dropping them outright was measured first, as the ceiling: **-0.0028 dB mean**
across the BC7 corpus for a large speed saving, worst case -0.049 dB. That is a
quality trade, and this encoder's mandate is faster *and* better - so instead the
extras are gated on the error the cheap seeds left behind. A block the first two
already fit well cannot be rescued by a third seed; a block they fit badly is
exactly where the extras earn their cost.

### Calibrated, not guessed

The residual-error distribution over those 21 847 blocks:

| gate | extras skipped | corpus quality |
|---|---:|---|
| SSE <= 64 | 29.5% | 0 worse |
| **SSE <= 256** | **83.5%** | **0 worse** |
| SSE <= 1024 | 96.4% | untested |

A gate of 64 fires on only 29.5% of blocks and measured neutral. 256 skips
**83.5%** of the extras - including that O(16^2) scan - and the full 102-case
corpus reports:

- **0 better, 98 same, 0 worse** on finite-PSNR cases
- mean **-0.00004 dB**, worst **-0.0035 dB**
- 4 lossless cases still lossless, 0 broken

### Work removed, counted

Deterministic, same probe and image, gate on against gate disabled:

| | `fit_indices_mode6` | per block |
|---|---:|---:|
| baseline | 90 833 | 4.158 |
| **gated** | **69 209** | **3.168** |

**-23.8%.** With 0.3.20's refine reuse, the search has gone **5.14 -> 3.168 fits
per block, -38.4%** across the two releases.

### Speed: +8.3%, measured properly

This box runs 73% busy from other processes, so wall-clock and even process CPU
under thread contention were both useless (a null A/B of one binary against
itself spanned 68.93-80.37 ms). Three changes to the instrument made the verdict
possible:

1. **Pin** every probe (mask 0x3c, high priority).
2. **Force the encoder serial** in both arms (`ENCODE_PARALLEL_MIN_BLOCKS` raised)
   so thread scheduling leaves the measurement.
3. **Report CPU time and compare paired**, with a win-rate and z-score rather
   than trusting magnitudes.

BC7 512^2 x 10 mips, pinned, forced serial, 20 paired samples:

| | |
|---|---|
| gated median | **42.969 ms** |
| baseline median | 46.875 ms |
| **gated wins** | **17 / 17** (3 ties) |
| **z** | **+4.12** |
| **paired improvement** | **median +8.33%**, mean +7.74% |

A first attempt measured a 1.5% *regression* — because it used a 128^2 x7 probe,
which fires this gate on **9.8%** of blocks against **78.2%** at the production
shape. It was measuring the restructure overhead with almost none of the
benefit. A probe must be representative in *content*, not merely in format.

### Notes

- **This is not byte-identical**: 20 of 102 corpus payloads change, all at equal
  or better PSNR. The frozen-payload tests still pass - those fixtures do not
  trip the gate, which is worth knowing about their coverage.
- Every measurement probe under `sim/examples/` is now pinned before timing.
- 97 tests pass, 90 on the scalar path.

## 0.3.20 - 2026-08-19

**Encoder: the BC7 mode-6 refine reuses the winning fit.** The search evaluates
up to five seed candidates, keeps the best, then least-squares refines it - by
calling back in with the winning *endpoints*, which re-quantized them, rebuilt
the palette and re-ran `fit_indices_mode6` for a candidate it had just evaluated.

Carrying the evaluated candidate itself into the refine skips that. **Byte-identical
output**, verified two ways.

### Counted, not timed

This box is degraded (one earlier sample read 59.13 ms against a ~25 ms median),
so the verdict rests on deterministic counters:

| | before | after |
|---|---:|---:|
| `fit_indices_mode6` calls | 112 239 | **90 833** |
| per block | 5.14 | **4.16** |

The drop is exactly 21 406 - precisely the number of refines - so one redundant
fit per refined block is gone. **-19.1% of all index fits.** Every other counter
(blocks, seeds tried, refines run, refines that improved) is unchanged, so the
search itself behaves identically.

### What that is worth, honestly

Ceiling probes inside the BC7 encode:

| stage | share |
|---|---:|
| `fit_indices` | ~18% |
| seed search | ~17% |
| `quantize_7p` | ~11% |
| `ls_endpoints` | ~0% |

So 19% fewer fits is **~3.4% of encode** - real, but under this box's noise
floor, and no timing improvement is claimed. Ten samples per arm showed no
separation (means 28.6 against 28.5), which is the expected result for a 3.4%
effect measured at +/-10%.

### Also

`palette_mode6` now uses the same `base + w * delta` factoring as the decoder:
`c0 * 64 + 32 + w * (c1 - c0)` is one multiply per component instead of two, 64
rather than 128 per call, and it is called up to seven times a block. Also
byte-identical; also unmeasurable here. Confirmed by reading the emitted
assembly: **`imul` 851 -> 835**.

### Notes

- Byte-identity verified by the frozen-payload tests (`quality_payloads_are_frozen`,
  `fast_payloads_are_frozen`, `rdo_payloads_are_frozen`) **and** by an explicit
  before/after payload hash across BC7, BC1, BC3 and BC5U - all four unchanged.
- Adds `examples/probe_encbytes.rs`, the payload-hash gate used for that check.
- 97 tests pass.

## 0.3.19 - 2026-08-19

**BC6H writes a block row per store.** The scatter that widens RGB to RGBA and
changes stride did sixteen separately range-checked indexed writes per block row.
Building the row as a register-resident array and writing it once leaves one
range check.

Same change that was worth +34.7% in BC5 (0.3.13). Here it is smaller.

### Measurement quality, stated plainly

The box was disturbed during this round - one arm spanned 75.7 to 132.4 Mpx/s,
and a first ABBA reported **+34.5%** which a re-run then contradicted. Per the
campaign's own rule, a verdict that flips on re-measurement is the instrument
deciding the answer, not noise to average.

Interference only ever slows a sample, so the estimate uses robust statistics
over 9 NEW against 18 OLD samples:

| estimator | NEW | OLD | |
|---|---:|---:|---:|
| max | 140.9 | 131.3 | +7.3% |
| p75 | 134.9 | 125.2 | +7.8% |
| median | 122.7 | 117.6 | +4.3% |

**+5-8%, not the +34.5% the first reading showed** - that arm was cold. All three
estimators agree in sign, which is the basis for keeping it; the magnitude is
reported as a range because the box did not permit better.

The peer comparison run in the same conditions shows every absolute number down
~25% from 0.3.18's measurement, so only the ratios are meaningful: **BC6H 4.58x**,
8.44x across formats, both consistent with 0.3.18 within the noise this box was
producing.

### Notes

- A doubling probe on the scatter was attempted first and was **invalid**:
  writing the same value twice to the same address is dead-store eliminated, so
  the duplicate never existed. Recorded because the probe looked reasonable.
- Non-aligned surfaces keep the per-pixel path; only full four-pixel rows take
  the block-row store.
- Decode output is unchanged, bit for bit. SIMD path 97 tests, scalar fallback 90.

## 0.3.18 - 2026-08-19

**Hardware half-float conversion for BC6H.** `vcvtph2ps` converts eight halves
per instruction, so a block's 48 components take six instructions instead of 48
scalar conversions.

Eight samples per arm, alternating order: **136.0 -> 146.8 Mpx/s, +7.9%.** The
arms overlap slightly; this is a modest result reported as one.

Against Microsoft DirectXTex on a cooked 1024^2 pack: **BC6H 144.1 vs 30.2 Mpx/s
- 4.77x**, up from 4.50x; **8.86x** across all formats.

### The 96 multiplies were the wrong target

The obvious next step after 0.3.17 was to vectorise the 16 x 3 x 2 interpolation
multiplies. Probing first said not to:

| probe (512^2 serial) | Mpx/s | reading |
|---|---:|---|
| full | 119.7 | - |
| interpolation removed | 156.1 | ~23% of the call |
| **2x interpolation work** | **115.5** | **only 3.5% slower** |

Removing it helps, doubling it costs almost nothing - so the interpolation sits
on the dependency chain but has spare throughput, and vectorising it (a
throughput fix) would buy little. The same shape that made three BC5 rounds
measure neutral.

Redirecting the same probe onto the downstream conversion gave the opposite
answer - doubling that work cost **19%** (121.3 -> 98.8) - which is what made it
the target.

### Notes

- F16C plus AVX are runtime-detected; the branchless scalar converter remains as
  the twin for everything else.
- Verified against the scalar converter across **all 65 536** half bit patterns,
  not merely the positive normals BC6H emits, with NaN compared as NaN.
- Decode output is unchanged, bit for bit. SIMD path 97 tests, scalar fallback 90.

## 0.3.17 - 2026-08-19

**In-house BC6H mode-11 block decode.** BC6H was the weakest format in the matrix
by a factor of three - 3.29x against DirectXTex where every other format sat at
9-12x - and the only decode path that had never received any of this campaign's
findings.

### Measured before written

| probe (512^2 serial) | Mpx/s | |
|---|---:|---|
| full | 113.8 | - |
| block decode removed | **227.3** | decode is **50%** of the call |
| **2x block decode work** | **67.5** | doubling costs 1.7x |

The doubling probe matters: BC6H is **throughput** bound, unlike BC5 which is
latency bound. Work removed here is work saved, so a leaner decoder pays
directly.

A mode histogram over the real content: **100% of blocks are mode 11** - one
subset, both endpoints stored explicitly at 10 bits, no partition table, no delta
compression, sixteen 4-bit indices. That is what this crate's encoder emits, and
it is the natural shape for smooth HDR gradients.

### Performance

Eight samples per arm, alternating order: **102.7 -> 124.1 Mpx/s, +20.8%.**

Against Microsoft DirectXTex on a cooked 1024^2 pack: **BC6H 141.5 vs 31.4 Mpx/s
- 4.50x**, up from 3.29x; **8.72x** across all formats, up from 7.88x.

### Notes

- The general decoder reaches mode 11 through a stateful `Bitstream` whose every
  read mutates the cursor, so the mode dispatch, six endpoint reads and sixteen
  index reads form one serial dependency chain. Reading each field by computed
  offset from an immutable `u128` makes them independent - the same fix worth
  +19% to +73% across BC1-BC5 and every BC7 mode.
- Signed content and every other mode fall through to the general decoder
  untouched.
- Verified bit-identical over **40 000** randomised mode-11 blocks including the
  `unquantize` saturating branches (`v == 0`, `v == 1023`) and the all-ones
  payload, with signed input and all 31 other mode fields asserted declined.
- Decode output is unchanged, bit for bit. SIMD path 96 tests, scalar fallback 90.

### Also

A doubling probe was run on BC7 mode 6, whose "at the limit" verdict rested on
two throughput edits. Doubling its weight-extraction work is free (295.9 vs
303.5 Mpx/s), which combined with its 2.5% subtraction ceiling means two
independent instruments now agree the weight extraction is not its cost. The
verdict stands, better supported.

## 0.3.16 - 2026-08-19

**BC4/BC5 palette packing without the serial chain.** Re-measuring the ceiling
against the SIMD kernel - rather than the stale scalar figure - moved the target
onto the palette build, which had become the largest remaining cost precisely
because everything around it got faster.

The packing loop carried both defects already fixed elsewhere in this kernel:

```rust
for (k, v) in p.iter().enumerate() { packed |= (...) << (8 * k); }
```

an eight-deep serial dependency chain on `packed`, reading back a `[i32; 8]` that
had just been stored to the stack. Rewritten as a balanced OR tree over
independent terms with no intermediate array.

Eight samples per arm, alternating order: **BC5U 563.9 -> 595.1 Mpx/s (+5.5%)**,
**BC4U 661.8 -> 699.1 (+5.6%)**. Positive in both formats, but the arms overlap -
this is a weak result reported as one.

Against Microsoft DirectXTex on a cooked 1024^2 pack: **BC5U 878.5 vs 77.8 Mpx/s
(11.29x)**, **BC4U 976.7 vs 106.8 (9.15x)**, **BC7 726.9 vs 72.2 (10.06x)**, and
**7.88x** across all formats.

### What the ceiling actually says now

Stage-by-stage against the SIMD kernel:

| probe | Mpx/s | share |
|---|---:|---:|
| full | ~572 | - |
| `pshufb` gather removed | ~641 | ~18% |
| `pdep` index unpack removed | ~601 | ~13% |
| **palette interpolation removed** | **~847** | **~32%** |

The palette probe is block-dependent by construction, because an earlier version
that substituted *constant* palettes let the compiler hoist the whole computation
out of the block loop and reported a far larger figure. A probe that removes more
than it names measures the wrong thing.

**32% remains uncaptured.** Two attempts failed and are recorded at the site:

- **Branchless endpoint selection.** The `e0 > e1` test is data-dependent per
  block and taken twice per BC5 block, so a mispredict looked like the obvious
  culprit. Computing both weight sets and selecting with a mask measured
  **neutral**: BC5 625.6 vs 609.3 Mpx/s, BC4 680.5 vs 685.3.
- The `65536`-sum identity from 0.3.15, which halves the multiplies per entry,
  was likewise neutral.

Whatever the palette costs, it is not the branch and not the multiply count.

### Notes

- Decode output is unchanged, bit for bit; the BC4/BC5 oracles (30 000 blocks
  each, signed and unsigned) pass unchanged.
- SIMD path 94 tests, scalar fallback 88 (`--no-default-features`).

## 0.3.15 - 2026-08-19

**BC4/BC5 palettes are built in a register.** 0.3.14 found a store-forwarding
stall in the BC5 index unpack and fixed it. The same stall was still present one
line away: `bc4_palette` returned a `[u8; 8]` built by eight narrow stores to the
stack, and the SIMD gather read it back with one wide load.

Building the palette into a `u64` and moving it to the vector unit with `movq`
removes the round trip. Eight samples per arm, alternating order: **508.7 ->
572.5 Mpx/s, +12.5%.**

Against Microsoft DirectXTex on a cooked 1024^2 pack: **BC5U 816.8 vs 70.8 Mpx/s
- 11.54x**, up from 10.66x; **7.65x** across all formats.

### Also

The BC4 interpolation weight pairs sum to exactly 65536 (`W6[5-k] + W6[k]`, and
likewise `W4`), so

```text
(W[n-k]*e0 + W[k]*e1 + 32768) >> 16  ==  e0 + ((W[k]*(e1 - e0) + 32768) >> 16)
```

- one multiply per palette entry instead of two, the same identity as the BC7
interpolation. **Measured neutral** at this block size: six multiplies saved
against a ~90-cycle block is below the noise floor. Kept because it is strictly
less work and shares the documented BC7 form, not counted as a win.

### Notes

- Decode output is unchanged, bit for bit; the BC4/BC5 oracles (30 000 blocks
  each, signed and unsigned) pass unchanged.
- The scalar path still unpacks the palette to an array and indexes it, because
  0.3.14 measured that as *faster* than shifting a register - an L1-resident
  table pipelines better than a dependent multiply-then-shift chain. Both forms
  now come from one packed builder.
- SIMD path 94 tests, scalar fallback 88 (`--no-default-features`).

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
