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
    "rusty_higher_psnr": 22,
    "tie": 2
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
| Bricks097_Color__bc1 | albedo | 6076 | 20156 | 0.301 | 34.91 | 34.28 | +0.63 | rusty_faster | rusty_higher_psnr |
| Bricks097_Color__bc7 | albedo | 7684 | 6121590 | 0.001 | 40.68 | 39.85 | +0.84 | rusty_faster | rusty_higher_psnr |
| Bricks097_NormalGL__bc5u | normal | 4991 | 23112 | 0.216 | 44.42 | 43.81 | +0.60 | rusty_faster | rusty_higher_psnr |
| Bricks097_NormalGL__bc5s | normal | 16349 | 24248 | 0.674 | 44.37 | 43.92 | +0.45 | rusty_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4u | mask | 3039 | 13813 | 0.220 | 43.01 | 42.42 | +0.59 | rusty_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4s | mask | 8639 | 13570 | 0.637 | 43.05 | 42.54 | +0.51 | rusty_faster | rusty_higher_psnr |
| Metal063_Color__bc1 | albedo | 8459 | 32279 | 0.262 | 40.23 | 38.93 | +1.31 | rusty_faster | rusty_higher_psnr |
| Metal063_Color__bc7 | albedo | 11065 | 11552700 | 0.001 | 47.42 | 47.00 | +0.42 | rusty_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5u | normal | 4428 | 41595 | 0.106 | ∞ | 99.04 | — | rusty_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5s | normal | 3727 | 39308 | 0.095 | 51.14 | 51.14 | -0.00 | rusty_faster | tie |
| Metal063_Roughness__bc4u | mask | 5033 | 27855 | 0.181 | 46.12 | 45.36 | +0.76 | rusty_faster | rusty_higher_psnr |
| Metal063_Roughness__bc4s | mask | 21942 | 31950 | 0.687 | 46.29 | 45.59 | +0.69 | rusty_faster | rusty_higher_psnr |
| Rock064_Color__bc1 | albedo | 9160 | 41674 | 0.220 | 34.10 | 32.81 | +1.30 | rusty_faster | rusty_higher_psnr |
| Rock064_Color__bc7 | albedo | 15476 | 14711600 | 0.001 | 39.48 | 38.28 | +1.20 | rusty_faster | rusty_higher_psnr |
| Rock064_NormalGL__bc5u | normal | 7080 | 46259 | 0.153 | 46.76 | 46.02 | +0.74 | rusty_faster | rusty_higher_psnr |
| Rock064_NormalGL__bc5s | normal | 42999 | 59869 | 0.718 | 46.89 | 46.19 | +0.69 | rusty_faster | rusty_higher_psnr |
| Rock064_Roughness__bc4u | mask | 4934 | 27045 | 0.182 | 47.05 | 46.20 | +0.86 | rusty_faster | rusty_higher_psnr |
| Rock064_Roughness__bc4s | mask | 20425 | 34166 | 0.598 | 47.16 | 46.44 | +0.72 | rusty_faster | rusty_higher_psnr |
| Wood095_Color__bc1 | albedo | 4997 | 15805 | 0.316 | 41.70 | 40.41 | +1.29 | rusty_faster | rusty_higher_psnr |
| Wood095_Color__bc7 | albedo | 5880 | 5408900 | 0.001 | 49.28 | 48.56 | +0.72 | rusty_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5u | normal | 3414 | 25786 | 0.132 | 53.61 | 52.02 | +1.59 | rusty_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5s | normal | 19710 | 26711 | 0.738 | 52.60 | 52.69 | -0.10 | rusty_faster | tie |
| Wood095_Roughness__bc4u | mask | 2916 | 13729 | 0.212 | 54.53 | 52.63 | +1.89 | rusty_faster | rusty_higher_psnr |
| Wood095_Roughness__bc4s | mask | 7155 | 14145 | 0.506 | 55.14 | 54.13 | +1.01 | rusty_faster | rusty_higher_psnr |
