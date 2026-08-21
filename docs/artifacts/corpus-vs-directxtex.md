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
    "directxtex_faster": 0,
    "rusty_faster": 24,
    "tie": 0
  }
}
```

| Case | Role | rusty µs | DX µs | Ratio | rusty PSNR | DX PSNR | Δ | Speed | Quality |
|------|------|----------|-------|-------|------------|---------|---|-------|----------|
| Bricks097_Color__bc1 | albedo | 15144 | 17911 | 0.845 | 34.90 | 34.28 | +0.62 | rusty_faster | rusty_higher_psnr |
| Bricks097_Color__bc7 | albedo | 44801 | 5519130 | 0.008 | 40.68 | 39.85 | +0.84 | rusty_faster | rusty_higher_psnr |
| Bricks097_NormalGL__bc5u | normal | 17760 | 27363 | 0.649 | 44.42 | 43.81 | +0.60 | rusty_faster | rusty_higher_psnr |
| Bricks097_NormalGL__bc5s | normal | 20339 | 30411 | 0.669 | 44.32 | 43.92 | +0.40 | rusty_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4u | mask | 11055 | 13294 | 0.832 | 43.01 | 42.42 | +0.59 | rusty_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4s | mask | 13542 | 15010 | 0.902 | 43.00 | 42.54 | +0.45 | rusty_faster | rusty_higher_psnr |
| Metal063_Color__bc1 | albedo | 27374 | 34854 | 0.785 | 40.23 | 38.93 | +1.30 | rusty_faster | rusty_higher_psnr |
| Metal063_Color__bc7 | albedo | 76084 | 10113000 | 0.008 | 47.42 | 47.00 | +0.42 | rusty_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5u | normal | 19354 | 37977 | 0.510 | ∞ | 99.04 | — | rusty_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5s | normal | 7312 | 32082 | 0.228 | 51.14 | 51.14 | -0.00 | rusty_faster | tie |
| Metal063_Roughness__bc4u | mask | 18916 | 24656 | 0.767 | 46.12 | 45.36 | +0.76 | rusty_faster | rusty_higher_psnr |
| Metal063_Roughness__bc4s | mask | 16041 | 26825 | 0.598 | 46.12 | 45.59 | +0.53 | rusty_faster | rusty_higher_psnr |
| Rock064_Color__bc1 | albedo | 31404 | 36011 | 0.872 | 34.09 | 32.81 | +1.28 | rusty_faster | rusty_higher_psnr |
| Rock064_Color__bc7 | albedo | 94149 | 11448100 | 0.008 | 39.48 | 38.28 | +1.20 | rusty_faster | rusty_higher_psnr |
| Rock064_NormalGL__bc5u | normal | 30099 | 44554 | 0.676 | 46.76 | 46.02 | +0.74 | rusty_faster | rusty_higher_psnr |
| Rock064_NormalGL__bc5s | normal | 27270 | 48742 | 0.559 | 46.72 | 46.19 | +0.53 | rusty_faster | rusty_higher_psnr |
| Rock064_Roughness__bc4u | mask | 16617 | 25262 | 0.658 | 47.05 | 46.20 | +0.86 | rusty_faster | rusty_higher_psnr |
| Rock064_Roughness__bc4s | mask | 14908 | 26863 | 0.555 | 46.99 | 46.44 | +0.56 | rusty_faster | rusty_higher_psnr |
| Wood095_Color__bc1 | albedo | 11083 | 14202 | 0.780 | 41.70 | 40.41 | +1.29 | rusty_faster | rusty_higher_psnr |
| Wood095_Color__bc7 | albedo | 30787 | 4646570 | 0.007 | 49.28 | 48.56 | +0.72 | rusty_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5u | normal | 11099 | 21493 | 0.516 | 53.61 | 52.02 | +1.59 | rusty_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5s | normal | 12421 | 23377 | 0.531 | 52.18 | 52.69 | -0.51 | rusty_faster | directxtex_higher_psnr |
| Wood095_Roughness__bc4u | mask | 5568 | 11997 | 0.464 | 54.53 | 52.63 | +1.89 | rusty_faster | rusty_higher_psnr |
| Wood095_Roughness__bc4s | mask | 5640 | 12605 | 0.447 | 54.53 | 54.13 | +0.40 | rusty_faster | rusty_higher_psnr |
