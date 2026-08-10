# Encode quality (corpus): rusty_dds vs DirectXTex

- Primary quality baseline: ambientCG proxy corpus.
- delta = rusty_psnr - directxtex_psnr. +/-0.25 dB deadband for tie.
- Source harvest: corpus-vs-directxtex.json.

## Summary

```json
{
  "cases": 24,
  "compared": 24,
  "rusty_higher_psnr": 16,
  "directxtex_higher_psnr": 3,
  "tie": 5
}
```

| Case | Content | Role | rusty PSNR | DirectXTex PSNR | Δ (rusty−dx) | Verdict |
|------|---------|------|------------|-----------------|--------------|---------|
| Bricks097_Color__bc1 | bc1 | albedo | 32.92 | 34.28 | -1.36 | directxtex_higher_psnr |
| Bricks097_Color__bc7 | bc7 | albedo | 40.08 | 39.85 | +0.23 | tie |
| Bricks097_NormalGL__bc5u | bc5u | normal | 44.41 | 43.81 | +0.60 | rusty_higher_psnr |
| Bricks097_NormalGL__bc5s | bc5s | normal | 44.32 | 43.92 | +0.39 | rusty_higher_psnr |
| Bricks097_Roughness__bc4u | bc4u | mask | 43.01 | 42.42 | +0.59 | rusty_higher_psnr |
| Bricks097_Roughness__bc4s | bc4s | mask | 43.00 | 42.54 | +0.45 | rusty_higher_psnr |
| Metal063_Color__bc1 | bc1 | albedo | 39.00 | 38.93 | +0.07 | tie |
| Metal063_Color__bc7 | bc7 | albedo | 47.35 | 47.00 | +0.35 | rusty_higher_psnr |
| Metal063_NormalGL__bc5u | bc5u | normal | ∞ | 99.04 | — | rusty_higher_psnr |
| Metal063_NormalGL__bc5s | bc5s | normal | 51.14 | 51.14 | -0.00 | tie |
| Metal063_Roughness__bc4u | bc4u | mask | 46.12 | 45.36 | +0.76 | rusty_higher_psnr |
| Metal063_Roughness__bc4s | bc4s | mask | 46.12 | 45.59 | +0.53 | rusty_higher_psnr |
| Rock064_Color__bc1 | bc1 | albedo | 32.37 | 32.81 | -0.44 | directxtex_higher_psnr |
| Rock064_Color__bc7 | bc7 | albedo | 38.40 | 38.28 | +0.12 | tie |
| Rock064_NormalGL__bc5u | bc5u | normal | 46.76 | 46.02 | +0.74 | rusty_higher_psnr |
| Rock064_NormalGL__bc5s | bc5s | normal | 46.72 | 46.19 | +0.53 | rusty_higher_psnr |
| Rock064_Roughness__bc4u | bc4u | mask | 47.05 | 46.20 | +0.86 | rusty_higher_psnr |
| Rock064_Roughness__bc4s | bc4s | mask | 46.99 | 46.44 | +0.56 | rusty_higher_psnr |
| Wood095_Color__bc1 | bc1 | albedo | 40.31 | 40.41 | -0.10 | tie |
| Wood095_Color__bc7 | bc7 | albedo | 48.99 | 48.56 | +0.43 | rusty_higher_psnr |
| Wood095_NormalGL__bc5u | bc5u | normal | 53.61 | 52.02 | +1.59 | rusty_higher_psnr |
| Wood095_NormalGL__bc5s | bc5s | normal | 52.18 | 52.69 | -0.51 | directxtex_higher_psnr |
| Wood095_Roughness__bc4u | bc4u | mask | 54.53 | 52.63 | +1.89 | rusty_higher_psnr |
| Wood095_Roughness__bc4s | bc4s | mask | 54.53 | 54.13 | +0.40 | rusty_higher_psnr |
