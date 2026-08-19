# Changelog

All notable changes to `rusty_dds`. Dates are release dates; every performance
figure is reproducible from the repo with the command given beside it.

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
