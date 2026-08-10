# Encode (corpus) vs Microsoft DirectXTex

- Primary encode baseline: ambientCG proxy corpus (not synthetic X-2D gradients).
- Roles: albedo->BC1/BC7, normal->BC5U/S, mask->BC4U/S. ~1024^2 CC0 maps.
- BC7 peer flag: TEX_COMPRESS_BC7_QUICK. ratio < 1 => rusty_dds faster.
- Source harvest: corpus-vs-directxtex.json / harvest_corpus_vs_dxtex.

## Summary

```json
{
  "cases": 24,
  "vs_directxtex": {
    "ahead": 24,
    "behind": 0,
    "tie": 0,
    "peer_ok_cases": 24
  }
}
```

| Case | Content | Role | rusty_dds (ns) | DirectXTex (ns) | Ratio (rusty/dx) |
|------|---------|------|----------------|-----------------|------------------|
| Bricks097_Color__bc1 | bc1 | albedo | 2065600 | 15006900 | 0.138 |
| Bricks097_Color__bc7 | bc7 | albedo | 23863200 | 4618520000 | 0.005 |
| Bricks097_NormalGL__bc5u | bc5u | normal | 4375000 | 18117000 | 0.241 |
| Bricks097_NormalGL__bc5s | bc5s | normal | 5136700 | 19419300 | 0.265 |
| Bricks097_Roughness__bc4u | bc4u | mask | 3376600 | 11005400 | 0.307 |
| Bricks097_Roughness__bc4s | bc4s | mask | 3512300 | 11658100 | 0.301 |
| Metal063_Color__bc1 | bc1 | albedo | 2606900 | 26652000 | 0.098 |
| Metal063_Color__bc7 | bc7 | albedo | 42334100 | 8219840000 | 0.005 |
| Metal063_NormalGL__bc5u | bc5u | normal | 3924200 | 31329700 | 0.125 |
| Metal063_NormalGL__bc5s | bc5s | normal | 2662700 | 27313200 | 0.097 |
| Metal063_Roughness__bc4u | bc4u | mask | 6605600 | 21492400 | 0.307 |
| Metal063_Roughness__bc4s | bc4s | mask | 6246200 | 22186500 | 0.282 |
| Rock064_Color__bc1 | bc1 | albedo | 3067800 | 30917700 | 0.099 |
| Rock064_Color__bc7 | bc7 | albedo | 53462800 | 9966950000 | 0.005 |
| Rock064_NormalGL__bc5u | bc5u | normal | 7232400 | 37626300 | 0.192 |
| Rock064_NormalGL__bc5s | bc5s | normal | 9138600 | 41713300 | 0.219 |
| Rock064_Roughness__bc4u | bc4u | mask | 5734400 | 21235300 | 0.270 |
| Rock064_Roughness__bc4s | bc4s | mask | 5433900 | 23213600 | 0.234 |
| Wood095_Color__bc1 | bc1 | albedo | 1819700 | 12500500 | 0.146 |
| Wood095_Color__bc7 | bc7 | albedo | 23347400 | 4139260000 | 0.006 |
| Wood095_NormalGL__bc5u | bc5u | normal | 2901600 | 18788000 | 0.154 |
| Wood095_NormalGL__bc5s | bc5s | normal | 3415800 | 19434000 | 0.176 |
| Wood095_Roughness__bc4u | bc4u | mask | 2064700 | 10633300 | 0.194 |
| Wood095_Roughness__bc4s | bc4s | mask | 2034900 | 11327400 | 0.180 |
