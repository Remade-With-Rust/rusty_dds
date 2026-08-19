# rusty_dds_sim

A deterministic DDS texture-streaming replay that runs the **same** core three
ways: headless (for numbers), as a live windowed pane (for watching), and as a
four-pane grid (for comparing). Behind
[`docs/plans/simulator-demo.md`](../docs/plans/simulator-demo.md).

## The four panes

The cockpit (`cargo run --release --features demo --bin cockpit`) gives each pane
two independent switches — **rusty_dds** and **rusty_alloc** — plus its graphics
API. That is the comparison worth making: turning them on one at a time is what
separates "rusty_dds helped" from "rusty_alloc helped" from "they only help
together". Three presets fill the four slots (isolate on D3D11, isolate on
Vulkan, stack x API), and any slot can be switched off.

From the command line:

```sh
# both stacks x both graphics APIs
./target/release/sim grid --pack pack/high192

# the 2x2 factorial on one API: neither / rusty_dds only / rusty_alloc only / both
./target/release/sim grid --pack pack/high192 --isolate d3d11
```

|  | rusty_dds | rusty_alloc | arm |
|---|---|---|---|
| baseline, what a conventional PC engine ships | ✗ | ✗ | `dxtex` |
| **rusty_dds alone** | ✓ | ✗ | `rusty` |
| **rusty_alloc alone** | ✗ | ✓ | `dxtex+ra` |
| both | ✓ | ✓ | `rusty+ra` |

Crossed with `--api d3d11` and `--api vulkan`, both of which are the off-the-shelf
system runtimes (`d3d11.dll`, `vulkan-1.dll`) driven through a conventional
renderer. Nothing of ours sits in the graphics path.

Every pane is a separate process, because `#[global_allocator]` is a
compile-time choice and an arm on `rusty_alloc` cannot share a process with one
on the system allocator. `sim` serves the plain arms and `sim-ra` the `+ra` ones;
the launcher picks per arm and refuses to run an arm its binary cannot honour.

**The grid is for seeing, the board is for measuring.** Four panes contend for
cores and one GPU, so the picture is comparable — every pane replays the
identical trace and shows the identical frame — but the numbers on screen are
indicative. Anything reportable comes from `sim bench`, one arm at a time,
pinned, with nothing else in the process.

## Quick start

```sh
cd sim
cargo build --release                     # headless harness
cargo build --release --features ui       # + the cockpit

# cook a procedural pack (no corpus checkout needed)
./target/release/sim cook --tier medium --textures 192 --out pack/medium192

# prove the harness is deterministic before trusting any number from it
./target/release/sim verify --pack pack/medium192

# 7 reps, ABBA-interleaved, pinned, one process per run
./target/release/sim bench --pack pack/medium192 --scenario traverse \
    --arms a,a2 --reps 7 --workers 4 --pin --out runs/traverse

./target/release/sim board --runs runs/traverse --out ../docs/artifacts/simulator-phase0-nullband.md

# headful
./target/release/cockpit
```

## What the pieces are

| | |
|---|---|
| `sim.rs` | The replay driver. One `step()` per frame; both front-ends drive it, so what the demo shows and what the board measures cannot drift apart. |
| `scenario.rs` | Tiers, world layout, camera traces, request generation, peak-demand probe. Nothing here reads the clock. |
| `provider.rs` | **The stack seam.** `TextureProvider` / `OpenTexture`. Phase 0 ships `RustyProvider`; Phase 2 adds DirectXTex over a C ABI. |
| `renderer.rs` | **The API seam.** `Renderer`. Phase 0 ships `NullRenderer` (real staging copy, hashed); Phase 1 D3D11, Phase 3 Vulkan. |
| `stream.rs` | Residency, eviction, and the worker pool. Deterministic under threads by construction. |
| `pack.rs` | Procedural cook + the pack manifest. |
| `metrics.rs` | Frame records, allocation counting, log histogram, hitch rule. |
| `board.rs` | CSV → markdown, and the gates that refuse to report. |
| `gpu/d3d11.rs` · `gpu/vulkan.rs` | The live viewports, one per graphics API, behind one `Viewport` trait. |
| `gpu/scene.rs` | Billboarded textured quads at the world positions the request generator already uses, so what you see is what the streamer was asked for. |
| `shaders/quad.wgsl` | The scene shader, compiled to SPIR-V by naga at build time (pure Rust — this box has no Vulkan SDK). |
| `view.rs` | One live pane: the replay driven into a swapchain, under the rails. |
| `panes.rs` | The grid: spawn, tile, and aggregate `TELEM` lines from N panes. |
| `bin/cockpit.rs` | The Dioxus desktop cockpit. |

