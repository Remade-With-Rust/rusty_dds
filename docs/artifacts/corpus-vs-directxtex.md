# Corpus encode: rusty_dds vs DirectXTex

- Proxy cook corpus (not Star Citizen / Cry). License: CC0 via ambientCG.
- Albedo Color → BC1/BC7; NormalGL → BC5U/S (R,G); Roughness → BC4U/S (R).
- BC7 peer: TEX_COMPRESS_BC7_QUICK. DirectXTex encode_ns from dxtex_roundtrip JSON.
- rusty encode: best of 3 iters (encode only). Quality: ±0.25 dB tie band.
- ratio < 1 ⇒ rusty_dds faster.
- rusty strip-parallel encode (≥4096 blocks); DX peer is TEX_COMPRESS_DEFAULT (no PARALLEL).

## Summary

```json
{
  "cases": 24,
  "quality": {
    "compared": 24,
    "directxtex_higher_psnr": 1,
    "rusty_higher_psnr": 22,
    "tie": 1
  },
  "speed": {
    "directxtex_faster": 3,
    "rusty_faster": 20,
    "tie": 1
  }
}
```

| Case | Role | rusty µs | DX µs | Ratio | rusty PSNR | DX PSNR | Δ | Speed | Quality |
|------|------|----------|-------|-------|------------|---------|---|-------|----------|
| Bricks097_Color__bc1 | albedo | 20057 | 17214 | 1.165 | 34.90 | 34.28 | +0.62 | directxtex_faster | rusty_higher_psnr |
| Bricks097_Color__bc7 | albedo | 42345 | 5101120 | 0.008 | 40.68 | 39.85 | +0.84 | rusty_faster | rusty_higher_psnr |
| Bricks097_NormalGL__bc5u | normal | 17223 | 21058 | 0.818 | 44.42 | 43.81 | +0.60 | rusty_faster | rusty_higher_psnr |
| Bricks097_NormalGL__bc5s | normal | 14379 | 21933 | 0.656 | 44.32 | 43.92 | +0.40 | rusty_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4u | mask | 9940 | 11849 | 0.839 | 43.01 | 42.42 | +0.59 | rusty_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4s | mask | 8133 | 12440 | 0.654 | 43.00 | 42.54 | +0.45 | rusty_faster | rusty_higher_psnr |
| Metal063_Color__bc1 | albedo | 29939 | 28445 | 1.053 | 40.23 | 38.93 | +1.30 | directxtex_faster | rusty_higher_psnr |
| Metal063_Color__bc7 | albedo | 56832 | 9017530 | 0.006 | 47.42 | 47.00 | +0.42 | rusty_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5u | normal | 20572 | 35083 | 0.586 | ∞ | 99.04 | — | rusty_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5s | normal | 6366 | 29742 | 0.214 | 51.14 | 51.14 | -0.00 | rusty_faster | tie |
| Metal063_Roughness__bc4u | mask | 20105 | 24505 | 0.820 | 46.12 | 45.36 | +0.76 | rusty_faster | rusty_higher_psnr |
| Metal063_Roughness__bc4s | mask | 16014 | 25534 | 0.627 | 46.12 | 45.59 | +0.53 | rusty_faster | rusty_higher_psnr |
| Rock064_Color__bc1 | albedo | 39631 | 35067 | 1.130 | 34.09 | 32.81 | +1.28 | directxtex_faster | rusty_higher_psnr |
| Rock064_Color__bc7 | albedo | 86305 | 10662200 | 0.008 | 39.48 | 38.28 | +1.20 | rusty_faster | rusty_higher_psnr |
| Rock064_NormalGL__bc5u | normal | 30901 | 41798 | 0.739 | 46.76 | 46.02 | +0.74 | rusty_faster | rusty_higher_psnr |
| Rock064_NormalGL__bc5s | normal | 25841 | 45336 | 0.570 | 46.72 | 46.19 | +0.53 | rusty_faster | rusty_higher_psnr |
| Rock064_Roughness__bc4u | mask | 16584 | 22493 | 0.737 | 47.05 | 46.20 | +0.86 | rusty_faster | rusty_higher_psnr |
| Rock064_Roughness__bc4s | mask | 15266 | 25489 | 0.599 | 46.99 | 46.44 | +0.56 | rusty_faster | rusty_higher_psnr |
| Wood095_Color__bc1 | albedo | 14581 | 14047 | 1.038 | 41.70 | 40.41 | +1.29 | speed_tie | rusty_higher_psnr |
| Wood095_Color__bc7 | albedo | 32871 | 4496070 | 0.007 | 49.28 | 48.56 | +0.72 | rusty_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5u | normal | 12300 | 22735 | 0.541 | 53.61 | 52.02 | +1.59 | rusty_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5s | normal | 11864 | 23134 | 0.513 | 52.18 | 52.69 | -0.51 | rusty_faster | directxtex_higher_psnr |
| Wood095_Roughness__bc4u | mask | 5980 | 11580 | 0.516 | 54.53 | 52.63 | +1.89 | rusty_faster | rusty_higher_psnr |
| Wood095_Roughness__bc4s | mask | 5273 | 12276 | 0.430 | 54.53 | 54.13 | +0.40 | rusty_faster | rusty_higher_psnr |
