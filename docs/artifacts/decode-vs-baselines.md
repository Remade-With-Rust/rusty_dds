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
| Bricks097_Color__bc1 | bc1 | albedo | 860940 | 7887980 | 0.109 |
| Bricks097_Color__bc7 | bc7 | albedo | 2121300 | 13772800 | 0.154 |
| Bricks097_NormalGL__bc5u | bc5u | normal | 929995 | 11340700 | 0.082 |
| Bricks097_NormalGL__bc5s | bc5s | normal | 795165 | 11582200 | 0.069 |
| Bricks097_Roughness__bc4u | bc4u | mask | 741675 | 7948520 | 0.093 |
| Bricks097_Roughness__bc4s | bc4s | mask | 753635 | 8681690 | 0.087 |
| Metal063_Color__bc1 | bc1 | albedo | 1302570 | 15155100 | 0.086 |
| Metal063_Color__bc7 | bc7 | albedo | 2718205 | 21935800 | 0.124 |
| Metal063_NormalGL__bc5u | bc5u | normal | 2079270 | 19992000 | 0.104 |
| Metal063_NormalGL__bc5s | bc5s | normal | 2103565 | 14053600 | 0.150 |
| Metal063_Roughness__bc4u | bc4u | mask | 1417045 | 16093500 | 0.088 |
| Metal063_Roughness__bc4s | bc4s | mask | 1335700 | 16778000 | 0.080 |
| Rock064_Color__bc1 | bc1 | albedo | 1392455 | 14629400 | 0.095 |
| Rock064_Color__bc7 | bc7 | albedo | 2931730 | 24914700 | 0.118 |
| Rock064_NormalGL__bc5u | bc5u | normal | 1930610 | 24371800 | 0.079 |
| Rock064_NormalGL__bc5s | bc5s | normal | 1909530 | 22461200 | 0.085 |
| Rock064_Roughness__bc4u | bc4u | mask | 1476330 | 16216700 | 0.091 |
| Rock064_Roughness__bc4s | bc4s | mask | 1395955 | 16852100 | 0.083 |
| Wood095_Color__bc1 | bc1 | albedo | 869760 | 7058380 | 0.123 |
| Wood095_Color__bc7 | bc7 | albedo | 2569330 | 12169700 | 0.211 |
| Wood095_NormalGL__bc5u | bc5u | normal | 1103855 | 10252200 | 0.108 |
| Wood095_NormalGL__bc5s | bc5s | normal | 1243500 | 11038100 | 0.113 |
| Wood095_Roughness__bc4u | bc4u | mask | 838335 | 8097710 | 0.104 |
| Wood095_Roughness__bc4s | bc4s | mask | 920210 | 8448860 | 0.109 |
