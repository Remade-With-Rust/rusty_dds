# Plan: closing the runtime streaming gap vs DirectXTex

Status: **rounds one to four complete** (2026-08-18). Runtime gap closed (§7-§9);
decode fixed and opened to caller threads (§10-§11); payload buffers recycled
(§12); **the harness's own instrument was 61% of its measurement and is fixed
(§13) — every board recorded before that is void.**
Scope: `rusty_dds` runtime (container parse + subresource/upload-plan queries).
Not in scope: the encoder (already ahead) or the decoder (already ahead).
Evidence: [`sim/`](../../sim/) — boards in [`docs/artifacts/`](../artifacts/),
profile via `cargo run --release --example profile_rusty_dds` in `sim/`.

---

## 1. The symptom

The simulator was built to see whether rusty_dds helps a game's frame stability.
On the **Stream** profile — the one a running game actually exercises — it does
not. It costs. Measured detached, pinned, ABBA, N=5-7, `traverse`/high tier,
192 textures:

| | DirectXTex (loader) | rusty_dds | delta |
|---|---:|---:|---|
| Container parse, total | **2.8 ms** | 433.4 ms | 155x |
| Run CPU | **1.906 s** | 2.406 s | +26% |
| Hitches (>1 ms) | **304** | 555 | +83% |
| Peak working set | 132.7 MiB | 134.7 MiB | +1.5% |

The four-pane isolation grid reproduces it and shows both main effects
replicating across the other factor — rusty_dds costs on both allocators, and
`rusty_alloc` recovers part of it on both stacks. The two roughly cancel, so
"both technologies on" currently lands on top of the conventional stack rather
than beating it.

---

## 2. Root cause, measured

Profiled on one 1024² BC7 texture, 1.33 MiB payload, 200 iterations:

```
Dds::read                       0.3458 ms/call, 1.0 allocations, 1.00x payload
upload_plan_compressed          12.0 allocations per subresource query
surface()                       6.0 allocations per subresource query
copy into a FRESH buffer        0.3864 ms
copy into a WARM buffer         0.0486 ms   (28.8 GB/s)
```

### 2.1 The dominant cost is first-touch page faults, not copying

`Dds::read` ([src/lib.rs:251](../../src/lib.rs)) reads the payload into a fresh
`Vec<u8>`. **7.9x of that call is the operating system faulting in and zeroing
pages we are about to overwrite anyway.** The copy itself runs at 28.8 GB/s; the
call runs at 3.85 GB/s.

This single mechanism explains every observation:

- **Why DirectXTex's loader wins.** `DDSTextureLoader` points into the caller's
  buffer. It touches no new pages, so it pays none of this.
- **Why `rusty_alloc` recovers 65%** (parse 433 -> 153 ms). A mimalloc-shaped
  allocator recycles segments instead of returning them to the OS, so the
  payload buffer is usually already resident. It is treating the symptom.
- **Why we tie DirectXTex's `ScratchImage` path** (447.0 vs 470.8 ms, inside the
  noise). That path copies too, so it pays the same tax — and we use **47% less
  peak memory** doing it.

A refuted hypothesis, recorded so it is not re-tried: this is **not** `Vec`
growth. `read_to_end` over a cursor allocates exactly once (1.00x the payload),
so reserving capacity up front buys nothing.

### 2.2 A `Box` per format query, twelve per subresource

`Dds::get_format()` ([src/lib.rs:328](../../src/lib.rs)) returns
`Option<Box<dyn DataFormat>>` — a heap allocation, every call. It is called by
`get_bits_per_pixel`, `get_pitch`, `get_pitch_height` and
`get_min_mipmap_size_in_bytes`, all of which sit underneath every subresource
offset computation.

Result: **12 allocations per `upload_plan_compressed`**, ~132 per texture over an
11-mip chain, repeated on every re-open after eviction. It is ~1.5% of wall time
but it is most of the *allocation count*, and allocation count is what drives
allocator tail latency — which is what a hitch is.

### 2.3 The same work, computed twice

`upload_plan_compressed` ([src/upload.rs:78](../../src/upload.rs)) calls both
`self.surface(id)` and `self.subresource_range(id)`. `surface()` internally calls
`subresource_range()`. So the subresource offset — an O(mips) walk of the mip
chain in `mip_offset_and_size_in_chain`
([src/surface.rs:265](../../src/surface.rs)) plus an O(mips) `get_array_stride` —
is computed **twice per query**. The allocation counts confirm it exactly:
`surface()` is 6, the plan is 12.

---

## 3. What the ceiling is — read before choosing work

**In the Stream profile, parity is the best outcome available.** Once the copy is
gone, both stacks do the same thing: hand the GPU BCn bytes that are already in
memory. There is no remaining work to be cleverer about. Removing the parse cost
takes run CPU from 2.406 s to roughly 1.97 s against DirectXTex's 1.906 s — a
tie, not a win.

That is not a reason to skip it. Being *slower* than the incumbent on the profile
a game runs is disqualifying no matter how good the encoder is; a studio will not
adopt a stack that costs them frame time. Parity here converts the conversation
back to where we are genuinely ahead:

| profile | where we stand | why |
|---|---|---|
| **Stream** (runtime) | behind -> **target parity** | both stacks just move bytes |
| **Transcode** (decode) | **24/24 ahead** | real CPU work, our decoder is faster |
| **Cook** (encode) | **21/3 ahead, 22/2/0 on PSNR, RDO -4..-15%** | bake farm, patch size |
| **Memory** | **47% below `ScratchImage`** | and a differentiator at fixed VRAM |

So: fix Stream to stop losing, and sell on Transcode, Cook and memory.

---

## 4. The work

Ordered by value per unit risk. Each item states its own gate.

### A. A borrowing parse path — `Dds` over `&[u8]`

**The fix.** Add a zero-copy constructor that borrows the caller's bytes instead
of owning them. Shape to settle in review; the constraint is that
`SurfaceView`/`upload_plan_*` must work unchanged on it.

```rust
// sketch, not a committed API
pub struct DdsRef<'a> { header: Header, header10: Option<Header10>, data: &'a [u8] }
impl<'a> DdsRef<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<DdsRef<'a>, Error>;
}
```

Engines already have the file bytes — from `fs::read`, a memory map, or an
archive decompressor. Making them hand those to us and pay for a second copy is
the whole defect.

**Expected:** parse 433 ms -> ~3 ms, matching DirectXTex's loader. Run CPU
2.406 -> ~1.97 s. Hitches 555 -> ~300.
**Gate:** `sim bench --arms rusty,dxtex` on `traverse`/high shows parse inside the
null band of DirectXTex's loader; `trace_hash` unchanged; the whole decode/encode
matrix still green.
**Risk:** additive API, no change to `Dds`. Lifetimes touch `SurfaceView`, which
already borrows.

### B. A pooled/caller-supplied payload buffer

**The fix.** `Dds::read_into(r, &mut Vec<u8>)` — or accept a buffer the caller
recycles — so the owning path stops faulting fresh pages per texture.

This exists because **A does not cover every caller.** A streaming engine that
decompresses from an archive has no borrowable buffer to point at; it needs
somewhere to put the bytes, and reusing one warm buffer is the whole win.

**Expected:** the same 7.9x on the owning path (0.386 -> 0.049 ms per texture).
**Gate:** the profile example's fresh-vs-warm gap closes.
**Risk:** low, additive.

### C. Kill the `Box<dyn DataFormat>` on the query path

**The fix.** `get_format()` is a public convenience and can stay. The internal
callers must not use it: give `get_pitch`, `get_pitch_height`,
`get_bits_per_pixel` and `get_min_mipmap_size_in_bytes` an allocation-free path
over the concrete `DxgiFormat` / `D3DFormat` enums (both `Copy`), matching on
which is present rather than boxing.

**Expected:** 12 allocations per subresource query -> 0. ~132 allocations per
texture removed. Small wall-time win; the real target is allocator tail latency,
i.e. hitches.
**Gate:** the profile example reports 0 allocations per query; `sim bench` shows
`Allocations` down and hitches no worse.
**Risk:** internal only, no API change. `get_format()` keeps working.

### D. Compute the subresource layout once

**The fix.** Have `upload_plan_compressed` compute the range once and derive the
`SurfaceView` from it, instead of calling both. Then consider caching the mip
chain: the offsets are a pure function of the header, so a small
`[u32; MAX_MIPS]` computed at parse turns every subresource query from two
O(mips) walks into an array index.

**Expected:** subresource query 0.48 µs -> well under 0.1 µs; halves the work
even before C.
**Gate:** byte-identical `upload_plan_compressed` output for the whole
`decode_matrix` / `encode_matrix` corpus.
**Risk:** low. The cache is derived state, so it must be built at parse and never
mutated afterwards — `Dds::data` is `pub`, so a caller *can* mutate the payload;
the cache must depend only on the header, which is not `pub`-mutable in practice.
Confirm that before caching.

### E. Re-measure the whole matrix

Not optional, and not last because it is least important. After A-D:

```sh
cd sim
./target/release/sim bench --pack pack/high192 --scenario traverse \
    --arms rusty,rusty+ra,dxtex,dxtex+ra --reps 7 --pin --out runs/after
./target/release/sim board --runs runs/after --out ../docs/artifacts/simulator-matrix.md
```

The prediction to falsify: **rusty_alloc's advantage should shrink**, because it
is currently paid for by our own page-fault tax. If it does not shrink, the
mechanism in §2.1 is wrong and this plan needs revisiting.

---

## 5. Sequencing

1. **C and D first.** Internal, no API surface, cheap, and they make the profile
   numbers legible before the big change lands.
2. **A next.** The headline. Ship behind the existing decode/encode gates.
3. **B alongside A**, for callers that cannot borrow.
4. **E**, and update the README's runtime claims honestly either way.

A and B change the public API, so they want a `0.3` and a migration note in
[docs/migration-ddsfile.md](../migration-ddsfile.md).

---

## 6. What not to do

- **Do not reserve capacity in `read_to_end`.** Measured: it already allocates
  once, at exactly 1.00x the payload. Refuted.
- **Do not reach for SIMD or a faster memcpy.** The warm copy already runs at
  28.8 GB/s. The cost is page faults, not bandwidth.
- **Do not ship `rusty_alloc` as the answer to this.** It recovers 65% of a tax
  we impose on ourselves, at ~124 MiB more peak working set. Fix the cause; then
  re-judge the allocator on its own merits.
- **Do not chase a Stream-profile win over DirectXTex.** §3 — parity is the
  ceiling, and claiming more would not survive a studio's own measurement.

---

## 7. Results — C and D, landed 2026-08-18

Gated on the **deterministic** numbers, not durations. An allocation count is
exact, reproducible, needs no pinning and no null band, and N=1 settles it; a
duration on this box needs seven pinned ABBA reps to say anything at all.

| | before | after | |
|---|---:|---:|---|
| Allocations per run (`traverse`/high, 10 500 frames) | 263 112 | **48 072** | **-81.7%** |
| ...against DirectXTex's arm | 46 362 | 46 362 | now **+3.7%**, was **+468%** |
| Allocations per `upload_plan_compressed` | 12 | **0** | |
| Allocations per `surface()` | 6 | **0** | |
| Uploaded bytes (correctness gate) | 822.241 MiB | **822.241 MiB** | identical |

The uploaded-byte total is the gate that matters: same work, same bytes, fewer
allocations. The whole `rusty_dds` test suite is green.

**C beat its own prediction.** The plan guessed "small wall-time win"; the
micro-benchmark shows the query path 13x faster (0.0053 -> 0.0004 ms per 11-mip
chain). Removing `dyn` did more than remove a `malloc` — it let the compiler
devirtualise and inline the format queries.

**D is unmeasurable at this granularity, and is landed on structure, not on a
number.** Per-query cost is 27-64 ns with 2x run-to-run spread, so the second
mip-chain walk is below the noise floor of anything this harness can resolve.
It removes provably duplicated work; that is the entire justification, and no
timing claim is attached to it.

**The mip-offset cache in D is now unnecessary.** It was proposed to turn an
O(mips) walk into an array index. At 27-64 ns for an eleven-mip chain the walk
is not worth caching, and a cache derived from a `pub` payload would be a
correctness hazard for no measurable gain. Dropped.

### What this does *not* fix

The dominant cost is untouched. Per run, C+D removes ~6 ms of query time out of
~2 400 ms — roughly **0.25%**. The 87% page-fault tax on the payload copy (§2.1)
is still there and is still the reason we lose to DirectXTex's borrowing loader.
**A and B are where the frame-time result lives.**

The allocation reduction should show up as fewer hitches rather than less CPU,
since allocation count drives allocator tail latency. That prediction is
untested and needs a pinned ABBA bench to confirm or refute.

---

## 8. Results — A, landed 2026-08-18

`Dds` is now generic over how its payload is stored:

```rust
pub struct DdsBase<D = Vec<u8>> { pub header: Header, pub header10: Option<Header10>, pub data: D }
pub type Dds        = DdsBase<Vec<u8>>;   // owns   — unchanged for every caller
pub type DdsView<'a> = DdsBase<&'a [u8]>; // borrows — DdsView::parse(&bytes)
```

One implementation serves both: the payload is touched in only six places
library-wide, because everything else already goes through `SurfaceView`, which
borrows. `Dds` keeps its exact spelling as an alias, so no existing caller
changes. `get_mut_data` / `surface_mut` moved to an `AsMut` block, which a
`DdsView` correctly cannot satisfy.

### The deterministic gate

| | before | after | DirectXTex |
|---|---:|---:|---:|
| Allocations per run | 263 112 | **46 362** | 46 362 |
| Container parse, total | 433.4 ms | **1.5 ms** | 2.0 ms |
| Uploaded bytes | 822.241 MiB | **822.241 MiB** | 822.241 MiB |
| `DdsView::parse` allocations | — | **0** | — |

The allocation counts are now **exactly equal**. Every allocation left is
harness-side and identical in both arms: neither stack allocates anything the
other does not. That is a stronger statement than any duration, and N=1 settles
it.

### The timing verdict (pinned, ABBA, N=7)

Every row **inside the null band** except container parse, where rusty_dds is now
**33% faster than DirectXTex** and outside it:

| metric | `dxtex` | `rusty` | verdict |
|---|---:|---:|---|
| Run CPU | 1.594 s | 1.656 s | inside the noise |
| Streaming CPU | 1290.7 ms | 1302.4 ms | inside the noise |
| **Container parse** | 2.028 ms | **1.519 ms** | **outside the band, ours** |
| Frame cost p99 | 1.026 ms | 1.022 ms | inside the noise |
| Hitches | 164 | 156 | inside the noise |

Compare only *within* a board, never across them: absolute numbers move with
machine load between sessions. Within this board the gap is gone.

### The prediction in §4E was confirmed

"rusty_alloc's advantage should shrink, because it is currently paid for by our
own page-fault tax." Four-arm matrix, N=5:

| | run CPU | peak working set |
|---|---:|---:|
| rusty_dds | 1.656 s | 134.2 MiB |
| rusty_dds + rusty_alloc | 1.375 s | 260.9 MiB |
| DirectXTex | 1.641 s | 134.2 MiB |
| DirectXTex + rusty_alloc | 1.391 s | 260.0 MiB |

`rusty_alloc` now helps **both stacks equally** (-17% and -15%), where before it
helped rusty_dds disproportionately. Its effect is no longer entangled with our
defect, which is what the prediction claimed. It still costs **~127 MiB more
peak working set**, on both stacks, and that trade should now be judged on its
own merits rather than as compensation for a copy we should never have made.

