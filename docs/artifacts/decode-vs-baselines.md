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
| Bricks097_Color__bc1 | bc1 | albedo | 811865 | 4641320 | 0.175 |
| Bricks097_Color__bc7 | bc7 | albedo | 1093310 | 7975910 | 0.137 |
| Bricks097_NormalGL__bc5u | bc5u | normal | 1356855 | 6745990 | 0.201 |
| Bricks097_NormalGL__bc5s | bc5s | normal | 1532480 | 9923400 | 0.154 |
| Bricks097_Roughness__bc4u | bc4u | mask | 1145960 | 5061600 | 0.226 |
| Bricks097_Roughness__bc4s | bc4s | mask | 1126870 | 6052840 | 0.186 |
| Metal063_Color__bc1 | bc1 | albedo | 1684245 | 9866790 | 0.171 |
| Metal063_Color__bc7 | bc7 | albedo | 1934710 | 16227900 | 0.119 |
| Metal063_NormalGL__bc5u | bc5u | normal | 2865025 | 13875800 | 0.206 |
| Metal063_NormalGL__bc5s | bc5s | normal | 2938380 | 14630400 | 0.201 |
| Metal063_Roughness__bc4u | bc4u | mask | 2210545 | 10794800 | 0.205 |
| Metal063_Roughness__bc4s | bc4s | mask | 2158130 | 11077600 | 0.195 |
| Rock064_Color__bc1 | bc1 | albedo | 1676935 | 9364020 | 0.179 |
| Rock064_Color__bc7 | bc7 | albedo | 1924235 | 15811800 | 0.122 |
| Rock064_NormalGL__bc5u | bc5u | normal | 3035205 | 13641500 | 0.222 |
| Rock064_NormalGL__bc5s | bc5s | normal | 3214200 | 17779800 | 0.181 |
| Rock064_Roughness__bc4u | bc4u | mask | 2261890 | 10363100 | 0.218 |
| Rock064_Roughness__bc4s | bc4s | mask | 2263555 | 12179800 | 0.186 |
| Wood095_Color__bc1 | bc1 | albedo | 853995 | 4413700 | 0.193 |
| Wood095_Color__bc7 | bc7 | albedo | 1344470 | 7794110 | 0.172 |
| Wood095_NormalGL__bc5u | bc5u | normal | 1441470 | 6710920 | 0.215 |
| Wood095_NormalGL__bc5s | bc5s | normal | 1428210 | 9221120 | 0.155 |
| Wood095_Roughness__bc4u | bc4u | mask | 1131950 | 5203360 | 0.218 |
| Wood095_Roughness__bc4s | bc4s | mask | 1112115 | 5728180 | 0.194 |
