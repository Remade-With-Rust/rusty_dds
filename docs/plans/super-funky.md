# super-funky: where we call the wrong compute

Nine cases in `corpus-vs-directxtex-serial.md` have DirectXTex ahead on speed.
This plan is about those nine. It is NOT about RDO — RDO is not on the BC4/BC5
path at all, and the two BC1 cases are a separate, much smaller story.

## The finding, measured before writing any of this

The nine slow cases split cleanly:

| group | cases | serial ratio |
|---|---|---|
| **BC4S / BC5S (signed)** | 7 | **3.6 – 7.9** |
| BC1 (albedo) | 2 | 1.55 – 1.66 |

Seven of the nine are **signed** BC4/BC5. Their unsigned twins — same image, same
block count, same sample values — run at **0.53–0.91**, i.e. FASTER than
DirectXTex. A 6–9x gap between two paths doing nearly the same work is not a
tuning problem, it is a structural one.

### The mechanism

`encode_alpha_block_signed` runs a ±4 exhaustive endpoint window — about 80
candidate pairs a block — gated only by `signed_sweep_gate(span, best_err)`
(`span in 8..=32 && best_err > 4`) and `!quality_is_fast()`.

Its unsigned twin, `unsigned_window_sweep`, is **DEFAULT OFF**, and the reasoning
is already written in the tree:

> DEFAULT OFF: it costs ~3.2s corpus CPU for +0.15..0.45 dB on 14 cases that
> already beat DirectXTex by 0.4-1.9 dB — the CPU budget instead funds the BC1
> lattice and BC7 mode 5, whose gains are 2-20x larger.

**The unsigned path did that cost/benefit review. The signed path never did.**

### What turning it off actually costs

Measured on the corpus, serial, signed sweep forced off:

| case | ratio with | ratio without | dB given up |
|---|---|---|---|
| Rock064 Roughness bc4s | 7.870 | **1.464** | -0.17 |
| Metal063 Roughness bc4s | 7.028 | **1.457** | -0.17 |
| Wood095 NormalGL bc5s | 5.885 | **1.178** | -0.42 |
| Bricks097 Roughness bc4s | 5.490 | **1.858** | -0.05 |
| Bricks097 NormalGL bc5s | 5.286 | **1.796** | -0.05 |
| Rock064 NormalGL bc5s | 5.137 | **1.515** | -0.17 |
| Wood095 Roughness bc4s | 3.620 | **0.822** | -0.61 |
| Metal063 NormalGL bc5s | 0.663 | **0.309** | 0.00 |

**3-5x the CPU for 0.05-0.61 dB.** And without it we STILL hold higher PSNR than
DirectXTex on 6 of 8 — only Wood095 NormalGL flips (-0.51 dB). We are spending
several times the encode budget to extend a lead we already have, on exactly the
grounds the unsigned path rejected.

---

## The twenty

Items 1-4 are the structural fix. 5-13 are the compute-shape work behind it.
14-20 keep us honest.

### Structural — the signed sweep policy

**1. Give the signed sweep the same default its unsigned twin has.**
Not "delete it" — put it behind `RUSTY_DDS_BC45S_WINDOW`, mirroring
`unsigned_window_enabled()`. Measured: ratios 3.6-7.9 collapse to 0.8-1.9 at a
cost of 0.05-0.61 dB, and 6 of 8 still beat DirectXTex. This one change moves
seven of the nine slow cases. **Gate: the corpus quality table must stay
`directxtex_higher_psnr = 0`, or the change gets scoped down rather than shipped.**

**2. If #1 is too blunt, tighten `signed_sweep_gate` instead of disabling it.**
The gate is `span in 8..=32 && best_err > 4`, harvest-tuned over 643k blocks for
GAIN only. The per-case dB is strongly bimodal — Wood gives up 0.42-0.61 dB,
Bricks only 0.05. Re-harvest with COST in the objective and find the gate that
keeps Wood's win and drops Bricks'.

**3. Make the sweep adaptive rather than a fixed ±4.**
80 pairs unconditionally. Start at ±1 (8 pairs) and widen only while the last
ring improved `best_err`. On smooth blocks the optimum is adjacent; on busy ones
the range-bound prune already kills most candidates.

