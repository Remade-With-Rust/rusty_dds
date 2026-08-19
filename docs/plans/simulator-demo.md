# Plan: `rusty_dds_sim` — a DX11 vs Vulkan texture-streaming simulator

Status: **Phases 0, 2 and 5 complete** (2026-08-18) — harness, null arm, the
DirectXTex peer arm and the rusty_alloc arm, all in [`sim/`](../../sim/).
Phases 2 and 5 were pulled forward ahead of the GPU backends at the user's
direction, so the *stack* and *allocator* axes could be measured before the
*API* axis existed. **Phases 1 and 3 are now complete too**: both viewports render live, and
`sim grid` runs all four panes at once. Phase 4 (tiers, scenarios, cold/warm
passes) and Phase 6 (capture + published board) remain.

Boards: [null band](../artifacts/simulator-phase0-nullband.md) ·
[vs DirectXTex](../artifacts/simulator-stack-vs-directxtex.md) ·
[allocator](../artifacts/simulator-allocator.md) ·
[full matrix](../artifacts/simulator-matrix.md).
Findings and deviations: [sim/README.md](../../sim/README.md).

### What the CPU-side measurement found (2026-08-18)

The dominant runtime cost on the streaming path is **`rusty_dds::Dds::read`
taking an owned copy of the payload**. `Dds` exposes only `read<R: Read>` and
`read_limited`, both of which own their `data: Vec<u8>`; DirectXTex's
`DDSTextureLoader`-shaped path borrows the caller's buffer instead. Measured on
`traverse`/high, 192 textures, pinned, ABBA, N=5-7:

| | container parse | run CPU | hitches | peak working set |
|---|---:|---:|---:|---:|
| DirectXTex (loader, borrows) | **2.8 ms** | 1.91 s | 304 | **132.7 MiB** |
| DirectXTex (scratch, copies) | 470.8 ms | 2.66 s | — | 254.3 MiB |
| rusty_dds + system alloc | 433.4 ms | 2.41 s | 555 | 134.7 MiB |
| rusty_dds + rusty_alloc | 153.3 ms | 1.81 s | 326 | 257.9 MiB |

Three things follow, and the mechanism is confirmed rather than inferred —
switching the peer to its *copying* path (`--peer scratch`) collapses the parse
gap to +5.3%, inside the noise:

1. **The gap is the copy, not parsing speed.** Against a peer that also copies,
   rusty_dds matches it on parse and CPU and uses **47% less peak memory**.
2. **rusty_alloc cuts the copy cost by 65%** (433 → 153 ms), buying ~25% run CPU
   and 41% fewer hitches — at **~124 MiB more peak working set**, on both stacks
   equally, which is what confirms the allocator axis is cleanly isolated.
3. **The actionable fix is in the library, not the harness**: a borrowing parse
   API (`Dds` over `&[u8]`, or a `SurfaceView`-only reader) would remove the copy
   the whole gap is made of. That is a rusty_dds change, tracked separately from
   this plan.

Uploaded bytes are byte-identical across all four arms on every frame
(`trace_hash 169a7205afde5605`), so these are like-for-like.
Scope: a deterministic, replayable "gameplay" harness that swaps exactly one thing —
**rusty_dds + rusty_alloc** vs **Microsoft DirectXTex + the system allocator** — across
two graphics APIs (D3D11, Vulkan) and three quality tiers (Ultra / High / Medium),
and reports CPU/GPU **stability**, not just averages.
Audience: an engine/streaming team at a studio with a large open-world DDS pack
(Star Citizen class: 4K BC7 albedo, BC5 normals, BC6H skyboxes, hot traversal streaming).

---

## 1. What this demo can honestly claim — read this first

A texture library does not make a GPU-bound scene render faster. If the demo shows
"more FPS because of a DDS loader" a studio graphics engineer will dismiss it in ten
seconds, and they will be right. The value of the harness comes from being *unable*
to lie:

**Work-count parity is the design constraint.** In the passthrough profile both arms
hand the GPU the *same BCn bytes* at the *same time*, so GPU shading work is provably
identical. That is enforced (§4), not assumed. Everything the demo measures then
lives on the CPU side of the fence, where the difference is real.

