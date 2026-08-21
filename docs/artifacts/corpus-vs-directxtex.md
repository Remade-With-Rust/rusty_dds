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
    "directxtex_faster": 4,
    "rusty_faster": 20,
    "tie": 0
  }
}
```

| Case | Role | rusty µs | DX µs | Ratio | rusty PSNR | DX PSNR | Δ | Speed | Quality |
|------|------|----------|-------|-------|------------|---------|---|-------|----------|
| Bricks097_Color__bc1 | albedo | 21078 | 15059 | 1.400 | 34.91 | 34.28 | +0.63 | directxtex_faster | rusty_higher_psnr |
| Bricks097_Color__bc7 | albedo | 37231 | 4567480 | 0.008 | 40.68 | 39.85 | +0.84 | rusty_faster | rusty_higher_psnr |
| Bricks097_NormalGL__bc5u | normal | 15249 | 18875 | 0.808 | 44.42 | 43.81 | +0.60 | rusty_faster | rusty_higher_psnr |
| Bricks097_NormalGL__bc5s | normal | 14548 | 20437 | 0.712 | 44.32 | 43.92 | +0.40 | rusty_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4u | mask | 8904 | 10773 | 0.826 | 43.01 | 42.42 | +0.59 | rusty_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4s | mask | 7499 | 11254 | 0.666 | 43.00 | 42.54 | +0.45 | rusty_faster | rusty_higher_psnr |
| Metal063_Color__bc1 | albedo | 38764 | 27320 | 1.419 | 40.23 | 38.93 | +1.31 | directxtex_faster | rusty_higher_psnr |
| Metal063_Color__bc7 | albedo | 51041 | 8563550 | 0.006 | 47.42 | 47.00 | +0.42 | rusty_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5u | normal | 20108 | 31980 | 0.629 | ∞ | 99.04 | — | rusty_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5s | normal | 9005 | 29041 | 0.310 | 51.14 | 51.14 | -0.00 | rusty_faster | tie |
| Metal063_Roughness__bc4u | mask | 17598 | 21684 | 0.812 | 46.12 | 45.36 | +0.76 | rusty_faster | rusty_higher_psnr |
| Metal063_Roughness__bc4s | mask | 17755 | 23461 | 0.757 | 46.12 | 45.59 | +0.53 | rusty_faster | rusty_higher_psnr |
| Rock064_Color__bc1 | albedo | 47310 | 34126 | 1.386 | 34.10 | 32.81 | +1.30 | directxtex_faster | rusty_higher_psnr |
| Rock064_Color__bc7 | albedo | 106374 | 9665220 | 0.011 | 39.48 | 38.28 | +1.20 | rusty_faster | rusty_higher_psnr |
| Rock064_NormalGL__bc5u | normal | 27769 | 38341 | 0.724 | 46.76 | 46.02 | +0.74 | rusty_faster | rusty_higher_psnr |
| Rock064_NormalGL__bc5s | normal | 22999 | 40835 | 0.563 | 46.72 | 46.19 | +0.53 | rusty_faster | rusty_higher_psnr |
| Rock064_Roughness__bc4u | mask | 15276 | 20750 | 0.736 | 47.05 | 46.20 | +0.86 | rusty_faster | rusty_higher_psnr |
| Rock064_Roughness__bc4s | mask | 12775 | 21771 | 0.587 | 46.99 | 46.44 | +0.56 | rusty_faster | rusty_higher_psnr |
| Wood095_Color__bc1 | albedo | 16940 | 12468 | 1.359 | 41.70 | 40.41 | +1.29 | directxtex_faster | rusty_higher_psnr |
| Wood095_Color__bc7 | albedo | 26983 | 4019840 | 0.007 | 49.28 | 48.56 | +0.72 | rusty_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5u | normal | 10869 | 18898 | 0.575 | 53.61 | 52.02 | +1.59 | rusty_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5s | normal | 10207 | 20354 | 0.501 | 52.18 | 52.69 | -0.51 | rusty_faster | directxtex_higher_psnr |
| Wood095_Roughness__bc4u | mask | 5377 | 10543 | 0.510 | 54.53 | 52.63 | +1.89 | rusty_faster | rusty_higher_psnr |
| Wood095_Roughness__bc4s | mask | 4678 | 10861 | 0.431 | 54.53 | 54.13 | +0.40 | rusty_faster | rusty_higher_psnr |