**4. Order the sweep by likelihood, not `d0` then `d1`.**
`best_err == 0` returns early and the range prune compares against the *current*
best, so a good candidate found sooner prunes more of what follows. Spiral out
from the centre instead of raster-scanning.

### Compute shape — the signed inner loop

**5. Vectorise `consider_alpha_s`.** The sweep's inner call, ~80x a block, scalar.
Sixteen samples against an 8-entry palette is the shape `fit_indices_mode6_avx2`
already solves in 18 instructions.

**6. Vectorise `alpha_sse_s` and `pack_alpha_indices_s`.** Same 16-sample walk,
same per-sample argmin over 8 entries.

**7. Hoist the per-candidate `snorm_i32_to_unorm_u8` conversions.** The signed
path scores in the UNORM domain, so every candidate converts its endpoints back.
Only 255 inputs exist — table it, or hoist it out of the candidate loop entirely.

**8. Build the palette once, in the layout the scorer wants.**
`alpha_palette6_s` / `alpha_palette4_s` run per candidate. This is the same
pack-then-immediately-unpack shape section 75 #2 removed from mode 6.

**9. BC4/BC5 never got the block-walk vectorisation BC1/BC7 got.**
`encode_bc4` does `pixels.map(|p| p[0])` — a scalar 16-element gather per channel.
`planar_avx2` already does that transpose in 22 instructions.

**10. `encode_bc5` encodes its two channels sequentially.** Two independent
16-sample problems that could share one pixel load.

**11. Check whether the signed path is actually unrolled.** Every static count
this campaign that looked cheap and was rolled turned out to be the opposite
(`ls_pixels_mode6`: static 34, dynamic 132). Use a back-edge check, not a static
count.

**12. `unique_values_u_capped` and its signed twin.** A capped distinct-value scan
per block, scalar. The presence-filter trick from section 75 #14 applies.

**13. `refine_alpha_s` neighbourhood search.** Unmeasured. **Size it before
touching it** — three ideas this campaign died because the target was already at
floor or outright dead code.

### BC1 — the smaller, separate story

**14. Measure `lattice_refine_bc1`'s firing rate and cost.** BC1 is only 1.55-1.66x
slower, and the tree says the unsigned sweep's CPU was redirected *into* the BC1
lattice. If the lattice is what makes BC1 slow, that is the same trade in a
different coat and deserves the same review.

**15. Count `pack_bc1_scored` / `consider_bc1` per block.** `consider_bc1` is 22
instructions but may run several times a block; `pack_bc1_scored` was 274 before
section 75 #18 took its executed path to 90.

**16. Weigh BC1's trade explicitly.** We win +0.63 to +1.31 dB while being 1.6x
slower. That is defensible — unlike the signed case, where the dB is smaller and
the multiple is 5x. It may be right to change nothing here.

### Keeping ourselves honest

**17. Confirm DirectXTex's BC7 flag reaches the encoder.** Nominally
`TEX_COMPRESS_BC7_QUICK`, yet 4.7-10.7 s per 1K texture. If QUICK is not being
applied, our ~130x BC7 win is against its slow path and the honest number is
smaller. **Settle this before quoting BC7 publicly.**

**18. Add BC4S/BC5S to the routine probe suite.** Every format that got attention
this campaign had a probe. The signed paths did not — which is exactly why an
80-pair sweep sat on the default path unnoticed.

**19. Make serial-vs-serial the default reported artifact.**
The parallel comparison reads 24/24 and mostly measures thread count. The serial
one is the number that says something about the code.

**20. Ask whether BC4/BC5 should have an RDO path at all.**
RDO covers BC1 and BC7 only. Masks and normals are a large share of a real cook
and their rate side is untouched. The one item here that adds capability rather
than removing waste — and it gets measured against the same ladder, not assumed.

---

## What this plan deliberately does not say

It does not say RDO is at fault. RDO is not on the BC4/BC5 path, and its ladder
was re-measured this session and is healthy. The instinct that something
structural was wrong was correct; the location was one layer over.

It also does not say "use more SIMD". Items 5-12 are vectorisation, but they are
worth perhaps 2x on a path that item 1 fixes by 4x for free. **Do item 1,
re-measure, and let the new numbers choose what follows** — three separate
attempts this campaign optimised something already at floor or already dead.
