# Changelog

All notable changes to `rusty_dds`. Dates are release dates; every performance
figure is reproducible from the repo with the command given beside it.

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
