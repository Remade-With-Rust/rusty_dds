# Corpus encode vs DirectXTex, SERIAL vs SERIAL

The headline `corpus-vs-directxtex.md` runs rusty_dds in its shipped
configuration, which is strip-parallel above 512 blocks, against a DirectXTex
peer built with `TEX_COMPRESS_DEFAULT` — no `TEX_COMPRESS_PARALLEL`. That is a
fair report of *what the two libraries do out of the box*, and a poor comparison
of *the encoders themselves*: it reads 24/24 in our favour, and most of that is
thread count.

This artifact removes threads from both sides. `ENCODE_PARALLEL_MIN_BLOCKS` was
raised to `usize::MAX` so a production-sized surface encodes serially, and the
DirectXTex peer is unchanged (already single-threaded).

## What changes

| | parallel (shipped) | serial vs serial |
|---|---|---|
| rusty faster | **24** | **12** |
| DirectXTex faster | 0 | **11** |
| tie | 0 | 1 |

**Quality is identical in both runs — 22 of 24 in our favour, 0 losses, 2 ties.**
PSNR is deterministic; threading cannot touch it. That is the claim that
survives the correction.

## Per-core, the speed picture is three separate stories

- **BC7: we win enormously** — ratios 0.006–0.008, so roughly **125–165x**
  faster. Far too large for thread count to explain, and it holds with threads
  removed. Our mode-6-class search against DirectXTex's `BC7_QUICK`.
- **BC4U / BC5U: we are modestly faster** — 0.53–0.91.
- **BC1: DirectXTex is faster** — 1.52–1.85x.
- **BC4S / BC5S (signed): DirectXTex is much faster** — **3.6x to 7.9x**. This is
  the clearest optimisation target the comparison surfaces; the signed paths have
  never had the attention the unsigned ones got.

## Table

| Case | Role | rusty µs | DX µs | Ratio | rusty PSNR | DX PSNR | Δ | Speed | Quality |
|------|------|----------|-------|-------|------------|---------|---|-------|----------|
| Bricks097_Color__bc1 | albedo | 25379 | 16421 | 1.546 | 34.91 | 34.28 | +0.63 | directxtex_faster | rusty_higher_psnr |
| Bricks097_Color__bc7 | albedo | 39811 | 4986100 | 0.008 | 40.68 | 39.85 | +0.84 | rusty_faster | rusty_higher_psnr |
| Bricks097_NormalGL__bc5u | normal | 16577 | 19298 | 0.859 | 44.42 | 43.81 | +0.60 | rusty_faster | rusty_higher_psnr |
| Bricks097_NormalGL__bc5s | normal | 108404 | 20508 | 5.286 | 44.37 | 43.92 | +0.45 | directxtex_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4u | mask | 9909 | 10890 | 0.910 | 43.01 | 42.42 | +0.59 | rusty_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4s | mask | 62864 | 11451 | 5.490 | 43.05 | 42.54 | +0.51 | directxtex_faster | rusty_higher_psnr |
| Metal063_Color__bc1 | albedo | 42941 | 28223 | 1.521 | 40.23 | 38.93 | +1.31 | directxtex_faster | rusty_higher_psnr |
| Metal063_Color__bc7 | albedo | 54438 | 8911250 | 0.006 | 47.42 | 47.00 | +0.42 | rusty_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5u | normal | 20946 | 31883 | 0.657 | ∞ | 99.04 | — | rusty_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5s | normal | 18632 | 28122 | 0.663 | 51.14 | 51.14 | -0.00 | rusty_faster | tie |
| Metal063_Roughness__bc4u | mask | 17613 | 21578 | 0.816 | 46.12 | 45.36 | +0.76 | rusty_faster | rusty_higher_psnr |
| Metal063_Roughness__bc4s | mask | 159275 | 22664 | 7.028 | 46.29 | 45.59 | +0.69 | directxtex_faster | rusty_higher_psnr |
| Rock064_Color__bc1 | albedo | 57493 | 31145 | 1.846 | 34.10 | 32.81 | +1.30 | directxtex_faster | rusty_higher_psnr |
| Rock064_Color__bc7 | albedo | 77525 | 10304700 | 0.008 | 39.48 | 38.28 | +1.20 | rusty_faster | rusty_higher_psnr |
| Rock064_NormalGL__bc5u | normal | 34876 | 41234 | 0.846 | 46.76 | 46.02 | +0.74 | rusty_faster | rusty_higher_psnr |
| Rock064_NormalGL__bc5s | normal | 321456 | 62575 | 5.137 | 46.89 | 46.19 | +0.69 | directxtex_faster | rusty_higher_psnr |
| Rock064_Roughness__bc4u | mask | 24661 | 24062 | 1.025 | 47.05 | 46.20 | +0.86 | speed_tie | rusty_higher_psnr |
| Rock064_Roughness__bc4s | mask | 183760 | 23349 | 7.870 | 47.16 | 46.44 | +0.72 | directxtex_faster | rusty_higher_psnr |
| Wood095_Color__bc1 | albedo | 20543 | 12402 | 1.656 | 41.70 | 40.41 | +1.29 | directxtex_faster | rusty_higher_psnr |
| Wood095_Color__bc7 | albedo | 30211 | 4359220 | 0.007 | 49.28 | 48.56 | +0.72 | rusty_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5u | normal | 15490 | 18783 | 0.825 | 53.61 | 52.02 | +1.59 | rusty_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5s | normal | 118862 | 20198 | 5.885 | 52.60 | 52.69 | -0.10 | directxtex_faster | tie |
| Wood095_Roughness__bc4u | mask | 5875 | 11200 | 0.525 | 54.53 | 52.63 | +1.89 | rusty_faster | rusty_higher_psnr |
| Wood095_Roughness__bc4s | mask | 41050 | 11339 | 3.620 | 55.14 | 54.13 | +1.01 | directxtex_faster | rusty_higher_psnr |


## Reproducing

Raise `ENCODE_PARALLEL_MIN_BLOCKS` to `usize::MAX` in `src/encode/blocks.rs`,
then `cargo run --release --example harvest_corpus_vs_dxtex`. Restore it
afterwards — the shipped default is 512.
