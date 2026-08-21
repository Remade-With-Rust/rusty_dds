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
| Bricks097_Color__bc1 | albedo | 14430 | 18214 | 0.792 | 34.90 | 34.28 | +0.62 | rusty_faster | rusty_higher_psnr |
| Bricks097_Color__bc7 | albedo | 41635 | 5475630 | 0.008 | 40.68 | 39.85 | +0.84 | rusty_faster | rusty_higher_psnr |
| Bricks097_NormalGL__bc5u | normal | 14251 | 22572 | 0.631 | 44.42 | 43.81 | +0.60 | rusty_faster | rusty_higher_psnr |
| Bricks097_NormalGL__bc5s | normal | 15932 | 23649 | 0.674 | 44.32 | 43.92 | +0.40 | rusty_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4u | mask | 8999 | 12578 | 0.715 | 43.01 | 42.42 | +0.59 | rusty_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4s | mask | 8494 | 13220 | 0.642 | 43.00 | 42.54 | +0.45 | rusty_faster | rusty_higher_psnr |
| Metal063_Color__bc1 | albedo | 24590 | 31177 | 0.789 | 40.23 | 38.93 | +1.30 | rusty_faster | rusty_higher_psnr |
| Metal063_Color__bc7 | albedo | 60403 | 9458700 | 0.006 | 47.42 | 47.00 | +0.42 | rusty_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5u | normal | 16668 | 42079 | 0.396 | ∞ | 99.04 | — | rusty_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5s | normal | 8118 | 33126 | 0.245 | 51.14 | 51.14 | -0.00 | rusty_faster | tie |
| Metal063_Roughness__bc4u | mask | 16812 | 28469 | 0.591 | 46.12 | 45.36 | +0.76 | rusty_faster | rusty_higher_psnr |
| Metal063_Roughness__bc4s | mask | 17949 | 28044 | 0.640 | 46.12 | 45.59 | +0.53 | rusty_faster | rusty_higher_psnr |
| Rock064_Color__bc1 | albedo | 31239 | 38292 | 0.816 | 34.09 | 32.81 | +1.28 | rusty_faster | rusty_higher_psnr |
| Rock064_Color__bc7 | albedo | 87306 | 12879500 | 0.007 | 39.48 | 38.28 | +1.20 | rusty_faster | rusty_higher_psnr |
| Rock064_NormalGL__bc5u | normal | 38843 | 47759 | 0.813 | 46.76 | 46.02 | +0.74 | rusty_faster | rusty_higher_psnr |
| Rock064_NormalGL__bc5s | normal | 37075 | 50094 | 0.740 | 46.72 | 46.19 | +0.53 | rusty_faster | rusty_higher_psnr |
| Rock064_Roughness__bc4u | mask | 18914 | 24782 | 0.763 | 47.05 | 46.20 | +0.86 | rusty_faster | rusty_higher_psnr |
| Rock064_Roughness__bc4s | mask | 17949 | 27022 | 0.664 | 46.99 | 46.44 | +0.56 | rusty_faster | rusty_higher_psnr |
| Wood095_Color__bc1 | albedo | 12526 | 15012 | 0.834 | 41.70 | 40.41 | +1.29 | rusty_faster | rusty_higher_psnr |
| Wood095_Color__bc7 | albedo | 35048 | 4949260 | 0.007 | 49.28 | 48.56 | +0.72 | rusty_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5u | normal | 13631 | 25702 | 0.530 | 53.61 | 52.02 | +1.59 | rusty_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5s | normal | 11690 | 23918 | 0.489 | 52.18 | 52.69 | -0.51 | rusty_faster | directxtex_higher_psnr |
| Wood095_Roughness__bc4u | mask | 5322 | 13229 | 0.402 | 54.53 | 52.63 | +1.89 | rusty_faster | rusty_higher_psnr |
| Wood095_Roughness__bc4s | mask | 5431 | 13494 | 0.402 | 54.53 | 54.13 | +0.40 | rusty_faster | rusty_higher_psnr |
