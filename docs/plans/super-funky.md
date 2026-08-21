# super-funky: the four cases where DirectXTex is still faster

## Where we stand

Full sweep against DirectXTex, idle machine, after the BC4/BC5 signed work:

| comparison | result |
|-----------------------------------|-------------------------------|
| decode speed (24) | **24 ahead / 0 behind** |
| encode speed, shipped parallel (24) | **24 faster / 0 slower** |
| encode speed, serial per core (24) | **20 faster / 4 slower** |
| encode quality, corpus (24) | **22 win / 1 loss / 1 tie** |
| encode quality, synthetic (54) | 19 win / 17 loss / 18 tie |

The four remaining per-core losses are **all BC1 albedo**:

| case                | ratio | our PSNR | DX PSNR | dB lead |
|---------------------|-------|----------|---------|---------|
| Metal063_Color bc1  | 1.656 | 40.23    | 38.93   | +1.31   |
| Rock064_Color bc1   | 1.590 | 34.10    | 32.81   | +1.30   |
| Bricks097_Color bc1 | 1.501 | 34.91    | 34.28   | +0.63   |
| Wood095_Color bc1   | 1.438 | 41.70    | 40.41   | +1.29   |

**We win quality on every one of them, by +0.63 to +1.31 dB.** That is the
central fact of this plan and it changes what "fix" means: unlike the signed
BC4/BC5 case — 5-8x CPU for 0.05-0.61 dB, which was plainly a bad trade — 1.5x
CPU for +1.3 dB is a *defensible* trade. The goal here is to keep the dB and
lose the multiple, not to buy speed with quality.

The synthetic set says the same thing from the other side: of its 17 losses,
**BC1 x5, BC2 x5, BC3 x5** — the BC1-BC3 family is where we are weakest in both
dimensions, while BC7 (5 wins) and BC4/BC5 (12 wins) carry us.

### What was resolved, and how (for context)

Both previously-tracked groups are closed:

- **Signed BC4/BC5, 7 cases at 3.6-7.9x.** `encode_alpha_block_signed` ran an
  ~80-pair exhaustive window by default while its unsigned twin had the same
  sweep `DEFAULT OFF` with the cost/benefit review already in the tree. Defaulted
  off: ratios fell to 0.8-1.9. Cost: one case, `Wood095_NormalGL` bc5s, went from
  tie to -0.51 dB.
- **The remaining 1.18-1.86x** was the *presweep* path. `pack_alpha_indices_s`
  assigned 16 samples to an 8-entry palette scalar — 738 instructions. Vectorised
  to 252 executed, byte-identical, and every signed case now beats DirectXTex per
  core (0.22-0.71).

## Measured sizes on the BC1 path

Each measured in isolation, `#[inline(never)]` on that function alone:

| function                | instructions | note |
|-------------------------|--------------|------|
| `encode_bc1_bytes`      | 951          | the whole per-block encoder |
| `extrema_opaque`        | 330 -> 51    | already vectorised this campaign |
| `pack_bc1_scored`       | 320          | executed path now ~90 |
| `lattice_refine_bc1`    | 270          | **never measured for firing rate** |
| `bc1_fit_4color_scalar` | 131          | fallback only; AVX2 arm is 125 |
| `consider_bc1`          | 22           | thin wrapper |
| `pca_extremes_rgb`      | 525          | **0.001 calls/blk — dead at this quality** |

---

## The twenty

### First, measure — three ideas died this campaign for skipping this

**1. Count every BC1 helper per block before touching any of them.**
`pca_extremes_rgb` looked like a 525-instruction target and fires 0.001 times a
block. `ls_endpoints_bc1` is 209 and fires 0.001. **Two of the seven functions
above are already dead at shipped quality.** Get calls/block for
`pack_bc1_scored`, `consider_bc1`, `lattice_refine_bc1` and `bc1_fit_4color`
before ranking anything. The blocks module is private, so add the accessor in
`encode/mod.rs`, not `lib.rs`.

**2. Split `encode_bc1_bytes`'s 951 into executed vs cold.**
Its tail (`pca_extremes_rgb`, the LS loop, `lattice_refine_bc1`) sits behind
`quality_is_fast() || best_err <= 16` and mostly does not run. The executed
figure is the only one worth ranking against DirectXTex's.

**3. Check the loop back-edges.** `ls_pixels_mode6` read 34 static and cost 132
dynamic because LLVM left it rolled. A static count is a dynamic count only when
the loop is unrolled.

### The lattice — the prime suspect

**4. Measure `lattice_refine_bc1`'s firing rate and per-call cost.**
Its gate is `best_err > bc1_lattice_min_err()` where the constant is **0** — so
it refines on *every* imperfect block, which on albedo is nearly all of them. At
270 instructions x 3 rounds (`BC1_LATTICE_ROUNDS = 3`) this is the single most
likely source of the 1.5x.