| Dimension | Can it legitimately differ? | Why |
|---|---|---|
| GPU shading / raster time | **No** — must be a wash | Identical blocks, identical resolutions. A delta here means the harness is broken. |
| VRAM footprint at fixed tier | **No** | BCn is BCn. RDO changes disk bytes, not decoded block size. |
| Streaming-thread CPU ms | **Yes** | Parse, subresource slicing, upload-plan construction; decode when the profile calls for it. |
| Frame-time p99 / p99.9, hitch count | **Yes** | Streaming work lands on frames; allocator tail latency and parse cost show up here, not in the mean. |
| Peak working set, RSS drift over a soak | **Yes** | Allocator behaviour: staging churn, fragmentation, cross-thread free. |
| Time-to-residency after a teleport | **Yes** | IO bytes (RDO −4..−15%) + parse + upload throughput. |
| On-screen sharpness at a fixed time budget | **Yes** | The one place a *screenshot* may honestly differ: whoever reaches full mips first looks better in frame 30. |
| Behaviour on malformed input | **Yes, measured** | Feed the existing `fuzz/` corpus mid-stream and record outcomes. Report what happens; do not allege crashes we did not observe. |

### 1.1 Three workload profiles, because the delta has three different sizes

This is the part most benchmark demos get wrong. Be explicit about which one is on screen.

| Profile | What the provider does per request | Expected delta | Honest framing |
|---|---|---|---|
| **Stream** (default) | Parse header → slice subresource → upload BCn blocks. No decode in either arm. | Small CPU delta; real allocator + IO + hitch delta | "Same pixels, steadier frames, smaller pack." |
| **Transcode** | Legacy/uncompressed or CPU-consumed surfaces decoded to RGBA8 (terrain heightfields, UI atlases, virtual-texture pages, CPU readback) | Large — decode board is 24/24 ahead of DirectXTex | "Where the CPU actually touches pixels, we win outright." |
| **Cook** (offline, no window) | Bake the asset pack: RGBA → BCn | Largest and most defensible: BC7 ~2× vs 0.1.2, 21/3 ahead of DirectXTex on speed, 22/2/0 on PSNR, RDO −4..−15% payload | "Your bake farm halves; your patch downloads shrink." |

The live demo runs **Stream**; the panel beside it shows **Cook** and **Transcode**,
which is where the headline numbers live.

---

## 2. Arm matrix

Three independent axes, plus a null arm.

- **Stack**: `A` = rusty_dds + rusty_alloc · `B` = DirectXTex + system allocator
- **API**: `d3d11` · `vulkan`
- **Tier**: `ultra` · `high` · `medium`

2 × 2 × 3 = **12 arms**, plus **`A` vs `A` (null)** and **`B` vs `B` (null)** per API.

> The null arm is not optional. It is the same build compared against itself, and it
> sets the resolution floor. On the campaign box the encode harness null band ran ~1%+;
> a frame-time null band will be wider. **No difference narrower than the null band is
> ever reported as a result.** (`rusty_alloc`'s own README kills its wall-clock claim on
> exactly this rule — the harness inherits that discipline.)

### 2.1 Tier definitions (fixed, not tuned per arm)

| | Res cap | Format mix | Streaming pool | Mip bias | RDO λ at cook |
|---|---|---|---|---|---|
| **Ultra** | 4096 | BC7 albedo · BC5 normal · BC6H sky · BC4 mask | 6 GB | 0 | 0 (byte-identical) |
| **High** | 2048 | BC7/BC1 mix · BC5 · BC4 | 3 GB | 0 | 4 |
| **Medium** | 1024 | BC1/BC3 · BC5 · BC4 | 1.5 GB | +1 | 10 |

Tier changes the *content*, identically for both stacks. Both arms read the **same
cooked pack** in Stream profile (cooked by rusty_dds), so the demo is not smuggling a
content advantage into a runtime measurement. A second Stream variant reading a
**DirectXTex-cooked pack** is run separately to isolate the RDO/IO effect.

---

## 3. Architecture

New crate, outside the published library (add `sim/*` to the `exclude` list in
[Cargo.toml](../../Cargo.toml)). It gets its own MSRV — `rusty_alloc` is edition 2024
while `rusty_dds` holds MSRV 1.73, and the library's MSRV must not be dragged up by a demo.

