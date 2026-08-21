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
| Bricks097_Color__bc1 | bc1 | albedo | 548765 | 5591810 | 0.098 |
| Bricks097_Color__bc7 | bc7 | albedo | 1457535 | 9489360 | 0.154 |
| Bricks097_NormalGL__bc5u | bc5u | normal | 664380 | 8342780 | 0.080 |
| Bricks097_NormalGL__bc5s | bc5s | normal | 668195 | 9151480 | 0.073 |
| Bricks097_Roughness__bc4u | bc4u | mask | 475460 | 5951800 | 0.080 |
| Bricks097_Roughness__bc4s | bc4s | mask | 566970 | 6788900 | 0.084 |
| Metal063_Color__bc1 | bc1 | albedo | 1065515 | 11979400 | 0.089 |
| Metal063_Color__bc7 | bc7 | albedo | 2004120 | 18625900 | 0.108 |
| Metal063_NormalGL__bc5u | bc5u | normal | 1401810 | 16835700 | 0.083 |
| Metal063_NormalGL__bc5s | bc5s | normal | 1437310 | 10700300 | 0.134 |
| Metal063_Roughness__bc4u | bc4u | mask | 906785 | 11800500 | 0.077 |
| Metal063_Roughness__bc4s | bc4s | mask | 1014250 | 12478000 | 0.081 |
| Rock064_Color__bc1 | bc1 | albedo | 1087625 | 10918200 | 0.100 |
| Rock064_Color__bc7 | bc7 | albedo | 2005955 | 17817700 | 0.113 |
| Rock064_NormalGL__bc5u | bc5u | normal | 1494195 | 15548300 | 0.096 |
| Rock064_NormalGL__bc5s | bc5s | normal | 1630855 | 16740400 | 0.097 |
| Rock064_Roughness__bc4u | bc4u | mask | 1039950 | 11831800 | 0.088 |
| Rock064_Roughness__bc4s | bc4s | mask | 950950 | 12974200 | 0.073 |
| Wood095_Color__bc1 | bc1 | albedo | 536195 | 5608570 | 0.096 |
| Wood095_Color__bc7 | bc7 | albedo | 1608325 | 9754440 | 0.165 |
| Wood095_NormalGL__bc5u | bc5u | normal | 747765 | 7783650 | 0.096 |
| Wood095_NormalGL__bc5s | bc5s | normal | 671115 | 8969180 | 0.075 |
| Wood095_Roughness__bc4u | bc4u | mask | 496685 | 6376600 | 0.078 |
| Wood095_Roughness__bc4s | bc4s | mask | 535990 | 6887840 | 0.078 |