### B followed — see §9.

---

## 9. Results — B, landed 2026-08-18

```rust
DdsView::read_into(r, &mut buf)                  // recycle your own buffer
DdsView::read_into_limited(r, &mut buf, max)     // ...with a hard ceiling
```

For callers who cannot borrow: an archive decompressor, a network stream. They
would otherwise be pushed back onto `Dds::read` and pay the page-fault tax
[§2.1](#21-the-dominant-cost-is-first-touch-page-faults-not-copying) all over
again. `buf` is cleared rather than reallocated, so its pages stay resident from
the second call onwards.

| path | per call | allocations | |
|---|---:|---:|---|
| `Dds::read` (fresh buffer) | 0.3122 ms | 1.0 | the old behaviour |
| `DdsView::read_into` (recycled) | **0.0461 ms** | **0.01** | **6.8x faster** |
| `DdsView::parse` (borrowed) | ~0 | **0** | when you already hold the bytes |

**The prediction was met exactly.** §2.1 measured the floor — a copy into memory
that is already resident — at 0.0486 ms. `read_into` lands at 0.0461 ms. What
remains is the copy and nothing else; the page-fault tax is gone rather than
reduced. The 0.01 allocations/call is the single buffer growth amortised over the
run.

`read_into_limited` inherits `read_limited`'s posture: the limit covers the
payload only and an overrun fails closed without buffering the rest.

### Tests added

Buffer reuse is exactly the shape that invites stale-data bugs, so it is gated
rather than trusted:

- `read_into_reuse_does_not_leak_the_previous_payload` — a large texture then a
  small one through the same buffer; the second must not see the first's tail,
  and must match `Dds::read` byte for byte.
- `read_into_limited_is_a_hard_ceiling` — the security posture.
- `view_and_owned_agree` — every fixture parsed both ways must agree on payload,
  dimensions and mip count.

### Where this leaves the library

Three parse paths, each right for a different caller, and none of them paying for
memory it does not need:

| you have | use | cost |
|---|---|---|
| the bytes already (mmap, archive, `fs::read`) | `DdsView::parse` | zero copy, zero allocation |
| a reader, and a buffer you can recycle | `DdsView::read_into` | one copy, warm pages |
| a reader, and you want ownership | `Dds::read` | one copy, fresh pages — unchanged |

---

## 10. Round two — the decode path

Stream is finished as an optimization target: rusty_dds now costs **1.5 ms of a
1264 ms** streaming run, 0.1%. The remaining runtime path worth profiling is
decode — the Transcode profile.

### 10.1 LANDED — the BC7 parallel threshold was set to the losing case

`decode_bc7` goes parallel above `BC7_PARALLEL_MIN_BLOCKS`. That constant was
**4 096**, and 4 096 blocks is precisely where spawning threads is a net loss.
Measured, 24-core box:

| blocks | serial | parallel | |
|---|---:|---:|---|
| 65 536 | 172.6 Mpx/s | 484.0 | par wins 2.8x |
| 16 384 | 172.3 Mpx/s | 265.4 | par wins 1.54x |
| **4 096** | **200.9 Mpx/s** | **88.9** | **par loses 2.26x** |

Raised to **16 384**, the smallest size where parallelism is *measured* to win.
The deterministic confirmation: at that size the call drops from **75
allocations to 1** — the 74 were thread spawns.

The true break-even is somewhere between 4 096 and 16 384 and would need a
dedicated sweep; the constant errs to the proven side.

### 10.2 LANDED — a syscall on every decode

`std::thread::available_parallelism()` ran per call. It cannot usefully change
within a process; now cached in a `OnceLock`.

### 10.3 REFUTED — scaling workers with the work

Hypothesis: 24 threads over-subscribe a small job, so `workers = blocks / 8192`
should help. **Measurement disagreed** — it cost mip 0 31% (484 -> 332 Mpx/s) and
mip 1 26% (265 -> 195). Above the threshold the spawn cost amortises fine and
more workers is simply better. Reverted, with a comment in `bcn.rs` so it is not
re-tried.

### 10.4 OPEN — the decode output buffer is 41% of a decode

`decode_rgba8` returns a fresh `vec![0u8; w*h*4]` every call. That is
`alloc_zeroed`: the OS hands over zeroed pages, and decode then overwrites every
one of them. It is the **same defect as §2.1**, on a buffer 3x larger than the
payload.

| | 4.00 MiB output buffer |
|---|---:|
| fresh `vec![0u8; n]` | **1.4446 ms** |
| refilling a resident buffer | 0.1108 ms |
| share of a 3.522 ms decode | **41%** |

**Proposed fix — `decode_rgba8_into(&mut Vec<u8>)`**, mirroring
[`DdsView::read_into`](#9-results--b-landed-2026-08-18). The caller recycles one
buffer per worker; the buffer is resized once and thereafter written directly, so
the zeroing disappears as well as the faulting.

**Expected:** decode 3.52 -> ~2.2 ms, roughly **-38%** on the Transcode path,
where we are already 24/24 ahead of DirectXTex.
**Gate:** byte-identical output against `decode_rgba8` across the decode matrix;
a reuse test in the shape of `read_into_reuse_does_not_leak_the_previous_payload`.
**Risk:** additive API, no change to existing calls.

### 10.5 FOUND — the parallel decode toll is ~1 ms per call, whatever the work

`decode_bc7_parallel` opens a `thread::scope` and spawns one worker per core on
**every call**. Measured directly on a 24-core box:

```
thread::scope with 24 no-op workers: 0.982 ms per call
```

Against the decodes it is meant to accelerate (1024², 65 536 blocks):

| format | decode | spawn toll | path |
|---|---:|---:|---|
| BC7 | 2.85 ms | **34%** | parallel |
| BC5U | 3.28 ms | would be 30% | serial |
| BC4U | 2.59 ms | would be 38% | serial |
| BC1 | **1.91 ms** | **exceeds the work** | serial |

That is why 24 threads buy only 2.8x — **12% parallel efficiency**. The toll is
fixed; only the work varies.

### 10.6 The fix is not "parallelise the other formats"

BC1, BC4 and BC5 decode are entirely serial, which looks like idle cores. It is
not: BC1 decodes a full 1024² surface in 1.91 ms, *less than the 0.98 ms toll
plus its own share*, so parallelising it under this model would make it slower.
Serial BC1 (626 Mpx/s) already beats parallel BC7 (368-502 Mpx/s).

The ceiling is the spawn model, and there are two ways past it:

1. **A persistent pool inside the library** (rayon, or hand-rolled). Removes the
   toll, but the library then owns threads — and a game engine already has a job
   system that will not appreciate a texture loader spawning 24 threads behind
   its back.
2. **Expose range-based decode and own no threads at all.** Something like
   `decode_rows_into(&self, id, rows: Range<u32>, dst: &mut [u8])`, so the
   caller's existing scheduler drives it. The toll disappears, *every* format
   becomes parallelisable, and the engine keeps control of its own cores.

**(2) is the recommendation.** It is the better fit for the audience, it is
additive, and it composes with §10.4's `decode_rgba8_into`: one buffer the caller
owns, filled by the caller's own workers.

### 10.7 Not worth touching — the encoder's identical pattern

`encode/blocks.rs` and `encode/blocks/oracles.rs` spawn the same way per call.
The difference is scale: a BC7 encode of the same surface is ~50 ms, so a ~1 ms
toll is ~2%. The encoder is frozen behind byte-identical gates from the 2026-08
campaign; the measured gain does not justify disturbing it. Recorded, not acted
on.

---

## 11. Round two, landed — decode into caller memory and caller threads

Two additive APIs, both fixing a measured defect by handing control to the caller:

```rust
dds.decode_rgba8_into(id, &mut buf)?;                 // your buffer
dds.decode_block_rows_into(id, rows, &mut band)?;     // your threads
dds.block_rows(id)?;                                  // how many to split into
```

Internally every decoder gained an `_into` core that writes to a caller slice;
the allocating entry points are now thin wrappers over it, so there is one
implementation per format, not two.

| path (1024^2 BC7) | time | |
|---|---:|---|
| `decode_rgba8` | 2.184 ms | allocates a fresh output |
| **`decode_rgba8_into`** | **1.158 ms** | **1.89x** |
| `decode_block_rows_into` x24 caller threads | 1.209 ms | 1.81x |

**Read the third row carefully.** The benchmark spawns its own threads per
iteration, so it pays the same ~0.73 ms toll the API exists to remove; the decode
work itself is ~0.48 ms. A caller with a persistent job system sees that. The
point of the API is not that it is faster today — it is that the toll becomes
*removable*, which it is not while the library owns the threads.

### The bug this nearly shipped

The first cut inverted validation and allocation. `decode_bcN` used to check the
payload length and *then* allocate; the refactor allocated first and validated
inside the closure. `parser_robustness` caught it immediately:

```
memory allocation of 274877906944 bytes failed   (256 GiB)
```

Width and height are header-derived and the output is 4 bytes a pixel, so a
corrupt header names a surface needing hundreds of gigabytes. Validation now
precedes allocation in `alloc_and_decode` and in `decode_rgba8_into`, with the
reason written at both sites. **The fuzz suite paid for itself here** — this is
exactly the unbounded-allocation class `read_limited` exists to prevent, and it
was introduced by a refactor that looked purely mechanical.

### Gates added

- `decode_into_reuse_matches_fresh_decodes` — large mip, then small, then large
  again through one buffer; each must match the allocating path byte-for-byte, so
  a stale tail cannot survive.
- `decode_block_rows_reassemble_into_the_whole_surface` — a split decode must
  equal the whole-surface decode. That is the contract a caller's scheduler
  depends on.

### Still open

`decode_block_rows_into` refuses volume textures: splitting them wants a slice
index as well as a row range, and no caller has asked. BC6H (`decode_rgba_f32`)
has the same allocating shape and would benefit from the same treatment — its
output is 16 bytes a pixel, so the fresh-buffer tax is 4x worse than RGBA8.

---

## 12. Round three — the buffer is most of the "file read"

The streaming run spent 477 ms of 1264 ms reading files. `std::fs::read`
allocates a fresh `Vec` per call, so the same question applied:

| 1.33 MiB file, warm page cache | |
|---|---:|
| `std::fs::read` -> fresh `Vec` | 0.9501 ms |
| `read_to_end` into a recycled `Vec` | **0.2145 ms** |
| | **4.43x — 77% of a "file read" is the buffer, not the file** |

**This only became recyclable because of §8.** With the owning `Dds::read`, an
engine that recycled its own file buffer still paid for the library's internal
copy. `DdsView` borrows, so the engine's buffer *is* the payload, and reuse
works end to end.

### Landed in the harness

The streamer now returns an evicted texture's payload buffer to a small pool and
reads the next texture into it, via a new `OpenTexture::reclaim`. The DirectXTex
arm reclaims too — closing its handle first, since on the loader path the shim
points into that very buffer.

A/B'd against itself with `--pool-buffers 0`, three runs each, pinned:

| `--pool-buffers` | file read (ms) | allocations |
|---|---|---:|
| 0 | 509.4, 494.9, 505.3 | 46 362 |
| 32 | **375.3, 373.3, 372.6** | 45 162 |

**-26% on file read**, ~129 ms off a 1264 ms streaming run, with non-overlapping
spreads. The trace hash is unchanged (`fc26977f252783f6`), so the work is
identical.

### What the sweep could *not* settle

| `--pool-buffers` | file read (ms) | allocations | peak RSS |
|---|---|---:|---:|
| 8 | 418.8, 409.9 | 45 507 | 149 MiB |
| 32 | 375.3, 373.3 | 45 162 | 157 MiB |
| 64 | 326.5, 316.1, 404.7, 377.7 | 44 809 | 168 MiB |
| 192 | 306.7, 497.0, 366.7, 348.2 | 44 735 | 175 MiB |

Larger pools *do* reuse more — the allocation counts are deterministic and fall
monotonically. But the timing ranges for 32/64/192 overlap completely once the
box got noisier, so **the default stays at 32**: it is the configuration whose
win was measured cleanly, and it costs the least memory of the three. Moving a
default on unresolvable data would be exactly the mistake this plan keeps
catching.

### Two things this suggests, unmeasured

- Reuse is imperfect because buffer sizes vary by format — a recycled BC4 buffer
  (0.35 MiB) does not fit a BC7 payload (1.33 MiB). **Bucketing the pool by size**
  should recover more of the micro-benchmark's 4.43x.
- The pool caps *count*, not *bytes*. Capping bytes would bound its memory
  honestly regardless of the format mix; RSS climbed 149 -> 175 MiB across the
  sweep, which is the trade a streaming engine actually cares about.

---

## 13. Round four — the instrument was most of the measurement

With parse at 1.5 ms and the buffer pool landed, `Staging copy` was the largest
line in the streaming run at ~820 ms. It is not a copy.

`NullRenderer::upload` folds every uploaded byte into an FNV-1a hash — the
work-count parity gate, the thing that proves both stacks handed the GPU the same
bytes. FNV is byte-at-a-time and each multiply depends on the previous one:

| one 1.33 MiB subresource | | |
|---|---:|---:|
| copy only | 0.064 ms | 21.8 GB/s |
| **FNV parity hash** | **1.527 ms** | **0.9 GB/s** |
| copy + hash | 1.481 ms | — |

**The hash was 97% of the "staging copy" row**, ~940 ms of a run that staged
822 MiB, and **~61% of the whole streaming total**. Both arms paid it equally so
comparisons stayed *fair* — but it diluted every real difference threefold.

### The fix

`hash::bulk_hash` keeps FNV-1a's mixing but runs **four independent lanes over
8-byte words**, so the CPU pipelines instead of stalling on one dependency chain.
It is a divergence detector, not a digest, and it is gated as one:

- `bulk_hash_detects_every_single_bit_flip` — every single-bit change, at lengths
  0/1/7/8/31/32/33/1000/4096/4097, including the unaligned tail; plus truncation,
  which the length fold covers.
- `bulk_hash_is_stable_and_seed_sensitive`.

| | before | after |
|---|---:|---:|
| hash throughput | 0.9 GB/s | **29.8 GB/s** (32.5x, now memory-bound) |
| `Staging copy` per run | 820.6 ms | **91.2 ms** |
| Streaming CPU per run | 1213.4 ms | **474.3 ms** |

### What this invalidates

**Every board recorded before this is void**, and two of its numbers were
substantially instrument artifact:

- **Hitch counts.** A hitch is a frame over 1 ms; the hash was pushing ordinary
  frames past that line. The same comparison reads **164 vs 156 hitches** before
  and **2 vs 5** after. Any earlier statement about hitch rates was measuring the
  harness.
- **p99 frame cost**, which nearly halved (1.03 -> 0.58 ms).

`trace_hash` also changes, by construction, so old and new runs cannot be mixed —
the board's comparability gate enforces that on its own.

### The re-run, on the sharpened harness

Pinned, ABBA, N=7. Everything inside the null band except container parse, where
rusty_dds is 25% ahead — and allocation counts now identical to the digit:

| metric | `dxtex` | `rusty` | verdict |
|---|---:|---:|---|
| Run CPU | 0.859 s | 0.891 s | inside the noise |
| Streaming CPU | 497.9 ms | 508.0 ms | inside the noise |
| **Container parse** | 2.121 ms | **1.699 ms** | **outside the band, ours** |
| Frame cost p99 | 0.580 ms | 0.592 ms | inside the noise |
| Allocations | 45 162 | 45 162 | identical |

### The lesson worth keeping

The instrument was never suspected because it was *fair* — both arms paid it, so
every A/B stayed valid. Fairness is not the same as fidelity: a tax both sides
pay still hides the signal underneath it. Measure the profiler, not just with it.

---

## §14 — The two paths nobody had measured (round five)

Rounds one to four all worked on the *streaming* path, because that is where the
simulator pointed. Two paths were never in the simulator at all, so nothing had
ever profiled them: **encode**, and **BC6H HDR decode**. `sim/examples/probe_encode.rs`
exists to close that gap.

### Encode: nothing to fix here

512², 10 mips, per format:

| format | time | allocations |
|---|---:|---:|
| BC1 | 14.92 ms | 159 |
| BC3 | 23.93 ms | 159 |
| BC5U | 18.89 ms | 159 |
| BC7 | 38.99 ms | 159 |

**159 allocations, identical across all four formats.** That is the structural
cost of the mip chain and the container, not per-block work — the encoders
themselves already allocate nothing per block. Encode was never leaking; there
is no win here to take. Recording it so nobody spends a round finding that out
again.

### BC6H: the LDR short-circuit the HDR path never got

`decode_rgba_f32` on 256²: **1.4704 ms, 3 allocations, 2.75 MiB for a 1.00 MiB
output** — 2.75× write amplification.

The cause is one missing early return. The LDR path has always short-circuited
`depth == 1` and returned the decoder's own buffer. The HDR path did not: it
built the surface, then built it *again* into a second full-size `Vec`, for the
single-slice 2D shape that every HDR texture in practice has.

Fixed: **1.4704 → 0.8364 ms, 3 → 2 allocations, 2.75 → 1.75 MiB.**

### Refuted: single-buffer in-place widening

`bcdec_rs::bc6h_float` writes contiguous RGB, so widening to RGBA is
unavoidable — but the *second buffer* looked avoidable. Decode RGB into the
front of one RGBA-sized `vec![0f32; n*4]`, then expand from the back, where the
write index `i*4` always leads the read index `i*3`.

It reaches the ideal on both deterministic metrics: **1 allocation, exactly
1.00 MiB for a 1.00 MiB output.** It measured slower anyway. A backward pass
over a buffer that aliases itself defeats the prefetcher, and the compiler
cannot prove non-aliasing within one slice, so it will not vectorise. The
forward `chunks_exact(3)` widen is worth more than the allocation it costs.

**Reverted, with the measurement in a code comment** so it is not re-tried.

### A caveat on the instrument, honestly

The same reverted-to code measured 0.8364 ms on one run and 1.3257 ms on
another. `probe_encode`'s wall-clock band is wide enough that the ms figures
above should be read as directional; the allocation and byte counts are
deterministic and are what the decision rested on. This is §13's lesson landing
a second time: **measure the profiler, not just with it.** A tightened BC6H
probe is the next thing this file wants.

### Still open

- `decode_rgba_f32_into` — the HDR twin of `decode_rgba8_into`. Same argument,
  same win, not yet written.
- Volume textures in `decode_block_rows_into` still return `UnsupportedFormat`.
- The sim's buffer pool is capped by count, not bytes, and is not size-bucketed.

---

## §15 — BC6H: the format nobody profiled (round six)

§14 said the next thing this file wanted was a tightened probe. It got one, and
the probe immediately found the largest single win of the whole campaign.

### The gap

Profiling every decode path at 1024² side by side, which had never been done:

| format | throughput | vs BC6H |
|---|---:|---:|
| BC7 (caller-parallel) | ~400 Mpx/s | 10.1× |
| BC1 | 337.2 Mpx/s | 8.5× |
| BC7 (internal parallel) | 232.1 Mpx/s | 5.8× |
| BC5U | 179.2 Mpx/s | 4.5× |
| **BC6H** | **39.7 Mpx/s** | — |

BC6H is the most expensive format we ship *and* the only one with no parallel
seam, no `_into` variant, and no caller split. Everything rounds one to five gave
the LDR path, HDR had none of.

### Cause one: a second pass nobody could see

`decode_bc6h` decoded into a full-surface RGB plane, then walked it again to
widen to RGBA. At 1024²: 12 MiB written, 12 MiB read back, 16 MiB written — 40
MiB of traffic for a 16 MiB result.

**The tell was in the numbers all along, and it was not a time.** Throughput
*fell* with surface size — 56.8 / 49.2 / 39.7 Mpx/s at 256 / 512 / 1024. Decode
cost per pixel does not depend on how many pixels there are; a number that
degrades with working-set size is a cache cliff, full stop.

Fixed by fusing both stages through the 192-byte block scratch the NPOT path
already used, which never leaves L1. Throughput flattens: 76.9 / 62.7 / 56.1.

**26.428 → 18.691 ms**, and the shape of the curve changed, which is the part
that proves the mechanism rather than just the result.

### Cause two: no seam

Added `decode_rgba_f32_into` and `decode_block_rows_f32_into` / `block_rows_f32`.

| 1024² BC6H_UF16 | time | throughput |
|---|---:|---:|
| before | 26.428 ms | 39.7 Mpx/s |
| fused pass | 18.691 ms | 56.1 Mpx/s |
| `_into` | 11.941 ms | 87.8 Mpx/s |
| **24-thread caller split** | **2.743 ms** | **382.3 Mpx/s** |

**9.6× end to end**, and BC6H now sits level with BC7 instead of 10× behind it.

### What was *not* done, and why

No internal thread pool. BC7's `thread::scope` costs a measured **1.531 ms of
pure spawn toll per call** before a pixel is touched — 34% of its 4.519 ms
parallel decode — and it still only scales 3.7× on 24 cores because BC7 decode is
memory-bandwidth bound. Handing the caller the split beats the library's own
threads by 1.7× *and* allocates nothing. Giving BC6H a pool would have bought a
worse version of a thing we already know how to do better.

The seam is also honest about when not to use it: at 256² a 24-thread split is
**0.56×**, because spawn cost dominates. That decision belongs to the caller's
scheduler, which knows what else is running. Ours does not.

### The lesson worth keeping

§13 said measure the profiler, not just with it. This round adds: **profile
everything, not just what the harness happens to exercise.** The simulator only
streamed LDR textures, so five rounds of optimisation never once touched the
slowest decode in the crate. The win was not hard to find — it was hard to *look
at*, because nothing pointed there.

### Still open

- Volume textures in both `decode_block_rows_into` and its HDR twin.
- The sim streams no HDR content at all, which is exactly how this went unseen.
- The sim's buffer pool is capped by count, not bytes, and is not size-bucketed.

---

## §16 — Closing the loop: HDR in the harness

§15 ended by naming the cause rather than the symptom: the simulator streams no
HDR content, which is why five rounds never profiled the slowest decode we ship.
This round fixes the harness, not the library — and the harness immediately
found a library bug.

### The pack now cooks BC6H

`Tier::content_for` returns a new sim-level `Content` (LDR + HDR) rather than the
crate's `DecodeContent`, which is LDR-only *by definition* — that type being
LDR-only is structurally how HDR stayed invisible. One texture in sixteen on the
top two tiers is now BC6H_UF16: the sky and the reflection probes, which is both
realistic and exactly the small fraction that is easy to forget.

`Dds::encode_bc6h_uf16` emits a single-mip container, so the chain is assembled
in the cooker: encode each level, splice payloads at `subresource_range`. The
source is a procedural HDR sky with a sun four orders of magnitude above the
horizon — a low-range source would let BC6H settle into one endpoint mode per
block and quietly flatter every number that follows.

### What it found immediately: BC6H had no GPU format

The first run failed on `open`. Not in the sim — in the crate. **BC6H was absent
from `gpu_format` entirely**, so `upload_plan_compressed` failed closed on every
HDR texture. rusty_dds could decode and encode a format it could not hand to a
renderer.

That is the whole argument for this round in one line: the gap was never going to
be found by reading the code, because nothing was asking the question.

### Parity holds with HDR in the pack

900 frames of `traverse`, 32 textures at 512², both stacks:

| arm | request hash | upload hash | uploaded | allocations |
|---|---|---|---:|---:|
| rusty_dds | `b869b26b98c929d0` | `9c28758ed5ce5689` | 2.84 MiB | 472 |
| DirectXTex | `b869b26b98c929d0` | `9c28758ed5ce5689` | 2.84 MiB | 472 |

Identical. The comparability gate that guards every board in this file now covers
HDR content too.

### The number that matters, and it is not 9.6×

`probe_pack_hdr` decodes the cooked pack across the full mip chain. Splitting
**every** level across 24 threads:

**0.53× — slower than serial.**

A ten-level chain is mostly small mips, and `std::thread::scope` costs ~50 µs
even to spawn one worker — more than the entire decode of every level past mip 4.
(That ~50 µs is the same per-thread figure as §15's 1.531 ms / 24 threads. The
two measurements agree, which is why both are believable.)

Splitting only above ~16 384 blocks — 512×512, the *same* crossover rusty_dds
measured independently for BC7 — and decoding the rest inline:

| cooked 512² pack, all mips | time | throughput |
|---|---:|---:|
| serial | 4.889 ms | 143.0 Mpx/s |
| split above threshold | **3.634 ms** | **192.4 Mpx/s** |

**1.35×**, every level bit-identical. That is the honest end-to-end figure. The
9.6× from §15 is mip 0 at 1024²; both are true, and which one a studio feels
depends entirely on their surface sizes. The threshold is now documented on
`decode_block_rows_f32_into` with these numbers, because a caller who splits
naively makes their decode *slower* and would have no way to know why.

### The lesson worth keeping

§15 said profile everything, not just what the harness exercises. This round adds
the corrective: **a synthetic win is a hypothesis until the harness carries real
content.** Nothing here refuted §15 — 1024² mip 0 really is 6.8× — but shipping
that number alone would have handed studios a rule that loses them performance on
every mip chain shorter than the headline.

### Still open

- The DirectXTex arm has no HDR *decode* comparison; the shim exposes
  `dxt_decode_rgba8` only. Streaming and upload are compared 1:1, decode is not.
- Volume textures in both `decode_block_rows_into` and its HDR twin.
- The sim's buffer pool is capped by count, not bytes, and is not size-bucketed.

---

## §17 — "BC6H is slow" — slow against *what*?

§16 shipped a 1.35× and left an honest note that BC6H decode runs at ~143 Mpx/s
against BC1's 337. That reads as a problem. It was the wrong comparison, and the
harness could not say so, because the DirectXTex shim exposed `dxt_decode_rgba8`
and no HDR twin. LDR decode was compared 1:1 on both stacks; **HDR was compared
on neither.**

### First, where the time actually is

Splitting the decode from the scatter around it, 512², 16 384 blocks:

| stage | time | share |
|---|---:|---:|
| `bcdec_rs::bc6h_float` into scratch | 1.771 ms | ~100% |
| scatter RGB→RGBA (measured alone) | 0.447 ms | hidden |
| both together | 1.768 ms | — |

The scatter is **free** — it retires in the decode's shadow. Every remaining
millisecond is inside the block decoder, so no further buffer restructuring can
touch it. That ruled out the entire class of fix §14 and §15 were made of, and
pointed at either a SIMD BC6H decoder or a reality check. The reality check is
cheaper and comes first.

### The comparison that was missing

Added `dxt_decode_rgba_f32` to the shim (`Decompress` to
`R32G32B32A32_FLOAT`), and `decode_rgba_f32` to both providers. Pixels are
asserted equal to 1e-3 before any timing — a speed number between two decoders
that disagree is meaningless.

| BC6H decode | rusty_dds | DirectXTex | ratio |
|---|---:|---:|---:|
| 512² mip 0 | 2.954 ms | 8.885 ms | **3.01×** |
| 256² mip 1 | 0.612 ms | 2.291 ms | **3.75×** |
| 128² mip 2 | 0.130 ms | 0.554 ms | **4.25×** |
| all HDR, all mips | **7.268 ms** (96.2 Mpx/s) | 23.949 ms (29.2 Mpx/s) | **3.30×** |

**We are 3.3× faster than Microsoft's own BC6H decoder, serial, before any
split.** With the §15 caller-parallel seam at 1024² — 382 Mpx/s — the gap is
roughly 13×.

BC6H looked slow because it was measured against BC1: a *different format* doing
a quarter of the work per block, not a different implementation. Fourteen modes,
delta-coded endpoints and half-float conversion cost what they cost.

### A build bug this turned up

`sim/build.rs` gated the CMake invocation on `if !have_libs(&libs)`. The
`rerun-if-changed` lines would correctly re-run the script when
`dxtex_provider.cpp` changed, and the script would then **skip the build**
because a stale `.lib` was already sitting there. Editing the peer's C++ did
nothing, silently.

For a benchmark harness that is worse than a hard failure: it measures last
week's peer code and reports it as today's. CMake's build is incremental, so it
is now always invoked.

### The lesson worth keeping

§13: measure the profiler. §15: profile everything, not just what the harness
runs. §16: a synthetic win is a hypothesis until real content carries it. This
round adds the one they all depend on: **a number without a peer is not a
result.** "143 Mpx/s" was true, reproducible, correctly measured — and it
supported exactly the wrong conclusion, because there was nothing on the other
side of it.

### Still open

- A SIMD BC6H decoder is still the only remaining lever on decode, and it is now
  clearly optional rather than urgent: it would extend a 3.3× lead, not close a
  gap.
- Volume textures in both `decode_block_rows_into` and its HDR twin.
- The sim's buffer pool is capped by count, not bytes, and is not size-bucketed.

---

## §18 — The conversion tail, and two refutations on the way

§17 established that BC6H decode is ~100% the block decoder and that we are 3.3×
ahead of DirectXTex. The goal for this round was the remaining time itself.

### Where the tail was

`bcdec_rs::bc6h_float` is `bc6h_half` into a `[u16; 48]` scratch, then 48 calls
to a half-to-float converter carrying **two branches each**:

| stage, 512², 16 384 blocks | time |
|---|---:|
| `bc6h_half` (block decode only) | 1.369 ms |
| `bc6h_float` (+ 48 conversions) | 1.621 ms |
| **conversion tail** | **0.251 ms — 15.5%** |

Taking the halves ourselves and converting branchlessly recovers most of that.
The conversion is verified exhaustively against the reference for **all 65 536
input bit patterns**, which is the only honest bar for replacing a numeric
primitive: Inf, NaN, denormals and negative zero all have exact bit patterns,
and "it works on sky textures" is not a proof.

### Refuted #1: fusing the conversion into the scatter

The obvious next step is to convert *while* scattering to RGBA — one pass over
the data instead of two. Measured, 1024² 24-thread: **1.72 ms fused against
1.61 ms unfused.** Slower.

48 independent conversions vectorise. A strided read (`s + i*3`) with a strided
write (`d + i*4`) and the conversion inline does not, and the vectoriser is worth
more than the pass it costs. This is the *same shape* as §14's refuted backward
in-place widen: **fusing is not free when it defeats vectorisation.** Two
refutations with one mechanism is a pattern, and it is now in a code comment at
the site so the third attempt does not happen.

### Refuted #2: my own first measurement

The fused version was initially reported here as a **1.56× win**. It was not. That
number came from comparing against a figure measured in an earlier session under
different thermal and cache conditions, rather than running the two versions
back to back. A controlled ABAB immediately showed the fused variant was a
*regression*.

This is §13's lesson — measure the profiler, not just with it — arriving as a
mistake rather than as advice, in the same file that records the advice. Worth
keeping visible: the discipline is not knowing the rule, it is running the A/B
when you already believe you know the answer.

Every figure in this section is from an ABAB against the immediately preceding
code, on the same box, in the same session.

### The result

1024² BC6H_UF16:

| | 0.3.2 | 0.3.3 |
|---|---:|---:|
| serial | 12.103 / 11.455 ms | **10.780 / 10.629 ms** |
| 24-thread split | 1.839 / 1.888 ms | **1.605 / 1.607 ms** |
| throughput | 555.5 / 570.3 Mpx/s | **653.2 / 652.6 Mpx/s** |

Against DirectXTex on the cooked pack, all mips: **3.75×**, up from 3.30×.

Cumulative since §15 opened this thread: **26.428 ms to 1.605 ms — 16.5×.**

### What is left, honestly

The tail is now spent. `bc6h_half` is 84% of the call and it is somebody else's
code: a per-pixel bitstream read, a partition-set branch, and three
interpolate-and-unquantize steps. Beating it means writing our own BC6H block
decoder — specialising the single-subset modes (10–13), which have no partition
table and contiguous index bits, and vectorising the 16-pixel interpolation.

That is a real project, not a round. It is also **optional**: it would extend a
3.75× lead over the reference implementation, not close a gap. Recording it as
the next big swing rather than the next increment.

### Still open

- A specialised BC6H block decoder (above).
- Volume textures in both `decode_block_rows_into` and its HDR twin.
- The sim's buffer pool is capped by count, not bytes, and is not size-bucketed.

---

## §19 — The whole matrix, and where the lead is thinnest

§18 ended by naming a specialised BC6H block decoder as the next big swing. Before
starting a project that size, one cheap question: **is BC6H actually where the
lead is thinnest?** Nobody had checked, because the LDR decode A/B had never been
run — both providers implement `decode_rgba8` and nothing ever called them.

### The matrix

Mip 0, cooked pack, agreement checked before timing:

| format | rusty_dds | DirectXTex | ratio |
|---|---:|---:|---:|
| BC1 | 684.7 Mpx/s | 107.8 Mpx/s | **6.35×** |
| BC5U | 421.6 Mpx/s | 72.8 Mpx/s | **5.79×** |
| BC4U | 543.4 Mpx/s | 98.2 Mpx/s | **5.53×** |
| BC6H | 114.8 Mpx/s | 31.3 Mpx/s | 3.67× |
| BC7 | 263.2 Mpx/s | 72.6 Mpx/s | **3.63×** |
| all | | | **4.82×** |

The answer: **BC7 is the thinnest lead, not BC6H** — and BC7 is the format modern
games actually ship most of. §18's plan was aimed one format to the left.

### Both remaining targets have no tail left

§18 won by finding a *tail* — work outside the block decoder. There isn't one
here. `decode_bc7_direct` writes straight into the caller's buffer with a pitch:
no scratch, no widen, no conversion. BC7 decode is **100% `bcdec_rs::bc7`**, the
same way BC6H is 84% `bc6h_half`.

So both remaining targets need the same thing — a custom block decoder — and
neither admits a cheap structural fix. That is worth knowing before spending a
round looking for one.

### The correctness finding

BC4 disagreed with DirectXTex in **exactly 50.000%** of bytes. Exactly half is
never rounding, and it wasn't:

| | rusty_dds | DirectXTex |
|---|---|---|
| BC4 pixel | `146,0,0,255` | `146,146,146,255` |
| R, A disagreement | 0 / 262 144 | |
| G, B disagreement | 262 144 / 262 144 | |

We emit what a GPU returns when sampling BC4 — absent channels zero, alpha one.
DirectXTex **replicates** the single channel so a roughness or height map previews
as greyscale. Neither is wrong. But a studio porting from
`DirectXTex::Decompress` would find every single-channel map turning red, with
nothing in our docs to explain it. Now documented on `decode_rgba8`, with the
measurement. DirectXTex does not replicate for BC5, so only BC4 is affected.

**This is the round's most valuable output, and it is not a speed number.** It
surfaced only because the A/B asserted agreement *before* timing — a benchmark
that had just measured throughput would have reported BC4 at 5.53× and said
nothing.

### The lesson worth keeping

§17 said a number without a peer is not a result. This round adds: **a peer
comparison you have not run is not a plan.** §18 named BC6H as the next target
from throughput intuition. One afternoon of measurement says BC7, and the same
measurement found a migration hazard nobody was looking for.

### Still open

- A specialised block decoder, **BC7 first** — the thinnest lead and the most
  shipped format. Single-subset modes have contiguous index bits and no
  partition lookup, and the 16-pixel interpolation vectorises.
- Volume textures in both `decode_block_rows_into` and its HDR twin.
- The sim's buffer pool is capped by count, not bytes, and is not size-bucketed.

---

## §20 — A specialised BC7 mode-6 decoder, and a measurement that nearly lied

§19 named BC7 as the thinnest lead and a custom block decoder as the way to widen
it. This is that decoder — and the more useful output is what the measurement did
on the way.

### Specialise what exists

Mode histogram over a real 192-texture pack (BC7's mode is unary-coded in the low
bits of byte 0):

| mode | share | shape |
|---|---:|---|
| 6 | **87.79%** | 1 subset, RGBA 7.7.7.7, 4-bit indices |
| 5 | 9.36% | 1 subset, rotation, RGB 7.7.7 A8 |
| 1 | 2.85% | 2 subsets, 6-bit partition |
| | **97.15%** | single-subset (4,5,6) |

Mode 6 is the simplest shape BC7 has and nearly nine blocks in ten. The general
decoder carries a bitstream reader, a partition-table lookup and an index-width
branch **per pixel** so it can handle all eight modes; in mode 6 every one of
those is loop-invariant. The fast path reads the block as one `u128`, extracts
eight 7-bit components and two p-bits with shifts, and interpolates sixteen
pixels. Non-mode-6 blocks are declined and fall through untouched.

Correctness is not argued, it is tested: **20 000 randomised mode-6 blocks** plus
the all-zero and all-ones payloads, asserted bit-identical to `bcdec_rs`, and
every non-mode-6 encoding asserted declined rather than mis-decoded.

### The measurement that nearly lied

The first ABAB said **+19%**. Running it again with the arms reversed said
**+3.5%**.

The difference was cold start. In the first sequence the OLD arm always ran
second, and its samples climbed run over run — 365 → 396 → 432 Mpx/s — while NEW
sat flat at 438-460. Those early OLD numbers were measuring a cold page cache,
not a slower decoder. **A 19% headline was one commit away.**

§18 already recorded getting a comparison wrong by trusting a remembered number.
This is the next layer: the A/B was real, same session, same box, back to back —
and still wrong, because arm order was fixed. Alternating the order is not
ceremony, it is the only thing that separates the change from the schedule.

### What the honest numbers say

Serial, into a recycled buffer, both orders pooled:

| surface | general | mode-6 path | |
|---|---:|---:|---|
| 1024² | 707-771 Mpx/s | 727-811 Mpx/s | no change |
| 512² | 292-321 | 308-312 | no change |
| **256²** | 201-206 | **235-242** | **+17%** |
| **128²** | 200-203 | **242-258** | **+24%** |
| **64²** | 196-220 | **254-261** | **+23%** |

**The win is real, and it is invisible at 1024².** §15 established BC7 decode
scales only 3.7× on 24 cores, which is the signature of a memory-bandwidth
limit. Saving ALU work against a bandwidth ceiling buys nothing. Once the
surface fits in cache the decoder is the limit again and the specialisation
shows at ~20%.

That is not a niche case. **A full mip chain is mostly small surfaces**, so a
streamer decoding chains spends most of its decode time in exactly the range
where this pays — and almost none at the size where it does not.

### The lesson worth keeping

An optimisation can be simultaneously real and unmeasurable, depending entirely
on which size you test. Had this been benchmarked only at 1024² — the size every
previous round in this file used for BC7 — it would have been reported as **no
effect and reverted**. The bottleneck moved, and the benchmark did not follow it.

### Still open

- **Mode 5** (9.4%) would take single-subset coverage to 97%. Given the
  bandwidth finding, expect it to matter only in the same cache-resident range,
  and worth roughly a ninth of what mode 6 was.
- Volume textures in both `decode_block_rows_into` and its HDR twin.
- The sim's buffer pool is capped by count, not bytes, and is not size-bucketed.

---

## §21 — Mode 5: implemented, verified, refuted

§20 closed by naming mode 5 as the next increment — 9.4% of blocks, taking
single-subset coverage from 88% to 97%, "worth roughly a ninth of what mode 6
was." It was written, and it is not worth anything measurable.

### It was correct

Mode 5 is the harder single-subset shape: RGB 7.7.7 with separate 8-bit alpha, no
p-bits, **two independent 2-bit index regions** at fixed offsets, and a rotation
that swaps alpha with one colour channel.

The first implementation applied the rotation by permuting the *endpoints* before
interpolating, which looks equivalent and is not: the two weights are assigned
**positionally** — the first three channels take the colour index, the fourth
takes the alpha index — so moving an endpoint does not move the weight that
applies to it. The oracle test caught it on the first run, at rotation 1, case 2.

Fixed, it matched the general decoder bit for bit across **all four rotations ×
10 000 randomised blocks**.

### It was not faster

Whole-surface, ABBA, warm samples only:

| | mode 5 path | general |
|---|---:|---:|
| 256² | 226.9 Mpx/s | 226.8 |
| 128² | 228.8 | 238.5 |
| 64² | 236.2 | 238.2 |

That could be dilution — 9.4% share × a 20% per-block win is ~1.9%, under the
noise. So the path was measured **in isolation**, on a synthetic surface where
every block is mode 5, rotations cycled so no branch predictor gets a free ride:

| all-mode-5 surface | mode 5 path | general |
|---|---:|---:|
| 128² (serial) | 157.2 Mpx/s | 158.9 |
| 256² (serial) | 158.9 | 164.3 |

Four ABBA samples each. **Neutral per block, not merely diluted.** Reverted, with
the measurement in a comment at the site.

### Why this matters more than the code

The obvious read of §20 was "specialising BC7 modes is a win, do more of them."
That generalisation is now **false as stated**. Mode 6's ~20% did not come from
specialisation as such; it came from something specific to mode 6 — most likely
that its 4-bit indices and single interpolation weight collapse to a handful of
shifts, where mode 5 still carries two index streams, a channel permutation and a
7→8-bit expansion that the general decoder was not paying much for anyway.

The separation of the two experiments is the point. The whole-surface number
alone would have been dismissed as "too small a share to see" — which is a story,
not a measurement, and it happens to be the wrong one. Isolating the path turned
an untestable excuse into a fact.

**Do not assume modes 1 or 3 will pay without measuring them the same way**: in
isolation first, share second.

### Still open

- Modes 1 (2.9%) and 3 — measure in isolation before writing anything.
- Volume textures in both `decode_block_rows_into` and its HDR twin.
- The sim's buffer pool is capped by count, not bytes, and is not size-bucketed.

---

## §22 — Modes 1 and 3: the mechanism, found at last

§21 refuted "specialising BC7 modes is a win" and left a rule: **measure in
isolation first, share second.** Following it produced both the largest per-mode
win of the campaign and the clearest explanation of why mode 5 failed.

### Isolation first: what the general decoder costs per mode

All-mode-N synthetic surfaces, 256², serial, general decoder only:

| mode | Mpx/s | |
|---|---:|---|
| 6 | 205.9 | 1 subset, one 4-bit index per pixel |
| 7 | 167.2 | 2 subsets |
| 1 | 162.0 | 2 subsets |
| 3 | 161.8 | 2 subsets |
| 2 | 159.9 | 3 subsets |
| 0 | 158.5 | 3 subsets |
| 5 | 152.0 | 1 subset, **two** index sets |
| 4 | 151.3 | 1 subset, two index sets |

Mode 6 is already the fastest *before* any specialisation, and mode 5 — also
single-subset — is the **slowest**. Subset count does not order this table.
Index-read count does.

### The mechanism

`bcdec_rs` reads indices through a stateful bitstream:

```rust
let bits = self.low & mask;
self.low >>= num_bits;
self.low |= (self.high & mask) << (64 - num_bits);
self.high >>= num_bits;
```

Every read **mutates** the cursor, so sixteen index reads are a sixteen-deep
serial dependency chain: read `n + 1` cannot issue until read `n` retires. That
is the cost, and it scales with **how many indices a mode reads**, not with how
many subsets it has.

This explains §21 exactly. Mode 5 reads *two* index sets per pixel — thirty-two
chained reads — and my fast path replaced them with thirty-two independent
extractions, which should have won. It did not, because mode 5 also carries a
rotation and a channel permutation I reintroduced per pixel. **Mode 5 was a bad
implementation of a good idea**, and §21 recorded it as a bad idea. Correcting
that is worth more than the code.

### The result

Modes 1 and 3 read one index per pixel, like mode 6, and the two-subset partition
lookup — which the format requires and no specialisation can remove — turns out
not to be the expensive part. Alternating-order ABBA, four samples per arm:

| mode | general | specialised | |
|---|---:|---:|---|
| 1 | 185-191 Mpx/s | **242-253** | **+31%** |
| 3 | 171-189 | **245-252** | **+38%** |

No overlap between arms in either case. **Larger than mode 6's +18%**, because
the two-subset modes started with more of the chain to remove.

### And it does not show on our packs

Whole-surface on the ultra pack, where mode 1 is 18.8% of blocks: 240.2 vs 242.5
Mpx/s at 256², 250.7 vs 252.1 at 128². **Flat.**

That is not a contradiction, it is a statement about our *encoder*: it emits ~88%
mode 6 and no mode 3 at all. The specialisation pays on content whose compressor
favours the two-subset modes, and this crate decodes textures it did not cook.
Shipping it on the isolated measurement, and saying plainly in the changelog that
packs cooked here will not show it.

### The lesson worth keeping

§21's rule was right and its conclusion was wrong. "Measure in isolation" caught
mode 5's numbers correctly but let me generalise from **one implementation** to
the whole idea. The isolated measurement tells you whether *this code* is faster;
it does not tell you whether the *approach* is sound. Separating those needed a
third thing neither §20 nor §21 had: a per-mode cost profile of the code being
replaced, which is what pointed at index-read count as the variable that matters.

**Profile the thing you intend to beat, before writing the thing that beats it.**

### Still open

- Modes 0 and 2 (3-subset) and 4 and 7 remain general. Mode 7 at 167.2 Mpx/s and
  one index set is the most promising of them; modes 4 and 5 read two sets and
  should be expected to behave like mode 5 did.
- Volume textures in both `decode_block_rows_into` and its HDR twin.
- The sim's buffer pool is capped by count, not bytes, and is not size-bucketed.

---

## §23 — Mode 7, and the shape of the whole problem

§22 predicted mode 7 would behave like modes 1 and 3: two subsets, but **one**
index set, so the sixteen-deep index-read chain is what it is paying. It does.

| mode | general | specialised | |
|---|---:|---:|---|
| 7 | 162.5-165.9 Mpx/s | **237.7-257.7** | **+52%** |

Alternating-order ABBA, four samples per arm, no overlap. The largest single-mode
gain of the campaign — and mode 7 was the **slowest** mode on the general
decoder.

### The picture that only appears once you have four of them

Per-mode, 256², serial:

| mode | Mpx/s | state |
|---|---:|---|
| 3 | 301.9 | specialised |
| 1 | 289.0 | specialised |
| 7 | 257.8 | specialised |
| 6 | 236.2 | specialised |
| 2 | 162.2 | general |
| 5 | 160.5 | general |
| 4 | 152.7 | general |
| 0 | 152.5 | general |

**Bimodal.** Every specialised mode lands in 236-302 Mpx/s; every general one in
152-162. And the gain is *inversely* ordered against the starting speed:

| mode | general | specialised | gain |
|---|---:|---:|---:|
| 6 | 205.9 | ~244 | +18% |
| 1 | 187.9 | 245.7 | +31% |
| 3 | 180.3 | 248.7 | +38% |
| 7 | 164.6 | 250.4 | +52% |

The specialised column is nearly **flat**. Whatever a mode costs on the general
decoder, specialising it lands at roughly the same place. That is the strongest
statement this campaign has produced about BC7 decode: **the general decoder's
152-206 spread is not mode complexity, it is per-pixel bitstream and dispatch
overhead.** Modes are not meaningfully different in cost once you stop reading
their indices through a mutating cursor.

### Which means §21 is now doubly wrong

Mode 5 sits at 160.5 on the general decoder. Every mode measured at that level
has, on specialisation, gone to ~250. §21 recorded mode 5 as "neutral per block,
not merely diluted" and reverted it — and §22 already suspected the
implementation rather than the idea. This table makes that near-certain: a
correct mode-5 path should reach ~250 Mpx/s, a **+56%** gain, the largest still
on the table.

The refutation in §21 was a sound measurement of unsound code. Recording it as a
property of the *approach* was the error, and it survived two rounds because
nothing challenged it until a fourth data point made the pattern visible.

### The lesson worth keeping

Three modes in, the story was "specialisation helps some modes." Four modes in,
the story is "all modes cost the same once specialised, and the general decoder's
variance is pure overhead." **The second story is not a refinement of the first,
it is a different claim** — and it only became visible with enough points to see
that the specialised column was flat.

Do not conclude from two measurements what four would contradict. And when a
negative result sits next to three positives with the same mechanism, re-examine
the negative before trusting it.

### Still open, in expected-value order

- **Mode 5, revisited.** 160.5 Mpx/s, predicted ~250. Two index sets, a rotation
  and a channel permutation — the parts my first attempt handled badly. This is
  now the largest remaining win, and §21 must be corrected in the file, not
  silently.
- **Mode 4** (152.7): two index sets *and* an index-selection bit. Same family
  as 5; attempt after it.
- **Modes 0 and 2** (152.5, 162.2): three subsets. The partition lookup is wider
  but the index chain is identical, so the mechanism should still apply.
- Volume textures in both `decode_block_rows_into` and its HDR twin.

---

## §24 — All eight modes, and the regression that only real content could see

§23 predicted every remaining BC7 mode would reach ~250 Mpx/s on specialisation,
and named mode 5 — refuted in §21 — as the largest win left. Both held.

### The results

Isolated, all-mode-N surfaces, alternating-order ABBA:

| mode | general | specialised | gain |
|---|---:|---:|---:|
| 4 | 146.7 Mpx/s | 253.8 | **+73%** |
| 5 | 158.2 | 261.0 | **+65%** |
| 7 | 164.6 | 250.4 | +52% |
| 3 | 180.3 | 248.7 | +38% |
| 1 | 187.9 | 245.7 | +31% |
| 2 | 163.4 | 202.1 | +24% |
| 0 | 158.4 | 194.1 | +22% |
| 6 | 205.9 | ~244 | +18% |

The prediction was right with one refinement: **one- and two-subset modes land at
~245-260; three-subset modes plateau at ~200.** Modes 0 and 2 carry a two-bit
subset index per pixel and six endpoints, and that part is irreducible.

### §21 is closed, correctly this time

Mode 5 gains **65%**. The §21 code was slow because it resolved the rotation with
a conditional `swap` inside the per-pixel loop — a branch and two bounds-checked
indexed accesses, sixteen times a block. Mode 4, the same family, made this
unmissable: hoisting the rotation into a channel map computed once took it from
146.7 to 253.8.

Three sections were needed to unwind one wrong conclusion. §21 measured
correctly and generalised from one implementation to the whole idea; §22
suspected it; §23 made it near-certain from the shape of the data; §24 proves it.
The measurement was never the problem. **The inference from it was**, and no
amount of re-measuring the same code would have found that.

### The regression only real content could see

With all eight written and chained as `||` probes, each `#[inline]`, the real
192-texture pack got **8-10% slower** than before modes 0/2/4/5 existed. Every
isolated mode was faster; the integration was a net loss.

Two causes, both invisible to the per-mode benchmark:

1. Eight decoders inlined into one dispatch blow the block loop's instruction
   footprint. An all-mode-N benchmark only ever exercises one of them and never
   pays for the other seven being resident.
2. An `||` chain is sequential. A mode-5 block paid seven failed probes first.

Fixed with one `trailing_zeros` and a `match` — a jump table — and the decoders
left out of line. Real content then goes **207.8 -> 223.0 Mpx/s, +7.3%**, four
ABBA samples per arm, no overlap.

**This is the exact inverse of §20's lesson.** There, an optimisation was real
but invisible at the only size being tested. Here, eight optimisations were each
real in isolation and *harmful together*. A benchmark that isolates the thing you
changed cannot see what the change costs everything else.

### Where BC7 decode stands

All eight modes specialised; `bcdec_rs::bc7` is now reached only for the reserved
encoding. Against Microsoft DirectXTex on a cooked 1024² pack: **BC7 503.5 vs
58.4 Mpx/s — 8.62x** — and **6.30x** across all formats.

### The lesson worth keeping

Isolation tells you whether a change is faster. Integration tells you whether it
is worth having. **Both are required, and they can disagree in either
direction.** This file now contains one case of each, three sections apart.

### Still open

- Volume textures in both `decode_block_rows_into` and its HDR twin.
- The sim's buffer pool is capped by count, not bytes, and is not size-bucketed.
- BC1-BC5 have no per-mode structure to exploit, but they run through the same
  `bcdec_rs` bitstream. Whether the same index-read argument applies to them has
  not been measured.

---

## §25 — The 7.3%, chased into the arithmetic

§24 shipped all eight BC7 modes and reported **+7.3%** on real content. That
number was modest because the pack is 88% mode 6, and mode 6 had turned out to be
the *slowest* of the one- and two-subset specialised modes — 216.8 Mpx/s against
mode 3's 279.4. Mode 6 interpolates four channels with 4-bit indices where modes
1 and 3 do three channels with constant alpha: a third more work per pixel.

That put the remaining real-content time squarely in the interpolation itself.

### The identity

The BC7 spec writes interpolation as

```text
(e0 * (64 - w) + e1 * w + 32) >> 6
```

Two multiplies per channel, both depending on the per-pixel weight. It is exactly
equal to

```text
(e0 * 64 + 32 + w * (e1 - e0)) >> 6
```

where `base = e0 * 64 + 32` and `delta = e1 - e0` do **not** depend on `w` and
are constant for the whole block. Sixteen pixels x four channels: 128 multiplies
become 64, and base/delta is computed once per endpoint pair.

No approximation and no reassociation of the rounding — the same integer
expression, rearranged. Every per-mode oracle test passed unchanged, which is
exactly what those tests are for.

### The result

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

The three-subset modes barely move, which is informative rather than
disappointing: their cost is the per-pixel partition lookup and six endpoint
pairs, not the interpolation. **Two independent optimisations have now failed to
shift modes 0 and 2**, which is a fairly strong statement about where their time
actually goes.

Real content, four ABBA samples per arm, no overlap between arms:

| high192 | before | after | |
|---|---:|---:|---|
| 256² | 240.6 Mpx/s | **273.7** | **+13.8%** |
| 128² | 242.3 | **274.6** | **+13.3%** |

Nearly double §24's whole-content gain, from a change that touches no structure
at all.

### The lesson worth keeping

§24 ended with the real-content number limited by the one mode that was *already*
specialised. The instinct at that point is structural: better dispatch, more
modes, SIMD. The actual fix was a line of algebra applied to a formula that had
been transcribed verbatim from the specification and carried through five
sections without anyone reading it as arithmetic.

**A specification tells you what to compute, not how to compute it.** Every
expression copied from a spec is worth re-deriving once — spec authors optimise
for unambiguity, and factoring your inner loop is not their job.

### Still open

- Modes 0 and 2 are partition-lookup bound. A packed subset representation that
  avoids the per-pixel shift-and-mask is the remaining idea, and it is small.
- SIMD across the four channels is now the obvious structural step, and the crate
  already has a `simd` feature with an established runtime-detected,
  scalar-fallback pattern.
- Volume textures in both `decode_block_rows_into` and its HDR twin.
- BC1-BC5 run through the same `bcdec_rs` bitstream and have never been examined
  for either the index-chain or the interpolation win.

---

## §26 — SIMD, and a correction to §25

§25 closed by naming SIMD across the four channels as the obvious structural step.
It was, and it was larger than expected — because §25 had already done the hard
part without noticing.

### The rearrangement was the enabler

Rewriting interpolation as `base + w * delta` halved the multiply count. It also
**bounded every intermediate**:

| term | range | fits `i16` |
|---|---|---|
| `base` = `e0 * 64 + 32` | `32 ..= 16_352` | yes |
| `delta` = `e1 - e0` | `-255 ..= 255` | yes |
| `w * delta` | `-16_320 ..= 16_320` | yes, so `mullo` is exact |
| `base + w * delta` | `32 ..= 16_352` | yes |

The spec form `e0 * (64 - w) + e1 * w + 32` has the same final range, but its
*intermediates* are two products that must each be held before summing. The
factored form needs one. That difference is what lets the whole computation live
in **16-bit lanes**, which hold eight channels — two entire pixels — per 128-bit
register instead of four.

So §25's win was not just fewer multiplies; it doubled the achievable lane count.
Neither effect was the reason it was written.

### SSE2, not AVX2

The kernel is SSE2, which is **baseline on x86_64**: no runtime detection, no
second code path, and the path that ships is the path the tests exercise. The
encoder AVX2 kernels are runtime-detected because AVX2 is genuinely optional;
copying that pattern here would have bought width the algorithm cannot use and a
fallback nobody runs.

### The result

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

Real content, four ABBA samples per arm, no overlap between arms:

| high192 | before | after | |
|---|---:|---:|---|
| 256² | 254.3 Mpx/s | **324.6** | **+27.6%** |
| 128² | 251.2 | **315.8** | **+25.7%** |

Against DirectXTex: **BC7 735.6 vs 70.6 Mpx/s — 10.42x**, 6.21x across formats.

### §25 was wrong about modes 0 and 2

§25 recorded that modes 0 and 2 "are partition-lookup bound", on the evidence
that **two independent optimisations had failed to move them** — and said so in
the confident register that two null results seem to earn.

They gained **77% and 76%** here. They were interpolation bound the whole time.
The scalar work simply never moved enough throughput past the other costs to be
visible.

That is the third time in this file a null result has been over-read (§21 on mode
5, §25 here). The pattern is consistent enough to name: **a failure to improve
something is evidence about the change you made, not about where the time goes.**
Only a measurement that isolates the cost can say that, and neither of §25's two
attempts did.

### Mode 6 gains least, and that is now explicable

Mode 6 is the only mode with 4-bit indices: sixteen index extractions of four
bits each, against two or three bits elsewhere. With interpolation vectorised,
the weight extraction is what remains, and mode 6 has the most of it. It is also
88% of our packs, so it caps the real-content figure — the same shape as §25,
one layer down.

### Still open

- Mode 6 weight extraction: sixteen shift-and-mask operations that could be
  done as a vector gather or unpacked in bulk from the 64-bit index field.
- BC1-BC5 have never been examined for either the index-chain or the
  interpolation win. BC1 already runs at 684 Mpx/s, but nothing has profiled
  *why*, and the same `base + w * delta` identity applies to its 2-bit
  interpolation.
- Volume textures in both `decode_block_rows_into` and its HDR twin.

---

## §27 — Mode 6 does not move, and the ceiling that proves it

§26 named mode 6 as the cap on real-content throughput: it is 88% of our packs,
it gained least from SIMD (+20% against +52-93% elsewhere), and it is the only
mode with 4-bit indices — sixteen wider weight extractions than anything else.
The obvious target was that extraction.

Two attempts, both refuted.

### Refuted: narrowing the index field for mode 6

Every BC7 index region is at most 47 bits, so reading it as `u64` instead of
`u128` should replace sixteen multi-instruction shifts with sixteen single ones.

For mode 6 it changed nothing, and the reason is instructive: its shift amounts
are `3 + (i - 1) * 4` — **compile-time constants in an unrolled loop**. LLVM had
already folded every one of them. The `u128` was never being shifted at runtime
at all.

### Refuted: removing the fix-up branch

Pixel 0 stores three bits with an implicit zero MSB, so the loop branched on
`i == 0`. Re-inserting that zero makes all sixteen indices uniformly four bits
and removes the branch. Eight samples per arm: **321.9 vs 326.8 Mpx/s** — neutral
to slightly worse.

Same cause. The branch was constant-folded by the same unroller, so the change
spent three real operations removing one that did not exist.

### The ceiling measurement that ended the round

Rather than attempt a third variant — a `pshufb` gather of all sixteen weights
in one instruction was the next idea — the headroom was measured directly by
replacing the entire per-pixel weight lookup with a constant. Wrong output, but
it is the **absolute ceiling** any weight-extraction optimisation could reach:

| mode 6 | Mpx/s |
|---|---:|
| current | 336.9 |
| **no weight lookup at all** | **345.7** |

**The whole weight extraction is worth ~2.5%.** A perfect vectorised gather could
recover at most that. The round ended there.

### What did work, elsewhere

The `u64` narrowing is a real win where shift amounts are **runtime-variable** —
the multi-subset modes, whose index offsets depend on the partition anchor. Six
samples per arm:

| mode | before | after | |
|---|---:|---:|---|
| 0 | 354.7 Mpx/s | **425.9** | **+20.1%**, no overlap |
| 3 | 526.5 | **589.7** | +12.0% |
| 1 | 515.1 | **571.4** | +10.9% |

Whole-content figures do not move, because our packs are 70-88% mode 6.

### The lesson worth keeping

**Measure the ceiling before building the optimisation.** Three sections of this
file have now spent effort on changes whose maximum possible payoff was never
established first. Stubbing the work out entirely is crude, produces wrong
output, takes two minutes, and would have prevented all of it — here it converted
"mode 6 is capped by weight extraction, let us vectorise the gather" into "the
extraction is 2.5% of the call" before a line of `pshufb` was written.

The corollary is worth stating too: **an optimisation can fail because the
compiler already did it.** Both refutations here were of work LLVM had performed
at compile time. Reading the assumption — "sixteen `u128` shifts", "a branch per
pixel" — as though it described the emitted code, rather than the source, is what
made both look promising.

### Still open

- Mode 6 is at the limit of this approach. What remains in it is the vectorised
  interpolation and the stores, both already minimal.
- BC1-BC5 have never been examined for either the index-chain or the
  interpolation win. BC1 runs at 684 Mpx/s and nothing has profiled why.
- Volume textures in both `decode_block_rows_into` and its HDR twin.

---

## §28 — BC1 through BC5, and the ceiling that said go

§27 ended with a rule earned the hard way: **measure the ceiling before building
the optimisation.** This round applied it first, and for once the answer was
"yes, build it."

### Ceiling first

Stubbing the block decoders out entirely, so only the loop and addressing remain:

| format | current | ceiling | block decode share |
|---|---:|---:|---:|
| BC1 | 621.6 Mpx/s | 1216.1 | **49%** |
| BC5U | 404.4 | 671.9 | 40% |
| BC4U | 498.1 | 683.8 | 27% |

Against mode 6's 2.5%, this is a different world. Two minutes of stubbing turned
"BC1-BC5 have never been examined" into a ranked work list.

### The same chain, again

The cause was the one §22 identified in BC7:

```rust
let idx = color_indices & 0x03;
...
color_indices >>= 2;      // sixteen dependent shifts
```

BC1 and BC2 shift by two, BC3/BC4/BC5 by three, and every read mutates the
cursor. Reading each index by computed offset from an immutable word makes all
sixteen independent — the identical fix, in a format family nobody had connected
to the BC7 work because they share no code.

BC4 and BC5 carried a second cost: they decoded sixteen single-channel bytes and
then made a **second pass** over the block to expand them to RGBA. Packed word
stores fuse the two.

### The result

Six samples per arm, alternating order:

| format | before | after | |
|---|---:|---:|---|
| **BC4U** | 441.4 Mpx/s | **669.3** | **+51.6%**, no overlap |
| **BC1** | 554.3 | **660.6** | **+19.2%**, no overlap |
| BC5U | 335.4 | 341.3 | +1.8%, neutral |

Against DirectXTex: **BC4U 842.6 vs 102.0 Mpx/s — 8.26x**, up from 5.34x. 6.69x
across all formats.

BC5 is kept despite being neutral, because the change *removes* code — one pass
instead of two, one implementation shared with BC4 — rather than adding any. That
is a different case from §21's mode 5, which added a hundred lines for nothing.

### BC3 alpha is not BC4 alpha

Wiring BC3 to `bc4_palette` failed the oracle immediately. BC4 interpolates with
fixed-point weights and `>> 16`; **BC3 alpha uses integer division by 7 and 5**,
and they disagree — for `a0 = 60, a1 = 133`, 74 by division against 75 by
weights. The reference draws the same distinction.

Worth recording as a hazard: the two look interchangeable, they are described
identically in most summaries of the formats, and only a bit-exact oracle
separates them. A visual check would never have found a one-level difference in
an alpha ramp.

### The lesson worth keeping

**A fix found in one place is a hypothesis about every place with the same
shape.** The serial index chain was diagnosed in BC7 in §22, and it sat unfixed
in five other formats for six sections — not because anyone rejected the idea,
but because BC1-BC5 share no code with BC7 and nothing connected them. The
connection was structural, not textual, and grep does not find that.

### Still open

- BC5 has 40% of its call in block decode by the ceiling measurement, and this
  round did not capture it. Two channels of independent palette lookups appear to
  saturate something the single-channel BC4 path does not; it has not been
  diagnosed.
- BC2 and BC3 have no real-content measurement — our packs contain neither.
- Volume textures in both `decode_block_rows_into` and its HDR twin.

---

## §29 — BC5, the loose end, closed by decomposition

§28 shipped BC1-BC5 in-house and left one honest gap: BC5 was neutral, with 40%
of its call in block decode by the ceiling measurement and no explanation.

### Decompose before guessing

Rather than propose a fix, each stage was stubbed in turn:

| probe | Mpx/s | implies |
|---|---:|---|
| full | 314.4 | — |
| palette build removed | 372.4 | palette ≈ 16% |
| per-pixel index reads removed | 576.6 | index + gather ≈ **45%** |

That also explains BC5's standing against BC4 without any further work: BC5 does
**two** index extractions and **two** palette gathers per pixel where BC4 does one
of each, and it runs at 314 against BC4's 588 — almost exactly the ratio.

### The fix, and the surprise in it

The obvious reading of "45% is the gather" points at `pshufb`, which is a
16-entry byte lookup in one instruction and is exactly the shape of an 8-entry
palette gather. That is SSSE3, needs runtime detection, and is a day of work.

The cheap thing first: write a whole **block row** in one store rather than four
separately range-checked four-byte stores. Ten samples per arm:

| format | before | after | |
|---|---:|---:|---|
| **BC5U** | 291.4 Mpx/s | **392.4** | **+34.7%**, no overlap |
| BC1 | 582.7 | 626.9 | +7.6%, overlapping |
| BC4U | 581.6 | 578.5 | neutral |

**Only BC5 moved.** The same edit, the same shape, applied to three formats, and
two of them do not care. The explanation that fits: LLVM already coalesces the
four stores for the single-channel formats, and BC5's two-channel word build —
two gathers and a shift feeding one word — has enough dependency depth that it
does not. BC5 was the only format still paying four range-checks per row.

Against DirectXTex: **BC5U 451.2 vs 59.2 Mpx/s — 7.62x**, up from 5.53x.

### The lesson worth keeping

**Decomposition names the stage; it does not name the fix.** The probe correctly
said "45% is index-and-gather", and the obvious inference — vectorise the gather —
would have been a day of SSSE3 work with runtime detection. The actual win was in
the *stores*, which the decomposition had not even isolated, and it took twenty
minutes.

Corollary, and it is the same shape as §27's compiler-folding refutations:
**identical edits do not have identical effects across similar code**, because
what the optimiser has already done differs. Three formats, one change, one clear
win, one weak, one nothing. Measure each; do not extrapolate from the one that
worked.

### Still open

- The gather itself is still ~45% of BC5 and untouched. A `pshufb` palette lookup
  remains available and is now the largest identified BCn win — but it needs the
  ceiling measured first (post-row-store), which this round did not redo.
- BC2 and BC3 still have no real-content measurement; our packs contain neither.
- Volume textures in both `decode_block_rows_into` and its HDR twin.

---

## §30 — The BC5 gather, and a probe that removed more than it meant to

§29 closed BC5's store problem and left the gather explicitly open, with a note
that its ceiling needed re-measuring first. Doing that changed the target.

### The probe that lied

§29's decomposition replaced the palette index with the loop variable:

```rust
let r = pr[col] as u32;      // col is 0..3 in an unrolled loop
```

That is a *constant* index. The compiler folded the load away **and** the
arithmetic feeding it, so the probe measured the removal of both and was read as
"the index math dominates". The correct probe keeps the arithmetic and drops only
the lookup:

```rust
let r = ((ir >> sh) & 0x7) as u32;   // index math kept, no table read
```

| probe | Mpx/s | implies |
|---|---:|---|
| full | ~371 | — |
| **lookup removed, index math kept** | ~655 | lookup ≈ **43%** |
| both removed | ~789 | index math ≈ 10% |

The conclusion inverted. **A probe that removes more than it names does not
isolate anything** — and it is easy to write, because a constant index looks like
a harmless simplification.

### The kernel

`pshufb` is a sixteen-entry byte gather in one instruction, which is precisely an
eight-entry palette looked up sixteen times. Eight samples per arm, alternating:
**378.2 → 489.9 Mpx/s, +29.5%.** Against DirectXTex, BC5U **10.66x**, up from
7.62x; 7.16x across formats.

### Two refutations on the way

- **Palette in a register.** Holding the eight entries in a `u64` and selecting
  with `>> (8 * idx)` to avoid a memory load: **9.8% slower**. An L1-resident
  table indexed by a computed value pipelines better than a dependent
  multiply → variable-shift → mask chain. The "avoid memory" instinct was simply
  wrong here.
- **Index bytes through an array.** Building sixteen index bytes into a
  `[u8; 16]` and loading it as a vector gave **+11.6% with overlapping arms**;
  building the same vector in registers with `pdep` gave **+25% cleanly**. The
  difference is a store-forwarding stall — sixteen narrow stores feeding one wide
  load — which is invisible in the source and shows up only as a disappointing
  number.

### The gate is part of the optimisation

`pdep` is BMI2, and BMI2 being *present* is the wrong question. **On AMD Zen 1
and Zen 2 `pdep` is microcoded at ~18 cycles**, against 3 on Intel Haswell+ and
AMD Zen 3+. Four per block against a ~100-cycle block budget would make this a
large regression on hardware that advertises the feature.

So the gate checks vendor and family through `cpuid` and refuses AMD below family
0x19. A portable register-only unpack was written and measured as the
alternative: **neutral against scalar**, so the win genuinely depends on fast
`pdep` and the gate is load-bearing rather than defensive.

**`is_x86_feature_detected!` answers "is it encodable", not "is it fast".** For
`pdep`/`pext` specifically, that gap is a factor of six.

### Still open

- The gather is now ~29% better and the ceiling was ~789 Mpx/s against 490 now;
  the remaining gap is the `pshufb`, the unpacks and the four stores, all real
  work.
- BC2 and BC3 still have no real-content measurement; our packs contain neither.
- Volume textures in both `decode_block_rows_into` and its HDR twin.

---

## §31 — The same stall, one line away

§30 found a store-forwarding stall in the BC5 index unpack — sixteen narrow
stores to a `[u8; 16]` feeding one wide vector load — and fixed it with `pdep`,
worth +13 points over the array version.

**The identical stall was still there, one line down.** `bc4_palette` returned a
`[u8; 8]`, built by eight narrow stores to the stack, and the gather read it back
with `_mm_loadl_epi64`. Building it into a `u64` and moving it across with `movq`:
**508.7 → 572.5 Mpx/s, +12.5%**, eight samples per arm.

Against DirectXTex: **BC5U 11.54x**, up from 10.66x. 7.65x across formats.

### Why it was missed

The two are the same defect with different names — "the index bytes" and "the
palette" — and I had just written the fix for one of them. What separated them
was that the index unpack was *code I was editing* and the palette was *a
function I was calling*. The stall lives at the boundary, in neither function's
body: `bc4_palette` is perfectly reasonable in isolation, and so is
`_mm_loadl_epi64`.

**A store-forwarding stall is invisible in any single function.** It only exists
in the pairing, so it will never be found by reading either side. After fixing
one, grep for every other array-to-vector handoff in the same kernel.

### The refuted-but-kept case

BC4's weight pairs sum to exactly 65536, so the palette interpolation collapses
to `e0 + ((W[k] * (e1 - e0) + 32768) >> 16)` — one multiply instead of two, the
same identity §25 found in BC7.

**Measured neutral.** Six multiplies saved against a ~90-cycle block is under the
noise floor. It is kept, and that is a deliberate exception to this file's
revert-what-does-not-prove-itself rule: the change is strictly *less* work and
shares a documented form with BC7, so keeping it costs nothing and removing it
would be churn. The rule targets complexity added for no gain, not simplification
that happens to be invisible.

### Where BC5 ended up

| version | Mpx/s | change |
|---|---:|---|
| 0.3.12 | 291 | in-house decode |
| 0.3.13 | 378 | row stores |
| 0.3.14 | 490 | `pshufb` gather + `pdep` unpack |
| 0.3.15 | 572 | palette in a register |

**~2x over four rounds**, and 11.54x the reference implementation.

### Still open

- The ceiling probe that motivated this thread measured ~789 Mpx/s with the
  gather removed entirely; it has not been re-run against the SIMD kernel, so the
  remaining gap is no longer a trustworthy number. Re-measure before another
  round on BC5.
- BC2 and BC3 still have no real-content measurement; our packs contain neither.
- Volume textures in both `decode_block_rows_into` and its HDR twin.

---

## §32 — The stale ceiling, and a probe that lied the same way twice

§31 ended by refusing to quote a remaining gap, because the ~789 Mpx/s ceiling
had been measured against the *scalar* BC5 kernel and never re-run after the SIMD
work. Re-running it moved the target completely.

### The target moved

Stage-by-stage against the current kernel:

| probe | Mpx/s | share |
|---|---:|---:|
| full | ~572 | — |
| `pshufb` gather removed | ~641 | ~18% |
| `pdep` index unpack removed | ~601 | ~13% |
| **palette interpolation removed** | **~847** | **~32%** |

The two things this thread spent three rounds optimising are now the *small*
parts. The palette build — untouched since §28 and never suspected — is the
largest single cost, precisely **because** everything around it got faster. A
ceiling is a statement about one moment in a kernel's life, and it expires the
moment you act on it.

### The probe lied the same way twice

The first palette probe substituted **constant** palettes and reported ~791
against ~524 full. That number was wrong: constant palettes let LLVM hoist the
entire computation out of the block loop and constant-fold into the shuffle. The
honest probe keeps the palette **block-dependent** but trivial:

```rust
let pr_packed = u64::from_le_bytes([blk[0]; 8]);   // cannot be hoisted
```

This is the identical error §30 recorded — a probe that replaced a lookup index
with a loop constant and folded away both the load and its arithmetic. **I wrote
that lesson down and then made the same mistake one section later.** The failure
mode is specific and worth naming precisely: *substituting a constant does not
remove one stage, it removes every stage upstream of it.* When probing, replace
with something cheap that still **depends on the input**.

### One weak win, two refutations

The packing loop carried both defects already fixed twice in this kernel — an
eight-deep serial OR chain, reading back a stack array. Rewritten as a balanced
tree over independent terms: **BC5U +5.5%, BC4U +5.6%**, positive in both but
with overlapping arms. Reported as weak, kept as strictly less work.

**32% is still uncaptured**, and two attacks on it failed:

- **Branchless endpoint selection.** `e0 > e1` is data-dependent per block and
  evaluated twice per BC5 block, so a mispredict was the obvious suspect — and
  this file already records a campaign where removing a mispredict was worth 35%.
  Computing both weight sets and masking measured **neutral** (BC5 625.6 vs
  609.3, BC4 680.5 vs 685.3).
- The 65536-sum identity (§31), halving the multiplies per entry: also neutral.

So the palette's cost is neither its branch nor its multiply count. That is a
genuinely open question rather than a to-do, and the next attempt should isolate
*within* the palette build before writing anything.

### Where things stand

Against DirectXTex: **BC4U 9.15x, BC5U 11.29x, BC7 10.06x — 7.88x overall.**

### The lesson worth keeping

**A ceiling measurement has a shelf life of exactly one optimisation.** Every
round in this thread invalidated the ceiling that justified it, and quoting a
stale one is how §31 nearly reported a 27% gap that had already moved elsewhere.
Re-measure before each round, not once per thread.

### Still open

- The palette interpolation, ~32% of BC5 and resistant to two attacks.
- BC2 and BC3 have no real-content measurement; our packs contain neither.
- Volume textures in both `decode_block_rows_into` and its HDR twin.

---

## §33 — BC6H, the format six sections walked past

§18 closed BC6H with "`bc6h_half` is 84% of the call and it is somebody else's
code", and moved on. Over the next fifteen sections that same somebody else's
code was beaten in BC7 modes 0-7, then BC1, BC2, BC3, BC4 and BC5 — **and nobody
went back**. BC6H sat at 3.29x against DirectXTex while every other format
climbed to 9-12x.

### The doubling probe, used as an instrument rather than a rescue

§32's finding — that subtraction probes confound throughput with latency, and
duplication separates them — was applied here **before** writing anything:

| probe (512², serial) | Mpx/s | |
|---|---:|---|
| full | 113.8 | — |
| block decode removed | **227.3** | decode is **50%** of the call |
| **2x block decode work** | **67.5** | doubling costs 1.7x |

**BC6H is throughput bound.** That is the opposite of BC5, and it is the whole
reason this round could succeed where three BC5 rounds could not: here, removing
work removes time.

One instrument, two formats, opposite answers, and the answer determined whether
to write the decoder at all. Neither could have been guessed.

### 100% of one mode

A histogram over the real content: every one of 43 694 blocks is **mode 11** —
one subset, both endpoints explicit at 10 bits, no partition table, no delta,
sixteen 4-bit indices. Our encoder emits only that, and it is the natural shape
for smooth HDR gradients generally.

So the specialisation is the same playbook as BC7 mode 6, and the same defect:
the general path reads the mode field, six endpoints and sixteen indices through
a **mutating cursor**, one serial chain. Computed offsets from an immutable
`u128` make them independent.

**102.7 → 124.1 Mpx/s, +20.8%**, eight samples per arm. Against DirectXTex:
**4.50x**, up from 3.29x; **8.72x** across formats, up from 7.88x.

### The lesson worth keeping

**A conclusion has a scope, and "not worth attacking" almost never keeps its
scope.** §18's verdict was correct *at the time*: the tools to beat bcdec's
bitstream did not exist yet, because the serial-cursor diagnosis was four
sections away. But the verdict outlived its evidence by fifteen sections while
the very technique that refuted it was applied eleven times next door.

When a technique lands, the question is not only "where else does this shape
appear" (§28's lesson) but **"what did I previously decide was hopeless, and was
it hopeless for a reason this technique changes?"** Re-read your own refusals
after every new capability.

### Also settled

BC7 mode 6's "at the limit" verdict rested on two throughput edits, which §32
showed can be neutral against a latency wall. Doubling its weight-extraction work
is **free** (295.9 vs 303.5 Mpx/s) — so it has spare capacity — and its
subtraction ceiling said 2.5%. Two independent instruments agree the weight
extraction is not its cost. The verdict stands, now for a supported reason rather
than an exhausted one.

### Still open

- BC6H captured ~20% of a ~100% ceiling. What remains is 96 multiplies per block
  (16 pixels x 3 channels x 2), which is a vectorisation candidate — 4 pixels of
  one channel per register — and it needs its own ceiling first.
- The BC5 palette chain (§32), latency bound and untouched.
- BC2 and BC3 have no real-content measurement; our packs contain neither.

---

## §34 — The 96 multiplies were the wrong target, and the probe said so first

§33 ended by naming the BC6H interpolation — 16 pixels x 3 channels x 2
multiplies — as the next candidate, "needs its own ceiling first". It did, and
the ceiling refused it.

### The probe that saved a day

| probe (512², serial) | Mpx/s | reading |
|---|---:|---|
| full | 119.7 | — |
| interpolation removed | 156.1 | ~23% of the call |
| **2x interpolation work** | **115.5** | **only 3.5% slower** |

Removing it helps; doubling it costs nothing. That pair means the interpolation
sits **on the dependency chain but has spare throughput** — precisely the shape
that made three consecutive BC5 rounds measure neutral. Vectorising 96 multiplies
is a throughput fix, and there was no throughput problem to fix.

**The candidate named at the end of §33 was wrong, and one build said so.** Had
the same paragraph been written without the probe, it would have been a day of
`mullo_epi32` work with runtime detection for a result already visible in three
numbers.

### The same probe, pointed elsewhere, found the real one

Aimed at the downstream half-to-float conversion, doubling the work cost **19%**
(121.3 → 98.8) — a throughput signal, not a latency one. `vcvtph2ps` converts
eight halves per instruction, so a block's 48 components become six instructions
instead of 48 scalar conversions.

**136.0 → 146.8 Mpx/s, +7.9%**, eight samples per arm, arms slightly overlapping.
Against DirectXTex: **BC6H 4.77x**, up from 4.50x; **8.86x** across formats.

Less than the 19% the probe promised, and that is expected — removing 19% of a
call cannot yield 19% when what remains still has to run. The probe bounds the
prize; it does not predict the delivery.

### The lesson worth keeping

**The doubling probe is now the campaign's cheapest and most decisive
instrument** — one build, three numbers, and it distinguishes the two failure
modes that have between them accounted for every neutral result in this file:

| removing it helps | doubling it hurts | meaning | what pays |
|---|---|---|---|
| yes | yes | throughput bound | do less work: fewer ops, SIMD |
| yes | **no** | latency bound, spare throughput | shorten the chain, or nothing |
| no | — | not the cost at all | stop |

Before §32 this file had spent five rounds across BC5, BC7 mode 6 and BC6H
attacking the middle row with tools that only help the top row. The instrument
costs one build and it now runs first.

### Still open

- BC6H's interpolation is ~23% of the call and latency-shaped; the only lever is
  shortening the chain, not vectorising it.
- The BC5 palette chain (§32), same shape, same open question.
- BC2 and BC3 have no real-content measurement; our packs contain neither.

---

## §35 — Downstream, and two ways a probe can lie

§34 left BC6H with the interpolation ruled out (latency-shaped) and the
conversion done. Downstream of both sits the **scatter**: widen RGB to RGBA,
change stride, sixteen separately range-checked indexed writes per block row.

The arithmetic pointed at it. Block-decode-removed was 227 Mpx/s against 120
full, so conversion plus scatter is ~53% of the call; the conversion measured
~19%. The remainder is the scatter.

### Lie #1 — the dead-store probe

The doubling probe was reached for first, and it was **invalid**:

```rust
out[d + i * 4] = fscratch[s + i * 3];   // written
out[d + i * 4] = fscratch[s + i * 3];   // "doubled" — and deleted by LLVM
```

Writing the same value twice to the same address is dead-store eliminated. The
duplicate never existed, so the probe measured nothing and reported ~7%.

**The doubling probe has a precondition the subtraction probe does not: the
duplicated work must be observable.** For arithmetic feeding a value, XOR-ing the
second copy with zero preserves it. For *stores*, nothing preserves them —
duplicate stores to one address are removable by definition. Doubling is not a
universal instrument.

### Lie #2 — the cold arm, again

The row-store change was then measured directly. The first ABBA reported
**+34.5%**, matching BC5's §29 result almost exactly, which made it feel
confirmed rather than suspicious. A re-run contradicted it — and one arm spanned
**75.7 to 132.4 Mpx/s**, so the box was disturbed.

Per §20's rule, a verdict that flips on re-measurement is the instrument deciding
the answer. Interference only ever slows a sample, so robust statistics over 9
NEW against 18 OLD:

| estimator | NEW | OLD | |
|---|---:|---:|---:|
| max | 140.9 | 131.3 | +7.3% |
| p75 | 134.9 | 125.2 | +7.8% |
| median | 122.7 | 117.6 | +4.3% |

**+5-8%, not +34.5%.** All three agree in sign, which is why it ships; the
magnitude is a range because the box did not permit better. The peer run in the
same conditions had every absolute number down ~25%, so only ratios were read
from it.

### The lesson worth keeping

**A result that matches a previous win is the easiest kind to believe and the
one most worth re-running.** +34.5% here against +34.7% in BC5, same change, same
shape — it looked like confirmation. It was a cold arm. Prior expectation makes a
number *more* dangerous, not less, because it suppresses the instinct to check.

And: **when the box is bad, change the estimator rather than the conclusion.**
Max and p75 over many samples extract a real signal from a contaminated run,
where a mean would have reported whatever the interference decided. What must not
happen is quoting the clean-looking first number.

### Still open

- BC6H's interpolation, ~23% of the call and latency-shaped (§34).
- The BC5 palette chain (§32), same shape.
- BC2 and BC3 have no real-content measurement.
- **This box needs re-checking before the next timing round** — two consecutive
  rounds have now been degraded by interference.

---

## §36 — The encoder, and an instrument for a box that will not hold still

The decoder campaign's findings were tested against the encoder. The encoder has
had its own campaign (quality-first: AVX2 fit kernels, RDO, +1.22x CPU vs 0.1.2,
21/3 on speed against DirectXTex), so the question was narrow: **does the
decoder's specific defect class appear there?**

### Where encode time is

| stage | share |
|---|---:|
| mip downsample | ~4% (BC7), ~0% (BC1) |
| **block encode** | **~96%** |

And within the BC7 block search, by disabling each mode in turn:

| mode | share |
|---|---:|
| **mode 6** | **~86%** |
| mode 5 | ~9% |
| mode 4 | ~5% |
| mode 1 | ~0% |

Mode 1 costing nothing is a **confirmation, not a null result**: it carries the
campaign's err-ceiling + 2x-promise + shortlist gates, and they are doing exactly
what they were built for. A gate that works is invisible in a profile.

### The transferable finding

The bit *writer* is not the analogue of the decoder's bit reader — it runs once
per block after the search, not on the hot path. But `palette_mode6` is:

```rust
pal[k][c] = (((64 - w) * c0[c] + w * c1[c] + 32) / 64) as u8;
```

**That is the same expression §25 factored in the decoder**, in the encoder,
called once per seed candidate plus each refine — up to seven times per block.
`c0 * 64 + 32 + w * (c1 - c0)` halves it to one multiply per component: 64
instead of 128 per call.

The encoder and decoder share no code and were optimised in separate campaigns
months apart. Nothing textual connects them; the palette is *built* in one and
*interpolated* in the other. This is §28's lesson landing a second time — a fix
is a hypothesis about every place with the same **shape**, and shape does not
grep.

### The box denied the verdict, so the instrument changed

Timing was worthless: eight ABBA samples, one OLD reading **59.13 ms** against a
~25 ms median, and min-estimator disagreeing with median-estimator on sign. Three
consecutive rounds have now been degraded.

Per this file's own rule for that situation — read the emitted assembly, which is
deterministic and needs no quiet box:

| | `imul` in the emitted library |
|---|---:|
| before | 851 |
| **after** | **835** |

**16 multiply instructions removed**, statically, byte-identically. That is a
confirmation the change does less work. It is **not** a speed claim, and none is
made.

### Shipped to master, not released

The change is byte-identical (the encode determinism tests gate it), strictly
less work (deterministically confirmed), and **unmeasurable here today**. It is
committed but no version is published: a release note is a claim, and there is
nothing yet to claim. Re-measure on a calm box before publishing.

### The lesson worth keeping

**When the box will not hold still, change the instrument, not the standard.**
The temptation is to average harder, quote the friendliest estimator, or ship on
"it must be faster, it does less work". The assembly count answers a *different*
and weaker question — does it do less work? — but answers it exactly, and that is
worth more than a timing number the machine is deciding.

### Still open

- The encoder's mode 6 is ~86% of BC7 encode and its fit kernel is already AVX2;
  a ceiling probe on the remainder has not been run.
- BC6H interpolation (§34) and the BC5 palette chain (§32), both latency-shaped.
- **This box needs attention before any further timing round.**

---

## §37 — The encoder's redundant fit, and a decomposition that was wrong twice

§36 found the decoder's `base + w*delta` identity living in the encoder's
`palette_mode6` and could not measure it. This round went a layer deeper, with
counters instead of a clock, because the box is still degraded.

### Counters found what timing could not

Instrumenting the BC7 mode-6 search over 21 847 blocks:

| | per block |
|---|---:|
| seeds tried | 3.18 |
| **`fit_indices` calls** | **5.14** |
| refines run | 98.0% of blocks |
| refines that improved | 65.9% |
| seed 0 / seed 1 / seed 2+ wins | 74.3% / 19.3% / **6.4%** |

3.18 seed fits plus ~1.96 from the refine gives 5.14 — and the refine reaches
that by calling back in with the winning *endpoints*, which re-quantizes them,
rebuilds the palette and re-runs the fit **for a candidate just evaluated**.
Carrying the candidate itself into the refine removes it:

**112 239 → 90 833 fits, exactly −21 406 = the refine count. −19.1%.**
Byte-identical, and every other counter unchanged.

### Then the arithmetic did not close

Removing 19% of what was assumed to be the dominant kernel produced **zero**
measurable change — ten samples per arm, means 28.6 against 28.5.

Per this file's own rule that indicts the decomposition rather than the code,
ceiling probes inside mode 6:

| stage | share of BC7 encode |
|---|---:|
| `fit_indices` | ~18% |
| seed search | ~17% |
| `quantize_7p` | ~11% |
| `ls_endpoints` | **~0%** |

`fit_indices` is 18%, not dominant. 19% of 18% is **3.4%** — exactly the size of
effect a ±10% box cannot see. The arithmetic closes, and the null result was
correct rather than mysterious.

**The assumption that failed was never measured.** "The 16×16 index search must
be the cost" is plausible, was believed through the whole implementation, and was
wrong by a factor of three. The ceiling probe that settled it takes one build and
could have run first.

### The 6.4%

Seeds 2 through 4 win **6.4%** of blocks while costing 1.18 of the 3.18 seed fits
per block. That is a search-skip-gate candidate worth roughly 6% of encode — but
it is **not** byte-identical, so it needs the encoder campaign's corpus A/B
(102 cases, PSNR + payload FNV), not a timing run. Logged, not attempted: the
mandate for that encoder is faster **and** better, and a quality trade cannot be
judged on this box either.

### The lesson worth keeping

**A counter and a ceiling probe answer different questions, and this round needed
both.** The counter proved the work was removed (−21 406 fits, exact). The
ceiling probe explained why removing it did not show (18%, not 60%). Either alone
would have misled — the counter into claiming a win, the timing into concluding
the change did nothing.

On a box that will not hold still, that pairing is the whole method: **count what
you removed, and bound what it was worth.**

### Still open

- Seed pruning (6.4% win rate) — needs the corpus A/B, not a stopwatch.
- The seed search itself is ~17% and `quantize_7p` ~11%; neither has had a
  ceiling probe of its internals.
- BC6H interpolation (§34) and the BC5 palette chain (§32).
- **The box.**

---

## §38 — The box, the seed gate, and three instruments doing three jobs

Three items, executed in order because each unblocked the next.

### 1. The box was never noisy — it was 73% busy

Four rounds of degraded measurement had one cause, and one command found it:
**LoadPercentage 73** on 24 logical cores, with ~21 000 CPU-seconds of VS Code,
5 301 of Task Manager, plus AWCC, Killer and Brave.

The fix was already in this campaign's own toolkit and simply not applied: the
sim harness pins (`0x3c` + HIGH_PRIORITY_CLASS), but **every crate-level probe ran
unpinned**. All fifteen now pin before timing.

Effect: probe spread 23% → 11-16%. Honest about the residual — a null A/B of one
binary against itself still spanned 68.93-80.37 ms, so the pinned floor is ~11%,
not the 3.5% the first quiet eight samples suggested. **Pinning improved the
instrument; it did not fix the machine.**

### 2. The seed gate — measured at every step

Counters said the two cheap seeds win **93.6%** of blocks and the three expensive
extras **6.4%**. Dropping the extras outright was measured *first*, as the
ceiling: **-0.0028 dB mean, -0.049 dB worst** across the BC7 corpus. A trade,
which the encoder's mandate forbids.

So gate on residual error instead — and calibrate rather than guess:

| gate | extras skipped | quality |
|---|---:|---|
| SSE <= 64 | 29.5% | 0 worse |
| **SSE <= 256** | **83.5%** | **0 worse** |
| SSE <= 1024 | 96.4% | untested |

**64 fires on only 29.5% of blocks, which is exactly why it measured neutral** —
a gate that rarely fires saves nothing, and that was visible in the distribution
before any timing. 256 skips 83.5% including the O(16²) farthest-pair scan, and
the full 102-case corpus reports 0 better / 98 same / 0 worse, mean -0.00004 dB.

Work removed, counted deterministically: **90 833 → 69 209 fits, -23.8%.** With
§37 that is **5.14 → 3.168 per block, -38.4%** across two releases.

### 3. The farthest-pair scan folded into 2

It was listed as the third target — ~17% of encode, an O(16²) scan. It lives
*inside* the extras the gate now skips on 83.5% of blocks, so item 3 was
delivered by item 2 rather than separately. Worth noting because the temptation
was to attack it directly; the cheaper move was to stop calling it.

### The method, stated once

Three instruments, three different questions, none substitutable:

| instrument | question | immune to a busy box |
|---|---|---|
| **counter** | did the work actually go away? | yes |
| **ceiling probe** | what was that work worth? | mostly |
| **corpus A/B (PSNR/FNV)** | did quality survive? | yes |
| pinned timing | how much faster? | **no** |

This round used the first three and refused the fourth. The timing column stayed
empty on purpose: `fit_indices` is ~18% of encode, so -23.8% of it is a few
percent, and the pinned floor is ~11%. **A number the machine decides is not a
result, and an empty column is more honest than a friendly estimator.**

### Still open

- The gate at 1024 skips 96.4% and is untested for quality — the obvious next
  calibration point, and cheap, since the quality gate is deterministic.
- `quantize_7p` is ~11% of encode and has never had its internals probed.
- **The box.** Pinning narrowed it; it did not fix it.

---

## §39 — The timing verdict, and a probe that was serial but not representative

§38 shipped the seed gate with the timing column deliberately empty. This closes
it: **+8.33% median, 17/17 paired wins, z = +4.12.** Getting there took three
fixes to the instrument and one refuted probe.

### Three fixes, each necessary

1. **Pin** (§38) — spread 23% → 11-16%. Not enough on its own.
2. **Force the encoder serial in both arms.** A 512² surface is 16 384 blocks,
   above `ENCODE_PARALLEL_MIN_BLOCKS`, so it strips across threads — and *total
   process CPU* then varies with scheduling and work-stealing even for identical
   work. A null A/B of one binary against itself spanned 14% in CPU time. Raising
   the threshold in both arms removes threads from the measurement entirely.
3. **Report CPU time, paired, with a z-score** — the campaign's own memory has
   said "CPU is the robust verdict on this loaded box" since the encoder
   campaign, and every probe I built measured wall.

### The probe that was serial and still wrong

The first serial probe used 128² × 7 mips and reported a **1.5% regression**
(z = −1.53), after an earlier 128² × 1 reported −2.00. Two negative results for a
change the counters said removed 23.8% of the work.

The cause was not the instrument's *precision* but its *content*:

| probe shape | gate fires on |
|---|---:|
| 128² × 7 mips | **9.8%** of blocks |
| 512² × 10 mips | **78.2%** of blocks |

Coarse sampling of the same procedural generator raises per-block error, so the
small probe almost never triggers the gate — it was measuring the restructure
overhead with none of the benefit. Same code, same format, same mip count
policy; **different content statistics, opposite verdict.**

Fixed by measuring the production shape with the threshold raised: 42.969 vs
46.875 ms median, 17/17 wins, z = +4.12, **+8.33%**.

### The lesson worth keeping

**"Serial" and "pinned" make a probe precise. Neither makes it representative.**
Three sections of this file have now been spent on instruments that were
carefully controlled and pointed at the wrong thing — §30's constant index that
folded its own arithmetic away, §35's dead-store duplicate, and this one.

The check that catches all three is the same and takes one counter: **before
trusting a probe, verify the code path you care about actually executes in it, at
the rate it executes in production.** A gate that fires 9.8% of the time in your
benchmark and 78.2% in reality is not a benchmark of that gate.

### Where the encoder stands

| release | change | verdict |
|---|---|---|
| 0.3.20 | refine reuses the winning fit | −19.1% fits, ~3.4% of encode, unmeasurable |
| 0.3.21 | residual-error seed gate | −23.8% fits, **+8.33% CPU, z = +4.12** |

Together: **5.14 → 3.168 index fits per block (−38.4%)**, 0 of 102 corpus cases
worse, mean −0.00004 dB.

### Still open

- The gate at SSE ≤ 1024 skips 96.4% of extras and is untested for quality; the
  quality gate is deterministic, so it is cheap to settle.
- `quantize_7p` is ~11% of encode and has never had its internals probed.
- The box is still 73% busy. Pinning, forcing serial and pairing now make that
  survivable rather than fixed.

---

## §40 — The floor, and why 3.0 is not reachable

§39 left the BC7 mode-6 search at **3.168 index fits per block**. The structural
floor is **3.0** — two base seeds plus one refine — so the question was whether
the last 5% could be taken.

It cannot, and the reason is worth recording precisely.

### The decomposition

| component | fits/block |
|---|---:|
| base seeds (extrema + channel min/max) | 2.000 |
| refine's second fit (runs on 98% of blocks) | 0.980 |
| gated extras | 0.188 |
| **total** | **3.168** |

Two candidates, both the same shape as the gate that worked in §38: gate the
**refine** on residual error, and gate **seed 1** on seed 0's error.

### Both are quality-bound, not opportunity-bound

Swept against the 102-case corpus:

| gate | worse cases | worst |
|---|---:|---:|
| refine <= 4 | **0** | -0.0035 dB |
| refine <= 16 | 7 | -0.018 |
| refine <= 64 | 13 | -0.178 |
| refine <= 256 | 21 | -0.776 |
| seed1 <= 16 | **0** | -0.0035 |
| seed1 <= 32 | 6 | -0.045 |
| seed1 <= 64 | 8 | -0.101 |

Quality-free thresholds exist — and at those thresholds the gates are **inert**:

| gate | fires on |
|---|---:|
| refine <= 4 | **0.2%** of blocks |
| seed1 <= 16 | **2.2%** of blocks |

Together: **3.168 → 3.147, 0.7%**, for two more tuned constants. Reverted.

### The distinction that matters

§38's extras gate looked identical in form and behaved completely differently:
it is quality-free at SSE <= 256, where it fires on **83.5%** of blocks. The same
mechanism, three applications, two of them worthless.

The difference is not the gate — it is what is being gated. **The seed extras
are speculative** (three more guesses at an endpoint pair, winning 6.4%), so
error predicts their value well. **The refine is corrective** — least squares
uses the indices already fitted, so it improves 65.9% of the blocks it runs on
*including* low-error ones, and residual error does not predict where it is
wasted.

A search-skip gate works when the skipped work is a **guess**. It does not work
when the skipped work is a **correction**, because a correction earns its cost
almost everywhere.

### Where the encoder stands

**3.147 against a floor of 3.0**, with the remaining 5% protected by the corpus
gate rather than by inattention. Index fits are finished at the quality-free
level. Anything further is a quality trade and belongs to a different mandate.

### Still open

- `quantize_7p` is ~11% of encode and has never had its internals probed. It is
  the largest unexamined stage now that fits are closed.
- The seed search itself is ~17%, of which the gate now skips 83.5% of the
  expensive part.
- BC6H interpolation (§34) and the BC5 palette chain (§32), both latency-shaped.

---

## §41 — `quantize_7p`: 512 answers, recomputed 24 times a call

§40 named `quantize_7p` as the largest unexamined stage now that index fits are
closed — ~11% of BC7 encode by ceiling probe. Opening it took one reading of the
function it calls.

### The whole search was a pure function of two bytes

`unquantize_7p_chan(q, p)` is `(q << 1) | p`. That makes the inner candidate
search a pure function of `(channel_value, p_bit)` — **512 possible inputs**.

The direct form re-derived one of those 512 answers **24 times per call**: two
p-bits x four channels x a three-wide candidate window, each step an unquantize
and a squared error. `quantize_7p` runs roughly six times per block.

Replaced by a `const fn` table — 768 bytes, permanently L1-resident, built by
running the *identical* search so equivalence is by construction rather than by
argument. Two p-bits x four channels = **8 lookups**.

**+7.26% median, 18/19 paired wins, z = +3.90, byte-identical.**

### Why it survived four rounds of looking

The function is thirty lines of obviously-correct code with no allocation, no
branching to speak of, and no obvious waste. Nothing about reading it says
"hot". It was found by **decomposition**, not by inspection — the §37 ceiling
probe put a number on it (~11%) two sections before anyone looked inside, and
that number is the only reason it was opened at all.

The tell, once inside, was not the loop but the **domain**: a `u8` in, a `u8` and
a small error out, no other state. **Any pure function over a small domain that
appears in a hot loop is a table.** The question to ask of a hot helper is not
"is this code efficient" but "how many distinct inputs can it have".

### The pattern, stated for reuse

| signal | action |
|---|---|
| pure function, domain <= a few thousand | precompute; equivalence by construction |
| pure function, large domain | look for a closed form |
| impure or large state | neither — profile instead |

`unquantize_7p_chan` had a 512-input domain sitting behind a 24-step search. The
same question is worth asking of every remaining hot helper in the encoder.

### Where the encoder stands

| release | change | verdict |
|---|---|---|
| 0.3.20 | refine reuses the winning fit | -19.1% fits |
| 0.3.21 | residual-error seed gate | -23.8% fits, **+8.33% CPU** |
| 0.3.22 | `quantize_7p` table | **+7.26% CPU**, byte-identical |

Index fits 5.14 -> 3.147 per block, and two independent CPU wins on top, all with
**0 of 102 corpus cases worse**.

### Still open

- The seed search is ~17% of encode; the gate now skips 83.5% of its expensive
  half, but the cheap half (extrema, channel min/max) has not been examined.
- `palette_mode6` and `best_index_pal` are the remaining per-candidate helpers.
- BC6H interpolation (§34) and the BC5 palette chain (§32), both latency-shaped.

---

## §42 — Two non-targets and one real one, all decided by counting

§41 named `palette_mode6`, `best_index_pal` and the seed search's cheap half as
the remaining candidates. Ceiling probes on a loaded box put all three inside the
noise, which decided nothing. **Call counts decided all three in one run.**

| helper | calls/block | verdict |
|---|---:|---|
| `palette_mode6` | 3.168 | one per fit — already minimal |
| **`best_index_pal`** | **0** | **never runs** |
| `extrema_rgba` | 2.245 | needs 1 |
| `channel_minmax_rgba` | 3.245 | needs 2 |
| `rgba_span_sum` | 1.245 | needs 0 |

### The one that never runs

`best_index_pal` searches 16 palette entries x 4 channels for every pixel. It
reads like the hottest thing in the encoder. It is called **zero** times:
`fit_indices_mode6` dispatches to the AVX2 kernel, leaving the scalar search as
the fallback and the oracle.

Two rounds ago that function would have been an obvious target. **A counter cost
one build and removed it from consideration entirely** — where a ceiling probe on
this box could not have, because zero and noise look identical when the box is
20% noisy.

### The real one

`extrema_rgba` ran once to initialise `best_seed` and again inside the seed
builder that returns the same pair. `rgba_span_sum` is a sum over
`channel_minmax_rgba` — which the seed builder had already computed for seed 1.
Three walks of sixteen pixels for two statistics.

Hoisted to one computation each, threaded through, `rgba_span_sum` deleted:
**+2.67%, 11/13 paired wins, z = +2.50, byte-identical.**

### The lesson worth keeping

**A call counter is a better first instrument than a ceiling probe**, and this
round is the clean demonstration. The ceiling probe answers "what is this worth"
but needs a quiet box; the counter answers "how often does this happen" and needs
nothing. On a contended machine the counter is often *sufficient* — 0 calls needs
no timing, and 2.245 calls where 1 is needed is a defect visible without a clock.

Order of instruments, revised for this campaign:

1. **Call counts** — free, exact, and frequently decisive on their own.
2. **Ceiling probe** — what the work is worth, when the counter says it happens.
3. **Paired CPU + z-score, forced serial, pinned** — the verdict.

### Still open

- `palette_mode6` at 3.168 calls per block is the last per-fit helper; a SIMD
  interpolation is possible but it is 64 multiplies against an already-factored
  form.
- The AVX2 `fit_indices_mode6` kernel itself has never been opened.
- BC6H interpolation (§34) and the BC5 palette chain (§32), both latency-shaped.

---

## §43 — The SIMD kernel was half scalar

§42 established that `best_index_pal` never runs, which means
`fit_indices_mode6_avx2` **is** the BC7 index fit. Opening it found the campaign's
own recurring defect, inside the code least suspected of having it.

### What an "AVX2 kernel" was doing

```rust
for (k, &entry) in pal.iter().enumerate() {   // 16 entries
    sse16_rgba(pixels, entry, &mut sse);      // vectorised
    for i in 0..16 {                          // SCALAR
        if sse[i] < best_e[i] { best_e[i] = sse[i]; best_i[i] = k as u8; }
    }
}
```

And one level down, `sse16_rgba` itself computed in registers, **stored eight
`i32` to a stack array**, and read four of them back scalar-ly.

So per palette entry: one store-forwarding stall inside the distance kernel,
another feeding the min loop, and sixteen scalar compare-branches. Times sixteen
entries, times 3.168 fits per block.

Kept in registers — two `__m256i` for the distances, compare-and-blend for the
running minimum, one extraction at the end: **+9.23%, 16/18 paired wins,
z = +3.30, byte-identical.** The largest single encoder win of this campaign.

### Why it hid

**Its name.** The function is called `fit_indices_mode6_avx2`, lives in
`simd.rs`, carries `#[target_feature(enable = "avx2")]`, and has a 200 000-case
oracle proving it exact. Everything about it says *this one is already done*.

Three earlier sections found store-forwarding stalls in decode and named the
pattern; none of them thought to look inside the file whose whole purpose is
vectorisation. **A module named for an optimisation is not evidence that the
optimisation is complete** — and it is the one place nobody re-reads.

The tell was structural and visible without a profiler: **a `for i in 0..16`
loop inside a kernel whose reason to exist is not doing that.**

### The instrument ordering held

§42 revised the order to counts → ceiling → paired verdict, and this round is
its payoff: the count that `best_index_pal` runs zero times is what promoted this
kernel to the top of the list. Without it the obvious target was the scalar
function, which does not execute.

### Where the encoder stands

| release | change | verdict |
|---|---|---|
| 0.3.20 | refine reuses the winning fit | -19.1% fits |
| 0.3.21 | residual-error seed gate | +8.33% |
| 0.3.22 | `quantize_7p` table | +7.26% |
| 0.3.23 | statistics hoist | +2.67% |
| **0.3.24** | **register-resident index fit** | **+9.23%** |

Four independent CPU wins, three of them byte-identical, none costing a single
one of the 102 corpus cases.

### Still open

- `palette_mode6` (3.168 calls/block) is the last per-fit helper; its
  interpolation is already factored, and a SIMD form is the remaining idea.
- The BC1 AVX2 kernel (`bc1_fit_4color_avx2`) has the **identical** scalar
  min-tracking loop and the same `sse16_rgba_noalpha` array round-trip. It was
  not touched this round and is the obvious next target.
- BC6H interpolation (§34) and the BC5 palette chain (§32), both latency-shaped.