```
sim/
  Cargo.toml            # path dep on rusty_dds; ash, windows, serde, hdrhistogram
  src/
    main.rs             # arm parsing, run loop, ABBA driver
    trace.rs            # deterministic scenario replay (spline + seeded requests)
    provider/
      mod.rs            # trait TextureProvider  <- THE STACK SWAP POINT
      rusty.rs          # rusty_dds
      dxtex.rs          # FFI -> libdxtex_provider (C ABI)
    gpu/
      mod.rs            # trait Renderer  <- THE API SWAP POINT
      d3d11.rs          # windows crate
      vulkan.rs         # ash
    stream.rs           # streaming threads, pool, residency bookkeeping
    metrics.rs          # per-frame record, allocator counters, CSV/JSON sink
    hud.rs              # live overlay + split-screen capture mode
  shim/                 # C ABI over DirectXTex, extends tools/dxtex_decode_bench
    dxtex_provider.cpp
    CMakeLists.txt
  scenarios/*.json      # traces (§4.1)
  report/               # CSV -> docs/artifacts-style markdown board
```

### 3.1 The two seams

```rust
/// Everything a streaming engine asks a DDS stack for. Both arms implement it.
trait TextureProvider {
    fn open(&self, bytes: &[u8]) -> Result<TextureId>;                          // container parse
    fn describe(&self, t: TextureId) -> TextureDesc;                            // fmt, mips, layers, pitches
    fn subresource(&self, t: TextureId, id: SubresourceId) -> Blocks<'_>;       // Stream profile
    fn decode_rgba8(&self, t: TextureId, id: SubresourceId) -> Result<Vec<u8>>; // Transcode profile
    fn close(&self, t: TextureId);
}

/// Everything the provider needs from a graphics API. Both backends implement it.
trait Renderer {
    fn create_texture(&mut self, desc: &TextureDesc) -> GpuTexture;
    fn upload(&mut self, dst: GpuTexture, id: SubresourceId, plan: &UploadPlan, src: &[u8]);
    fn frame(&mut self, cam: &Camera, visible: &[GpuTexture]) -> FrameTimings;
}
```

`rusty.rs` is a thin wrapper over `Dds::read` + `upload_plan_compressed` /
`upload_plan_decoded_rgba8` — which is exactly why [src/upload.rs](../../src/upload.rs)
being API-agnostic pays off here: **one** `UploadPlan` feeds both
`ID3D11DeviceContext::UpdateSubresource` and `vkCmdCopyBufferToImage`.

`dxtex.rs` calls a small C ABI shim over `LoadFromDDSMemory` + `ScratchImage` +
`Decompress`, built by extending the CMake project that already produces
`dxtex_roundtrip.exe`. **Peer fairness note:** at runtime the fair DirectXTex peer for
Stream is `DDSTextureLoader`-style header parse + direct upload — that is what shipping
engines use — *not* the heavyweight `ScratchImage` path. The shim implements both and
the board names which one each row used. Cook-profile flags stay as the existing boards
set them (`TEX_COMPRESS_BC7_QUICK`, `TEX_COMPRESS_DEFAULT`).

### 3.2 Allocator arm

`rusty_alloc` enters as `#[global_allocator]` behind a cargo feature, so arm `A` and arm
`B` differ by a feature flag and nothing else in the Rust code. Two honesty caveats to
print on the board:

- DirectXTex's own allocations happen inside C++ and go to the CRT heap regardless. The
  arm is therefore **"our stack" vs "their stack"**, not a clean allocator A/B. A
  third arm — rusty_dds + system allocator — isolates the allocator's contribution.
- `rusty_alloc` is 0.4.0, and 0.3.2-and-earlier were declared unsound by its own release
  notes. **Pin `>=0.4.0`**, and note that its release profile is `panic = "abort"`.

---

## 4. Determinism: the scenario replay

A "gameplay demo" that is actually a benchmark must do identical work in every arm.

- **Fixed timestep.** Simulation advances by a constant dt per frame. Nothing —
  camera, LOD selection, request issue, eviction — reads the wall clock. Rendering
  can miss its budget without changing what is simulated.
- **Camera trace.** A recorded spline per scenario, replayed frame-by-frame from
  `scenarios/*.json`. No input, no physics jitter.
- **Seeded request generator.** LOD/residency decisions are a pure function of
  (frame index, camera pose, tier, pool budget). Same seed ⇒ same request set.
- **Per-frame request hash.** Each frame emits `fnv(sorted(requested subresources))`.
  Two arms whose hash streams differ are **rejected**, not compared — the same reflex
  as the encode campaign's payload-FNV pass.
