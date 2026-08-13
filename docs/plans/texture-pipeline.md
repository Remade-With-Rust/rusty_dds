# Plan: Texture pipeline beyond the DDS container

Status: Phase 5 complete (productization); **Phase 6 encoder campaign (2026-08) in flight** — see §Phase 6; post-1.0 = BC6H / publish / quality RDO  
Scope: turn `rusty_dds` from a container envelope parser into a rendering-ready DDS texture stack  
Baseline: fork of [PistonDevelopers/ddsfile](https://github.com/PistonDevelopers/ddsfile) (container parse/compose only)

## 1. Problem

Stock `ddsfile` (and this fork today) answers: *“What is in this `.dds` file?”*

A conventional DDS-for-rendering stack also answers:

1. *“Give me GPU-uploadable compressed blocks (or RGBA) for mip N / layer L / face F.”*
2. *“Decode this surface to RGBA8 / RGBA16F when the GPU or CPU path needs uncompressed pixels.”*
3. *“Optionally encode RGBA → BC/DXT and write a valid `.dds`.”*

Until those exist, `rusty_dds` is not a drop-in competitor to full DDS texture libraries (`image_dds`, image-rs `dds`, DirectXTex-class tools).

## 2. Goals / non-goals

### Goals

- Keep and harden the existing **container** API (`Dds::read` / `write`, headers, layout metadata).
- Add a **surface model**: typed views into mip / array layer / cubemap face / volume slice.
- Add **decode** paths for the formats that matter for games and asset pipelines (BC1–BC7 first).
- Expose **GPU-ready descriptors**: format enum ↔ wgpu/Vulkan/DXGI mapping hints, row/slice pitches, subresource byte ranges — without depending on a specific graphics API.
- Land **correctness gates** (reference oracles) and **benches** before claiming performance wins.
- Stay **pure Rust** for core decode (no C/C++ codec deps in the default path).

### Non-goals (initially)

- Being a full asset conditioner / mipmap generator / texture tooling suite.
- Binding directly to OpenGL/Vulkan/wgpu in the core crate (optional thin adapters later).
- Bit-exact parity with every DirectXTex corner case on day one.
- ASTC / exotic YUV / video formats in phase 1.
- Replacing every consumer of `ddsfile` without an API-compat story.

## 3. Architecture sketch

Keep layers separable so container work stays usable alone:

```text
┌─────────────────────────────────────────────────────────┐
│  rusty_dds (crate, feature-gated modules)               │
│                                                         │
│  container   — Header / Header10 / Dds read-write       │
│  layout      — subresource ranges, pitches, sizes       │
│  surface     — SurfaceView { mip, layer, face, bytes }  │
│  decode      — Format → ImageRgba8 / ImageRgba16f       │
│  encode      — Image → blocks + header (later)          │
│  gpu_hints   — format maps, usage notes (no GPU deps)   │
└─────────────────────────────────────────────────────────┘
         ▲ optional adapters (later crates / features)
         │  rusty_dds_wgpu / examples only
```

Suggested Cargo features:

| Feature | Default | Contents |
|---------|---------|----------|
| `container` | yes | current `ddsfile`-compatible surface |
| `decode` | yes (once ready) | BC + common uncompressed → RGBA |
| `encode` | no | RGBA → BC (expensive; opt-in) |
| `rayon` | no | parallel decode of large mips/arrays |

Rename the package to `rusty_dds` when publishing is intentional; keep a `ddsfile`-compatible facade or re-exports during migration if needed.

## 4. Capability backlog (phased)

### Phase 0 — Harness and fixtures

- [x] Package renamed to `rusty_dds` (`Cargo.toml` / README / bins)
- [x] `tests/fixtures/` with representative `.dds` files (see `tests/fixtures/README.md`):
  - DXT1 / DXT5 (legacy FourCC)
  - BC1 / BC3 (DX10 header) — BC5 / BC7 goldens deferred to decode phase
  - Uncompressed `R8G8B8A8_UNorm` (+ mip chain); cubemap / 2D array deferred to Phase 1
- [x] Criterion **Parse A/B**: crates.io `ddsfile` `0.5.2` vs local `rusty_dds` (`benches/parse_ab.rs`)
- [ ] **Decode A/B** (once decode exists): vs `image_dds` and/or image-rs `dds`
- [x] Provenance: regenerate with `cargo run --example gen_fixtures` (pure Rust, no DirectXTex)

**Exit:** `cargo bench --bench parse_ab` and fixture load work on a clean checkout; parse A/B is wired even if numbers are noise while code is still forked 1:1.

### Phase 1 — Layout / surface API

Build on existing pitch / mip / array helpers already in the container:

- [x] `SubresourceId { mip, layer, face }` (+ `CubemapFace`) in `src/surface.rs`
- [x] `Dds::subresource_range(id) -> Result<Range<usize>>` into `data`
- [x] `Dds::surface` / `surface_mut` → `SurfaceView` / `SurfaceViewMut`
- [x] Validate mip/layer/face; `Error::TruncatedData` when payload is short
- [x] Fixtures: `rgba8_32x32_array3.dds`, `bc1_32x32_cube.dds`
- [x] Tests: `tests/surface.rs` (mip 0/last, array, cubemap, OOB, truncate, mut)

**Exit:** unit tests covering mip 0/last, array layer, cubemap face offsets; no silent OOB. ✅

### Phase 2 — Decode (CPU, pure Rust)

Priority order (coverage vs payoff):

1. [x] **BC1 / DXT1**, **BC3 / DXT5** (+ BC2/DXT3)
2. [x] **BC4 / BC5** (UNorm → R/RG expanded to RGBA8)
3. [x] **BC7** (via `bcdec_rs`, same core as `image_dds`)
4. [ ] **BC6H** — HDR (defer; needs `ImageRgba16f` / f32 path)
5. [x] **Uncompressed** — `R8G8B8A8_*`, `B8G8R8A8_*` (BGRA→RGBA)

API:

```rust
pub struct ImageRgba8 { pub width: u32, pub height: u32, pub pixels: Vec<u8> }
impl Dds {
    pub fn decode_rgba8(&self, id: SubresourceId) -> Result<ImageRgba8, Error>;
}
```

Correctness gates:

- [x] Known BC1/BC3 block → RGBA matches `bcdec_rs` bit-exact (`tests/decode.rs`)
- [x] BC7 random blocks bit-exact vs `bcdec_rs`
- [x] sRGB policy: **stored bytes, no linearization** (documented in `src/decode/mod.rs`)
- [x] Decode bench: `benches/decode_ab.rs` (`rusty_dds` vs raw `bcdec_rs` tiling)

**Exit:** BC1/BC3/BC7 + RGBA8 match oracle bit-exact. ✅ (BC6H deferred)

### Phase 2b — Decode completeness (gate before Phase 3)

Phase 2 landed the API; **Phase 3 must not start until the LDR decode matrix below is green.**
“Completeness” means every **content type** × every **context type** has (1) a fixture or
synthetic surface, (2) a bit-exact oracle test vs `bcdec_rs` (or identity for uncompressed),
and (3) a Criterion arm in `decode_ab`.

#### Content types (LDR → `ImageRgba8`)

| ID | Format | Status |
|----|--------|--------|
| C-BC1 | DXGI BC1_* / D3D DXT1 | ✅ |
| C-BC2 | DXGI BC2_* / D3D DXT2/DXT3 | ✅ |
| C-BC3 | DXGI BC3_* / D3D DXT4/DXT5 | ✅ |
| C-BC4U | DXGI BC4_UNorm / Typeless | ✅ |
| C-BC4S | DXGI BC4_SNorm | ✅ |
| C-BC5U | DXGI BC5_UNorm / Typeless | ✅ |
| C-BC5S | DXGI BC5_SNorm | ✅ |
| C-BC7 | DXGI BC7_* | ✅ |
| C-RGBA | DXGI R8G8B8A8_UNorm / _sRGB / Typeless / UInt | ✅ |
| C-BGRA | DXGI B8G8R8A8_UNorm / _sRGB / Typeless | ✅ |

**Out of this gate (documented deferrals):** BC6H / float HDR (`ImageRgbaf32`), exotic DXGI
(packed RGB10A2, etc.), D3D uncompressed bitmasks beyond what DXGI covers.

#### Context types

| ID | Context | Status |
|----|---------|--------|
| X-2D | Single 2D, mip 0 | ✅ |
| X-MIP | Mip ≠ 0 (incl. last / 1×1 tip) | ✅ |
| X-ARRAY | 2D texture array layer | ✅ |
| X-CUBE | Cubemap face | ✅ |
| X-NPOT | Width/height not multiple of 4 (partial block copy) | ✅ |
| X-VOL | Volume (`depth > 1`): decode all depth slices into `ImageRgba8 { depth }` | ✅ |

#### Exit criteria

- [x] Matrix tests: every C-* × applicable X-* (`tests/decode_matrix.rs`)
- [x] `cargo bench --bench decode_ab` covers every C-* at X-2D plus X-MIP / X-ARRAY / X-CUBE / X-VOL / X-NPOT arms
- [x] README “headline” states completeness honestly (what is gated vs deferred)
- [x] No silent OOB / truncated payload on any matrix cell

**Then** Phase 3 (GPU upload plans).

### Official peer artifact (Microsoft DirectXTex)

Not a Rust-crate A/B — decode/encode boards compare against
**[Microsoft DirectXTex](https://github.com/microsoft/DirectXTex)**. The C++ harness
is **local-only** (not shipped in this repo; see [`tools/README.md`](../../tools/README.md)).
Published results: [`docs/artifacts/`](../artifacts/).

Protocol: same `.dds` bytes → RGBA8 (`Dds::read`+`decode_rgba8` vs
`LoadFromDDSMemory`+`Decompress`/`Convert`).

### Phase 2c — Decode hot-path (BC5 / BC7 / tiling) ✅

- Stack scratch for 4×4 blocks (no per-call `Vec`).
- BC4/BC5 expand to RGBA while blitting (no full-image RG intermediate).
- Aligned surfaces: decode BCn straight into output pitch.
- BC7: `std::thread` strip parallelism when block count ≥ 4096 (e.g. ≥256×256 POT);
  smaller surfaces stay sequential (spawn cost dominated the 32×32 bench).
- Oracle gate unchanged: bit-exact vs `reference` / `bcdec_rs` (`tests/decode_matrix.rs`).

### Phase 3 — GPU-ready (API-agnostic) ✅

- `GpuFormat`: DXGI ↔ wgpu / Vulkan format **names** (string data, no API deps).
- `UploadPlan`: subresource `data_offset` / `data_len`, `bytes_per_row`, `rows_per_image`.
- Path A: `upload_plan_compressed` — pass through DDS block bytes.
- Path B: `upload_plan_decoded_rgba8` — after `decode_rgba8` (web / limited GPU fallback).
- Tests: `tests/upload_plan.rs`.

Optional later: `examples/wgpu_quad` behind a feature, not in the core dependency graph.

**Exit:** unit tests that pitches match wgpu/Vulkan tightly packed / BCn block-row rules. ✅

### Phase 4 — Encode (pure Rust) ✅

Same **content** and **context** matrix as Phase 2b decode (`DecodeContent::ALL_LDR` ×
X-2D / X-MIP / X-ARRAY / X-CUBE / X-NPOT / X-VOL).

- API: `EncodeLayout` + `Dds::encode_from_rgba8` / `ImageRgba8::encode_dds`
- BC1–BC5 U/S + BC7 (mode 6) + RGBA/BGRA; mips via box filter
- Round-trip gate: bit-exact uncompressed; PSNR floors for BCn (`tests/encode_matrix.rs`)
- Quality notes: `docs/artifacts/encode-quality.md`

**Exit:** encode builds; round-trip matrix green; quality notes published. ✅

### Phase 5 — Productization ✅

- Package `rusty_dds` `0.1.0`, MSRV **1.73**, `exclude` for third_party build trees.
- Features: `default = ["decode", "encode"]`; `bcdec_rs` optional behind `decode`.
- CLI: `rusty-dds info|decode|encode|retag` (`ddsinfo` / `retag` deprecation shims).
- Docs: README as full toolkit; [formats.md](../formats.md) catalog;
  [migration-ddsfile.md](../migration-ddsfile.md) — **no** `ddsfile` type facade.
- Unsupported formats fail closed via `Error::UnsupportedFormat`.

**Exit:** feature matrix builds; CLI works; docs match shipping surface. ✅

### Phase 6 — Encoder speed+quality campaign (2026-08, mandate: both axes, no trades)

Gate harness: `examples/bench_encode_corpus.rs` (PNG corpus + 16 CryTIF from
CRYTEK/GameSDK + 10 USC-SIPI TIFFs; per-case round-trip PSNR + payload FNV in a
deterministic pass, best-of-N timing separately) + `bench/ab_encode.ps1`
(ABBA-interleaved, core-pinned, CPU-time verdicts).

| Brick | Type | Result |
|---|---|---|
| BC7 palette precompute + fused SSE + seed dedup + interior gather | speed, byte-identical | **BC7 2.05×** (10/10 pairs, z=3.16; CPU 37→18 s) |
| BC1 PCA seed + iterated LS refine | quality, monotone | Bricks +1.65 / Rock +1.41 / Wood +0.57 dB — all three DXT losses flip to wins |
| BC1 inverted-565 mode fix | quality (latent bug) | stored c0>c1 decodes 4-color; old code fitted a 3-color palette there |
| BC1 fused pack+score with early abort | speed, byte-identical (oracle: 200k-block twin test) | pays for the signed window |
| BC4S/BC5S ±4 windowed endpoint sweep (harvest-gated: span 8–32, err>4) | quality, monotone | Wood BC5S 52.18→52.60 (DXT 52.69 = tie band); all signed cases +0.4–0.6 dB |
| BC3 alpha: full BC4-grade search replaces min/max-only | quality, monotone | (gating in flight) |

Method notes: signed-sweep gate tuned from a 643k-block observe-only harvest
(`signed_sweep_harvest` ignored test); full-span sweep ceiling kept in-tree as
the `bc5s_wood_ceiling` oracle. Iterated-LS-alone was REFUTED for the Wood gap
(+0.000 dB — the LS fixed point is the local optimum; the win is discrete
UNORM-lattice search near the incumbent).

#### Phase 6b follow-ups (second pass, same session)

| Brick | Type | Result |
|---|---|---|
| BC7 projection-window index fit (±2 around the pixel's axis projection; exhaustive fallback for near-degenerate palettes) | speed, quality-gated | BC7 total 1122→935 ms pinned (**1.20×**); 0/102 corpus cases move at 0.0001 dB; contract oracle over 400k adversarial cases |
| Unsigned BC4/BC5 ±4 window (same gate + prune as signed) | quality → **demoted to opt-in** (`RUSTY_DDS_BC45U_WINDOW=1`) | +0.15..0.45 dB × 14 already-winning cases for ~3.2s corpus CPU — traded out to fund the lattice + mode 5 (2–20× the gain/s) |
| err>16 tight unsigned gate | REVERTED | kept only 10–45% of smooth-map gains (they live at err 5..16) |
| **BC1 565-lattice contract refine** | quality, monotone | My "marginal EV" estimate was WRONG (user called it): full ±1 window = +0.05..+1.04 dB × 45 cases. Harvest (1.3M wins): ~82% of gain is INTERVAL CONTRACTION — ships contract-only ≤3 hill-climb rounds keeping 78–118% at half cost. Wood bc1 +0.82, Bricks +0.34 |
| **BC7 mode 5** (rotation 0, decoupled color/alpha indices) | quality, monotone | Alpha-gradient CryTIF: PC_OnFoot **+9.63 dB**, PC_Vehicle +9.04, HardLeft +6.34; trialed on alpha-varying blocks, picked by same RGBA SSE |
| Alpha threshold selector (static per-mode sorted order → 7 threshold compares) | speed, byte-identical **proven by exhaustion** (all 16.7M endpoint×sample combos) | CPU-neutral in situ (early-aborts pay the build); kept for exactness + future wider windows |
| BC1 projection index fit (4-color) | tried, DISABLED in place | 4→3 SSE evals saved ~nothing, cost ≤0.012 dB — the BC7 win (16→5) doesn't scale down |

**Phase 6c — "win all 4" round (user-directed):**

| Brick | Result |
|---|---|
| BC7 mode 5 rotations 1–3 | all 20 bc7 cases up (+0.2..+1.3 dB — surprise winner on OPAQUE content: the odd-gradient color channel earns its own index set) |
| BC7 mode 4 (3-bit alpha idx) | HardLeft +1.42, computer_key +0.90 on top of mode 5 |
| BC7 mode 1 (2-subset, 8-shape harvest shortlist) | startscreen **+13.53 dB**, bumpSign +2.67; three refuted gates recorded (err floors kill the smooth-block wins; shape 2 = 83% of all gain) |
| AVX2 fit kernels (`simd` feature) | exact vs scalar (200k-case oracles), identical bytes on every CPU; whole stack **1.17× less CPU than 0.1.2** with everything on |
| BC6H decode | `decode_rgba_f32` + `ImageRgbaF32`, full context matrix vs bcdec oracle; encode remains the one deferred item |

**Final Phase 6 state vs 0.1.2:** 89/102 corpus cases better PSNR / 0 worse;
CPU 6/6 ABBA pairs ~1.17× less machine — with two-subset + decoupled-alpha
BC7, the BC1 lattice, the signed window, and BC6H decode all shipped.

**Board status:** `corpus-vs-directxtex` was regenerated clean earlier in the
campaign (sanity gate: byte-identical bc4u/bc5u read their known 0.33–0.40
standing). The post-follow-up refresh is PENDING a calm box — the 2026-08-12
afternoon attempts were poisoned by a standing `ocr_batch` + IDE load at 100%
CPU (physically impossible ratios: bc7 "loss" at 300× advantage). Expected
honest deltas when re-run: bc7 ratios improve ~1.2×, bc4u/bc5u move from
~0.33–0.40 toward ~0.85–1.0 (quality purchase, same trade already named for
the signed formats).

#### Deferred items, re-scoped by this campaign

- **BC6H / float HDR** (the remaining format-matrix hole): needs `ImageRgbaf32`
  (decode) + half-float endpoint machinery (encode). Decode first — oracle is
  `bcdec_rs::bc6h_float` (already a dep) — then encode mode-11-only (the
  BC7-mode-6 analog: single subset, 16-bit endpoints, 4-bit indices, no
  partitions) gated by the same round-trip + corpus discipline. The corpus
  needs real HDR sources (EXR/HDR → float TIFF); acquisition parallels the
  CryTIF hunt. Sized at a full session; not attempted inside this campaign.
- **Quality RDO** (rate-distortion for BC7 mode selection) becomes relevant
  only when more BC7 modes exist; mode 6 alone has no rate axis. Blocked on
  multi-mode BC7, which the speed headroom from this campaign now affords.
- **BC1 true punch-through** (c0<=c1 3-color mode for alpha<128 content):
  the campaign's mode-fix revealed the encoder never deliberately emits
  3-color mode; BC1A content currently loses its transparency. Small brick,
  needs an alpha-aware seed pass + the punch SSE accounting already present.

## 5. Competitive baselines ✅

| Peer | Role |
|------|------|
| **Microsoft DirectXTex** | Sole competitive peer — decode (`Decompress`/`Convert`) + encode (`Compress` / `TEX_COMPRESS_BC7_QUICK`) |
| crates.io `ddsfile` | Parse A/B only (Criterion `parse_ab`; container lineage, not a speed rival) |

**Harness:** `cargo run --release --example bench_baselines` /
`harvest_corpus_*` (optional local DirectXTex peer; not shipped in-tree).

- Every `DecodeContent` × every context (`X-2D` / `X-MIP` / `X-ARRAY` / `X-CUBE` /
  `X-NPOT` / `X-VOL`), single-surface.
- Artifacts: [decode-vs-baselines](../artifacts/decode-vs-baselines.md) ·
  [encode-vs-baselines](../artifacts/encode-vs-baselines.md)

Strategy: **win on API clarity + pure-Rust default + measurable parity with DirectXTex**, not on claiming every format on day one.

## 6. Correctness and performance discipline

1. **Fixtures before speed claims.** No decode bench without an oracle gate on the same files.
2. **One capability brick per land.** e.g. “BC1 decode + tests” before “SIMD BC1.”
3. **Measure with Criterion**; pin process when comparing; preload bytes so disk I/O is not the story.
4. **Profile before micro-optimizing**; BC7 will dominate wall time long before parse does.
5. **Fail closed** on unknown formats and truncated subresources — never return partial silent garbage.

## 7. Suggested first implementation slice

Smallest useful vertical after Phase 0:

1. `SubresourceId` + `surface()` + range tests  
2. BC1 decode → `ImageRgba8` with 4×4 block unit tests + one real fixture vs `image_dds`  
3. Criterion: parse A/B + BC1 decode vs `image_dds`  
4. Doc update: “what we are / are not yet”

That proves the layering and the measurement story before investing in BC7/encode.

## 8. Open decisions

- ~~Package name timeline (`ddsfile` compat vs hard rename).~~ → **`rusty_dds`**; no facade (see `docs/migration-ddsfile.md`).
- ~~Whether `decode` is default-on.~~ → **yes** (`encode` too); opt out via `default-features = false`.
- Bit-exact vs tolerance for BC7 (some decoders differ on edge modes).
- Whether cubemap faces are stored in DirectX face order only, or also expose GL-facing remaps.
- wgpu example in-tree vs separate repo.
- crates.io publish when Remade org remote is ready.

## 9. Success criteria

`rusty_dds` is “rendering-ready” when a consumer can:

1. `Dds::read` a common game DDS,  
2. select mip/layer/face,  
3. either upload compressed blocks with a correct pitch plan **or** `decode_rgba8`,  
4. optionally `encode_from_rgba8` round-trip on the LDR matrix,  
5. trust automated oracle / PSNR tests + published benches vs DirectXTex / `ddsfile`.

**Phase 0–5 exit this bar for LDR.** Remaining: BC6H, crates.io publish, optional wgpu example.