**5. Re-harvest `BC1_LATTICE_MIN_ERR` with cost in the objective.**
This is precisely the `signed_sweep_gate` story again: a gate tuned for GAIN
only. The signed gate was harvest-tuned over 643k blocks and still cost 5x. Find
the residual below which a lattice round cannot repay itself.

**6. Re-harvest `BC1_LATTICE_ROUNDS`.** Three rounds, fixed. Measure the marginal
dB of round 2 and round 3 separately — if round 3 is worth 0.02 dB it is the
signed sweep in miniature.

**7. Make the lattice adaptive instead of fixed-round.** Stop when a round fails
to improve `best_err`, exactly as `polish_mode6_endpoints` does. Free if most
blocks converge in one round.

**8. Vectorise the lattice's inner scorer.** Whatever it evaluates per candidate
is a 16-pixel walk against a 4-entry palette — the shape `bc1_fit_4color_avx2`
already does in 125 instructions and `alpha_select_avx2` does in 85.

### The seed path

**9. Count `consider_bc1` calls per block.** It is 22 instructions but calls
`pack_bc1_scored` (executed ~90) plus a fit. Three seeds are possible
(`extrema_opaque`, `channel_minmax_rgb`, `pca_extremes_rgb`); the third is dead,
so measure whether the second earns its keep.

**10. Check whether the second seed fires when it cannot win.**
`if !(mx == max_c && mn == min_c)` guards it. Measure the hit rate — if the two
seeds agree often, that comparison is cheap and the guard is doing its job; if
they rarely agree, the second seed is a full extra fit on most blocks.

**11. `bc1_fit_4color_avx2` is 125 instructions and runs ~15x a block.**
That is ~1,875 a block, the largest *confirmed* item on this path. Its loop over
4 colours is unrolled; the pixel loads are CSE'd. Look for the remaining scalar
palette assembly, which was worth -449 the last time it was examined.

**12. Fuse `pack_bc1_scored`'s palette build with the fit.** Same pack-then-widen
round trip removed from mode 6 (-537) and from the RDO ring buffer. Partly done —
verify no byte palette is still built on the AVX2 path.

**13. `to_565` / `from_565` round trips.** The 565 quantisation is exact and
cheap, but check it is not being done twice per candidate.

### Structural questions worth asking

**14. Should BC1 spend its budget differently?**
The tree says the unsigned BC4/BC5 window was turned off specifically to fund
"the BC1 lattice and BC7 mode 5". That trade was made when the lattice's cost was
unmeasured. Re-examine it now that BC4/BC5 no longer needs the budget.

**15. Is the 1.5x worth removing at all?**
We are 1.44-1.66x slower and +0.63 to +1.31 dB better. DirectXTex's BC1 is a
single-pass fit. **It may be correct to change nothing here** and instead say so
explicitly in the README. Decide this deliberately rather than by default.

**16. Offer the trade as a quality tier.** If the lattice is what costs 1.5x,
`EncodeQuality::Fast` should already skip it — verify, and if the Fast tier
matches DirectXTex on speed while keeping most of the dB, that is the answer to
ship rather than a code change.

### The synthetic-set quality losses

**17. Characterise the BC1/BC2/BC3 synthetic losses.** Fifteen of the seventeen.
They are concentrated in X-MIP, X-ARRAY, X-CUBE, X-NPOT and X-VOL contexts, at
-0.31 to -1.04 dB, while X-2D wins. That pattern says the loss is in **small mip
levels or non-power-of-two edge blocks**, not in the core encoder.

**18. Check `gather_block`'s edge path.** Its interior fast path is ~30
instructions; the edge path clamps per pixel. NPOT and small mips hit the edge
path constantly, and it is the one place where a correctness difference could
masquerade as a quality difference.

**19. Verify our mip generation matches DirectXTex's filter.** If we downsample
differently, the synthetic mip losses are a *resampling* difference and not an
encoder difference at all — which would explain why X-2D wins and every
mip-bearing context loses.

### Honesty

**20. Settle the DirectXTex BC7 flag before quoting 125-165x.**
Nominally `TEX_COMPRESS_BC7_QUICK`, yet 4.7-10.7 s per 1K texture. If QUICK is
not reaching the encoder we are beating its slow path, and the honest number is
smaller. This is the one claim in the whole comparison that looks too good.

---

## Sequencing

Items 1-3 first, always. Then 4-8, because the lattice is the prime suspect and
`BC1_LATTICE_MIN_ERR = 0` is the same unmeasured-gate shape that made the signed
sweep cost 5x. Only then 9-13.

**Item 15 is a real possible outcome, not a formality.** The signed sweep was
worth removing because it bought 0.05-0.61 dB for 5x. The BC1 lattice buys
+0.63-1.31 dB for 1.5x, which is an order of magnitude better trade. If the
measurements say it is earning its cost, the correct action is to document it and
stop.