## Determinism

`sim verify` asserts three properties, and the harness is not admissible for any
A/B until all three pass:

1. **Repeatability** — the same arm run twice agrees.
2. **Thread-independence** — `--workers 0` (inline) and `--workers N` (pooled)
   agree. Worker completion order varies; the frame hash must not.
3. **Arm-independence** — `a` and `a2` are the same code and agree.

All three reduce to one number, the `trace_hash`: a fold of every frame's
request hash and uploaded-byte hash. It is what makes the Phase 2 claim
defensible — when the DirectXTex arm lands, an identical `trace_hash` proves
both stacks handed the GPU the same bytes, so GPU-side parity is demonstrated
rather than argued.

Three properties make that possible under a thread pool: the frame's batch is
chosen on the main thread from a sorted request list against a byte budget; the
frame joins before it ends; and per-upload hashes are **sorted** before folding.

## Findings from Phase 0

The null arm ran first, exactly as the plan requires, and it earned its keep by
refuting three designs before any second stack existed to be compared:

1. **The first workload measured nothing.** 5 requests per frame, zero uploads
   after warm-up, `resident_pct` pinned at 1.0 — deterministic, reproducible and
   completely empty. Fixed by sizing the world against the cull distance and
   raising `NEAR_DIST` so the visible set sits at expensive mips.

2. **A fraction-of-pack pool budget cannot bind, at any fraction.** Pool grows as
   `frac · N · S` while demand grows as `~0.19 · N · S`, so the failure was
   scale-invariant — adding textures would never have helped. The budget is now
   derived from the scenario's own measured peak demand
   (`scenario::peak_demand_bytes`), which self-calibrates to any pack.

3. **The rolling-median hitch rule was measuring "did work happen".** With a
   median frame of 0.01 ms, twice the median is still timer quantisation; the
   detector flagged 250 frames per 1000. Phase 0 uses an absolute 1 ms
   threshold. Phase 1 replaces it with the definition studios use — a frame that
   missed its present deadline — which needs a present.

4. **One headline null band would have thrown away the usable metrics.** From the
   same runs, median frame cost carried a ±40% band while working set carried
   ±1.5%. The board now prints a band per metric.

5. **Pinning matters more than anything else measured so far.** Unpinned →
   pinned (`0x3c`, high priority) took run CPU from ±88% to ±39%, container
   parse from ±59% to ±14%, and worst-frame from ±4151% to ±94%.

Current null band on this box (24 cores, loaded, pinned to `0x3c`): staging copy
±8.7%, median frame ±11.4%, parse ±13.5%, streaming CPU ±18.3%, p99 ±19.5%,
working set ±1.5%. Allocation counts and uploaded bytes are bit-identical across
arms. **No later phase may report a difference narrower than these.**

## Safety rails on the live viewports

The first version of the D3D11 viewport **hung the machine**, and the rails below
exist because of it. What happened, precisely:

The streaming pool is over-committed on purpose, so it evicts and re-requests
textures continuously. The viewport treated eviction as "destroy the GPU
resource" and re-request as "create it again", on a `Present(0)` loop with vsync
off. That is thousands of `CreateTexture2D` + `CreateShaderResourceView` +
release cycles per second, each up to several MB of VRAM, submitted as fast as
the CPU could build frames. The run's own telemetry recorded a **3442 ms frame**
while it was happening, and the display driver went down with it.

