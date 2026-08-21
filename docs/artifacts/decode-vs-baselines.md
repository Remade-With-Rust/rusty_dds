# Decode (corpus) vs Microsoft DirectXTex

- Primary decode baseline: ambientCG proxy corpus (~1024^2), not synthetic X-grid.
- Same DDS bytes for both peers (encoded by rusty_dds from corpus PNGs).
- Roles: albedo / normal / mask. ratio < 1 => rusty_dds faster.
- DirectXTex decode included

## Summary

```json
{
  "cases": 24,
  "vs_directxtex": {
    "ahead": 24,
    "behind": 0,
    "peer_ok_cases": 24
  }
}
```

| Case | Content | Role | rusty_dds (ns) | DirectXTex (ns) | Ratio (rusty/dx) |
|------|---------|------|----------------|-----------------|------------------|
| Bricks097_Color__bc1 | bc1 | albedo | 1013820 | 10071200 | 0.101 |
| Bricks097_Color__bc7 | bc7 | albedo | 2278395 | 19496200 | 0.117 |
| Bricks097_NormalGL__bc5u | bc5u | normal | 1071070 | 14582700 | 0.073 |
| Bricks097_NormalGL__bc5s | bc5s | normal | 1275190 | 14620800 | 0.087 |
| Bricks097_Roughness__bc4u | bc4u | mask | 887465 | 11738400 | 0.076 |
| Bricks097_Roughness__bc4s | bc4s | mask | 884600 | 12786400 | 0.069 |
| Metal063_Color__bc1 | bc1 | albedo | 1826735 | 21306300 | 0.086 |
| Metal063_Color__bc7 | bc7 | albedo | 10180710 | 36122900 | 0.282 |
| Metal063_NormalGL__bc5u | bc5u | normal | 3384275 | 23923300 | 0.141 |
| Metal063_NormalGL__bc5s | bc5s | normal | 2736780 | 19490600 | 0.140 |
| Metal063_Roughness__bc4u | bc4u | mask | 2027090 | 18916600 | 0.107 |
| Metal063_Roughness__bc4s | bc4s | mask | 3782770 | 19694200 | 0.192 |
| Rock064_Color__bc1 | bc1 | albedo | 2360695 | 20610600 | 0.115 |
| Rock064_Color__bc7 | bc7 | albedo | 7984340 | 53302200 | 0.150 |
| Rock064_NormalGL__bc5u | bc5u | normal | 2960400 | 28322400 | 0.105 |
| Rock064_NormalGL__bc5s | bc5s | normal | 4707135 | 29696800 | 0.159 |
| Rock064_Roughness__bc4u | bc4u | mask | 2960840 | 22514500 | 0.132 |
| Rock064_Roughness__bc4s | bc4s | mask | 3595255 | 24037900 | 0.150 |
| Wood095_Color__bc1 | bc1 | albedo | 1194975 | 9983600 | 0.120 |
| Wood095_Color__bc7 | bc7 | albedo | 8611040 | 23267400 | 0.370 |
| Wood095_NormalGL__bc5u | bc5u | normal | 1273780 | 16979100 | 0.075 |
| Wood095_NormalGL__bc5s | bc5s | normal | 1750775 | 14773200 | 0.119 |
| Wood095_Roughness__bc4u | bc4u | mask | 1144665 | 11915500 | 0.096 |
| Wood095_Roughness__bc4s | bc4s | mask | 1240700 | 14784200 | 0.084 |
