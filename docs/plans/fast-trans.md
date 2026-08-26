# Plan: fast-trans — deploying rusty-fast-transcendentals across rusty_dds

Status: **all bricks landed** (2026-08-26). Libm rounding call sites in the
lib: **20 → 0** (`roundf` 10→0, `round` 6→0, `ceilf` 4→0), verified by the
`--emit asm` count. Byte-identity: all 24 probe hashes unchanged
(`probe_encbytes`, `probe_6hbytes`, `probe_rdo_bytes`, and the new
`probe_snorm_bytes` added for the signed LS path). New equivalence oracles:
`round_clamp_bc6h_matches_round_then_clamp`,
`round_clamp_snorm_matches_round_then_clamp`, `ceil_i32_matches_ceil`,
`bucket_matches_unhoisted` (sim). Tier 3's probe **did** show the four
`ceilf` sites, so `ceil_i32` (exact for both signs — truncation already IS
the ceiling for negatives) was landed rather than left. Tier 4's LOD
`log2` stays libm by design (feeds `request_hash`); pack-cook sites stay
untouched (untimed).
Scope: every scalar libm transcendental / non-baseline rounding call in
`src/` and `sim/`, judged by the skill's targeting rule (exp/ln/tanh/round
yes; sqrt/min/max/abs are SSE2 baseline — never touch).
Skill: `rusty-fast-transcendentals`. Sibling discipline:
`codec-measurement` for any number, byte-identical A/B for every brick.

---

## 1. What the survey found

The central kernel **already exists and is already proven**:
[`round_clamp_u8`](../../src/encode/blocks.rs) (blocks.rs:52) documents the
clamp-first + widen-to-f64 + `+0.5` argument, killed 49 `callq roundf` sites
in a prior campaign, has a lane-wise SIMD twin
([simd.rs:1444](../../src/encode/blocks/simd.rs)) and an exhaustive oracle
test (`round_clamp_u8_matches_round_then_clamp`, oracles.rs:394).

So this campaign is **not** about writing a polynomial `exp` — there is no
per-element `exp`/`tanh` loop anywhere in the codec. It is entirely about
the skill's §4 trap: *the kernel exists and the shipping path doesn't reach
it*. Four `ls_endpoints`-shaped functions solve the same least-squares
epilogue; two call the fast kernel, two still pay libm.

| site | calls | status |
|---|---|---|
| bc7.rs:627, bc7.rs:1277, rdo.rs:697, bc1.rs:667 | `round_clamp_u8` | fixed |
| **m1.rs:336, alpha.rs:598, alpha.rs:999, bc6h.rs:376** | **libm `roundf`** | **unfixed** |

---

## 2. Targets, ranked

### Tier 1 — reachability gaps: direct swap, byte-identical by existing proof

1. **[m1.rs:336-337](../../src/encode/blocks/m1.rs)** — BC7 mode-1
   `ls_endpoints` writes `((…)/det).round().clamp(0.0, 255.0) as u8` —
   the *exact* expression `round_clamp_u8` replaces, six calls per LS
   solve. Swap to `super::round_clamp_u8(…)`. The sibling in
   [bc7.rs:627](../../src/encode/blocks/bc7.rs) already did this; m1 is
   the drifted copy the skill's §7 predicts.
2. **[alpha.rs:598-600](../../src/encode/blocks/alpha.rs)** —
   `ls_alpha_endpoints_u` (BC4/BC5/BC3-alpha LS refit): same expression,
   two calls per solve. Direct swap.

### Tier 2 — small new variants of the proven kernel

