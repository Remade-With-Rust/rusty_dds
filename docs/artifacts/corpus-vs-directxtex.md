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
    "directxtex_faster": 3,
    "rusty_faster": 21,
    "tie": 0
  }
}
```

| Case | Role | rusty µs | DX µs | Ratio | rusty PSNR | DX PSNR | Δ | Speed | Quality |
|------|------|----------|-------|-------|------------|---------|---|-------|----------|
| Bricks097_Color__bc1 | albedo | 17229 | 34888 | 0.494 | 34.57 | 34.28 | +0.29 | rusty_faster | rusty_higher_psnr |
| Bricks097_Color__bc7 | albedo | 41625 | 10648600 | 0.004 | 40.08 | 39.85 | +0.23 | rusty_faster | tie |
| Bricks097_NormalGL__bc5u | normal | 16230 | 39269 | 0.413 | 44.42 | 43.81 | +0.60 | rusty_faster | rusty_higher_psnr |
| Bricks097_NormalGL__bc5s | normal | 26788 | 42716 | 0.627 | 44.37 | 43.92 | +0.45 | rusty_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4u | mask | 9780 | 18965 | 0.516 | 43.01 | 42.42 | +0.59 | rusty_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4s | mask | 22166 | 20229 | 1.096 | 43.05 | 42.54 | +0.51 | directxtex_faster | rusty_higher_psnr |
| Metal063_Color__bc1 | albedo | 35863 | 63888 | 0.561 | 39.91 | 38.93 | +0.99 | rusty_faster | rusty_higher_psnr |
| Metal063_Color__bc7 | albedo | 58621 | 18969400 | 0.003 | 47.35 | 47.00 | +0.35 | rusty_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5u | normal | 14122 | 63250 | 0.223 | ∞ | 99.04 | — | rusty_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5s | normal | 8933 | 67422 | 0.132 | 51.14 | 51.14 | -0.00 | rusty_faster | tie |
| Metal063_Roughness__bc4u | mask | 35108 | 49552 | 0.709 | 46.12 | 45.36 | +0.76 | rusty_faster | rusty_higher_psnr |
| Metal063_Roughness__bc4s | mask | 48109 | 51299 | 0.938 | 46.29 | 45.59 | +0.69 | rusty_faster | rusty_higher_psnr |
| Rock064_Color__bc1 | albedo | 23421 | 67448 | 0.347 | 33.78 | 32.81 | +0.97 | rusty_faster | rusty_higher_psnr |
| Rock064_Color__bc7 | albedo | 56788 | 22169900 | 0.003 | 38.40 | 38.28 | +0.12 | rusty_faster | tie |
| Rock064_NormalGL__bc5u | normal | 28307 | 79804 | 0.355 | 46.76 | 46.02 | +0.74 | rusty_faster | rusty_higher_psnr |
| Rock064_NormalGL__bc5s | normal | 79851 | 86702 | 0.921 | 46.89 | 46.19 | +0.69 | rusty_faster | rusty_higher_psnr |
| Rock064_Roughness__bc4u | mask | 13649 | 40891 | 0.334 | 47.05 | 46.20 | +0.86 | rusty_faster | rusty_higher_psnr |
| Rock064_Roughness__bc4s | mask | 50547 | 46126 | 1.096 | 47.16 | 46.44 | +0.72 | directxtex_faster | rusty_higher_psnr |
| Wood095_Color__bc1 | albedo | 16912 | 24003 | 0.705 | 40.88 | 40.41 | +0.48 | rusty_faster | rusty_higher_psnr |
| Wood095_Color__bc7 | albedo | 29972 | 9680480 | 0.003 | 48.99 | 48.56 | +0.43 | rusty_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5u | normal | 12823 | 31712 | 0.404 | 53.61 | 52.02 | +1.59 | rusty_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5s | normal | 41151 | 37432 | 1.099 | 52.60 | 52.69 | -0.10 | directxtex_faster | tie |
| Wood095_Roughness__bc4u | mask | 7866 | 19760 | 0.398 | 54.53 | 52.63 | +1.89 | rusty_faster | rusty_higher_psnr |
| Wood095_Roughness__bc4s | mask | 18034 | 22407 | 0.805 | 55.14 | 54.13 | +1.01 | rusty_faster | rusty_higher_psnr |