Four rails, all in [`gpu::GpuLimits`](src/gpu/mod.rs), all overridable, all
defaulting to values a healthy pane never reaches:

| rail | default | what it prevents |
|---|---|---|
| GPU texture cache never freed on eviction; LRU trim only, `max_destroys_per_frame` | 2 / frame | the create/destroy storm that caused the hang |
| `Present(1)` — vsync on | always | a submit loop outrunning the display |
| `max_uploads_per_frame` · `max_upload_bytes_per_frame` | 24 · 24 MiB | one frame pushing hundreds of MB at the driver |
| `frame_abort_ms` · `abort_after_slow_frames` · `max_run_secs` | 500 ms · 20 · 900 s | a struggling pane continuing to hammer a driver in trouble |

Two supporting changes came out of the same incident:

- **Pacing announces itself.** The run that hung the machine completed 3000
  frames in seconds while believing it was paced at 60 Hz, and nothing in its
  output said otherwise. `sim view` now prints a `TELEM_START` line naming the
  pacing mode and every active ceiling, and the realtime path has a per-frame
  floor as well as deadline pacing, so an arithmetic mistake in one cannot
  free-run the loop.
- **Deferred uploads are visible.** The per-frame upload ceiling means work can
  queue; the run reports its deferred-upload peak on exit, so throttling shows up
  as a number rather than as mysteriously slow residency.

### A second GPU fault, found by the grid

The first four-pane run lost one pane to `ERROR_DEVICE_LOST` — Vulkan +
DirectXTex — while the other three finished. It did not reproduce in isolation,
and the two providers' descriptors are byte-identical, which ruled out a format
or pitch mismatch and pointed at concurrency.

The bug was a **use-after-free on the Vulkan staging buffer**: uploads are
`memcpy`d in during the view loop, which runs *before* `frame()` waits on the
in-flight fence, so the CPU could overwrite bytes the GPU was still reading for
the previous frame's copies. Single-buffered staging made that a race; under
four-pane contention it corrupted a copy and the device went down. The staging
buffer is now two alternating regions, flipped only after the frame's copies are
submitted, and `max_upload_bytes_per_frame` is capped below one region.

D3D11 never had this exposure — `UpdateSubresource` copies into driver-managed
memory immediately. It is a good illustration of why both APIs are worth having:
each one tests things the other cannot.

## The cockpit, and where numbers come from

Dioxus desktop on Windows is a WebView2 surface. It renders the control panel and
telemetry; it does **not** own a D3D11 or Vulkan swapchain, so it is not where
GPU timings will come from — the plan's render viewport is a separate native
window with its own device and timestamp queries (Phases 1 and 3).

The cockpit also shares a process with a Chromium compositor, so its live figures
are indicative. Anything reportable comes from `sim bench`, which runs detached,
one arm at a time, pinned, with no UI in the process. The cockpit's *Detached
benchmark* button launches exactly that.

## Deviations from the plan, and why

- **Scenarios are defined in code**, not loaded from `scenarios/*.json`. A parser
  buys nothing until traces are *recorded* rather than generated, and it would be
  a dependency in a harness whose job is to be above suspicion.
- **`chaos` is not implemented yet.** It needs the fuzz corpus wired in as a
  mid-stream fault injector; it lands with Phase 4 alongside the other scenarios.
- **`peak_rss` is Windows-only.** The demo is a PC story; elsewhere the column
  reads zero rather than guessing.

## Dependencies

`rusty_dds` only, for the headless harness. The cockpit adds `dioxus` and
`tokio` behind the `ui` feature, so the thing that produces numbers never needs a
WebView toolchain to build. Later phases add `windows` (D3D11), `ash` (Vulkan)
and `rusty_alloc`, each behind its own feature.

This crate is excluded from the published `rusty_dds` package and keeps its own
MSRV — the library's MSRV 1.73 claim must not be dragged up by a demo.
