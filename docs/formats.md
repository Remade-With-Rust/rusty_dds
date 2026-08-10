# Format support catalog

`Error::UnsupportedFormat` is returned when a format is outside the LDR matrix
below. Fail closed — never invent pixels.

## Supported (LDR matrix)

Same IDs as decode/encode completeness tests.

| Content | DXGI / D3D | Decode | Encode | Compressed upload |
|---------|------------|:------:|:------:|:-----------------:|
| C-BC1 | `BC1_*` / DXT1 | ✅ | ✅ | ✅ |
| C-BC2 | `BC2_*` / DXT2–3 | ✅ | ✅ | ✅ |
| C-BC3 | `BC3_*` / DXT4–5 | ✅ | ✅ | ✅ |
| C-BC4U | `BC4_UNorm` | ✅ | ✅ | ✅ |
| C-BC4S | `BC4_SNorm` | ✅ | ✅ | ✅ |
| C-BC5U | `BC5_UNorm` | ✅ | ✅ | ✅ |
| C-BC5S | `BC5_SNorm` | ✅ | ✅ | ✅ |
| C-BC7 | `BC7_*` | ✅ | ✅ (mode 6) | ✅ |
| C-RGBA | `R8G8B8A8_*` | ✅ | ✅ | ✅ |
| C-BGRA | `B8G8R8A8_*` | ✅ | ✅ | ✅ |

**sRGB:** `_sRGB` tags keep **stored** channel bytes (no linearization) on decode;
GPU names map to `*Srgb` variants in [`GpuFormat`](../src/upload.rs).

## Explicitly unsupported (today)

| Format family | Status |
|---------------|--------|
| BC6H (HDR float) | deferred — needs `ImageRgbaf32` |
| ASTC / ETC / PVRTC | out of scope (not DDS-primary desktop BCn) |
| Packed RGB10A2, R16G16, float RGBA, etc. | container may parse; no LDR decode/encode |
| Legacy D3D uncompressed bitmasks beyond DXGI map | `UnsupportedFormat` on decode/encode |
| Video / YUV DXGI formats | unsupported |

## Contexts (all supported for the table above)

X-2D · X-MIP · X-ARRAY · X-CUBE · X-NPOT · X-VOL — see
[texture-pipeline.md](plans/texture-pipeline.md) Phase 2b / 4.

## Feature gates

| Cargo feature | Effect |
|---------------|--------|
| `decode` (default) | `decode_rgba8` + `bcdec_rs` |
| `encode` (default) | `encode_from_rgba8` |
| _(none)_ | container + surfaces + `UploadPlan` + `decode_content()` classification |