- **Uploaded-byte hash.** In Stream profile, `fnv(bytes handed to the GPU)` per frame
  must be **identical across stacks**. When it is, GPU-side parity is proved rather
  than argued — and that single check is what makes the whole demo credible.

### 4.1 Scenarios

| Scenario | Duration | What it stresses | Studio-recognisable as |
|---|---|---|---|
| `traverse` | 3 min | Continuous streaming at speed | Flying/driving across a landing zone |
| `arrival` | 20 s ×10 | Cold burst — full working set requested in ~2 s | Quantum-travel arrival / fast-travel pop-in |
| `hub` | 5 min | Dense unique-asset churn, high texture count | Crowded city interior |
| `soak` | 30 min | Fragmentation and RSS drift under 8 streaming threads | An actual play session |
| `chaos` | 3 min | Malformed/truncated DDS injected from the `fuzz/` corpus | Corrupt patch / bad disk |

`arrival` is the money shot: it is where residency-completeness diverges visibly, and
where the RDO payload reduction converts into on-screen sharpness at a fixed budget.

---

## 5. Measurement

### 5.1 Per-frame record (CSV, one row per frame per arm)

```
frame, sim_time, cpu_frame_ms, gpu_frame_ms, present_ms,
stream_cpu_ms, parse_ms, decode_ms, upload_ms,
bytes_read, bytes_uploaded, requests, resident_pct,
alloc_count, alloc_bytes, peak_rss_mb, vram_used_mb, vram_budget_mb,
request_hash, upload_hash, hitch
```

- **GPU time**: D3D11 timestamp + disjoint queries; Vulkan `vkCmdWriteTimestamp` with
  `timestampPeriod`. Both double-buffered so readback never stalls the frame.
- **VRAM**: `IDXGIAdapter3::QueryVideoMemoryInfo` / `VK_EXT_memory_budget`.
- **Working set**: `GetProcessMemoryInfo` sampled once per frame (cheap), plus
  `QueryWorkingSetEx` at scenario boundaries.
- **Allocator counters**: a counting `GlobalAlloc` shim wraps whichever allocator is
  active, so both Rust arms are instrumented identically. **Measure the shim's own tax
  with a null run** before trusting any allocation-side number.
- **Hitch**: `cpu_frame_ms > 2 × rolling_median`. Report count and worst case, never mean.

### 5.2 Run protocol

1. Warm-up scenario discarded (shader cache, page cache, driver JIT).
2. **ABBA interleaving across process launches**: `A B B A` per repetition, N ≥ 7
   repetitions, so drift and thermals hit both arms equally.
3. Report **median of per-run medians** for central tendency and **p99 / p99.9 pooled**
   for stability, with the **null band drawn on every chart**.
4. Both **cold-cache** (drop the standby list between runs) and **warm-cache** passes —
   the IO story only exists in the cold pass, and saying so is what makes it survive
   scrutiny.
5. CPU time per thread alongside wall time. On a loaded box CPU is the robust verdict;
   wall swung 2–3× during the encode campaign.
6. **Sanity gate before any board is published**: byte-identical paths must read their
   known standing. If Stream-profile `upload_hash` streams disagree, or if GPU frame
   time differs by more than the null band, the run is junk — investigate, do not report.

### 5.3 Controlled variables (the "everything necessary" checklist)

Pin these, log them into the run manifest, and refuse to compare runs whose manifests differ.

**Machine / OS**
GPU driver version · Windows build · power plan = High performance · CPU core parking off ·
SMT state · CPU affinity mask + process priority (the encode harness uses mask 60 + High) ·
Defender exclusions on the repo and pack dir (Defender spikes poisoned first runs all
campaign) · page file fixed · no background jobs (the campaign lost an afternoon of
boards to a stray batch job).

**GPU**
Clocks locked (`nvidia-smi -lgc` or vendor equivalent) · power/thermal limits logged ·
HAGS (hardware-accelerated GPU scheduling) state · Resizable BAR state · MUX/hybrid
graphics fixed to the dGPU.

**Present path**
V-sync **off**, `IMMEDIATE` present mode / DXGI flip model with tearing allowed ·
identical swapchain image and back-buffer counts · identical window size, borderless,
no compositor-path differences · driver frame pacing disabled.

