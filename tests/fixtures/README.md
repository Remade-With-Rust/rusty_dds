# DDS fixtures

Committed binary fixtures for parse / surface / decode tests and Criterion benches.

## Contents

| File | Format | Context notes |
|------|--------|---------------|
| `dxt1_64x64.dds` | D3D DXT1 | X-2D |
| `dxt3_32x32.dds` | D3D DXT3 | X-2D |
| `dxt5_64x64.dds` | D3D DXT5 | X-2D |
| `dxt1_64x64_mips.dds` | D3D DXT1 | X-MIP (7 levels) |
| `bc1_64x64_dx10.dds` | DXGI BC1_UNorm | X-2D |
| `bc2_32x32_dx10.dds` | DXGI BC2_UNorm | X-2D |
| `bc3_64x64_dx10.dds` | DXGI BC3_UNorm | X-2D |
| `bc4_32x32_dx10.dds` | DXGI BC4_UNorm | X-2D |
| `bc5_32x32_dx10.dds` | DXGI BC5_UNorm | X-2D |
| `bc7_32x32_dx10.dds` | DXGI BC7_UNorm | X-2D |
| `rgba8_64x64.dds` | DXGI R8G8B8A8_UNorm | X-2D |
| `bgra8_32x32.dds` | DXGI B8G8R8A8_UNorm | X-2D |
| `rgba8_256x256_mips.dds` | DXGI R8G8B8A8_UNorm | X-MIP |
| `rgba8_32x32_array3.dds` | DXGI R8G8B8A8_UNorm | X-ARRAY |
| `bc1_32x32_cube.dds` | DXGI BC1_UNorm | X-CUBE (6 faces, 2 mips) |
| `rgba8_16x16x4_vol.dds` | DXGI R8G8B8A8_UNorm | X-VOL |
| `bc1_16x16x4_vol.dds` | DXGI BC1_UNorm | X-VOL |

Payload bytes are a deterministic `(i % 251)` pattern — valid container size. Decode
tests that need meaningful BCn texels synthesize blocks in-process; the matrix still
asserts bit-exact agreement with `bcdec_rs` on whatever bytes are present.

## Provenance

```text
cargo run --example gen_fixtures
```
