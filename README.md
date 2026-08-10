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

> **Status — 0.1 / pre-1.0, Phase 5 (productization).** LDR decode/encode matrix
> green (BC1–BC5 U/S, BC7, RGBA/BGRA × 2D/mips/array/cube/NPOT/volume). Features:
> `decode` + `encode` (default on). **Deferred:** BC6H / float HDR. Catalog:
> [docs/formats.md](docs/formats.md).

---

## The headline

> **Measured honestly vs Microsoft DirectXTex** on an ambientCG CC0 proxy corpus
> (~1024² albedo / normal / mask → BC1 / BC4 / BC5 / BC7). Where we win, we show
> it; where DirectXTex wins, we name it. Reproduce with `harvest_corpus_*`.

| Board (24 cases) | rusty_dds vs DirectXTex | Artifact |
|---|---|---|
| **Encode speed** | **24 ahead / 0 behind** | [encode-vs-baselines](docs/artifacts/encode-vs-baselines.md) |
| **Encode quality (PSNR)** | **16 higher / 3 lower / 5 tie** (±0.25 dB) | [encode-quality-vs-directxtex](docs/artifacts/encode-quality-vs-directxtex.md) |
| **Decode speed** | **24 ahead / 0 behind** | [decode-vs-baselines](docs/artifacts/decode-vs-baselines.md) |
| Combined cook table | speed + PSNR per map | [corpus-vs-directxtex](docs/artifacts/corpus-vs-directxtex.md) |

Notes on those numbers:

- Peer encode flag for BC7: `TEX_COMPRESS_BC7_QUICK` (mode-6 class, matches our encoder).
- rusty encode uses strip parallelism at ≥4096 blocks; DirectXTex peer is
  `TEX_COMPRESS_DEFAULT` (no `TEX_COMPRESS_PARALLEL`).
- Quality losses we still call out: Bricks/Rock **BC1**, Wood **BC5S**.
- Proxy corpus is **not** a studio asset pack — drop in your maps for the real gate.

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

# Corpus bake-off vs DirectXTex (needs tools/dxtex_decode_bench built)
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
- **Encode** — Same matrix in; mips via box filter; BC7 mode-6; `EncodeQuality::{Quality,Fast}`.
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
| BC6H / float HDR | ⏳ deferred |

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

→ **[github.com/Remade-With-Rust/rusty_dds](https://github.com/Remade-With-Rust/rusty_dds)**

## License

MIT — [LICENSE-MIT](LICENSE-MIT). Upstream copyright (c) 2018 Michael Dilger and
`ddsfile` contributors retained.

## Trademark

"Remade With Rust", "Mata", and "Mata Network" are marks of Mata Network. DDS /
DirectDraw / DirectX are marks of their respective owners; this project is not
affiliated with Microsoft.