**Renderer parity across APIs** — DX11 and Vulkan are a *second* variable and must not
smuggle in a third:
same shaders (one HLSL source → DXBC and → SPIR-V) · same sampler state, aniso level and
mip bias · same texture usage flags and `OPTIMAL` tiling · same upload-path shape
(DX11 `UpdateSubresource` / `Map(DISCARD)` on a dedicated deferred context vs Vulkan
staging ring + transfer queue), both documented and both fixed across stacks ·
same queue and thread counts.

**Content**
Same cooked pack (plus the separate DirectXTex-cooked comparison) · pack hashed into the
manifest · same eviction policy and pool budget · same residency target.

**Build**
Same rustc/MSVC versions · `lto = "thin"`, `codegen-units = 1`, symbols on for profiling ·
binary mtime + marker verified before every A/B (stale binaries have burned this
workspace before).

### 5.4 Acceptance criteria — what "the demo worked" means

On a quiet box, with the null band drawn:

- Stream profile: `upload_hash` identical across stacks on 100% of frames, **and**
  GPU frame time indistinguishable (inside the null band). *This is the credibility gate.*
- A measurable, outside-the-null-band improvement in **at least one** of: p99 frame time;
  hitch count on `traverse` / `arrival`; time-to-95%-residency on `arrival`; peak working
  set on `soak`.
- Cook profile reproduces the published boards (encode speed + PSNR + RDO payload).
- Transcode profile reproduces the decode board direction.
- `chaos` outcomes recorded factually for both arms.

If a tier/API cell shows **no** difference, the board says so. The 0.2.0 README already
names the three cases where DirectXTex is ahead; the simulator inherits that posture.
A demo that admits its null cells is the one a studio believes.

---

## 6. Phasing