3. **[bc6h.rs:376-377](../../src/encode/bc6h.rs)** — BC6H `ls_endpoints`:
   `.round().clamp(0.0, 65504.0) as i32`, six calls per solve per refine
   round. Needs a `round_clamp_bc6h(x) -> i32` sibling. The same
   equivalence argument holds verbatim: clamp first (integer bounds),
   value is then non-negative so half-away-from-zero = `floor(x + 0.5)`,
   and the `+0.5` in f64 is exact (65504.5 needs ~17 mantissa bits against
   f64's 53). NaN cannot reach it (`det` guard), but the test should sweep
   NaN/±inf anyway and assert both spellings agree.
4. **[alpha.rs:999-1001](../../src/encode/blocks/alpha.rs)** —
   `ls_alpha_endpoints_s` (signed BC4/BC5): `.clamp(-127.0, 127.0)`, so
   the non-negative shortcut does not apply. Signed helper:
   `let y = x.clamp(-127.0, 127.0) as f64; (y + 0.5f64.copysign(y)) as i32`
   — `as` truncates toward zero, and trunc(|y|+0.5) with the sign restored
   **is** round-half-away-from-zero. Exhaustive sweep test vs
   `.round().clamp(…) as i32` over a dense grid plus every half-integer
   tie in range.

### Tier 3 — the `ceil` sites in RDO (measure before touching)

5. **[rdo.rs:213, 261, 273, 319](../../src/encode/blocks/rdo.rs)** —
   `(best_j + lam * SAVE).ceil() as i32`. `ceil` needs SSE4.1, above the
   x86-64 baseline, so it lowers to a call — but these sit in scalar
   control flow (one limit per candidate class per block), **not** in a
   vectorizable loop. The win is call overhead only, no 8-lane tail
   behind it. If the instruction-count harness (the d8c7105 discipline)
   says it matters: branch-free exact
   `fn ceil_pos_i32(x: f32) -> i32 { let t = x as i32; t + ((x > t as f32) as i32) }`
   with an equivalence sweep. Otherwise leave it.

### Tier 4 — sim crate (the measurement rig; a different bar)

6. **[scenario.rs:497-498](../../sim/src/scenario.rs)** — LOD select:
   `.log2()` + `.round()` per visible object per frame. Two libm calls,
   but the output feeds `request_hash`, the work-count parity gate — any
   approximation that shifts one LOD boundary changes the *workload*, and
   the gate exists precisely to reject that. Only an exactly-equivalent
   reformulation is admissible, and only if a profile ever shows
   `requests()` hot. Default: leave it.
7. **[metrics.rs:293](../../sim/src/metrics.rs)** — histogram `bucket()`
   recomputes the **constant** `(HIST_HI_MS / HIST_LO_MS).ln()` on every
   sample. Hoisting it to a `const`/`LazyLock` is the
   codec-eliminate-redundancy move — free, exact, halves the transcendental
   count per record. The remaining variable `.ln()` per sample is fine at
   histogram rates. `bucket_value`'s `.exp()` (metrics.rs:299) runs at
   render time — cold, skip.
8. **[pack.rs:328](../../sim/src/pack.rs)** (`value_noise` `.floor()`) and
   **[pack.rs:427](../../sim/src/pack.rs)** (`enc_unit` `.round()`) are
   per-pixel — but they run at pack-cook time, which 6adc4f1 deliberately
   faults in **before anything is timed**. Untimed code gets no bricks.

---

## 3. Non-targets (checked, and why)

- **Every `sqrt`** — bc1.rs:594, pack.rs:422, scenario.rs:492 — SSE2
  baseline, already vectorises; the skill's week-wasting column.
- **`psnr_rgba8`'s `log10`** ([mod.rs:525](../../src/encode/mod.rs)) and
  every PSNR/log2 in tests, examples, and oracles.rs — **oracle code stays
  libm.** An approximated gate is not a gate.
- **gpu/math.rs:44 `tan`** — once per projection matrix.
- **probe/example `sin`/`cos` pattern generators** — one-time fixture
  synthesis, and their exact output is what the A/B corpus is anchored to.
- **decode/bc6h.rs "magic"** — already the bit-trick denormal path; no
  libm there.
- **No polynomial `exp`/`tanh`/`sigmoid`/`erf` work anywhere** — the
  workspace has no per-element transcendental loop to widen. If one ever
  appears (e.g. a perceptual metric), the skill's recipe applies wholesale.

---

## 4. Gates and order

One brick per commit, each A/B'd against the corpus baseline anchor
(`bench_encode_corpus`), in this order:

1. **Brick 1** (Tier 1): m1.rs + alpha.rs:598 swaps. Gate: encode output
   **byte-identical** on the full corpus (the helper's oracle test already
   proves the expression identity; the corpus run proves reachability
   didn't change anything else).
2. **Brick 2** (Tier 2a): bc6h helper + swap. Gate: new exhaustive
   equivalence test (in-range dense sweep + ties + NaN/inf) **and**
   byte-identical BC6H corpus encode.
3. **Brick 3** (Tier 2b): signed helper + alpha.rs:999 swap. Same gate
   shape.
4. **Brick 4** (Tier 4, sim): metrics.rs constant hoist. Gate: bucket
   indices identical over a sweep of recorded values.
5. **Tier 3 only if** the instruction-count probe shows the `ceil` calls
   at all; expected verdict is "leave".

Expected size: honest — these are 2-8 scalar calls per LS solve in the
per-block refit loops, not a 16M-element barrier; the skill's 4.71x was
the rounding *step*, not the encoder. The prior campaign judged the same
substitution worth 49 sites, and reachability bugs are free wins: exact,
safe-Rust, one-line diffs. Measure with instruction counts first
(deterministic, no pinning), wall time second.
