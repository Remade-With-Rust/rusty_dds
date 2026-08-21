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
| Bricks097_Color__bc1 | albedo | 3416 | 15700 | 0.218 | 34.91 | 34.28 | +0.63 | rusty_faster | rusty_higher_psnr |
| Bricks097_Color__bc7 | albedo | 5364 | 5065030 | 0.001 | 40.68 | 39.85 | +0.84 | rusty_faster | rusty_higher_psnr |
| Bricks097_NormalGL__bc5u | normal | 2857 | 19077 | 0.150 | 44.42 | 43.81 | +0.60 | rusty_faster | rusty_higher_psnr |
| Bricks097_NormalGL__bc5s | normal | 11911 | 19738 | 0.603 | 44.37 | 43.92 | +0.45 | rusty_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4u | mask | 2612 | 10794 | 0.242 | 43.01 | 42.42 | +0.59 | rusty_faster | rusty_higher_psnr |
| Bricks097_Roughness__bc4s | mask | 6905 | 11399 | 0.606 | 43.05 | 42.54 | +0.51 | rusty_faster | rusty_higher_psnr |
| Metal063_Color__bc1 | albedo | 6530 | 27272 | 0.239 | 40.23 | 38.93 | +1.31 | rusty_faster | rusty_higher_psnr |
| Metal063_Color__bc7 | albedo | 8857 | 9113880 | 0.001 | 47.42 | 47.00 | +0.42 | rusty_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5u | normal | 3536 | 34117 | 0.104 | ∞ | 99.04 | — | rusty_faster | rusty_higher_psnr |
| Metal063_NormalGL__bc5s | normal | 4292 | 27968 | 0.153 | 51.14 | 51.14 | -0.00 | rusty_faster | tie |
| Metal063_Roughness__bc4u | mask | 3720 | 22453 | 0.166 | 46.12 | 45.36 | +0.76 | rusty_faster | rusty_higher_psnr |
| Metal063_Roughness__bc4s | mask | 17343 | 25418 | 0.682 | 46.29 | 45.59 | +0.69 | rusty_faster | rusty_higher_psnr |
| Rock064_Color__bc1 | albedo | 6636 | 32938 | 0.201 | 34.10 | 32.81 | +1.30 | rusty_faster | rusty_higher_psnr |
| Rock064_Color__bc7 | albedo | 11089 | 10290900 | 0.001 | 39.48 | 38.28 | +1.20 | rusty_faster | rusty_higher_psnr |
| Rock064_NormalGL__bc5u | normal | 4180 | 37995 | 0.110 | 46.76 | 46.02 | +0.74 | rusty_faster | rusty_higher_psnr |
| Rock064_NormalGL__bc5s | normal | 23480 | 40376 | 0.582 | 46.89 | 46.19 | +0.69 | rusty_faster | rusty_higher_psnr |
| Rock064_Roughness__bc4u | mask | 3244 | 20442 | 0.159 | 47.05 | 46.20 | +0.86 | rusty_faster | rusty_higher_psnr |
| Rock064_Roughness__bc4s | mask | 13448 | 21856 | 0.615 | 47.16 | 46.44 | +0.72 | rusty_faster | rusty_higher_psnr |
| Wood095_Color__bc1 | albedo | 2631 | 12100 | 0.217 | 41.70 | 40.41 | +1.29 | rusty_faster | rusty_higher_psnr |
| Wood095_Color__bc7 | albedo | 3742 | 4026870 | 0.001 | 49.28 | 48.56 | +0.72 | rusty_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5u | normal | 2273 | 18210 | 0.125 | 53.61 | 52.02 | +1.59 | rusty_faster | rusty_higher_psnr |
| Wood095_NormalGL__bc5s | normal | 11784 | 19902 | 0.592 | 52.60 | 52.69 | -0.10 | rusty_faster | tie |
| Wood095_Roughness__bc4u | mask | 1797 | 10682 | 0.168 | 54.53 | 52.63 | +1.89 | rusty_faster | rusty_higher_psnr |
| Wood095_Roughness__bc4s | mask | 4988 | 10723 | 0.465 | 55.14 | 54.13 | +1.01 | rusty_faster | rusty_higher_psnr |
