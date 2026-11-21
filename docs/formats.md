# Format support catalog

`Error::UnsupportedFormat` is returned when a format is outside the matrices
below. Fail closed — never invent pixels.

## Supported (LDR matrix)

Same IDs as the decode/encode completeness tests.

| Content | DXGI / D3D | Decode | Encode | Compressed upload |
|---------|------------|:------:|:------:|:-----------------:|
| C-BC1 | `BC1_*` / DXT1 | ✅ | ✅ | ✅ |
| C-BC2 | `BC2_*` / DXT2–3 | ✅ | ✅ | ✅ |
| C-BC3 | `BC3_*` / DXT4–5 | ✅ | ✅ | ✅ |
| C-BC4U | `BC4_UNorm` | ✅ | ✅ | ✅ |
| C-BC4S | `BC4_SNorm` | ✅ | ✅ | ✅ |
| C-BC5U | `BC5_UNorm` | ✅ | ✅ | ✅ |
| C-BC5S | `BC5_SNorm` | ✅ | ✅ | ✅ |
| C-BC7 | `BC7_*` | ✅ | ✅ modes 1 / 4 / 5 / 6 | ✅ |
| C-RGBA | `R8G8B8A8_*` | ✅ | ✅ | ✅ |
| C-BGRA | `B8G8R8A8_*` | ✅ | ✅ | ✅ |

Legacy (non-DX10) headers whose masks name the same byte layouts classify the
same way: `A8B8G8R8` is C-RGBA and `A8R8G8B8` is C-BGRA for `decode_content`,
`decode_rgba8` and `decode_rgba_f32` (their upload plan is the decoded-RGBA8
path). The padded layouts `X8R8G8B8`, `X8B8G8R8` and `B8G8R8X8_UNORM` decode
through `decode_rgba8` too, with alpha forced to 255 whatever the padding byte
holds (DirectXTex and FFmpeg do the same); they are not encode targets, so
`decode_content` still refuses them.

BC7 encode picks per block: mode 6 is the always-tried baseline, modes 5 and 4
decouple colour and alpha indices (they win on content whose alpha gradient
disagrees with its colour gradient — UI, decals), and mode 1 adds two-subset
partitioning over a harvest-chosen 8-shape shortlist.

## Supported (HDR matrix)

| Content | DXGI | Decode | Encode | Compressed upload |
|---------|------|:------:|:------:|:-----------------:|
| C-BC6H-UF16 | `BC6H_UF16` | ✅ `decode_rgba_f32` | ✅ `encode_bc6h_uf16` (mode 11) | ✅ |
| C-BC6H-SF16 | `BC6H_SF16` | ✅ `decode_rgba_f32` | ❌ deferred | ✅ |
| C-BC6H-Typeless | `BC6H_Typeless` | ✅ `decode_rgba_f32` | ❌ | ✅ |

`decode_rgba8` refuses BC6H rather than tone-mapping it.

## Supported (f32 matrix, `decode_rgba_f32`)

`decode_rgba_f32` (and `_into`, `_with`, and the `decode_block_rows_f32_into`
seam) accepts **every** format in the two tables above plus the uncompressed
layouts below. Floats come back unclamped, UNORM in `0..=1`, SNORM in
`-1..=1` (most negative code clamped to `-1`). Missing channels read `0`,
missing alpha `1` — the GPU sampling convention; pass
`DecodeOptions::new().single_channel(SingleChannel::Grey)` to expand BC4 and
the one-channel formats to `(r, r, r, 1)` instead.

