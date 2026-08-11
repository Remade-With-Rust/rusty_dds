# Plan: Texture pipeline beyond the DDS container

Status: Phase 5 complete (productization); post-1.0 = BC6H / publish / quality RDO  
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
