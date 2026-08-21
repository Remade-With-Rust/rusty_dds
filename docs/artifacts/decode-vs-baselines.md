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
| Bricks097_Color__bc1 | bc1 | albedo | 487400 | 5043140 | 0.097 |
| Bricks097_Color__bc7 | bc7 | albedo | 1088785 | 8185150 | 0.133 |
| Bricks097_NormalGL__bc5u | bc5u | normal | 571955 | 7271800 | 0.079 |
| Bricks097_NormalGL__bc5s | bc5s | normal | 619760 | 7702170 | 0.080 |
| Bricks097_Roughness__bc4u | bc4u | mask | 454030 | 5587780 | 0.081 |
| Bricks097_Roughness__bc4s | bc4s | mask | 459350 | 5916510 | 0.078 |
| Metal063_Color__bc1 | bc1 | albedo | 1092075 | 10637500 | 0.103 |
| Metal063_Color__bc7 | bc7 | albedo | 1714570 | 16634200 | 0.103 |
| Metal063_NormalGL__bc5u | bc5u | normal | 1387635 | 15361000 | 0.090 |
| Metal063_NormalGL__bc5s | bc5s | normal | 1289095 | 9602540 | 0.134 |
| Metal063_Roughness__bc4u | bc4u | mask | 1020400 | 10653200 | 0.096 |
| Metal063_Roughness__bc4s | bc4s | mask | 1367970 | 11742700 | 0.116 |
| Rock064_Color__bc1 | bc1 | albedo | 1083365 | 10491900 | 0.103 |
| Rock064_Color__bc7 | bc7 | albedo | 1643290 | 16374700 | 0.100 |
| Rock064_NormalGL__bc5u | bc5u | normal | 1373555 | 14448200 | 0.095 |
| Rock064_NormalGL__bc5s | bc5s | normal | 1481270 | 15910600 | 0.093 |
| Rock064_Roughness__bc4u | bc4u | mask | 882595 | 10926100 | 0.081 |
| Rock064_Roughness__bc4s | bc4s | mask | 1072260 | 12058100 | 0.089 |
| Wood095_Color__bc1 | bc1 | albedo | 561615 | 4813880 | 0.117 |
| Wood095_Color__bc7 | bc7 | albedo | 1377285 | 8626480 | 0.160 |
| Wood095_NormalGL__bc5u | bc5u | normal | 591020 | 7259520 | 0.081 |
| Wood095_NormalGL__bc5s | bc5s | normal | 762915 | 7997540 | 0.095 |
| Wood095_Roughness__bc4u | bc4u | mask | 537825 | 5711060 | 0.094 |
| Wood095_Roughness__bc4s | bc4s | mask | 538260 | 6076320 | 0.089 |
