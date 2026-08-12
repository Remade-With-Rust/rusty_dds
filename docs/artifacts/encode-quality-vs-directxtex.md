# Encode quality: rusty_dds vs DirectXTex (same sources)

- Identical fill_rgba sources as encode-quality / bench_baselines.
- DirectXTex via dxtex_roundtrip: Compress|Convert then Decompress|Convert.
- BC7 peer flag: TEX_COMPRESS_BC7_QUICK (mode-6 class).
- BC4: R only; BC5: R+G; BC1: RGB; else full RGBA. RGBA/BGRA: bit-exact preferred.
- verdict uses ±0.25 dB deadband for tie.
- Δ = rusty_psnr − directxtex_psnr (positive ⇒ rusty_dds higher fidelity on this source).

## Summary

```json
{
  "cases": 54,
  "compared": 54,
  "directxtex_higher_psnr": 17,
  "rusty_higher_psnr": 12,
  "tie": 25
}
```

| Case | Content | Context | rusty PSNR | DirectXTex PSNR | Δ (rusty−dx) | Verdict |
|------|---------|---------|------------|-----------------|--------------|---------|
| bc1__X-2D | bc1 | X-2D | 33.28 | 33.18 | +0.10 | tie |
| bc2__X-2D | bc2 | X-2D | 32.30 | 33.44 | -1.14 | directxtex_higher_psnr |
| bc3__X-2D | bc3 | X-2D | 34.52 | 34.43 | +0.09 | tie |
| bc4u__X-2D | bc4u | X-2D | 51.42 | 51.14 | +0.28 | rusty_higher_psnr |
| bc4s__X-2D | bc4s | X-2D | 60.17 | 50.63 | +9.54 | rusty_higher_psnr |
| bc5u__X-2D | bc5u | X-2D | 51.42 | 51.14 | +0.28 | rusty_higher_psnr |
| bc5s__X-2D | bc5s | X-2D | 60.17 | 50.63 | +9.54 | rusty_higher_psnr |
| bc7__X-2D | bc7 | X-2D | 35.09 | 35.13 | -0.04 | tie |
| rgba8__X-2D | rgba8 | X-2D | ∞ | ∞ | — | tie_exact |
| bgra8__X-2D | bgra8 | X-2D | ∞ | ∞ | — | tie_exact |
| bc1__X-MIP | bc1 | X-MIP | 15.49 | 15.84 | -0.35 | directxtex_higher_psnr |
| bc2__X-MIP | bc2 | X-MIP | 16.74 | 17.09 | -0.35 | directxtex_higher_psnr |
| bc3__X-MIP | bc3 | X-MIP | 16.74 | 17.09 | -0.35 | directxtex_higher_psnr |
| bc4u__X-MIP | bc4u | X-MIP | 35.12 | 35.96 | -0.84 | directxtex_higher_psnr |
| bc4s__X-MIP | bc4s | X-MIP | 36.59 | 35.89 | +0.70 | rusty_higher_psnr |
| bc5u__X-MIP | bc5u | X-MIP | 35.12 | 35.96 | -0.84 | directxtex_higher_psnr |
| bc5s__X-MIP | bc5s | X-MIP | 36.59 | 35.89 | +0.70 | rusty_higher_psnr |
| bc7__X-MIP | bc7 | X-MIP | 17.08 | 17.06 | +0.03 | tie |
| rgba8__X-MIP | rgba8 | X-MIP | ∞ | ∞ | — | tie_exact |
| bgra8__X-MIP | bgra8 | X-MIP | ∞ | ∞ | — | tie_exact |
| bc1__X-ARRAY | bc1 | X-ARRAY | 27.44 | 27.79 | -0.36 | directxtex_higher_psnr |
| bc2__X-ARRAY | bc2 | X-ARRAY | 28.07 | 28.73 | -0.67 | directxtex_higher_psnr |
| bc3__X-ARRAY | bc3 | X-ARRAY | 28.68 | 29.04 | -0.36 | directxtex_higher_psnr |
| bc4u__X-ARRAY | bc4u | X-ARRAY | 51.14 | 46.95 | +4.19 | rusty_higher_psnr |
| bc4s__X-ARRAY | bc4s | X-ARRAY | 49.76 | 49.76 | +0.00 | tie |
| bc5u__X-ARRAY | bc5u | X-ARRAY | 51.14 | 46.95 | +4.19 | rusty_higher_psnr |
| bc5s__X-ARRAY | bc5s | X-ARRAY | 49.76 | 49.76 | +0.00 | tie |
| bc7__X-ARRAY | bc7 | X-ARRAY | 29.14 | 29.15 | -0.02 | tie |
| rgba8__X-ARRAY | rgba8 | X-ARRAY | ∞ | ∞ | — | tie_exact |
| bgra8__X-ARRAY | bgra8 | X-ARRAY | ∞ | ∞ | — | tie_exact |
| bc1__X-CUBE | bc1 | X-CUBE | 27.44 | 27.79 | -0.36 | directxtex_higher_psnr |
| bc3__X-CUBE | bc3 | X-CUBE | 28.68 | 29.04 | -0.36 | directxtex_higher_psnr |
| bc7__X-CUBE | bc7 | X-CUBE | 29.14 | 29.15 | -0.02 | tie |
| rgba8__X-CUBE | rgba8 | X-CUBE | ∞ | ∞ | — | tie_exact |
| bc1__X-NPOT | bc1 | X-NPOT | 16.21 | 16.56 | -0.35 | directxtex_higher_psnr |
| bc2__X-NPOT | bc2 | X-NPOT | 17.46 | 17.81 | -0.35 | directxtex_higher_psnr |
| bc3__X-NPOT | bc3 | X-NPOT | 17.46 | 17.81 | -0.35 | directxtex_higher_psnr |
| bc4u__X-NPOT | bc4u | X-NPOT | ∞ | ∞ | — | tie_exact |
| bc4s__X-NPOT | bc4s | X-NPOT | ∞ | ∞ | — | tie_exact |
| bc5u__X-NPOT | bc5u | X-NPOT | ∞ | ∞ | — | tie_exact |
| bc5s__X-NPOT | bc5s | X-NPOT | ∞ | ∞ | — | tie_exact |
| bc7__X-NPOT | bc7 | X-NPOT | 17.66 | 17.76 | -0.10 | tie |
| rgba8__X-NPOT | rgba8 | X-NPOT | ∞ | ∞ | — | tie_exact |
| bgra8__X-NPOT | bgra8 | X-NPOT | ∞ | ∞ | — | tie_exact |
| bc1__X-VOL | bc1 | X-VOL | 21.46 | 21.84 | -0.38 | directxtex_higher_psnr |
| bc2__X-VOL | bc2 | X-VOL | 22.54 | 23.03 | -0.49 | directxtex_higher_psnr |
| bc3__X-VOL | bc3 | X-VOL | 22.71 | 23.09 | -0.38 | directxtex_higher_psnr |
| bc4u__X-VOL | bc4u | X-VOL | 45.70 | 40.83 | +4.87 | rusty_higher_psnr |
| bc4s__X-VOL | bc4s | X-VOL | 43.01 | 41.48 | +1.53 | rusty_higher_psnr |
| bc5u__X-VOL | bc5u | X-VOL | 45.70 | 40.83 | +4.87 | rusty_higher_psnr |
| bc5s__X-VOL | bc5s | X-VOL | 43.01 | 41.48 | +1.53 | rusty_higher_psnr |
| bc7__X-VOL | bc7 | X-VOL | 23.10 | 23.11 | -0.01 | tie |
| rgba8__X-VOL | rgba8 | X-VOL | ∞ | ∞ | — | tie_exact |
| bgra8__X-VOL | bgra8 | X-VOL | ∞ | ∞ | — | tie_exact |
