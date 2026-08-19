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

> **Status — 0.3 / pre-1.0. Runtime campaign complete (2026-08); encoder campaign complete (2026-08).**
> LDR decode/encode matrix green (BC1–BC5 U/S, BC7, RGBA/BGRA ×
> 2D/mips/array/cube/NPOT/volume). Encoder rebuilt for Pareto wins: **89 of 102
> corpus cases higher PSNR, 0 regressed, ~1.17× less CPU** than 0.1.2, plus
> opt-in RDO for smaller shipped payloads. **0.3 adds a zero-copy parse
> ([`DdsView`](#streaming-zero-copy-and-your-threads)) and decode into caller
> memory and caller threads** — measured against Microsoft DirectXTex in a
> streaming simulator. Features: `decode` + `encode` (default on).
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
  worth +1.8..+3.2 dB on the CryTIF set. 89 of 102 cases improved vs 0.1.2,
  zero regressed, while whole-corpus encode CPU dropped ~1.17×.
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

**BC7 encode** — 512², forced serial, process-pinned, **process CPU time**, 14
paired samples with the leading arm alternated, against 0.3.30. Byte-identical
throughout. Two fixtures, because the default one never enters BC7 mode 4 at all
(its alpha varies by under one code across a 4-pixel span, so the mode's gate
never fires) — a fixture that never enters a mode cannot measure a change to it:

| fixture | 0.3.30 | now | verdict |
|---|---|---|---|
| alpha-structured (modes 4 + 5 both run) | 83.6123 ms | **30.9710 ms** | 14/14, z = +3.74, **+63.0%** (2.70×) |
| default | 33.5752 ms | **22.8795 ms** | 14/14, z = +3.74, **+31.9%** (1.47×) |

```sh
PROBE_FMT=bc7 PROBE_ALPHA=1 cargo run --release --example probe_encode_serial --manifest-path sim/Cargo.toml
```

**Block decode** — every BCn decoder is now vectorised. 512², process-pinned,
**process CPU time**, 12 paired samples per format with the leading arm alternated,
against 0.3.27. All byte-identical; each kernel is oracle-tested against the scalar
twin it replaces.

| Format | 0.3.27 | now | verdict |
|---|---|---|---|
| BC1 | 0.1445 ms | **0.0878 ms** | 12/12, z = +3.46, **+39.2%** |
| BC2 | 0.2471 ms | **0.1019 ms** | 12/12, z = +3.46, **+58.7%** |
| BC3 | 0.2894 ms | **0.1605 ms** | 12/12, z = +3.46, **+44.5%** |
| BC4 | 0.1393 ms | **0.1104 ms** | 12/12, z = +3.46, **+20.8%** |
| BC5 | 0.1551 ms | 0.1547 ms | untouched — flat at z = +0.33 |
| BC6H | 0.7129 ms | **0.5046 ms** | 12/12, z = +3.46, **+29.2%** |

```sh
DEC_FMT=bc1 cargo run --release --example probe_dec --manifest-path sim/Cargo.toml
```

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
| Memory-safety (core path) | C/C++ tools historically CVE-prone | **safe Rust** — `forbid(unsafe_code)` without `simd`; with it, `unsafe` is confined to oracle-gated AVX2 kernels |
| Role | container + decode + GPU glue | **container + decode + encode + upload plan** |
| Pure Rust default | often C DirectXTex / vendor SDK | **yes** (`bcdec_rs` decode; in-house encode; no `*-sys`) |
| GPU | API-tied helpers | **API-agnostic** `UploadPlan` + DXGI / wgpu / Vulkan names |
| License + embedding | mixed | **MIT** |

---

## Streaming: zero copy, and your threads

A game does not decode textures; it *streams* them. 0.3 rebuilds that path around
one rule: **rusty_dds allocates nothing you did not ask for, and owns no threads.**

```rust
use rusty_dds::{DdsView, SubresourceId};

// You already hold the bytes — mmap, archive, fs::read. Nothing is copied.
let dds = DdsView::parse(&file_bytes)?;
let plan = dds.upload_plan_compressed(SubresourceId::mip_layer(0, 0))?;

// Or read from anywhere into a buffer you recycle.
let mut buf = Vec::new();
let dds = DdsView::read_into(reader, &mut buf)?;

// Decode into your buffer, split across your job system.
let mut pixels = Vec::new();
dds.decode_rgba8_into(id, &mut pixels)?;
dds.decode_block_rows_into(id, 0..dds.block_rows(id)?, &mut pixels)?;

// HDR has the same two seams — and needs them most. BC6H is the priciest
// format we ship: 26.4 ms serial at 1024^2, 2.7 ms across 24 of your threads.
let mut hdr = Vec::new();
dds.decode_rgba_f32_into(id, &mut hdr)?;
dds.decode_block_rows_f32_into(id, 0..dds.block_rows_f32(id)?, &mut hdr)?;
```

`Dds` still owns its payload and every existing call is unchanged — it is now an
alias for `DdsBase<Vec<u8>>`, with `DdsView<'a> = DdsBase<&'a [u8]>` sharing one
implementation.

**Measured against Microsoft DirectXTex** in [`sim/`](sim/) — a deterministic
texture-streaming replay on D3D11 and Vulkan, pinned, ABBA-interleaved, N=7,
1024² BC7/BC1/BC5/BC4, 192 textures, 10 500 frames. Both stacks are gated on
handing the GPU byte-identical data:

| | DirectXTex (`DDSTextureLoader` path) | rusty_dds |
|---|---:|---:|
| Container parse, total | 2.121 ms | **1.699 ms** |
| Allocations per run | 45 162 | **45 162** |
| Streaming CPU | 497.9 ms | 508.0 ms (inside the noise) |
| Frame cost p99 | 0.580 ms | 0.592 ms (inside the noise) |
| Uploaded bytes | 822.2 MiB | 822.2 MiB (identical) |

Where each number came from, including the ones that went against us:
[docs/plans/optimization-plan.md](docs/plans/optimization-plan.md).

## Install

```toml
rusty_dds = "0.3"
# decode-only, zero unsafe (e.g. WASM loaders):
# rusty_dds = { version = "0.3", default-features = false, features = ["decode"] }
```

| Feature | Default | Provides |
|---------|---------|----------|
| `decode` | yes | `decode_rgba8`, `decode_rgba_f32` (BC6H), `bcdec_rs` |
| `encode` | yes | `encode_from_rgba8`, `encode_bc6h_uf16`, `EncodeLayout`, `EncodeQuality`, opt-in RDO |
| `simd` | yes | AVX2 encode kernels — runtime-detected, scalar fallback, **byte-identical output**. Turn it off for a build the compiler proves contains no `unsafe`. |
| `tuning` | **no** | Development only. Re-opens the frozen encoder constants to `RUSTY_DDS_*` environment overrides for campaign sweeps. Never ship with it on — output would stop being a pure function of its inputs. |

Always on: container R/W, zero-copy `DdsView`, `SubresourceId` / `surface()`,
`decode_content()`, `UploadPlan` / `GpuFormat`.

**Determinism is part of the contract.** A payload is a pure function of
(source bytes, crate version, `EncodeLayout`) — no environment variable, no CPU
feature, no thread count changes it. `tests/encode_determinism.rs` freezes that
as payload hashes across every format, both quality tiers and RDO.

MSRV: **1.73**, verified by building against that toolchain. Changes: [CHANGELOG.md](CHANGELOG.md). Migrating from `ddsfile`: [docs/migration-ddsfile.md](docs/migration-ddsfile.md).

## Untrusted input

`Dds::read` reads to end-of-stream with no cap — correct for a file on disk,
wrong for bytes off a network or out of a mod archive. For those:

```rust
use rusty_dds::{Dds, Error};

match Dds::read_limited(reader, 64 * 1024 * 1024) {
    Err(Error::SizeLimitExceeded { limit, .. }) => { /* refuse, nothing buffered past the budget */ }
    other => { let _ = other?; }
}
# Ok::<(), Error>(())
```

Every size computation downstream of the header uses checked arithmetic, so a
header whose declared geometry cannot exist fails closed instead of wrapping
into a size that would then slice the payload. Two harnesses hold that line:
`tests/parser_robustness.rs` (pure Rust, stable, deterministic, runs on every
`cargo test`) and [`fuzz/`](fuzz/README.md) (cargo-fuzz, opt-in, excluded from
the published package so no C toolchain enters the shipped graph). Both drive
the same `tests/common/driver.rs`. Anything either one finds is pinned in
`tests/fixtures/regressions/` and replayed forever.

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
- **RDO** — Opt-in rate-distortion optimization for BC1/BC7, set per encode call
  via `EncodeLayout::with_rdo(Rdo::lambda(λ))`: smaller *compressed* payloads at
  parity-or-better quality. `Rdo::Off` (the default) is byte-identical to the
  plain encoder, gated by payload hash in `tests/encode_determinism.rs`.
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
`ddsfile` contributors retained. Attribution for included third-party work
(notably the `bcdec_rs` BC7 partition table) and corpus provenance:
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

**MIT, permanently.** 100% of the shipping code path is MIT — every format,
every optimization, every RDO mode. No paid tier, no feature held back, no
future relicense, no CLA assigning us your copyright. Fork it, vendor it, ship
it commercially, owe us nothing. Studios that want a λ calibrated on *their*
corpus, an SLA, or integration work can buy that as a service — the model, and
the things we commit to never selling, are written down in
[docs/commercial-model.md](docs/commercial-model.md).

## Trademark

"Remade With Rust", "Mata", and "Mata Network" are marks of Mata Network. DDS /
DirectDraw / DirectX are marks of their respective owners; this project is not
affiliated with Microsoft.
