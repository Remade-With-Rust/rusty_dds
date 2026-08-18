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

LDR and HDR fail closed on each other: `decode_rgba8` refuses BC6H and
`decode_rgba_f32` refuses the LDR set, rather than silently converting.

**sRGB:** `_sRGB` tags keep **stored** channel bytes (no linearization) on
decode; GPU names map to `*Srgb` variants in [`GpuFormat`](../src/upload.rs).

## Explicitly unsupported (today)

| Format family | Status |
|---------------|--------|
| ASTC / ETC / PVRTC | out of scope (not DDS-primary desktop BCn) |
| Packed RGB10A2, R16G16, float RGBA, etc. | container may parse; no decode/encode |
| Legacy D3D uncompressed bitmasks beyond the DXGI map | `UnsupportedFormat` on decode/encode |
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