| Family | DXGI | Legacy D3D9 header |
|--------|------|--------------------|
| 32-bit float | `R32G32B32A32_FLOAT`, `R32G32B32_FLOAT`, `R32G32_FLOAT`, `R32_FLOAT` | `A32B32G32R32F`, `G32R32F`, `R32F` |
| 16-bit float | `R16G16B16A16_FLOAT`, `R16G16_FLOAT`, `R16_FLOAT` | `A16B16G16R16F`, `G16R16F`, `R16F` |
| 16-bit norm | `R16G16B16A16_UNORM/SNORM`, `R16G16_UNORM/SNORM`, `R16_UNORM/SNORM` | `A16B16G16R16`, `G16R16`, `L16` |
| 8-bit norm | `R8G8B8A8_SNORM`, `B8G8R8X8_UNORM`, `R8G8_UNORM/SNORM`, `R8_UNORM/SNORM`, `A8_UNORM` | `A8B8G8R8`, `A8R8G8B8`, `X8R8G8B8`, `X8B8G8R8`, `R8G8B8`, `A8`, `L8`, `A8L8`, `A4L4` |
| Packed | `R10G10B10A2_UNORM`, `R11G11B10_FLOAT`, `R9G9B9E5_SHAREDEXP`, `B5G6R5_UNORM`, `B5G5R5A1_UNORM`, `B4G4R4A4_UNORM` | `A2B10G10R10`, `A2R10G10B10`, `R5G6B5`, `A1R5G5B5`, `X1R5G5B5`, `A4R4G4B4`, `X4R4G4B4` |

BC4 and BC5 decode to `f32` straight from their endpoints, keeping the
interpolated levels the 8-bit path rounds. BC1/BC2/BC3/BC7/RGBA8/BGRA8 are
defined at 8 bits and come back as an exact `x / 255`. Typeless and integer
(`_UINT`/`_SINT`) layouts stay `UnsupportedFormat`: they have no defined float
value.

**sRGB:** `_sRGB` tags keep **stored** channel bytes (no linearization) on
decode; GPU names map to `*Srgb` variants in [`GpuFormat`](../src/upload.rs).

## Explicitly unsupported (today)

| Format family | Status |
|---------------|--------|
| ASTC / ETC / PVRTC | out of scope (not DDS-primary desktop BCn) |
| Uncompressed float / wide / packed formats (other than the 8-bit RGBA/BGRA/X layouts above) | `decode_rgba_f32` only (above); no RGBA8 decode, no encode |
| Typeless, `_UINT` / `_SINT`, `A8R3G3B2`, `Q16W16V16U16`, `CxV8U8` | `UnsupportedFormat` |
| Video / YUV DXGI formats | unsupported |

## Contexts (all supported for the tables above)

X-2D · X-MIP · X-ARRAY · X-CUBE · X-NPOT · X-VOL — see
[texture-pipeline.md](plans/texture-pipeline.md).

## Untrusted input

`Dds::read` reads the payload to end-of-stream with no cap, which is right for
a file on disk and wrong for bytes off a network or out of a mod archive. For
those, `Dds::read_limited(r, max_data_len)` fails closed with
`Error::SizeLimitExceeded` without buffering the overrun.

Every size computation downstream of the header uses checked arithmetic: a
header whose declared geometry cannot exist yields `UnsupportedFormat` or
`OutOfBounds`, never a wrapped size used to slice the payload. This is gated by
`tests/parser_robustness.rs` and by the `fuzz/` targets.

## Feature gates

| Cargo feature | Default | Effect |
|---------------|:-------:|--------|
| `decode` | ✅ | `decode_rgba8`, `decode_rgba_f32`, `bcdec_rs` |
| `encode` | ✅ | `encode_from_rgba8`, `encode_bc6h_uf16`, `EncodeLayout`, `Rdo` |
| `simd` | ✅ | AVX2 encode kernels; byte-identical output. Off ⇒ `forbid(unsafe_code)` |
| `tuning` | ❌ | **Development only.** Re-opens the frozen encoder constants to `RUSTY_DDS_*` environment overrides. Never enable in a shipped build — encoder output would stop being a pure function of its inputs. |
| _(none)_ | — | container + surfaces + `UploadPlan` + `decode_content()` classification |
