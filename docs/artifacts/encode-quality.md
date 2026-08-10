# Encode quality notes (Phase 4)

Peer gate: **round-trip** `decode(encode(rgba))` — not bit-exact vs DirectXTex encode.

Measured single-surface C×X PSNR (same sizes as speed baselines):
[`encode-quality.json`](encode-quality.json) via
`cargo run --release --example harvest_encode_quality`.
Speed peer (Microsoft DirectXTex only):
[`encode-vs-baselines.md`](encode-vs-baselines.md).

**PSNR vs DirectXTex on the same sources:**
[`encode-quality-vs-directxtex.md`](encode-quality-vs-directxtex.md)
(`cargo run --release --example harvest_corpus_vs_dxtex` → publish into baselines).
Δ = rusty − DirectXTex (positive ⇒ we win fidelity on that cell).

**Primary bake-off surface:** ambientCG CC0 corpus (~1024²), not the synthetic
X-2D gradient matrix. See [`corpus/README.md`](../../corpus/README.md).

## Content × context matrix

Same IDs as decode completeness (`DecodeContent::ALL_LDR` × X-2D / X-MIP / X-ARRAY / X-CUBE / X-NPOT / X-VOL).

| Content | Round-trip gate |
|---------|-----------------|
| C-RGBA / C-BGRA | bit-exact |
| C-BC1 / C-BC2 / C-BC3 | PSNR ≥ 18 dB (opaque-aware sources for BC1) |
| C-BC4U / C-BC5U | PSNR ≥ 28 dB on preserved channels |
| C-BC4S / C-BC5S | PSNR ≥ 28 dB; endpoints are `i8` bit patterns (matches `bcdec_rs`) |
| C-BC7 | PSNR ≥ 22 dB (mode-6 only encoder) |

### X-2D (32²) vs DirectXTex (after quality plans 1–3)

| Content | rusty PSNR | DirectXTex | Δ | Encode vs DX (approx) |
|---------|------------|------------|---|------------------------|
| BC1 / BC3 | ~33.2 / 34.4 | ~33.2 / 34.4 | ≈ tie | **faster** (~7 / ~11 µs vs ~23 / ~28) |
| BC7 (mode 6) | 35.09 | 35.13 | ≈ tie | **~30× faster** |
| BC4U / BC5U | ∞ | 51.1 | rusty | slower on 32² (quality search) |
| BC4S / BC5S | 57.2 | 50.6 | **+6.5 dB** | slower on 32² (quality search) |
| RGBA / BGRA | exact | exact | — | memcpy-class |

Scoreboard (54 cells): DirectXTex higher **22** · rusty higher **8** · tie **24**.
Encode speed vs DX: **27 ahead / 27 behind** (BC1–3 + BC7 carry cook speed; BC4/5 pay for fidelity).

## Encoder profile

- Pure Rust, always on (no C/`*-sys`).
- BC1–BC3: luminance seed; chroma second seed only when colorful.
- BC4/5: decoder-matched palettes; unique/axis dispatch; LS + neighborhood search-skip; near-flat surface fast path; signed scores UNORM recon.
- BC7: **mode 6 only** (variance-gated seeds; LS on winner).
- Strip-parallel encode when ≥4096 blocks (decode-matched threshold).
- `EncodeLayout::quality`: `Quality` (default, corpus bake-off) or `Fast` (dual+LS only on BC4/5).
- Mips: box filter from mip 0 when `EncodeLayout::mipmap_levels > 1`.

## API

```rust
Dds::encode_from_rgba8(pixels, EncodeLayout { .. })
ImageRgba8::encode_dds(content)
```

Tests: `tests/encode_matrix.rs`.
