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
    "directxtex_faster": 10,
    "rusty_faster": 14,
    "tie": 0
  }
}
```

| Case | Role | rusty µs | DX µs | Ratio | rusty PSNR | DX PSNR | Δ | Speed | Quality |
|------|------|----------|-------|-------|------------|---------|---|-------|----------|
| Bricks097_Color__bc1 | albedo | 21964 | 15726 | 1.397 | 34.91 | 34.28 | +0.63 | directxtex_faster | rusty_higher_psnr |
| Bricks097_Color__bc7 | albedo | 37578 | 4715300 | 0.008 | 40.68 | 39.85 | +0.84 | rusty_faster | rusty_higher_psnr |
| Bricks097_NormalGL__bc5u | normal | 15295 | 18422 | 0.830 | 44.42 | 43.81 | +0.60 | rusty_faster | rusty_higher_psnr |
| Bricks097_NormalGL__bc5s | normal | 34987 | 19484 | 1.796 | 44.32 | 43.92 | +0.40 | directxtex_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4u | mask | 8893 | 10261 | 0.867 | 43.01 | 42.42 | +0.59 | rusty_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4s | mask | 20847 | 11223 | 1.858 | 43.00 | 42.54 | +0.45 | directxtex_faster | rusty_higher_psnr |
| Metal063_Color__bc1 | albedo | 38994 | 26465 | 1.473 | 40.23 | 38.93 | +1.31 | directxtex_faster | rusty_higher_psnr |
| Metal063_Color__bc7 | albedo | 53445 | 8242790 | 0.006 | 47.42 | 47.00 | +0.42 | rusty_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5u | normal | 19135 | 33802 | 0.566 | ∞ | 99.04 | — | rusty_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5s | normal | 8952 | 29015 | 0.309 | 51.14 | 51.14 | -0.00 | rusty_faster | tie |
| Metal063_Roughness__bc4u | mask | 18441 | 21742 | 0.848 | 46.12 | 45.36 | +0.76 | rusty_faster | rusty_higher_psnr |
| Metal063_Roughness__bc4s | mask | 36268 | 24891 | 1.457 | 46.12 | 45.59 | +0.53 | directxtex_faster | rusty_higher_psnr |
| Rock064_Color__bc1 | albedo | 49297 | 32441 | 1.520 | 34.10 | 32.81 | +1.30 | directxtex_faster | rusty_higher_psnr |
| Rock064_Color__bc7 | albedo | 80415 | 9655230 | 0.008 | 39.48 | 38.28 | +1.20 | rusty_faster | rusty_higher_psnr |
| Rock064_NormalGL__bc5u | normal | 27986 | 37743 | 0.741 | 46.76 | 46.02 | +0.74 | rusty_faster | rusty_higher_psnr |
| Rock064_NormalGL__bc5s | normal | 61775 | 40787 | 1.515 | 46.72 | 46.19 | +0.53 | directxtex_faster | rusty_higher_psnr |
| Rock064_Roughness__bc4u | mask | 14965 | 20839 | 0.718 | 47.05 | 46.20 | +0.86 | rusty_faster | rusty_higher_psnr |
| Rock064_Roughness__bc4s | mask | 32180 | 21980 | 1.464 | 46.99 | 46.44 | +0.56 | directxtex_faster | rusty_higher_psnr |
| Wood095_Color__bc1 | albedo | 17849 | 12341 | 1.446 | 41.70 | 40.41 | +1.29 | directxtex_faster | rusty_higher_psnr |
| Wood095_Color__bc7 | albedo | 26490 | 3969850 | 0.007 | 49.28 | 48.56 | +0.72 | rusty_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5u | normal | 10522 | 18372 | 0.573 | 53.61 | 52.02 | +1.59 | rusty_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5s | normal | 22446 | 19050 | 1.178 | 52.18 | 52.69 | -0.51 | directxtex_faster | directxtex_higher_psnr |
| Wood095_Roughness__bc4u | mask | 5418 | 10381 | 0.522 | 54.53 | 52.63 | +1.89 | rusty_faster | rusty_higher_psnr |
| Wood095_Roughness__bc4s | mask | 8946 | 10878 | 0.822 | 54.53 | 54.13 | +0.40 | rusty_faster | rusty_higher_psnr |
