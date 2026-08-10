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
    "directxtex_higher_psnr": 3,
    "rusty_higher_psnr": 16,
    "tie": 5
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
| Bricks097_Color__bc1 | albedo | 2066 | 15007 | 0.138 | 32.92 | 34.28 | -1.36 | rusty_faster | directxtex_higher_psnr |
| Bricks097_Color__bc7 | albedo | 23863 | 4618520 | 0.005 | 40.08 | 39.85 | +0.23 | rusty_faster | tie |
| Bricks097_NormalGL__bc5u | normal | 4375 | 18117 | 0.241 | 44.41 | 43.81 | +0.60 | rusty_faster | rusty_higher_psnr |
| Bricks097_NormalGL__bc5s | normal | 5137 | 19419 | 0.265 | 44.32 | 43.92 | +0.39 | rusty_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4u | mask | 3377 | 11005 | 0.307 | 43.01 | 42.42 | +0.59 | rusty_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4s | mask | 3512 | 11658 | 0.301 | 43.00 | 42.54 | +0.45 | rusty_faster | rusty_higher_psnr |
| Metal063_Color__bc1 | albedo | 2607 | 26652 | 0.098 | 39.00 | 38.93 | +0.07 | rusty_faster | tie |
| Metal063_Color__bc7 | albedo | 42334 | 8219840 | 0.005 | 47.35 | 47.00 | +0.35 | rusty_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5u | normal | 3924 | 31330 | 0.125 | ∞ | 99.04 | — | rusty_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5s | normal | 2663 | 27313 | 0.097 | 51.14 | 51.14 | -0.00 | rusty_faster | tie |
| Metal063_Roughness__bc4u | mask | 6606 | 21492 | 0.307 | 46.12 | 45.36 | +0.76 | rusty_faster | rusty_higher_psnr |
| Metal063_Roughness__bc4s | mask | 6246 | 22186 | 0.282 | 46.12 | 45.59 | +0.53 | rusty_faster | rusty_higher_psnr |
| Rock064_Color__bc1 | albedo | 3068 | 30918 | 0.099 | 32.37 | 32.81 | -0.44 | rusty_faster | directxtex_higher_psnr |
| Rock064_Color__bc7 | albedo | 53463 | 9966950 | 0.005 | 38.40 | 38.28 | +0.12 | rusty_faster | tie |
| Rock064_NormalGL__bc5u | normal | 7232 | 37626 | 0.192 | 46.76 | 46.02 | +0.74 | rusty_faster | rusty_higher_psnr |
| Rock064_NormalGL__bc5s | normal | 9139 | 41713 | 0.219 | 46.72 | 46.19 | +0.53 | rusty_faster | rusty_higher_psnr |
| Rock064_Roughness__bc4u | mask | 5734 | 21235 | 0.270 | 47.05 | 46.20 | +0.86 | rusty_faster | rusty_higher_psnr |
| Rock064_Roughness__bc4s | mask | 5434 | 23214 | 0.234 | 46.99 | 46.44 | +0.56 | rusty_faster | rusty_higher_psnr |
| Wood095_Color__bc1 | albedo | 1820 | 12500 | 0.146 | 40.31 | 40.41 | -0.10 | rusty_faster | tie |
| Wood095_Color__bc7 | albedo | 23347 | 4139260 | 0.006 | 48.99 | 48.56 | +0.43 | rusty_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5u | normal | 2902 | 18788 | 0.154 | 53.61 | 52.02 | +1.59 | rusty_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5s | normal | 3416 | 19434 | 0.176 | 52.18 | 52.69 | -0.51 | rusty_faster | directxtex_higher_psnr |
| Wood095_Roughness__bc4u | mask | 2065 | 10633 | 0.194 | 54.53 | 52.63 | +1.89 | rusty_faster | rusty_higher_psnr |
| Wood095_Roughness__bc4s | mask | 2035 | 11327 | 0.180 | 54.53 | 54.13 | +0.40 | rusty_faster | rusty_higher_psnr |