**Phase 0 — harness before arms (the honesty gate). ✅ COMPLETE 2026-08-18.**
Crate scaffold, trace replay, metrics sink, request/upload hashing, CSV → board generator,
plus the Dioxus cockpit (added mid-phase at the user's direction — see §6.1).
Ran **A vs A only**.
*Exit met:* `sim verify` passes repeatability, thread-independence and arm-independence;
all 14 runs of the published board share one `trace_hash`; every metric reads *inside the
noise*; per-metric null bands published.

Phase 0 refuted three of its own designs before any second stack existed to be compared —
which is the entire argument for running the null arm first:

1. The first workload **measured nothing** (5 requests/frame, zero steady-state uploads,
   `resident_pct` pinned at 1.0). Deterministic, reproducible, empty.
2. A **fraction-of-pack pool budget cannot bind at any fraction** — pool and demand both
   scale with `N·S`, so the failure was scale-invariant. The budget is now derived from the
   scenario's own measured peak demand.
3. The **rolling-median hitch rule measured "did work happen"**, flagging 250 frames per
   1000, because the median frame was idle at timer resolution.

It also established that **pinning dominates everything else measured so far**: unpinned →
`0x3c` + high priority took run CPU from ±88% to ±39% and worst-frame from ±4151% to ±94%.

**Null band on the development box (24 cores, loaded, pinned), which no later phase may
report anything narrower than:** staging copy ±8.7% · median frame ±11.4% · parse ±13.5% ·
streaming CPU ±18.3% · p99 ±19.5% · p99.9 ±34.3% · worst frame ±94% · working set ±1.5%.
Allocation counts and uploaded bytes are bit-identical across arms.

**Phase 1 — D3D11 + rusty arm.**
`windows` crate device/swapchain, deferred-context upload, timestamp queries, HUD.
`rusty.rs` provider over `UploadPlan`. Stream profile, `traverse` scenario, one tier.
*Exit:* 3-minute run at stable frame pacing, residency telemetry sane.

**Phase 2 — DirectXTex arm.**
C ABI shim in `sim/shim/` extending
[tools/dxtex_decode_bench/CMakeLists.txt](../../tools/dxtex_decode_bench/CMakeLists.txt);
both peer paths (`DDSTextureLoader`-style and `ScratchImage`).
*Exit:* upload-hash parity with the rusty arm on 100% of Stream frames — the gate that
makes every later number defensible.

**Phase 3 — Vulkan backend.**
`ash` device, staging ring, transfer queue, timestamp queries, SPIR-V from the same
shader source.
*Exit:* DX11 and Vulkan agree on `upload_hash` and residency for the same trace; the
GPU-time delta between APIs is *reported as an API property*, not attributed to either stack.

**Phase 4 — tiers, scenarios, RDO ladder.**
Ultra/High/Medium packs cooked at λ = 0 / 4 / 10; `arrival`, `hub`, `soak`, `chaos`;
cold- and warm-cache passes.
*Exit:* full 12-arm matrix + nulls, ABBA, N ≥ 7.

**Phase 5 — allocator arm.**
`rusty_alloc` behind a feature; add the rusty_dds + system-allocator third arm to isolate
the allocator's own contribution. Soak-focused.
*Exit:* RSS/fragmentation curves over 30 min, allocation tail-latency histograms.

### 6.1 The cockpit (added during Phase 0)

The demo is a **headful desktop application**, built with Dioxus. One constraint drives its
architecture and must not be forgotten later: **Dioxus desktop on Windows is a WebView2
surface.** It cannot own a D3D11 or Vulkan swapchain, so it is not where GPU timings come
from. The split is therefore:

- **Cockpit (Dioxus)** — controls, live telemetry, gate status, and the demo surface.
- **Render viewport (native window)** — tao/winit + `windows` (D3D11, Phase 1) and `ash`
  (Vulkan, Phase 3), owning the swapchain and the timestamp queries.
- **Shared replay driver** — both front-ends drive the same `Sim`, so what the demo shows
  and what the board measures cannot drift apart.

The cockpit shares a process with a Chromium compositor, which competes for cores now and
for the GPU once a viewport exists. Live figures are therefore **indicative**; everything
reportable comes from detached, pinned `sim bench` runs with no UI in the process. The
cockpit's *Detached benchmark* button launches exactly that, and is the only path from the
window to a board.

**Phase 6 — presentation.**
Split-screen synchronised capture (both arms replaying the same trace side by side),
live HUD (p99, hitch count, resident %, VRAM, working set), and a
`docs/artifacts/simulator-<date>.md` board in the existing house format, reproducible
from the repo with one command.

---

## 7. Risks and how they are handled

| Risk | Handling |
|---|---|
| **Stream-profile delta is small and the demo looks weak** | Expected and planned for — it is why Transcode and Cook are first-class profiles, and why stability (p99 / hitch / RSS), not FPS, is the headline. Do not inflate Stream. |
| Frame-time noise wider than the effect | Null band on every chart; CPU time alongside wall; pinning + priority; refuse sub-null claims. This rule already killed a wall-clock claim in `rusty_alloc` — correctly. |
| Unfair DirectXTex peer (strawman) | Implement both runtime peer paths, name the one used per row, keep cook flags as the published boards set them, and invite the reader to swap in their own loader. |
| `rusty_alloc` 0.x instability | Pin `>=0.4.0`; keep the allocator behind a feature so it can be dropped without touching the DDS story; run `soak` before ever demoing it. |
| MSRV/edition clash (edition 2024 vs MSRV 1.73) | The sim is a separate, excluded crate with its own MSRV. The library's MSRV claim does not move. |
| API axis smuggling in a third variable | §5.3 renderer-parity list; shaders from one source; upload-path shapes documented and fixed. |
| Box under load producing junk boards | Manifest-gated runs, sanity gate before publication, Defender exclusions, and the standing campaign rule: don't kill the user's other jobs, just re-run later. |
| Claims outrunning evidence in `chaos` | Record observed outcomes only. No CVE rhetoric on a slide the harness did not measure. |

---

## 8. Open questions to settle before Phase 0

1. **Content.** Which pack? The proxy corpus (ambientCG + CryTIF + USC-SIPI) is enough
   to build the harness, but the studio-facing run needs their maps — the README already
   says the proxy corpus is not a studio asset pack. Plan a "drop your pack here" path.
2. **Geometry.** How much scene is needed to make the frame credible? Proposal: an
   instanced scene with a heavy material count. The textures are the subject; a
   complicated renderer only adds confounds.
3. **DX12 later?** The Vulkan arm carries the explicit-API story; DX12 would be a third
   backend behind the same `Renderer` trait if a studio asks.
4. **BC6H in Ultra.** Decode ships and mode-11 UF16 encode ships; SF16 encode does not —
   so an HDR sky in the Ultra tier must be cooked UF16 or supplied pre-cooked.
