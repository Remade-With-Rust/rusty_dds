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
| Bricks097_Color__bc1 | albedo | 13634 | 15954 | 0.855 | 34.90 | 34.28 | +0.62 | rusty_faster | rusty_higher_psnr |
| Bricks097_Color__bc7 | albedo | 44934 | 5110020 | 0.009 | 40.68 | 39.85 | +0.84 | rusty_faster | rusty_higher_psnr |
| Bricks097_NormalGL__bc5u | normal | 14408 | 19589 | 0.736 | 44.42 | 43.81 | +0.60 | rusty_faster | rusty_higher_psnr |
| Bricks097_NormalGL__bc5s | normal | 13949 | 23114 | 0.603 | 44.32 | 43.92 | +0.40 | rusty_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4u | mask | 7850 | 11030 | 0.712 | 43.01 | 42.42 | +0.59 | rusty_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4s | mask | 7602 | 11483 | 0.662 | 43.00 | 42.54 | +0.45 | rusty_faster | rusty_higher_psnr |
| Metal063_Color__bc1 | albedo | 20680 | 29489 | 0.701 | 40.23 | 38.93 | +1.30 | rusty_faster | rusty_higher_psnr |
| Metal063_Color__bc7 | albedo | 57589 | 9013400 | 0.006 | 47.42 | 47.00 | +0.42 | rusty_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5u | normal | 14500 | 31485 | 0.461 | ∞ | 99.04 | — | rusty_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5s | normal | 8356 | 27430 | 0.305 | 51.14 | 51.14 | -0.00 | rusty_faster | tie |
| Metal063_Roughness__bc4u | mask | 16818 | 22785 | 0.738 | 46.12 | 45.36 | +0.76 | rusty_faster | rusty_higher_psnr |
| Metal063_Roughness__bc4s | mask | 15210 | 25260 | 0.602 | 46.12 | 45.59 | +0.53 | rusty_faster | rusty_higher_psnr |
| Rock064_Color__bc1 | albedo | 25737 | 35897 | 0.717 | 34.09 | 32.81 | +1.28 | rusty_faster | rusty_higher_psnr |
| Rock064_Color__bc7 | albedo | 81207 | 11132500 | 0.007 | 39.48 | 38.28 | +1.20 | rusty_faster | rusty_higher_psnr |
| Rock064_NormalGL__bc5u | normal | 25190 | 41622 | 0.605 | 46.76 | 46.02 | +0.74 | rusty_faster | rusty_higher_psnr |
| Rock064_NormalGL__bc5s | normal | 24064 | 43876 | 0.548 | 46.72 | 46.19 | +0.53 | rusty_faster | rusty_higher_psnr |
| Rock064_Roughness__bc4u | mask | 14721 | 22868 | 0.644 | 47.05 | 46.20 | +0.86 | rusty_faster | rusty_higher_psnr |
| Rock064_Roughness__bc4s | mask | 13585 | 24101 | 0.564 | 46.99 | 46.44 | +0.56 | rusty_faster | rusty_higher_psnr |
| Wood095_Color__bc1 | albedo | 9915 | 13947 | 0.711 | 41.70 | 40.41 | +1.29 | rusty_faster | rusty_higher_psnr |
| Wood095_Color__bc7 | albedo | 30587 | 4689160 | 0.007 | 49.28 | 48.56 | +0.72 | rusty_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5u | normal | 9720 | 21663 | 0.449 | 53.61 | 52.02 | +1.59 | rusty_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5s | normal | 10930 | 22886 | 0.478 | 52.18 | 52.69 | -0.51 | rusty_faster | directxtex_higher_psnr |
| Wood095_Roughness__bc4u | mask | 5128 | 12069 | 0.425 | 54.53 | 52.63 | +1.89 | rusty_faster | rusty_higher_psnr |
| Wood095_Roughness__bc4s | mask | 5288 | 11832 | 0.447 | 54.53 | 54.13 | +0.40 | rusty_faster | rusty_higher_psnr |
