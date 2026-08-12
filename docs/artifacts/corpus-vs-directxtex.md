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
    "directxtex_higher_psnr": 0,
    "rusty_higher_psnr": 20,
    "tie": 4
  },
  "speed": {
    "directxtex_faster": 14,
    "rusty_faster": 9,
    "tie": 1
  }
}
```

| Case | Role | rusty µs | DX µs | Ratio | rusty PSNR | DX PSNR | Δ | Speed | Quality |
|------|------|----------|-------|-------|------------|---------|---|-------|----------|
| Bricks097_Color__bc1 | albedo | 53806 | 31496 | 1.708 | 34.57 | 34.28 | +0.29 | directxtex_faster | rusty_higher_psnr |
| Bricks097_Color__bc7 | albedo | 134396 | 12183200 | 0.011 | 40.08 | 39.85 | +0.23 | rusty_faster | tie |
| Bricks097_NormalGL__bc5u | normal | 1265969 | 70035 | 18.076 | 44.42 | 43.81 | +0.60 | directxtex_faster | rusty_higher_psnr |
| Bricks097_NormalGL__bc5s | normal | 1094296 | 42806 | 25.564 | 44.37 | 43.92 | +0.45 | directxtex_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4u | mask | 232121 | 19405 | 11.962 | 43.01 | 42.42 | +0.59 | directxtex_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4s | mask | 318558 | 22690 | 14.040 | 43.05 | 42.54 | +0.51 | directxtex_faster | rusty_higher_psnr |
| Metal063_Color__bc1 | albedo | 79774 | 76705 | 1.040 | 39.91 | 38.93 | +0.99 | speed_tie | rusty_higher_psnr |
| Metal063_Color__bc7 | albedo | 103469 | 21318100 | 0.005 | 47.35 | 47.00 | +0.35 | rusty_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5u | normal | 129685 | 56409 | 2.299 | ∞ | 99.04 | — | directxtex_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5s | normal | 63094 | 55337 | 1.140 | 51.14 | 51.14 | -0.00 | directxtex_faster | tie |
| Metal063_Roughness__bc4u | mask | 61560 | 46116 | 1.335 | 46.12 | 45.36 | +0.76 | directxtex_faster | rusty_higher_psnr |
| Metal063_Roughness__bc4s | mask | 76739 | 41397 | 1.854 | 46.29 | 45.59 | +0.69 | directxtex_faster | rusty_higher_psnr |
| Rock064_Color__bc1 | albedo | 22362 | 63182 | 0.354 | 33.78 | 32.81 | +0.97 | rusty_faster | rusty_higher_psnr |
| Rock064_Color__bc7 | albedo | 89866 | 21612100 | 0.004 | 38.40 | 38.28 | +0.12 | rusty_faster | tie |
| Rock064_NormalGL__bc5u | normal | 51233 | 79806 | 0.642 | 46.76 | 46.02 | +0.74 | rusty_faster | rusty_higher_psnr |
| Rock064_NormalGL__bc5s | normal | 107205 | 94886 | 1.130 | 46.89 | 46.19 | +0.69 | directxtex_faster | rusty_higher_psnr |
| Rock064_Roughness__bc4u | mask | 33297 | 45205 | 0.737 | 47.05 | 46.20 | +0.86 | rusty_faster | rusty_higher_psnr |
| Rock064_Roughness__bc4s | mask | 58740 | 41773 | 1.406 | 47.16 | 46.44 | +0.72 | directxtex_faster | rusty_higher_psnr |
| Wood095_Color__bc1 | albedo | 21097 | 28601 | 0.738 | 40.88 | 40.41 | +0.48 | rusty_faster | rusty_higher_psnr |
| Wood095_Color__bc7 | albedo | 37556 | 8940760 | 0.004 | 48.99 | 48.56 | +0.43 | rusty_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5u | normal | 54442 | 37654 | 1.446 | 53.61 | 52.02 | +1.59 | directxtex_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5s | normal | 73093 | 36840 | 1.984 | 52.60 | 52.69 | -0.10 | directxtex_faster | tie |
| Wood095_Roughness__bc4u | mask | 13002 | 20790 | 0.625 | 54.53 | 52.63 | +1.89 | rusty_faster | rusty_higher_psnr |
| Wood095_Roughness__bc4s | mask | 29614 | 22325 | 1.326 | 55.14 | 54.13 | +1.01 | directxtex_faster | rusty_higher_psnr |
