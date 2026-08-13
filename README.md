# rusty_dds

[![Remade With Rust](https://img.shields.io/badge/Remade%20With-Rust-000?logo=rust&logoColor=fff)](https://github.com/Remade-With-Rust)
[![By Mata Network](https://img.shields.io/badge/by-Mata%20Network-5b2be0)](https://www.mata.network)
[![Crates.io](https://img.shields.io/crates/v/rusty_dds.svg)](https://crates.io/crates/rusty_dds)
[![Docs.rs](https://img.shields.io/docsrs/rusty_dds)](https://docs.rs/rusty_dds)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE-MIT)
![Platforms: Windows · macOS · Linux · Web](https://img.shields.io/badge/platforms-Windows%20%C2%B7%20macOS%20%C2%B7%20Linux%20%C2%B7%20Web-informational)
![MSRV: 1.73](https://img.shields.io/badge/MSRV-1.73-informational)

> **rusty_dds** is a memory-safe DirectDraw Surface (`.dds`) texture toolkit —
> **container + decode + encode + GPU upload plans** — pure Rust for game and
> asset pipelines that need DDS without a C/C++ DirectXTex stack. Container
> lineage: MIT [ddsfile](https://github.com/PistonDevelopers/ddsfile).

> **Status — 0.1 / pre-1.0, Phase 6 (encoder speed+quality campaign, 2026-08).**
> LDR decode/encode matrix green (BC1–BC5 U/S, BC7, RGBA/BGRA ×
> 2D/mips/array/cube/NPOT/volume). Encoder rebuilt for Pareto wins: BC7 2×
> faster byte-identical; BC1/BC3-alpha/BC4S/BC5S quality up 65/102 corpus cases
> with zero regressions. Features: `decode` + `encode` (default on).
> BC6H: decode + UF16 mode-11 encode shipped (SF16 encode deferred). Catalog: [docs/formats.md](docs/formats.md).

---

## The headline

> **Measured honestly vs Microsoft DirectXTex** on an ambientCG CC0 proxy corpus
> (~1024² albedo / normal / mask → BC1 / BC4 / BC5 / BC7). Where we win, we show
> it; where DirectXTex wins, we name it. Reproduce with `harvest_corpus_*`.

| Board (24 cases) | rusty_dds vs DirectXTex | Artifact |
|---|---|---|
| **Encode quality (PSNR)** | **22 higher / 2 tie / 0 lower** (±0.25 dB) † | [corpus-vs-directxtex](docs/artifacts/corpus-vs-directxtex.md) |
| **Encode speed** | **21 ahead / 3 behind** (behind = 3 signed cases at ~1.10×) ‡ | [corpus-vs-directxtex](docs/artifacts/corpus-vs-directxtex.md) |
| **Decode speed** | **24 ahead / 0 behind** | [decode-vs-baselines](docs/artifacts/decode-vs-baselines.md) |
| Synthetic C×X quality grid | 0 DirectXTex-higher cases | [encode-quality-vs-directxtex](docs/artifacts/encode-quality-vs-directxtex.md) |

† Quality is deterministic per source, so it is valid on any machine; this row is a
re-measure taken after the BC7 mode-1/4/5 and BC1 lattice bricks, which postdate the
committed board file. ‡ Speed row is the last **sanity-gated** board run (a run is
rejected when byte-identical paths drift from their known standing); a refresh is
pending a quiet machine.

Notes on those numbers (2026-08 encoder campaign):

- Peer encode flag for BC7: `TEX_COMPRESS_BC7_QUICK` (mode-6 class, matches our encoder).
- rusty encode uses strip parallelism at ≥4096 blocks; DirectXTex peer is
  `TEX_COMPRESS_DEFAULT` (no `TEX_COMPRESS_PARALLEL`).
- Every 0.1 quality loss (Bricks/Rock **BC1**, Wood **BC5S**) is erased: BC1
  gained +0.5..+1.6 dB from a PCA seed + iterated least-squares refine, and the
  signed BC4/BC5 window search took Wood BC5S to the ±0.25 dB tie band.
- The trade we name in return: BC4S/BC5S spend most of their former ~3× speed
  headroom on that quality — 3 signed cases now run ~1.10× behind DirectXTex
  (each +0.5..+0.7 dB higher PSNR there, or a tie). BC7 encode is 2× faster
  than 0.1 (~300× vs DirectXTex-QUICK on this box).
- Additional real-content gate in-tree: 16 CryTIF `.tif` (CRYTEK GameSDK) +
  10 USC-SIPI TIFFs via `bench_encode_corpus` — BC3 alpha search alone was
  worth +1.8..+3.2 dB on the CryTIF set. 65 of 102 cases improved vs 0.1,
  zero regressed, while whole-corpus encode CPU dropped ~1.2×.
- Proxy corpus is **not** a studio asset pack — drop in your maps for the real gate.

## Performance snapshot

Full per-case ledger (102 cases × PSNR + pinned wall) reproduces from the repo:

```sh
cargo run --release --example bench_encode_corpus   # quality + speed, per case
cargo run --release --example harvest_rdo           # rate/quality ladder (deflate)
powershell -File bench/ab_encode.ps1                # pinned ABBA A/B harness
```

**Encoder vs the 0.1.2 release** — real-content corpus (ambientCG PNG + 16 CryTIF from
CRYTEK GameSDK + 10 USC-SIPI TIFF), pinned, interleaved:

| | 0.1.2 | now |
|---|---|---|
| Cases with higher PSNR | — | **89 / 102** (0 regressed) |
| Whole-corpus encode CPU | 44.6–45.6 s | **38.0–39.0 s** (~1.17× less, 6/6 pairs, zero overlap) |
| BC7 wall (pinned min, 25 cases) | 2364 ms | **1167 ms** (2.03×) |
| Largest single-case gain | — | **+13.53 dB** (BC7, UI startscreen) |

**Rate-distortion optimization** (opt-in, `λ=0` is byte-identical — verified by payload
hash on all 102 cases). Rate is *measured*: payloads deflated at level 8, the same channel
a zip-based game archive uses.

| Format | λ | Compressed size | Quality |
|---|---|---|---|
| BC1 | 25 | **−7.1%** | **+0.17 dB** |
| BC1 | 50 | **−10.4%** | **+0.11 dB** |
| BC1 | 100 | −15.6% | −0.07 dB |
| BC7 | 4 | −2.3% | **+0.03 dB** (all 30 maps smaller) |
| BC7 | 10 | **−3.9%** | **+0.02 dB** |
| BC7 | 50 | −15.1% | −0.31 dB |

Candidates are always legal BCn blocks, so conformance is free — RDO changes only the
rate/quality point, never decodability. Cost is ~3.5× encode on affected formats, cook-time
only. Distribution matters more than the mean here: at BC7 λ=4 every map shrinks, 26 of 30
improve and 4 land within 0.003 dB; per-block damage is scaled by the error a block already
carries, and exact blocks are never touched.

**BC6H** (`decode_rgba_f32` + `encode_bc6h_uf16`, mode 11): Polyhaven CC0 HDRIs round-trip at
**48.0–56.6 dB** log-PSNR, 3.1–9.4 Mpx/s encode.

| Dimension | Conventional DDS stacks | **rusty_dds (Rust)** |
|---|:---:|:---:|
| Memory-safety (core path) | C/C++ tools historically CVE-prone | **safe Rust** |
| Role | container + decode + GPU glue | **container + decode + encode + upload plan** |
| Pure Rust default | often C DirectXTex / vendor SDK | **yes** (`bcdec_rs` decode; in-house encode; no `*-sys`) |
| GPU | API-tied helpers | **API-agnostic** `UploadPlan` + DXGI / wgpu / Vulkan names |
| License + embedding | mixed | **MIT** |

---

## Install

```toml
rusty_dds = "0.1"
# decode-only (e.g. WASM loaders):
# rusty_dds = { version = "0.1", default-features = false, features = ["decode"] }
```

| Feature | Default | Provides |
|---------|---------|----------|
| `decode` | yes | `decode_rgba8`, `bcdec_rs` |
| `encode` | yes | `encode_from_rgba8`, `EncodeLayout`, `EncodeQuality` |

Always on: container R/W, `SubresourceId` / `surface()`, `decode_content()`,
`UploadPlan` / `GpuFormat`.

MSRV: **1.73**. Migrating from `ddsfile`: [docs/migration-ddsfile.md](docs/migration-ddsfile.md).

## Quick start

```rust
use rusty_dds::{Dds, EncodeLayout, DecodeContent, SubresourceId, UploadPath};
use std::fs::File;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut file = File::open("texture.dds")?;
    let dds = Dds::read(&mut file)?;
    let id = SubresourceId::mip_layer(0, 0);

    let plan = dds.upload_plan_compressed(id)?;
    assert_eq!(plan.path, UploadPath::Compressed);
    let _blocks = &dds.data[plan.data_offset..plan.data_offset + plan.data_len];

    let img = dds.decode_rgba8(id)?;
    let roundtrip = Dds::encode_from_rgba8(
        &img.pixels,
        EncodeLayout::flat_2d(DecodeContent::Bc7, img.width, img.height),
    )?;
    let _ = roundtrip;
    Ok(())
}
```

```sh
# Unified CLI
cargo run --bin rusty-dds -- info path/to/file.dds
cargo run --bin rusty-dds -- decode path/to/file.dds -o out.rgba
cargo run --bin rusty-dds -- encode --width 64 --height 64 --format bc7 pixels.rgba -o out.dds

# Corpus bake-off vs DirectXTex (optional local harness; not in this repo)
python corpus/fetch_ambientcg.py   # once
cargo run --release --example harvest_corpus_vs_dxtex
cargo run --release --example harvest_corpus_decode_vs_dxtex

# Matrices + fixtures
cargo test --tests
cargo run --example gen_fixtures
cargo bench --bench parse_ab
cargo bench --bench decode_ab
```

## Features

- **Container** — Magic, `Header` / `Header10`, D3D + DXGI formats, mips/array/cube/volume.
- **Surfaces** — Typed `SubresourceId`, fail-closed ranges, cubemap helpers.
- **Decode** — LDR matrix → `ImageRgba8` (sRGB = stored bytes). Oracle: `bcdec_rs`.
- **Encode** — Same matrix in; mips via box filter; BC7 modes 1/4/5/6; `EncodeQuality::{Quality,Fast}`.
- **RDO** — Opt-in rate-distortion optimization for BC1/BC7 (`RUSTY_DDS_RDO_LAMBDA`):
  smaller *compressed* payloads at parity-or-better quality; `λ=0` is byte-identical.
- **HDR** — BC6H decode (`decode_rgba_f32`) + UF16 mode-11 encode (`encode_bc6h_uf16`).
  Notes: [docs/artifacts/encode-quality.md](docs/artifacts/encode-quality.md).
- **GPU plans** — `upload_plan_compressed` / `upload_plan_decoded_rgba8` (Vulkan / wgpu / DXGI
  names; no graphics API dependency).
- **CLI** — `rusty-dds info|decode|encode|retag` (`ddsinfo` / `retag` kept as shims).
- **Measured** — ambientCG corpus boards + Criterion parse/decode A/B.

### Capability table

| Capability | Status |
|---|---|
| D3D9 FourCC + DX10 DXGI container | ✅ |
| `SubresourceId` / `surface()` | ✅ |
| LDR decode (BC1–5 U/S, BC7, RGBA/BGRA) × contexts | ✅ |
| GPU upload plan (API-agnostic) | ✅ |
| LDR encode (same matrix) | ✅ |
| Feature-gated `decode` / `encode` | ✅ Phase 5 |
| BC6H / float HDR | ✅ decode (`decode_rgba_f32`) · ✅ encode UF16 mode-11 (`encode_bc6h_uf16`) |

## Architecture

```text
┌─────────────────────────────────────────────────────────┐
│  rusty_dds                                              │
│                                                         │
│  container   — Header / Header10 / Dds read-write  ✅   │
│  content     — DecodeContent classification        ✅   │
│  surface     — SubresourceId / SurfaceView         ✅   │
│  decode      — → ImageRgba8          [feature]     ✅   │
│  encode      — ImageRgba8 → DDS      [feature]     ✅   │
│  upload      — UploadPlan / GpuFormat              ✅   │
└─────────────────────────────────────────────────────────┘
```

Plan: [docs/plans/texture-pipeline.md](docs/plans/texture-pipeline.md) ·
Formats: [docs/formats.md](docs/formats.md).

## Platform support

| Platform | Status |
|---|---|
| Windows / macOS / Linux | ✅ |
| Web (WASM) | 🎯 decode-only feature graph (no C deps) |

## Remade With Rust

**Remade With Rust** ([Mata Network](https://www.mata.network)) rebuilds essential
C/C++ tools in Rust — memory safety, predictable performance, permissive license.

Part of the same family as **[remade_ffmpeg_rs](https://github.com/Remade-With-Rust/remade_ffmpeg_rs)** —
FFmpeg remade in Rust for memory-safe encode/decode and the broader Remade media stack
(H.264, AV1, Opus, PNG/JPEG, and more).

→ **[github.com/Remade-With-Rust/rusty_dds](https://github.com/Remade-With-Rust/rusty_dds)** ·
**[github.com/Remade-With-Rust/remade_ffmpeg_rs](https://github.com/Remade-With-Rust/remade_ffmpeg_rs)**

## License

MIT — [LICENSE-MIT](LICENSE-MIT). Upstream copyright (c) 2018 Michael Dilger and
`ddsfile` contributors retained.

## Trademark

"Remade With Rust", "Mata", and "Mata Network" are marks of Mata Network. DDS /
DirectDraw / DirectX are marks of their respective owners; this project is not
affiliated with Microsoft.
