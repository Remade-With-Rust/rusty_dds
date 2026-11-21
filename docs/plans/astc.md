# ASTC toolkit for rusty_dds: build plan

Status: plan written 2026-11-21; decisions recorded 2026-11-21 (section 13); build in progress on branch `feat/astc`. Progress ledger: `docs/plans/astc-ledger.md`.
Tracking issue: Remade-With-Rust/rusty_dds#6 (reopened 2026-11-21).

This plan is written for a builder who has not seen the research. Every external fact carries its URL. Numbers marked **scout** were taken once, unpinned, in WSL2, and are for sizing only; they are not admissible for a README claim.

## How to use this plan

1. Read sections 1 to 4 once. Section 4 is the list of places ASTC implementations go wrong; keep it open while writing the decoder.
2. Execute section 5 brick by brick, in order. A brick is done only when its gate passes. Do not start a brick while an earlier gate is red.
3. Keep a ledger at `docs/plans/astc-ledger.md`: one line per brick (state, gate result, commit), plus every refuted idea with its measurement. The ledger is the resume point across sessions.
4. Load these skills before the matching phase: `codec-bringup-decoder` (phase D), `codec-bringup-encoder` (phase E), `codec-measurement` (before taking any number you will act on), `instruction-counting` and `codec-optimize` (phase P), `codec-tune-quality` (encoder quality bricks), `openheimer` (fuzz bricks), `github-replies` (the issue #6 reply), `building-the-new-internet` (before adding a dependency or releasing).
5. Stop and ask Tim at the items in section 12. They are decisions, not engineering.

---

## 1. Research findings

### 1.1 The repo (rusty_dds 0.10.1, default branch `master`)

Read at `origin/master` = `dbd8964` (local clone `F:/coding/rusty_dds_work`).

| Topic | Finding | Where |
|---|---|---|
| Shape | Single-package repo, lib `rusty_dds` plus bins `rusty-dds`, `ddsinfo`, `retag`. About 24.5k lines in `src/`. | `Cargo.toml`, `src/` |
| MSRV / edition | Rust 1.73, edition 2021. Consequences: no `LazyLock` (1.80), no float arithmetic in `const fn` (1.82). `OnceLock` (1.70), `div_ceil` (1.73) and let-else are available. | `Cargo.toml` |
| std | The crate is `std` (uses `std::io`, `Vec`, `std::thread::scope`, `OnceLock`). It is not `no_std`. | `src/lib.rs`, `src/decode/mod.rs` |
| Features | `decode` (pulls `bcdec_rs`), `encode`, `simd` (default on), `tuning` (dev only). | `Cargo.toml` |
| Unsafe policy | `#![cfg_attr(not(feature = "simd"), forbid(unsafe_code))]`. Without `simd` the crate cannot contain `unsafe`. With it, `unsafe` lives only in SIMD kernels, each with a scalar twin and a `SAFETY:` comment. | `src/lib.rs`, `src/decode/simd.rs` |
| SIMD fencing | SSE2 is used unconditionally on x86_64 (baseline). SSSE3/SSE4.1/AVX2/F16C are runtime-detected once and cached in an `AtomicU8` (`probe`). NEON mirror in `src/decode/neon.rs`. Whole-surface loops live inside the `#[target_feature]` function because the ABI boundary of a per-block call cost more than the kernel saved. Every SIMD kernel has an exhaustive or randomized `*_matches_scalar` test. | `src/decode/simd.rs` |
| Scalar oracles | `decode::reference` (public) re-decodes with `bcdec_rs` per block; tests and benches A/B against it. Encoder kernels keep scalar twins in `src/encode/blocks/oracles.rs`. | `src/decode/reference.rs` |
| Decode API shape | Methods on `DdsBase<D>`: `decode_rgba8`, `decode_rgba8_into(&mut Vec<u8>)`, `decode_rgba8_with(DecodeOptions)`, `decode_block_rows_into(id, rows, &mut [u8])`, `block_rows`, and the `f32` twins `decode_rgba_f32`, `decode_rgba_f32_into`, `decode_block_rows_f32_into`, `block_rows_f32`. Block-level functions take `(data, width, height, out)`. | `src/decode/mod.rs`, `src/decode/bcn.rs` |
| Threads | The decoder spawns threads only for large BC7 surfaces. The documented position is that callers split work through the block-row seams ("a texture library that seizes cores inside a frame is a library an engine has to work around"). | `src/decode/mod.rs` docs |
| Classification | `DecodeContent` (LDR matrix, `#[non_exhaustive]`, public `ALL_LDR`) and a separate `HdrDecodeContent` for BC6H "so the LDR matrix is untouched". | `src/content.rs` |
| Upload plans | `GpuFormat { dxgi_name, wgpu_name, vulkan_name, block_bytes, block_width, block_height, compressed }`, `UploadPath`, `UploadPlan`, all `#[non_exhaustive]`. `block_width`/`block_height` already exist as fields. | `src/upload.rs` |
| DDS layout math | **Hardcodes 4x4 blocks**: `get_pitch` uses `(width + 3) / 4`, `get_pitch_height` returns 4, the mip chain uses `current /= 4` with a minimum size. None of this holds for a 6x6 or 12x10 footprint. | `src/format/dxgi.rs`, `src/format/mod.rs`, `src/surface.rs`, `src/lib.rs` |
| `DxgiFormat` | `#[non_exhaustive]`, ends at `V408 = 132`. No ASTC values. | `src/format/dxgi.rs` |
| Errors | `Error` is `#[non_exhaustive]`: `UnsupportedFormat`, `TruncatedData`, `OutOfBounds`, `InvalidField(String)`, `BadMagicNumber`, `SizeLimitExceeded`, `Io`, and others. | `src/error.rs` |
| Tests | `tests/decode_matrix.rs`, `encode_matrix.rs`, `encode_determinism.rs` (frozen FNV-1a payload hashes), `parser_robustness.rs` (stable, deterministic), `f32_decode.rs` (independent f64 oracle), fixtures in `tests/fixtures/`. | `tests/` |
| Fuzz | `fuzz/` is a standalone workspace (nightly, libFuzzer), targets `parse`, `read_limited`, `encode_roundtrip`. Stable half is `tests/parser_robustness.rs`. Both drive `tests/common/driver.rs`. A found input goes to `tests/fixtures/regressions/` with the fix. | `fuzz/README.md` |
| Bench | Criterion benches `benches/decode_ab.rs`, `parse_ab.rs`. Campaign harnesses are examples (`bench_encode_corpus`, `probe_*`). `bench/ab_encode.ps1` is the pinned ABBA runner (affinity mask 60, High priority). `bench/dds02/` is a **standalone harness crate** with its own deps, and it sets rusty_alloc as `#[global_allocator]`. | `bench/` |
| Corpus | `corpus/` holds a manifest and a fetch script; raw images are gitignored. | `corpus/README.md` |
| CI | `portfolio-check.yml` (shared org harness, runs `test`) and `release-plz.yml` (release PR on every push to `master`; publish on merge). An open release PR for 0.10.2 exists (#9). | `.github/workflows/` |
| Packaging | `exclude` drops `fuzz/`, `sim/`, `tools/`, `docs/plans/`, corpora, probe examples. `tests/fixtures/` **is** packaged, so fixtures must stay small. | `Cargo.toml` |
| Docs | `docs/formats.md` lists "ASTC / ETC / PVRTC: out of scope". `docs/plans/texture-pipeline.md` excludes ASTC from phase 1. README claims are tied to reproducible commands. | `docs/` |
| Licence | MIT, with `THIRD-PARTY-NOTICES.md`. | repo root |

### 1.2 The normative spec

- Khronos Data Format Specification, ASTC chapter (normative). Rendered: https://registry.khronos.org/DataFormat/specs/1.3/dataformat.1.3.html#ASTC and https://registry.khronos.org/DataFormat/specs/1.4/dataformat.1.4.html#ASTC . Source text read for this plan: https://github.com/KhronosGroup/DataFormat/blob/main/astc.txt (90 KB AsciiDoc; section names below are from it).
- ARM overview: https://github.com/ARM-software/astc-encoder/blob/main/Docs/FormatOverview.md
- `.astc` file format: https://github.com/ARM-software/astc-encoder/blob/main/Docs/FileFormat.md
- Decode-mode extension (`decode_unorm8`, `decode_rgb9e5`): https://registry.khronos.org/OpenGL/extensions/EXT/EXT_texture_compression_astc_decode_mode.txt
- GL enums: https://registry.khronos.org/OpenGL/extensions/KHR/KHR_texture_compression_astc_hdr.txt and (3D) https://registry.khronos.org/OpenGL/extensions/OES/OES_texture_compression_astc.txt
- Design paper (encoder background): Nystad et al., "Adaptive Scalable Texture Compression", HPG 2012, https://www.highperformancegraphics.org/previous/www_2012/media/Papers/HPG2012_Papers_Nystad.pdf

Facts taken from the spec text that the plan depends on (all from `astc.txt`):

- Every block is 128 bits. 2D footprints: 4x4, 5x4, 5x5, 6x5, 6x6, 8x5, 8x6, 8x8, 10x5, 10x6, 10x8, 10x10, 12x10, 12x12 (14). 3D footprints: 3x3x3 to 6x6x6 (10).
- Three operation modes (sRGB, linear LDR, HDR) and three decode modes (`decode_float16`, `decode_unorm8`, `decode_rgb9e5`). sRGB always returns 8-bit UNORM. `decode_unorm8` in HDR mode is invalid.
- LDR interpolation: endpoints expand to 16 bits by replication (`(C << 8) | C`) in linear mode and as `(C << 8) | 0x80` in sRGB mode, then `C = (C0 * (64 - i) + C1 * i + 32) / 64`.
  - `decode_float16`: `C == 65535` gives 1.0 (0x3C00), otherwise `C / 65536` converted to FP16 **rounding toward zero**.
  - `decode_unorm8` and sRGB: the **top 8 bits** of `C`.
- HDR interpolation: 12-bit endpoints shift left 4, interpolate the same way, then the LNS conversion (`E = C >> 11`, `M = C & 0x7FF`, the three-segment `Mt`, `Cf = (E << 10) + (Mt >> 3)`), read as FP16, with Inf/NaN replaced by 0x7BFF.
- Error colour: magenta `(1, 0, 1, 1)`, or all-NaN (recommended bit pattern 0xFFFF) in `decode_float16`. Both are allowed for linear LDR and HDR in `decode_float16`; `decode_unorm8` and sRGB are magenta.
- In LDR operation, a block that mixes HDR and LDR endpoint modes across partitions is **not** an error block: only texels of the HDR partition become the error colour.
- A HDR void-extent block decoded in LDR mode gives the error colour for the whole block. HDR void-extent constants that are Inf or NaN are undefined behaviour.
- Illegal encodings (ten conditions) are listed in the "Illegal Encodings" section; decoders must detect all of them.
- Unallocated bits "are not read by the decoding process and can have any value".
- Weight infill "must be followed exactly, to ensure bit exact results". 3D uses simplex interpolation.
- LDR profile and HDR profile are both 2D-only subsets; 3D belongs to the full profile.

### 1.3 The oracle: ARM astcenc

- Repo: https://github.com/ARM-software/astc-encoder (Apache-2.0).
- **Latest release is 5.7.1** (published 2026-11-03, tag commit `0affb51aaf0e0de3a8b038fa32487d59754a1eed`). `main` is at `22ba8a0` (2026-11-19), 141 commits past 5.7.1, and its `Docs/ChangeLog-6x.md` describes 6.0.0 as "Released November 2026", but **no 6.0.0 tag or GitHub release existed on 2026-11-21**. Pin the oracle to the 5.7.1 commit. Re-pin deliberately when 6.0.0 is tagged.
- CLI modes (from `Source/astcenccli_toplevel_help.cpp`): compress `-cl` (linear LDR), `-cs` (sRGB LDR), `-ch` (HDR RGB + LDR alpha), `-cH` (HDR RGBA); decompress `-dl`, `-ds`, `-dh`, `-dH`; test (round trip plus metrics) `-tl`, `-ts`, `-th`, `-tH`. Presets `-fastest` (0), `-fast` (10), `-medium` (60), `-thorough` (98), `-verythorough` (99), `-exhaustive` (100), or any float 0 to 100. `-j <threads>`. `-decode_unorm8` (encoder rounding target). Decompression inputs: `.astc`, `.ktx`. Decompression outputs: `.png`/`.bmp`/`.tga` (8-bit), `.exr`/`.hdr` (16-bit float), `.ktx`/`.dds` (8-bit for LDR profiles, 16-bit float for HDR profiles).
- **The decode mode follows the output type.** In `astcenc_decompress_image`, `blk.decode_unorm8 = image_out.data_type == ASTCENC_TYPE_U8` (`Source/astcenc_entry.cpp`). So `astcenc -dl x.astc x.png` is `decode_unorm8` (top 8 bits), and a float16 output is `decode_float16`. The sRGB profile always uses the 8-bit rule (`get_u8_component_mask` in `Source/astcenc_internal.h`).
- **Error colours in astcenc** (`Source/astcenc_decompress_symbolic.cpp`, `astcenc_color_unquantize.cpp`, `astcenc_image.cpp`):
  - Illegal block, or HDR void-extent in an LDR profile: NaN internally (`0xFFFFE000` as f32, which becomes FP16 `0xFFFF`). A U8 store turns NaN into magenta `FF 00 FF FF`.
  - HDR endpoint mode in an LDR profile: the endpoints of **that partition** become `(0xFF, 0x00, 0xFF, 0xFF)`, so its texels decode to magenta `(1, 0, 1, 1)` even in float16 output, not NaN.
  - So in float16 output astcenc produces two different error colours depending on the cause. Both are legal per the spec. We match astcenc exactly.
- sRGB alpha: a comment in `unpack_color_endpoints` says alpha is expanded by replication, but the code does `lsl<8>(output) | 0x80` on all four lanes, and the spec says the same for every component. Follow the code and the spec.
- Void-extent checks in `physical_to_symbolic` (`Source/astcenc_symbolic_physical.cpp`): 2D reserved bits 10 and 11 must be `11`; coordinates must satisfy low < high unless all four are all-ones; 3D has no reserved-bit check.
- astcenc ISA builds produce identical output by design ("All normal builds are designed to be invariant", `Docs/Building.md`). Confirmed by scout: `astcenc-avx2` and `astcenc-none` gave identical PSNR to four decimals on every preset tried.
- Test material shipped in the repo (`Test/`): image sets `Small` (LDR-RGB, LDR-RGBA, LDR-L, LDR-XY, LDRS-RGBA, HDR-RGB, HDR-RGBA; 256x256 and one 127x128), `Khronos` (glTF-Asset-Generator extracts, MIT, up to 2048x2048), `HDRIHaven` (CC0), plus `Kodak`/`KodakSim`/`KodakMnt` fetched by `Test/astc_test_image_dl.py` from http://r0k.us/graphics/kodak/ . Reference result CSVs (PSNR, SSIM, coding rate per image, block size and preset) are committed, for example `Test/Images/Kodak/astc_reference-6.0-avx2_medium_results.csv`. Container negative tests: `Test/Data/negative_*.astc`. Fuzzers: `Source/Fuzzers/fuzz_astc_physical_to_symbolic.cpp` and two others.
- Kodak mean PSNR from astcenc's own reference CSVs (main branch, AVX2, 12 threads on a Xeon w7-3465X): 4x4 `fastest 44.19`, `fast 44.59`, `medium 45.27`, `thorough 45.51` dB; 6x6 `37.51 / 37.64 / 38.22 / 38.46`; 8x8 `33.54 / 33.69 / 34.41 / 34.64`; 12x12 `29.49 / 29.60 / 30.45 / 30.80`. The whole preset spread is about 1.3 dB. That is the scale our quality targets live on.

Oracle rig already built for this plan (WSL Ubuntu 26.04, gcc 15.2, cmake 4.2.3, valgrind 3.26, on ext4):

```
/root/astcws/astc-encoder      full clone, main @ 22ba8a0
/root/astcws/rel571            git worktree at tag 5.7.1 (0affb51)
/root/astcws/rel571/inst/bin   astcenc-avx2, astcenc-none   (built, ~2 min at -j4)
/root/astcws/scout             scout outputs and the Rust baseline crate
```

Build command that worked:

```sh
cd /root/astcws/rel571
cmake -S . -B build -DCMAKE_BUILD_TYPE=Release -DASTCENC_ISA_AVX2=ON -DASTCENC_ISA_NONE=ON \
      -DCMAKE_INSTALL_PREFIX=/root/astcws/rel571/inst
cmake --build build -j4 && cmake --install build
```

WSL rules for this work: one heavy process at a time, `CARGO_BUILD_JOBS=4`, work on ext4 (`/root/...`, never `/mnt/f` for cargo; 9p hangs it), never `wsl --shutdown`. From Git Bash set `MSYS_NO_PATHCONV=1` and feed scripts on stdin (`wsl -d Ubuntu -- bash -s <<'EOF'`); a non-login shell needs `source ~/.cargo/env`.

### 1.4 Existing Rust crates (checked on crates.io 2026-11-21)

| Crate | Version, date | What it is | ASTC coverage |
|---|---|---|---|
| `astc-decode` https://crates.io/crates/astc-decode | 0.3.1, 2021-06-19, Apache-2.0 | Pure-Rust decoder. README: "supports ASTC LDR profile". | 2D LDR only. HDR endpoint modes become magenta. One 8-bit output; no sRGB mode, no float output, no 3D. Callback-per-pixel API. |
| `texture2ddecoder` https://crates.io/crates/texture2ddecoder | 0.1.2, 2025-03-21, MIT OR Apache-2.0 | Pure-Rust `no_std` multi-format decoder. | 2D, LDR and HDR endpoint modes, but HDR is tone-clamped to 8-bit BGRA `u32`. No float output, no sRGB mode, no 3D. |
| `dds` (image-rs/image-dds) https://crates.io/crates/dds | 0.2.0, 2025-11-11 | DDS de/encoder. This is what Freehold Material uses today. | Decode only, by calling `astc-decode` (`src/decode/astc.rs`). Maps `ASTC_*_UNORM` and `ASTC_*_UNORM_SRGB` to the same decoder. No ASTC encode. |
| `astcenc-rs` / `astcenc-sys` https://crates.io/crates/astcenc-rs | 0.2.0, 2025-12-12 | Bindings to ARM's C++ library. | Full, but C++. |
| `ctt` https://crates.io/crates/ctt | 0.6.1, 2026-10-28 | CLI/library that "binds to established open-source compression libraries" (astcenc for ASTC); writes KTX2. | C++ underneath. |
| `intel_tex_2` https://crates.io/crates/intel_tex_2 | 0.5.0 | Bindings to Intel's ISPC texture compressor. | ASTC encode through ISPC, not Rust. |
| `basis-universal` https://crates.io/crates/basis-universal | 0.3.1 | Bindings to Binomial's C++ library. | C++. |
| `basisu` https://crates.io/crates/basisu | 0.1.0, 2026-07-18 | Pure-Rust Basis Universal **transcoder**. | Emits ASTC blocks from UASTC/ETC1S. Not an image-to-ASTC encoder. |
| `ktx2` https://crates.io/crates/ktx2 | 0.5.0 | KTX2 container parser. | Container only. |

No pure-Rust image-to-ASTC encoder was found. Re-run this check at release before claiming it.

**Scout measurement** of the two pure-Rust decoders against astcenc 5.7.1 (one 2048x2048 RGB texture, `Khronos/LDR-RGB/ldr-rgb-diffuse.png`, encoded `-cl -medium`; single thread; WSL2; system allocator; best of 7 for Rust, three runs for astcenc):

| Footprint | astcenc-avx2 `-j 1` decode | astcenc-none | `astc-decode` 0.3.1 | `texture2ddecoder` 0.1.2 |
|---|---|---|---|---|
| 4x4 | 64 to 71 MT/s | about 25 to 33 | 46.3 | 44.1 |
| 6x6 | 83 to 85 | about 27 to 32 | 51.0 | 41.9 |
| 8x8 | 78 to 90 | about 29 to 35 | 66.4 | 69.0 |
| 12x12 | 95 to 101 | not run | 83.6 | 74.5 |

(MT/s = million texels per second. astcenc numbers are its own "Decoding rate" line.)

**Neither existing crate is bit-exact with astcenc.** Same texture, comparing 8-bit output:

| Compared with | `astc-decode` | `texture2ddecoder` |
|---|---|---|
| `astcenc -dl` to PNG (linear, `decode_unorm8`) | 27% to 37% of pixels differ by 1 (1,136,538 of 4,194,304 at 4x4; 1,567,442 at 6x6) | same pattern (1,136,506 at 4x4) |
| `astcenc -ds` to PNG (sRGB), 6x6 | 7,062 pixels differ by 1 | 6,953 differ by 1 |

Cause, from their sources: both interpolate replicated 16-bit endpoints and then **round** to 8 bits (`(c * 255 + 32767) / 65536` and `(c * 255 + 32768) / 65536`), which is neither the `decode_unorm8` rule (top 8 bits) nor the sRGB rule (`0x80` expansion, top 8 bits). This is the gap issue #6 asks us to close ("exact for LDR").

**Scout measurement** of astcenc 5.7.1 encode, single thread, same texture:

| Footprint | Preset | astcenc-avx2 MT/s | astcenc-none MT/s | PSNR dB |
|---|---|---|---|---|
| 4x4 | fastest / fast / medium / thorough | 15.4 / 14.0 / 3.9 / 0.43 | 4.2 / 4.8 / 1.4 / not run | 52.39 / 52.60 / 54.02 / 54.96 |
| 6x6 | fastest / fast / medium / thorough | 13.1 / 13.7 / 4.8 / 0.93 | 4.6 / 3.6 / 1.5 / not run | 43.16 / 43.21 / 44.86 / 45.81 |
| 8x8 | fastest / fast / medium / thorough | 13.3 / 11.1 / 3.2 / 0.85 | 4.7 / 3.7 / 1.0 / not run | 38.31 / 38.44 / 40.26 / 40.91 |

### 1.5 FFmpeg 9.1

- FFmpeg `Changelog`, "version 9.1": "ASTC image encoding and decoding via libastcenc, with .astc and KTX 1.0 muxers and demuxers" (https://github.com/FFmpeg/FFmpeg/blob/master/Changelog; tag `n9.1` exists).
- Encoder `libastcenc`, codec id `AV_CODEC_ID_ASTC` (https://github.com/FFmpeg/FFmpeg/blob/master/libavcodec/libastcenc.c). Options: `block_size` (string `WxH[xD]`, default `8x8`), `quality` (0 to 100, default 60), `profile` (`ldr`, `ldr-srgb`, `hdr-ldr-a`, `hdr`; unknown defaults to sRGB), `perceptual` (default 0), `alpha_weight` (default 1). Input pixel formats: `rgba`, `rgb24`, `rgbaf16`, `rgbaf32`, `gbrapf32`. Float input auto-promotes the profile to `hdr-ldr-a`; 8-bit input with an HDR profile is an error. The packet is the raw block payload; the 16-byte `.astc` header is published as extradata.
- Decoder (https://github.com/FFmpeg/FFmpeg/blob/master/libavcodec/libastcdec.c): option `dec_profile`; output `rgba` for LDR, `rgbaf16` for HDR; a raw `.astc` with no profile defaults to `ldr-srgb`; a linear KTX with unknown HDR-ness decodes with `hdr-ldr-a`.
- `.astc` demuxer/muxer: `libavformat/astcdec.c`, `astcenc.c`. KTX 1.0: `libavformat/ktxdec.c`, `ktxenc.c`. The KTX demuxer accepts little-endian, 2D, one face, one mip, GL internal formats `0x93B0..0x93BD` (linear) and `0x93D0..0x93DD` (sRGB). The KTX muxer refuses HDR profiles and has a `srgb` option (`auto`, `linear`, `srgb`).

### 1.6 Containers

| Container | Facts | Source |
|---|---|---|
| `.astc` | 16-byte header: magic `13 AB A1 5C`, `block_x`, `block_y`, `block_z` (u8), `dim_x`, `dim_y`, `dim_z` (24-bit little-endian each); then 16 bytes per block. No profile, no mips. | `Docs/FileFormat.md` (URL above) |
| KTX 1.0 | 64-byte header, endianness word `0x04030201`, `glInternalFormat` carries the ASTC format, per-mip `imageSize` plus 4-byte padding. | https://registry.khronos.org/KTX/specs/1.0/ktxspec.v1.html |
| KTX 2.0 | `vkFormat` in the header, level index with 64-bit offsets, mandatory Data Format Descriptor, optional supercompression (none, BasisLZ, Zstandard, ZLIB). | https://registry.khronos.org/KTX/specs/2.0/ktxspec.v2.html |
| Vulkan formats | `VK_FORMAT_ASTC_4x4_UNORM_BLOCK = 157`, `_SRGB_BLOCK = 158`, ... `ASTC_12x12_UNORM = 183`, `_SRGB = 184`; `ASTC_4x4_SFLOAT_BLOCK = 1000066000` ... `12x12 = 1000066013`; 3D `ASTC_3x3x3_UNORM_BLOCK_EXT = 1000288000` onward in threes (UNORM, SRGB, SFLOAT). | https://github.com/KhronosGroup/Vulkan-Headers/blob/main/include/vulkan/vulkan_core.h |
| DDS | There is **no official DXGI ASTC format**. The de facto convention is `DXGI_FORMAT_ASTC_4X4_TYPELESS = 133`, `_UNORM = 134`, `_UNORM_SRGB = 135`, then four values per footprint up to `ASTC_12X12_UNORM_SRGB = 187`. `image-dds` implements it and ships 14 test files. There is no HDR tag and no 3D tag. | https://github.com/image-rs/image-dds/blob/main/src/header.rs , https://github.com/image-rs/image-dds/tree/main/test-data/images/astc |

**DDS decision:** support it. It is what issue #6 asks for ("The ASTC payload inside DDS (the `DXGI_FORMAT_ASTC_*` values)"), and the `dds` crate Material is leaving already reads these files. Document it as a convention, read the three variants per footprint, and write `_UNORM` / `_UNORM_SRGB` only.

### 1.7 Public test material

| Set | Use | Source |
|---|---|---|
| astcenc `Test/Images/Small`, `Khronos`, `HDRIHaven` | Source images for the natural corpus | in the astcenc repo (MIT / CC0 per the `LICENSE.txt` files there) |
| Kodak 24 | Quality benchmark | http://r0k.us/graphics/kodak/ (fetch, never commit) |
| google/astc-codec `src/decoder/testdata` | 30 `.astc` files across all 14 footprints with `.bmp` references (LDR). Archived repo, Apache-2.0. Use the `.astc` files as extra inputs; decode them with astcenc for the expected output rather than trusting the `.bmp` rounding. | https://github.com/google/astc-codec/tree/master/src/decoder/testdata |
| image-dds ASTC DDS files | 14 DDS files with ASTC DXGI formats | URL above |
| KTX-Software `tests/resources` | `ktx/astc_8x8_unorm_array_7.ktx`, `ktx2/astc_8x8_unorm_array_7.ktx2`, `ktx2/Desk_astc_hdr6x6.ktx2`, `ktx2/ktx_app_astc_8x8.ktx2`, `ktx2/r8g8b8a8_srgb_mip_astc.ktx2` | https://github.com/KhronosGroup/KTX-Software/tree/main/tests/resources |
| VK-GL-CTS `tcuAstcUtil.cpp` | Design reference for a directed block generator: it builds void-extent, weight-grid, weight-ISE, CEM, partition-seed, endpoint-value and endpoint-ISE cases by field. Read it for the case list; do not copy code. | https://github.com/KhronosGroup/VK-GL-CTS/blob/main/framework/common/tcuAstcUtil.cpp |

No public conformance suite covers every class, so most of the corpus is generated (section 6.1).

### 1.8 ArtCraft's requirements (issue #6)

LDR and HDR profiles; all 2D footprints (3D optional); sRGB and linear LDR decode; HDR to half float; ASTC inside DDS, KTX2 and raw `.astc`; void-extent and error colours per spec; errors, not panics, on bad input; a truncated block stream must error. Suggested API: `decode_astc(data, width, height, block_w, block_h, profile) -> Image`, plus routing in the existing DDS loader. Their verification: against `astcenc -d`, exact for LDR, exact or within spec tolerance for HDR; speed against `astc-decode` with paired runs. They asked for a reply on the issue when it is on crates.io.

### 1.9 rff (remade_ffmpeg_rs)

- rusty_dds is **not** consumed in `F:/coding/remade_ffmpeg_rs` today, and rff has no DDS, ASTC or KTX format.
- The pattern to mirror is WebP: `crates/rff-codec-webp/src/lib.rs` registers a `Codec { id, name, long_name, media_type, decoder, encoder }`; `crates/rff-format-webp/src/lib.rs` registers a `Format { name, extensions, demuxer, muxer, probe, mux_caps: MuxCaps::single(&[...]).image() }`. Registration is in `crates/rff/src/lib.rs`. The "output extension implies codec" table is in `crates/rff/src/transcode.rs` (the `required` match near line 914).
- `CodecId` (`crates/rff-core/src/media.rs`) is `#[non_exhaustive]` with no `Astc`. `PixelFormat` has `Rgb24` and `Rgba` and **no float format**, so HDR ASTC cannot flow through rff until one is added. `Stream` has `extradata` and `color` but no `profile` field. Encoders receive options through `Encoder::configure(&Dictionary)`.
- rff MSRV is 1.85 and its licence is Apache-2.0; rusty_dds (1.73, MIT) is compatible as a dependency. rff pins `rusty_alloc-api = "=2.5.0"`.

---

## 2. Goals, non-goals, success numbers

### 2.1 Goals

1. A decoder for every ASTC block: LDR and HDR endpoint modes, all 14 2D footprints, all 10 3D footprints, sRGB and linear LDR operation, 8-bit and half-float output, void-extent, and every illegal encoding mapped to the reference error colour. Total: never panics, defined output for any 16 bytes.
2. A conformant LDR encoder with presets `Fastest`, `Fast`, `Medium` first, `Thorough` and `Exhaustive` after, deterministic, with quality measured against astcenc.
3. Containers: `.astc`, KTX 1.0, KTX 2.0 (read first, write second), and ASTC inside DDS.
4. Integration into rusty_dds's API and docs, and an rff codec/format hook that mirrors FFmpeg 9.1's command lines.
5. Pure Rust, no C, no `build.rs`, builds for `wasm32-unknown-unknown`, zero `unsafe` without the `simd` feature.

### 2.2 Non-goals (for the releases planned here)

- `decode_rgb9e5` output.
- LZ-RDO for ASTC (astcenc's `-rdo-lambda`). Possible later; rusty_dds already has an `Rdo` type for BCn.
- KTX2 BasisLZ/UASTC transcoding. Zstandard/ZLIB supercompression is deferred (section 12).
- A GPU decode path.
- Matching astcenc's encoder output bytes. Only its **decoder** output is matched bit-for-bit.
- Making the whole of rusty_dds `no_std`. The ASTC core is written to `core` + `alloc` only (section 3.5).

### 2.3 Success criteria

**Decoder correctness (hard gates; a failure blocks the release):**

| ID | Criterion | Number |
|---|---|---|
| DC1 | Byte-identical to astcenc 5.7.1 (`0affb51`) over the whole corpus in each of six cells: `LdrSrgb x U8`, `Ldr x U8`, `LdrSrgb x F16`, `Ldr x F16`, `HdrRgbLdrA x F16`, `Hdr x F16` (F16 compared as raw half bit patterns) | 0 differing bytes |
| DC2 | Random and mutated blocks through the batch oracle, per (footprint, cell) | at least 10^7 random plus 10^7 mutated, 0 differing bytes; 24 footprints x 6 cells |
| DC3 | Block-mode table equals the oracle for all 2^11 modes on all 24 footprints | 49,152 of 49,152 entries |
| DC4 | Partition assignment equals the oracle for all 3 x 1024 (count, seed) pairs on all 24 footprints | 73,728 of 73,728 maps |
| DC5 | Census: every class cell in section 6.2 has decoded at least 32 blocks bit-exactly | 100% of cells |
| DC6 | No panic: stable robustness test over 10^6 arbitrary blocks per footprint in `cargo test`; libFuzzer `astc_decode` target clean | at least 1 CPU-hour, 0 crashes |
| DC7 | Scalar and SIMD paths byte-identical; `--no-default-features --features decode,astc` builds with `forbid(unsafe_code)` and for `wasm32-unknown-unknown` | 0 differing bytes |

**Decoder speed (claim gates; a failure removes the claim, not the release).** Single thread, pinned, ABBA, bench binary under rusty_alloc, 8-bit LDR output, four footprints (4x4, 6x6, 8x8, 12x12), at least three 2048x2048 textures:

| ID | Criterion | Number |
|---|---|---|
| DS1 (first release floor) | Never slower than `astc-decode` 0.3.1 or `texture2ddecoder` 0.1.2 on any measured footprint | ratio at least 1.0 on all |
| DS2 (first release target) | At least 1.5x the faster of the two Rust crates, and at least 1.0x `astcenc-avx2 -j 1` | per footprint |
| DS3 (performance phase) | At least 2.0x `astcenc-avx2 -j 1` | per footprint |

**Encoder correctness (hard gates):**

| ID | Criterion | Number |
|---|---|---|
| EC1 | Every emitted block is legal: astcenc decode of our output equals our decode of our output, over the conformance matrix (every corpus image x every footprint x every preset x both LDR profiles x `decode_unorm8` on and off) | 0 differing bytes, 0 blocks flagged illegal by the oracle |
| EC2 | The encoder's own reconstruction equals `decode(encode(x))` | exact, as a standing test on real content |
| EC3 | Determinism: same bytes across runs, across thread counts, scalar versus SIMD, and x86_64 versus aarch64 versus wasm32 | frozen hashes |

**Encoder quality (release targets, LDR).** Corpus: Kodak 24, the astcenc `Khronos` set, the `Small` set. Footprints 4x4, 5x5, 6x6, 8x8, 10x10, 12x12. PSNR computed by our harness on decoded pixels. "Gap" is ours minus astcenc 5.7.1 at the same preset name. BD numbers are computed over the PSNR-versus-bitrate curve formed by the six footprints (8.00 down to 0.89 bits per pixel):

| Preset | Mean PSNR gap per footprint | Worst single image | BD-rate | Release |
|---|---|---|---|---|
| `Fastest` | at least -0.30 dB | at least -1.5 dB | at most +8% | first |
| `Fast` | at least -0.30 dB | at least -1.5 dB | at most +8% | first |
| `Medium` | at least -0.50 dB | at least -1.5 dB | at most +10% | first |
| `Thorough` | at least -0.25 dB | at least -1.0 dB | at most +5% | second |
| `Exhaustive` | at least -0.25 dB | at least -1.0 dB | at most +5% | second |
| long term | within 0.10 dB at every preset | | within 2% | later |

These are proposals. Brick E3 ends with a harvest that either confirms them or replaces them with measured numbers before any further encoder work (section 7.6). SSIM and Delta E are reported for every row and gated only as non-regression.

**Encoder speed (claim gates).** Single thread, same OS and core mask for both arms, ABBA:

| ID | Criterion | Number |
|---|---|---|
| ES1 (first release floor) | At equal quality (astcenc `-quality` float tuned to match our PSNR within 0.05 dB), our MT/s divided by `astcenc-none` MT/s | at least 0.5 at `Fast` and `Medium` |
| ES2 (performance phase) | Same ratio against `astcenc-none`; and against `astcenc-avx2` with our AVX2 twins | at least 1.0; at least 0.5 |

The README reports the ratio against `astcenc-avx2` whatever it is.

---

## 3. Architecture

### 3.1 Module layout

Everything ASTC lives under `src/astc/`, and nothing in it depends on the DDS half of the crate except through `Error`. That boundary makes a later extraction into its own crate a move, not a rewrite (section 12, decision 1).

```
src/astc/
  mod.rs            public API, re-exports, AstcFootprint, AstcProfile, AstcBlockInfo, AstcDecodeReport
  footprint.rs      footprint validation, the 14 + 10 legal sizes, block grid arithmetic
  bits.rs           128-bit block as (u64, u64); LSB-first reads; bit-reversed weight view
  ise.rs            integer sequence decode and encode; trit and quint block tables
  block_mode.rs     2D and 3D block-mode decode; weight grid, range, dual-plane; legality
  quant.rs          colour and weight unquantisation tables; the encoder's quantise maps
  partition.rs      hash52, select_partition, per-block partition map
  cem.rs            colour endpoint mode unpack (LDR modes); cem_hdr.rs (HDR modes)
  infill.rs         weight grid to texel weights (2D bilinear, 3D simplex)
  symbolic.rs       SymbolicBlock: the parsed, unquantised form shared by decoder and encoder
  parse.rs          physical (16 bytes) to SymbolicBlock, with the illegal-encoding reasons
  pack.rs           SymbolicBlock to physical (encoder side, feature "encode")
  texel.rs          interpolation and output conversion: unorm8, sRGB, unorm16 to FP16, LNS to FP16
  decode.rs         block decode and surface drivers (u8, f16, f32; rows; slices)
  reference.rs      the frozen scalar oracle path (public, like decode::reference)
  simd.rs / neon.rs SIMD twins (feature "simd"), added in phase P only
  tables/           generated .rs tables that cannot be const fn at MSRV 1.73
  encode/           mod.rs (options, presets, driver), ideal.rs (endpoints and weights),
                    modes.rs (block mode and quant search), partition_search.rs, dual_plane.rs,
                    cem_pack.rs, refine.rs, error.rs (error metrics), tuning.rs (frozen constants)
  container/        astc_file.rs (.astc), ktx1.rs, ktx2.rs
```

Outside `src/astc/`: `src/format/dxgi.rs` gains the ASTC values; `src/format/mod.rs`, `src/surface.rs` and `src/lib.rs` gain block-dimension-aware layout; `src/content.rs` gains `AstcContent`; `src/decode/mod.rs` and `src/upload.rs` route to ASTC; `src/bin/rusty_dds.rs` gains the CLI surface.

Tools and harnesses, none of them in the published package:

```
tools/astc-oracle/     oracle.cpp, build.sh, README.md   (C++ against astcenc sources; WSL only)
tools/astc-tablegen/   a small Rust program that regenerates src/astc/tables/*.rs
bench/astc/            standalone harness crate: rusty_alloc global allocator, baselines, metrics
fuzz/fuzz_targets/     astc_decode.rs, astc_container.rs, astc_encode_roundtrip.rs
tests/                 astc_tables.rs, astc_parse.rs, astc_decode.rs, astc_containers.rs,
                       astc_encode.rs, astc_determinism.rs, astc_robustness.rs
tests/fixtures/astc/   compact golden vectors (section 6.1)
```

### 3.2 Data types

```rust
/// Parsed block, unquantised. Mirrors what astcenc calls symbolic_compressed_block, so that
/// every field can be diffed against the oracle.
pub(crate) struct SymbolicBlock {
    kind: BlockKind,                 // VoidLdr([u16; 4]) | VoidHdr([u16; 4]) | Normal | Illegal(IllegalReason)
    mode: u16,                       // the raw 11-bit block mode
    grid: (u8, u8, u8),              // weight grid width, height, depth
    weight_range: u8,                // index into the 12 weight ranges
    dual_plane: bool,
    plane2_component: u8,            // 0..=3, meaningful when dual_plane
    partitions: u8,                  // 1..=4
    partition_seed: u16,             // 10 bits
    cem: [u8; 4],                    // 0..=15 per partition
    color_range: u8,                 // index into the 17 colour ranges (6..=256 levels)
    color_values: [u8; 18],          // unquantised 0..=255
    weights: [u8; 64],               // unquantised 0..=64; plane 2 interleaved as the spec orders them
    // Bit-budget fields, kept so tests can diff the *layout*, not only the values:
    weight_bits: u8, color_bits: u8, color_start: u8, below_weights_pos: u8,
}

pub(crate) enum IllegalReason {
    ReservedBlockMode, VoidExtentReservedBits, VoidExtentBadCoords, TooManyWeights,
    WeightBitsTooMany, WeightBitsTooFew, GridLargerThanFootprint, TooManyColorIntegers,
    ColorBitsTooFew, DualPlaneFourPartitions,
}
```

- **Block mode table.** `const fn` over integers, generated at compile time: `BLOCK_MODE_2D: [ModeEntry; 2048]` and `BLOCK_MODE_3D: [ModeEntry; 2048]`, each entry `{ w, h, d, range, dual_plane, class }` where `class` is `Normal`, `VoidExtent` or `Reserved`. Footprint-dependent legality (grid fits the footprint, weight count at most 64, weight bits in 24..=96) is evaluated at parse time in the oracle path and tabulated per footprint in phase P.
- **ISE.** `TRITS: [[u8; 5]; 256]` and `QUINTS: [[u8; 3]; 128]` built by `const fn` from the spec's bit equations; the encoder's inverse tables (`[u8; 243]`, `[u8; 125]`) built by `const fn` inversion. Bit counts are `(8n + 4) / 5` for trits and `(7n + 2) / 3` for quints, plus `n * bits`.
- **Colour unquantisation.** Seventeen `const` tables (ranges with 6, 8, 10, 12, 16, 20, 24, 32, 40, 48, 64, 80, 96, 128, 160, 192, 256 levels) generated by `const fn` from the spec's A/B/C/D recipe. Weight unquantisation: twelve `const` tables, including the `if w > 32 { w += 1 }` step.
- **Colour range selection.** The spec's iterative rule (largest range whose ISE size fits the remaining bits), tabulated by `const fn` as `[[u8; 128]; 10]` (pairs 1..=9 by available bits). The iterative form stays in `reference.rs` and a test checks the table against it for every input.
- **Partition map.** The oracle path computes `select_partition` per texel per block (pure, no state). Phase P adds a cache.
- **Infill.** The oracle path computes the spec arithmetic per texel. Phase P precomputes, per (footprint, grid), four weight indices and four contributions per texel.

### 3.3 Table generation strategy

| Kind | Strategy | Reason |
|---|---|---|
| Integer tables under about 8 KB (block modes, ISE, unquantisation, colour-range selection) | `const fn`, evaluated at compile time | No runtime state, no startup cost, works without `alloc`, MSRV 1.73 allows integer `const fn` with loops |
| Float or large tables needed by the encoder (angular-step sine/cosine tables, mode shortlists harvested from our corpus) | Generated `.rs` files checked in under `src/astc/tables/`, produced by `tools/astc-tablegen`, with a test that regenerates and compares | Float arithmetic in `const fn` needs Rust 1.82; `core` has no `sin`/`cos`; baking the literals also removes libm from the determinism surface |
| Per-(footprint, grid) infill tables and per-(footprint, count, seed) partition maps (hundreds of KB if all were built) | Lazy, in a **caller-visible context** (`AstcDecoder` scratch owned by the call or by the caller), never a global | No global mutable state, no `OnceLock` in the core, deterministic memory, works on wasm and without `std` |

No `build.rs`, and nothing generated from C.

### 3.4 Scalar oracle first

- Phase D produces one implementation: a direct transcription of the spec, written for reading, with no caches and no SIMD. It becomes `astc::reference` and is frozen once DC1 to DC6 pass.
- Phase P adds fast paths and SIMD twins. Each is gated byte-identical against `astc::reference` on the corpus and on random blocks, in the same style as `src/decode/simd.rs` (`*_matches_scalar` tests, exhaustive where the domain allows).
- SIMD follows the crate policy exactly: behind `feature = "simd"`, runtime-detected above the architecture baseline, `unsafe` only inside the kernel files with `SAFETY:` comments, whole-run loops inside the `#[target_feature]` function. With `simd` off the crate keeps `forbid(unsafe_code)`.
- A narrower SIMD lane than its scalar oracle is a known trap in this house: write the value-range table for every kernel (as `simd.rs` does for BC7's 16-bit lanes) before choosing a lane width. ASTC interpolation needs 22 bits (`65535 * 64 + 32 = 4,194,272`); 16-bit lanes are not enough without rearrangement.

### 3.5 no_std and wasm

- `src/astc/` except `container/` uses only `core` and `alloc`. No `std::io`, no threads, no `OnceLock`, no `HashMap`, no `f32` methods that need libm (`sqrt` is the exception and needs a decision: `core` has no `f32::sqrt` at MSRV 1.73, so the encoder uses a small exact integer or Newton routine in `encode/error.rs`, tested against `std` in a `#[cfg(test)]`).
- Enforcement: a CI-checked `tools/astc-nostd-check` crate (`#![no_std]`, `extern crate alloc`) that includes the core modules by `#[path]`. It fails to compile if `std` leaks in.
- wasm: a CI step builds `--target wasm32-unknown-unknown --no-default-features --features decode,encode,astc`. The ASTC code spawns no threads; callers split work through the block-row seams.
- Containers use `std::io::{Read, Write}` only at the edges; parsing works on `&[u8]` like `DdsView::parse`.

### 3.6 Threads

The ASTC decoder and encoder never spawn threads. They expose block-row seams (section 9). The `rusty-dds` binary and the rff adapter fan out over block rows with `std::thread::scope`. Encoder output is independent of the split because blocks are independent.

---

## 4. Where ASTC bugs hide

Each row is a place where a wrong implementation still produces plausible pictures. The right-hand column says how the plan catches it.

| Area | The trap | Caught by |
|---|---|---|
| Block mode | Ten 2D layout rows. The third row stores `H` before `W`. `rho` is split (bit 4 plus bits 0..1 or 2..3). In the `W+6, H+6` row, bits 9 and 10 are part of `H`, and dual-plane and high-precision are forced to 0. | DC3, exhaustive |
| Block mode | Void-extent is `(mode & 0x1FF) == 0x1FC`; bit 9 selects HDR. `bits[8:6] == 111` with `bits[1:0] == 00` and anything else in `bits[5:2]` is reserved. `bits[3:0] == 0000` is reserved. | DC3 |
| Legality | Weight count over 64 counts **both** planes. Weight bits must be in 24..=96. Grid larger than the footprint in any axis is illegal. Dual-plane with 4 partitions is illegal. Colour integers over 18 is illegal. Colour range below 6 levels is illegal. | DC2, directed illegal blocks (6.1 d) |
| Void-extent | 2D bits 10 and 11 must be `11`. Coordinates must be low < high unless all are all-ones. The 3D layout has 9-bit coordinates starting at bit 10 and no reserved bits. | Directed blocks, D3 gate |
| CEM, multi-partition | Selector `00` means one 4-bit CEM for all partitions at bits 25..28. Otherwise the per-partition class bits and the low mode bits spill into a "high part" of `3 * partitions - 4` bits **immediately below the weight data**, so its position depends on the weight bit count. | D3 per-field gate including `below_weights_pos` |
| Bit budget | Colour bits = 111 (one partition) or 99 (several), minus weight bits, minus the high part, minus 2 for dual-plane, clamped at 0. The dual-plane selector is the 2 bits below the high part. A value can match while the budget is wrong, and the error shows only at another block size. | D3 gates the budget fields, not only decoded values |
| ISE | Trit blocks hold 5 values in 8 bits interleaved with the low bits (`m0 T0 T1 m1 T2 T3 m2 T4 m3 T5 T6 m4 T7`); quint blocks hold 3 values in 7 bits. A final partial block is read with the missing bits as zero. Lengths that are multiples of 5 or 3 hide this. | D2 gate: every length 1..=64, all 21 ranges |
| ISE direction | Colour values are read LSB-first from bit 17 (one partition) or 29 (several). Weights are read from bit 127 **downward**: reverse the 128 bits and read LSB-first. | D2/D3 |
| Colour unquantisation | Trit and quint ranges use the A/B/C/D recipe with an XOR, so the stored order of values is not monotonic. An encoder that assumes monotonic order emits wrong endpoints. | D4 exhaustive; E1 round trip |
| Weight unquantisation | Trit/quint recipe, then `if w > 32 { w += 1 }` so that 64 is reachable. | D4 exhaustive |
| Dual-plane | The two planes' weights are interleaved in the ISE stream (value `2n` is plane 1, `2n + 1` is plane 2). The second-plane component applies to one channel only. | D6 gate with dual-plane and decimated grids |
| Infill | `Ds = (1024 + Bs / 2) / (Bs - 1)`; `gs = (cs * (W - 1) + 32) >> 6`; `w11 = (fs * ft + 8) >> 4`; final `(sum + 8) >> 4`. A full-size grid makes infill an identity and hides every error here. Square footprints hide swapped axes. | D6 gate on non-square footprints and decimated grids |
| 3D infill | Simplex interpolation with six cases selected by comparing fractions; the fourth point is always `v0 + W*H + W + 1`. | D9 |
| Partitions | `small_block` is texel count under 31, which includes 6x5 (30) and excludes 6x6 (36). `seed += (partitions - 1) * 1024`. The shift pattern depends on `partitions == 3`. With fewer than 3 (4) partitions, `c` (`d`) is zeroed. | DC4 exhaustive |
| LDR endpoints | `bit_transfer_signed`, blue contraction, the sum test that swaps endpoints in modes 8, 9, 12, 13, clamping after delta modes, the odd bit layout of modes 1 and 5. Symmetric endpoints hide ordering errors. | D5 gate: exhaustive for 2-value modes, 10^7 random for the rest |
| HDR endpoints | Modes 7 and 11 pack a mode value and major component across the high bits of several values, with per-mode shifts; mode 11 has a direct sub-mode; mode 15 has its own alpha mode bits. Results are 12-bit and shifted left 4. HDR modes that carry no alpha (2, 3, 7, 11) default it to `0x780` (12-bit, 1.0) per the spec; astcenc does that in the `hdr` profile and uses LDR `0xFF` in the `hdr-ldr-a` profile (`unpack_color_endpoints`). Both decode to 1.0, but through different paths. | D5 gate against the oracle's endpoints, in both HDR profiles |
| Interpolation | sRGB expands **all four** channels with `0x80` (the astcenc comment says otherwise; the code and spec agree). | DC1 with sRGB content that has alpha gradients |
| Output, 8-bit | Top 8 bits, not rounding. The two existing Rust crates round and are off by one on about a third of pixels. | DC1 |
| Output, float | `65535` maps to 1.0; everything else is `C / 65536` rounded toward zero to FP16. Do it in integers. | DC1 on raw half bits |
| Output, HDR | LNS to FP16 in integers; clamp Inf/NaN to `0x7BFF`. | DC1 |
| Error colour | Three cases with different outputs (section 1.3). In float16: illegal block gives `0xFFFF` in all four channels; HDR partition in an LDR profile gives `(0x3C00, 0, 0x3C00, 0x3C00)` for that partition's texels only. | DC1, DC5 cells for each cause |
| Unused bits | Bits not covered by any field must not affect the result. Encoders write zeros there, so only mutated blocks test it. | DC2 mutation set |
| Surface | Blocks are stored row-major (x, then y, then z). Edge blocks are cropped. Image sizes that are multiples of the footprint hide cropping errors. | D8 gate with 127x128 and 1x1 images |

### "Degenerate first block" for ASTC

ASTC has no inter-block state, so the corner-block trap from the decoder skill becomes a **content** trap. Each of these passes every test while a whole class is broken:

1. Flat images: every block is void-extent, and nothing else runs.
2. 4x4 footprint with a 4x4 grid: no infill.
3. One partition: no partition function, no multi-partition CEM layout, no high part.
4. Power-of-two ranges only: no trits, no quints.
5. Opaque LDR colour images: no alpha paths, no dual-plane on alpha, no HDR.
6. Square footprints and square grids: swapped axes are invisible.
7. Sizes that are multiples of the footprint: no cropping.
8. Encoder-written blocks: unused bits are always zero, illegal blocks never occur.

So the first image decoded is never evidence. The census (DC5) is the completion signal, and bring-up starts on 6x5 (non-square, small-block boundary) rather than 4x4.

---

## 5. Phased bricks

House method, translated for a block codec with no entropy-coder state:

- The per-symbol `(dif, rng, cnt)` trace of the decoder skill becomes a **per-field symbolic dump**: every parsed field of every block, including the bit-budget fields, diffed against astcenc's `physical_to_symbolic`. Pixel diffs say that you diverged; the field dump says where.
- Stage isolation: ISE, then configuration, then unquantisation, then endpoints, then weights and partitions, then pixels. Each stage is fed inputs the oracle has already confirmed.
- "Decode and stop": implement stage N, return `Unsupported` for N + 1, diff the prefix.
- The census (section 6.2) is the completion signal.
- The encoder is the inverse of the decoder, reuses its primitives, and is gated two ways on every brick: round trip through our decoder, and strict decode by astcenc.

Sizes are lines of Rust including unit tests. Days are focused working days for one builder.

### Phase 0: rig (2 to 3 days)

| Brick | Work | Size | Gate |
|---|---|---|---|
| 0.1 | Oracle workspace. Reuse `/root/astcws` (section 1.3) or rebuild it with the command there. Clone rusty_dds to ext4 (`/root/astcws/rusty_dds`) for cargo work in WSL. Record the astcenc commit in `tools/astc-oracle/README.md`. | 0 | `astcenc-none -version` prints 5.7.1 |
| 0.2 | `tools/astc-oracle/oracle.cpp`: a batch program that reads 16-byte blocks on stdin and writes fixed-size binary records on stdout. Link `build/Source/libastcenc-none-static.a` (it exists after the build in section 1.3), include `Source/astcenc_internal.h`, and copy the ISA defines from the `astcenc-none` compile line. Commands in section 6.3. | about 500 C++ | Self-test: a `.astc` decoded block by block through `oracle decode` equals `astcenc-none -dl/-ds/-dh/-dH` on the same file, byte for byte |
| 0.3 | Corpus v0 (section 6.1 a and e) and the census script. | about 300 | Census report generated; shows which cells are empty |
| 0.4 | **Score the reference under the harness.** Run `astcenc-avx2` against `astcenc-none` on corpus v0 in all six cells, and 5.7.1 against `main @ 22ba8a0`. | 0 | 0 differing bytes between ISA builds, or a written list of the classes that differ (expect only HDR void-extent Inf/NaN, if any). A written list of 5.7.1 versus main differences |
| 0.5 | `bench/astc/` harness crate skeleton: rusty_alloc global allocator, loads `.astc`, runs `astc-decode` and `texture2ddecoder`, prints MT/s and a mismatch count. The scout crate at `/root/astcws/scout/rsbase` is a starting point. | about 300 | Reproduces the scout table of section 1.4 within noise |

### Phase D: decoder, scalar oracle (7 to 9 days)

Bring up on **6x5** first, then 12x10, then the rest. Each brick's gate runs on all 14 2D footprints before the brick closes.

| Brick | Work | Size | Gate |
|---|---|---|---|
| D1 | `footprint.rs`, `bits.rs`, `block_mode.rs` (2D and 3D tables by `const fn`, legality classification). | 400 | DC3: all 2048 modes x 24 footprints equal `oracle tables` (class, grid, range, dual-plane, weight count, weight bits) |
| D2 | `ise.rs` decode. | 250 | Trit and quint tables equal the spec's equations for all 256 and 128 inputs. Sequence decode equals `oracle ise` for all 21 ranges, every length 1..=64, 10^5 random bit strings each |
| D3 | `parse.rs`: void-extent (2D), partitions, seed, CEM single and multi, high-part position, bit budget, colour range, dual-plane selector, all ten illegal reasons. Stop before unquantisation. | 500 | Per-field equality with `oracle symbolic` on corpus v0 and on 10^7 random blocks per footprint: kind, mode, partitions, seed, `cem[4]`, colour range, plane-2 component, and the four budget fields |
| D4 | `quant.rs`: colour and weight unquantisation. | 200 | Exhaustive: 17 colour ranges and 12 weight ranges equal the oracle's unquantised `color_values` and `weights` |
| D5a | `cem.rs`: LDR modes 0, 1, 4, 5, 6, 8, 9, 10, 12, 13. | 300 | Endpoints equal `oracle endpoints` in `ldr` and `ldr-srgb`: exhaustive for 2-value modes (65,536 inputs), 10^7 random plus edge values per mode otherwise |
| D5b | `cem_hdr.rs`: HDR modes 2, 3, 7, 11, 14, 15, plus the HDR/LDR flags and default alpha. | 450 | Same gate in `hdr` and `hdr-ldr-a`; in LDR profiles the oracle's magenta endpoints and validity flag match |
| D6 | `partition.rs`, `infill.rs` (2D). | 250 | DC4 for the 14 2D footprints. Per-texel weights for both planes equal `oracle weights` for every legal grid on every footprint with random weights |
| D7 | `texel.rs`: interpolation and the output rules; error colours; void-extent output. | 400 | DC1 on corpus v0 in all six cells; then DC2 |
| D8 | `decode.rs`: surface driver, cropping, `_into` variants, block rows, depth slices, `AstcDecodeReport`. | 350 | Image-level equality with the astcenc CLI on the natural corpus, including 127x128 and 1x1 images, in `-dl`, `-ds`, `-dh`, `-dH` |
| D9 | 3D: void-extent layout, simplex infill, z in the partition function, the 10 footprints. | 300 | DC1 to DC4 on the 3D footprints |
| D10 | Robustness: `tests/astc_robustness.rs` (deterministic, 10^6 blocks per footprint, plus every (mode, partition count) cell at least 1000 times with random remaining bits), fuzz target `astc_decode`, extension of `tests/common/driver.rs`. | 250 | DC6 |
| D11 | Corpus v1 (section 6.1 b, c, d), census complete, golden vectors committed, `astc::reference` frozen. | 300 | DC5. `docs/artifacts/astc-conformance.md` written from the run |

**3D decision.** 3D decode is in the first release. It is about 300 lines on top of a decoder that is written generic over `(bx, by, bz)` from D1, astcenc decodes 3D so the oracle covers it, and the differential harness makes proving it cheap. 3D encode waits (E8). If D9 slips, it is the first thing to move behind the cut line; nothing else depends on it.

### Phase C: containers (3 to 4 days for C1 to C4a)

| Brick | Work | Size | Gate |
|---|---|---|---|
| C1 | `.astc` read and write. Checked arithmetic on the 24-bit dimensions; payload length validated before any allocation. | 150 | Parses astcenc output. `astcenc -d*` accepts ours. astcenc's `Test/Data/negative_*.astc` each return `Err`, no panic. A truncated payload returns `Error::TruncatedData` |
| C2 | KTX 1.0 read (both endiannesses, mips, arrays, cube faces, 2D and 3D ASTC GL formats, key/value data skipped) and write (little-endian; LDR linear and sRGB; HDR, see decision 5 in section 12). | 450 | We read astcenc-written `.ktx` and KTX-Software's `astc_8x8_unorm_array_7.ktx`. `astcenc -dl` reads ours and decodes to the same pixels. Fuzz target `astc_container` clean for 30 minutes |
| C3 | DDS: `DxgiFormat` values 133..=187; block-dimension-aware pitch, mip sizes and array stride; `AstcContent`; `gpu_format` rows; decode routing. **Refactor first, byte-identical, then add ASTC.** | 400 | Before adding ASTC: every existing test passes and the frozen hashes in `tests/encode_determinism.rs` are unchanged. After: image-dds's 14 ASTC DDS files decode to the same pixels as astcenc decoding their payload; our DDS-ASTC files are read by the `dds` crate in `bench/astc` |
| C4a | KTX 2.0 read: header, level index, `vkFormat` to footprint and profile, supercompression scheme 0 only (others return `UnsupportedFormat`). | 300 | Reads those of KTX-Software's `astc_8x8_unorm_array_7.ktx2`, `ktx_app_astc_8x8.ktx2`, `Desk_astc_hdr6x6.ktx2`, `r8g8b8a8_srgb_mip_astc.ktx2` that are scheme 0, and decodes to the same pixels as astcenc on the extracted payload |
| C4b (after first release) | KTX 2.0 write with a correct Data Format Descriptor. | 300 | KTX-Software's `ktx validate` (built in WSL as an oracle) accepts our files |

### Phase E: encoder, LDR (first release: E1 to E4, about 11 days)

| Brick | Work | Size | Gate |
|---|---|---|---|
| E1 | `pack.rs`: ISE encode, quantise maps (inverse of D4), CEM packing for the direct modes, block assembly, void-extent. | 600 | (1) `parse(pack(s)) == s` for generated symbolic blocks covering the census. (2) `oracle symbolic` on our packed bytes equals `s`. (3) For every legal corpus block whose unused bits are zero, `pack(parse(b)) == b` |
| E2 | Baseline encoder: void-extent for constant blocks; otherwise one partition, direct CEM (8 or 12; 0 or 4 for grey), one fixed block mode per footprint, endpoints from the principal axis, weights by projection, simple grid decimation. Edge blocks padded by clamping coordinates. | 500 | EC1, EC2, EC3 on the conformance matrix. PSNR within 3 dB of `astcenc -fastest` (a sanity floor, not a quality target) |
| E3 | One-partition search: weight grid x weight range x colour range, ideal weights for decimated grids, endpoint refit after weight quantisation, CEM choice among LDR modes including base+scale, base+offset and blue contraction, exact evaluation of the top candidates. Ends with the first harvest (section 7.6). | 1500 | EC1 to EC3. The harvest table exists and the section 2.3 targets are confirmed or replaced |
| E4 | Two partitions (candidate search, per-partition CEMs with same-class and mixed-class signalling) and dual-plane (component choice). Presets `Fastest`, `Fast`, `Medium` frozen. | 1400 | EC1 to EC3. Quality rows for the three presets in section 2.3. ES1 |
| E5 (second release) | Three and four partitions, more candidates, weight realignment, `decode_unorm8`-aware rounding, alpha-weighted error, channel weights, normal-map mode, shortlists harvested from our own corpus. Preset `Thorough`. | 900 | Quality row for `Thorough` |
| E6 (second release) | Preset `Exhaustive`; calibrate early-out thresholds with `codec-search-skip-gate`. | 300 | Quality row for `Exhaustive`; every skip gate has a harvest behind it |
| E7 (third release) | HDR encode: modes 7 and 11 first, then 14 and 15; LNS-domain error. | 1200 | EC1 in `-dh` and `-dH`. mPSNR against `astcenc -ch`/`-cH` reported |
| E8 (third release) | 3D encode. | 400 | EC1 on the 3D footprints |

Encoder traps specific to ASTC, each tied to a gate:

- **A round trip through our own decoder proves nothing about legality.** A packer and parser that share a wrong high-part position agree with each other. EC1 uses astcenc, and E1 gate (2) uses astcenc's parser on our bytes.
- **Content masks.** Smooth images produce one-partition, direct-CEM blocks only. The conformance matrix must include noise, hard two-colour edges, alpha cut-outs, normal maps, grey images, and a constant DC offset image, so that every encoder path emits blocks.
- **The untested axis.** Every preset, every footprint, both profiles, `decode_unorm8` on and off. A feature that is clean at 4x4 and wrong at 10x5 is the usual failure (infill and small-block effects).
- **Edge blocks.** Image sizes that are not multiples of the footprint, down to 1x1.
- **Trial versus emit.** If candidate evaluation and final packing use different quantisation or rounding paths, EC2 catches it. Keep one code path: evaluate the packed-then-parsed symbolic block.

### Phase P: performance (first release: P1, 2 days; the rest after)

| Brick | Work | Gate |
|---|---|---|
| P1 | Decoder, scalar: per-(footprint, grid) infill tables and partition maps in a decode context; fast paths for void-extent, single-partition single-plane, and full-size grids; direct 8-bit output; bounds checks removed by construction. Callgrind census first (`instruction-counting`), one change per commit, revert if not faster. | DC7 (byte-identical to `astc::reference`); DS1; DS2 if reachable |
| P2 | Decoder SIMD twins: SSE2 baseline, SSSE3/AVX2 runtime, NEON. Four-channel interpolation and infill. | Byte-identical; DS3 |
| P3 | Encoder SIMD: error evaluation, covariance accumulation, weight quantisation. Fixed-order sums so scalar and SIMD agree bit for bit. | EC3; ES2 |
| P4 | Threaded convenience in the `rusty-dds` binary and the rff adapter over the block-row seams. | Output identical for 1 and N threads |

### Phase I: integration and release

| Brick | Work | Gate |
|---|---|---|
| I1 | rusty_dds API (section 9), `rusty-dds` CLI: `info`, `decode`, `encode` accept `.astc`, `.ktx`, `.ktx2`, `.dds`; new `--format astc --block 6x6 --preset medium --profile srgb`. | Doc tests compile; CLI round trip `png -> astc -> png` matches the library |
| I2 | Docs: `docs/formats.md`, README, CHANGELOG, `docs/artifacts/astc-*.md` (section 10). | Every number in the README has an artifact file and a command |
| I3 | Release 0.11.0 (section 10). Reply on issue #6 with measured numbers (`github-replies`). | Published tarball contains the ASTC module (verify the tarball, not the tag) |
| I4 | rff wiring (section 9.6), after 0.11.0 is on crates.io. | `rff -i tex.astc out.png` and `rff -i img.png -c:v astc out.astc` work; outputs equal the library's; the exe is rebuilt with `-p rff-cli` and its mtime checked |

---

## 6. Test and measurement plan

### 6.1 Corpus construction

Five sources. The working corpus lives outside git (`corpus/astc/`, gitignored, rebuilt by a script). Only the compact golden set is committed.

a. **Natural.** astcenc's `Small`, `Khronos` and `HDRIHaven` images, plus Kodak, encoded by `astcenc-avx2` at every 2D footprint (14) and every 3D footprint (10, using `-zdim` stacks), with `-thorough` (for mode diversity) and `-fastest`, in `-cl`, `-cs`, `-ch`, `-cH`. Decoded by the oracle in the legal cells.

b. **Directed.** A Rust generator (`examples/astc_blockgen.rs`, built on `pack.rs`; before E1 exists, built on a minimal field packer written for the purpose) that constructs blocks by field so that every census cell is hit on purpose. The case list follows the categories in `tcuAstcUtil.cpp` (section 1.7). A packer bug shared with the parser cannot hide here, because the oracle, not our parser, supplies the expected output.

c. **Random and mutated.** Uniform random 128-bit blocks (a large share are legal), and corpus blocks with 1 to 8 random bit flips, including flips in unused bits. Generated on the fly from a seeded PRNG; never stored.

d. **Illegal.** One directed family per illegal condition in section 4, per footprint, in every cell.

e. **Public third-party.** The 30 `.astc` files of google/astc-codec, the 14 DDS files of image-dds, the ASTC `.ktx`/`.ktx2` files of KTX-Software, and `Test/Data` from astcenc.

**Golden vectors (committed).** `tests/fixtures/astc/golden_v1.bin` plus an index: for each (footprint, cell), about 64 blocks per census class with the oracle's expected output stored as an FNV-1a 64 hash per block and the raw expected bytes for a small subset. Budget: at most 1.5 MB in `tests/fixtures/astc/` (it ships in the crate package; check with `cargo package --list`). Larger sets go under `corpus/astc/` and the tests that need them skip when absent, as existing campaign instruments do.

CI runs pure-Rust tests against the golden set. astcenc is never built in CI and is never a dependency.

### 6.2 Census

A block counts toward a cell only after it has decoded bit-exactly in every legal output cell. The census is the cross product of these axes, pruned to legal combinations:

| Axis | Values |
|---|---|
| Footprint | 14 2D, 10 3D |
| Block kind | void-extent LDR, void-extent HDR, normal, illegal x 10 reasons |
| Partitions | 1, 2, 3, 4 |
| CEM | each of the 16; and for multi-partition: same-CEM signalling, and each mixed-class selector (01, 10, 11) |
| Planes | single; dual with second-plane component 0, 1, 2, 3 |
| Weight range | each of the 12 |
| Colour range | each of the 17 |
| Weight grid | full size; decimated; each of the block-mode layout rows (10 in 2D, 7 in 3D) |
| HDR sub-modes | mode 7: each mode value and major component; mode 11: each mode value, each major component, and the direct form; mode 15: each alpha mode |
| LDR branches | blue contraction taken and not taken; endpoint swap taken and not; delta carry positive and negative |
| Profile x output | the six cells of DC1 |
| Error cause | illegal block; HDR void-extent in LDR; HDR partition next to an LDR partition in LDR |

The census script prints empty cells. Target: at least 32 blocks per cell (DC5). The table is published as `docs/artifacts/astc-census.md`.

### 6.3 Differential harness against astcenc

`tools/astc-oracle/oracle` commands (all read blocks on stdin, all single-threaded, all deterministic):

| Command | Output per block | Used by |
|---|---|---|
| `tables <bx> <by> <bz>` | Once: for each of 2048 modes, legal flag, grid, quant method, dual-plane, weight count, weight bits; for each legal grid, per-texel weight indices and contributions; for each (count, seed), the partition map | D1, D6 |
| `ise <quant> <count>` | `count` decoded values from `decode_ise` | D2 |
| `symbolic <bx> <by> <bz>` | `block_type`, `block_mode`, `partition_count`, `partition_index`, `color_formats[4]`, `quant_mode`, `color_values[4][8]`, `plane2_component`, `weights[64]` | D3, D4, E1 |
| `endpoints <profile>` | For (format, 8 input values): `ep0`, `ep1` (4 ints each), `rgb_hdr`, `alpha_hdr`, validity, from `unpack_color_endpoints` | D5 |
| `weights <bx> <by> <bz>` | Per-texel plane-1 and plane-2 weights from `unpack_weights` | D6 |
| `decode <profile> <u8 or f16> <bx> <by> <bz>` | The block's texels through `astcenc_decompress_image` on a one-block image, plus the return code | D7, DC1, DC2, EC1 |

The astcenc functions named here (`physical_to_symbolic`, `decode_ise`, `unpack_color_endpoints`, `unpack_weights`, `decompress_symbolic_block`, `init_block_size_descriptor`) are non-static and declared in `Source/astcenc_internal.h`. The budget fields of D3 are not stored by astcenc; the oracle recomputes them with the same expressions `physical_to_symbolic` uses and prints them.

Workflow: generate blocks (Rust) to a file, run the oracle (WSL), compare (Rust). One driver, `examples/astc_diff.rs`, does generate, run, compare for a named stage and prints the first mismatch as a field-by-field table with the block's hex. Triage rules from the decoder skill apply: a wrong value with a right bit budget is the stage you just added; a right value with a wrong budget is upstream.

For EC1 the same `decode` command checks our encoder's output, and `symbolic` confirms that the oracle parses the fields we meant to write.

### 6.4 Fuzzing

| Target | Property |
|---|---|
| `astc_decode` (libFuzzer) | For any bytes, footprint and profile: no panic, output buffers fully written, `AstcDecodeReport.illegal_blocks` consistent with a second pass |
| `astc_container` | `.astc`, KTX 1.0, KTX 2.0 and DDS-ASTC parsers: `Ok` or `Err`, never a panic, never an allocation larger than the input justifies |
| `astc_encode_roundtrip` | For arbitrary pixels, sizes 1..=64, footprint, preset, profile: encode succeeds, every block is legal by our parser, decode has the declared size |
| `tests/astc_robustness.rs` (stable, always on) | The deterministic half: seeded PRNG, 10^6 blocks per footprint, plus the (mode, partition count) sweep of D10 |

"Every 128-bit pattern maps to a defined output" is established by construction and by test: `decode_block` is a total function (`[u8; 16]` in, texels and a status out, no `Result`, no indexing that can fail), the crate forbids `unsafe` on this path, and DC2 plus D10 sample it at 10^7 per cell with the oracle agreeing on every sample.

### 6.5 Property tests

- Block-mode table: all 2^11 modes (DC3).
- ISE: `decode(encode(v)) == v` for every range and length; encoded bit count equals the formula.
- Quantisation: `unquant(quant(x))` is the nearest representable value for all 256 inputs and every range; quantise maps are inverse to the unquantisation tables.
- Colour-range table equals the iterative rule for every (pairs, bits).
- `parse(pack(s)) == s` (E1).
- Encoder: `decode(encode(x))` equals the encoder's reconstruction (EC2); a constant image encodes to void-extent blocks only; encoding is invariant to the block-row split.
- Decoder: decoding a surface by block rows equals decoding it whole; `_into` equals the allocating form.

### 6.6 Performance measurement

Rules (`codec-measurement`, and the house allocator rule in `F:/coding/remade_ffmpeg_rs/CLAUDE.md`):

- **Allocator.** Benches run under rusty_alloc, set as `#[global_allocator]` in the bench binary root only (`bench/astc/src/main.rs`), never in the library. Use the same `rusty_alloc-api` pin as rff at the time (`=2.5.0` today). `bench/astc/` is its own workspace with its own MSRV, like `bench/dds02/`, so the library's MSRV and dependency graph are untouched.
- **Correctness gate first.** A timing is recorded only for a build that passes DC7 or EC1 to EC3 on the same inputs.
- **Instruction counts for our own changes.** Callgrind Ir in WSL on a fixed input set, cache-resident sizes, one change per measurement. Keep or revert on Ir; confirm kept changes with wall time.
- **Wall time for claims.** Pinned ABBA: both arms on the same core mask, High priority, interleaved A B B A, at least 10 pairs, with a null arm (A against A) to show the noise floor. Copy `bench/ab_encode.ps1` to `bench/astc/ab.ps1` and adapt it.
- **Same OS for both arms.** Compare against astcenc built natively for the OS under test: Windows (astcenc built per `Docs/Building.md`) for the headline, WSL as a second data point. Never compare a Windows number with a WSL number.
- **Work parity.** Both arms decode the same blocks to the same output format into a preallocated buffer; state it beside every number. astcenc's "Decoding rate" excludes file I/O; our harness must too.
- **Per-image tables always.** A mean that hides a sign flip is not reported; a per-image sign flip in an encoder feature is a dispatch signal, not a loss.

Quality metrics (computed in `bench/astc`, in code we control, on pixels decoded by our own decoder once DC1 holds):

- PSNR on RGB and on RGBA, per image; mean per (footprint, preset).
- SSIM on luma, standard 11x11 Gaussian window.
- Delta E 2000 from sRGB through CIELAB: mean and 99th percentile.
- BD-PSNR and BD-rate over the six-footprint curve (section 2.3).
- Equal-quality speed ratio (ES1): bisect astcenc's float `-quality` until its PSNR matches ours within 0.05 dB, then compare MT/s.
- HDR (E7): astcenc's mPSNR definition (`-mpsnr`) reimplemented, plus RMSE in log2 RGB.

Artifacts written by the harness and committed: `docs/artifacts/astc-conformance.md`, `astc-census.md`, `astc-decode-vs-baselines.md`, `astc-encode-quality.md`, `astc-encode-speed.md`, each with the exact command, machine, date and commit.

---

## 7. Encoder strategy

### 7.1 What astcenc does

From `Source/astcenc_compress_symbolic.cpp` and the files it calls, and the tuning knobs documented in `Source/astcenccli_toplevel_help.cpp` and `Source/astcenc_preset_configs.h` (https://github.com/ARM-software/astc-encoder/tree/main/Source):

1. **Constant block** becomes a void-extent block.
2. **One partition, one plane.** Compute per-partition averages and principal directions (`astcenc_averages_and_directions.cpp`), project texels on the line to get ideal endpoints and ideal weights (`astcenc_ideal_endpoints_and_weights.cpp`). For each candidate weight grid, fit decimated weights. For each weight range, find the best low/high alignment of the quantised steps (`astcenc_weight_align.cpp`). Estimate the error of every (block mode, endpoint format, colour range) combination (`astcenc_pick_best_endpoint_format.cpp`) and keep the best few (`-candidatelimit`: 2, 3, 3, 4, 6, 8 from fastest to exhaustive). For each kept candidate: recompute endpoints for the quantised weights, pack them (`astcenc_color_quantize.cpp`), then iterate weight realignment and endpoint recomputation (`-refinementlimit`: 2, 3, 3, 4, 4, 4), and measure the true error by decoding.
3. **Block-mode pruning.** Only modes inside a precomputed popularity percentile are tried (`-blockmodelimit`: 43, 55, 77, 94, 98, 100; tables in `astcenc_percentile_tables.cpp`).
4. **Dual plane.** Tried per component when the lowest inter-channel correlation is under a preset limit (`-2planelimitcorrelation`: 0.50, 0.65, 0.85, 0.95, 0.98, 0.99).
5. **Two to four partitions.** k-means clustering of texel colours, then ranking of the 1024 partition patterns by mismatch against the clustering, then an error estimate for the best ranked (`-2partitionindexlimit`: 10, 18, 34, 82, 256, 512), then full trials of the top few (`-2partitioncandidatelimit`: 2, 2, 2, 3, 20, 32) (`astcenc_find_best_partitioning.cpp`). `-partitioncountlimit` is 2, 3, 4, 4, 4, 4.
6. **Early outs.** Stop when block PSNR passes `-dblimit` (a function of texel count), and do not try N + 1 partitions unless N improved on N - 1 by a preset factor (`-2partitionlimitfactor`, `-3partitionlimitfactor`).

### 7.2 What to port first, by quality per effort

| Order | Piece | Why here | Brick |
|---|---|---|---|
| 1 | Void-extent and a fixed-mode single-partition encoder | Conformance first; exercises the whole packer | E2 |
| 2 | Block-mode and quantisation search for one partition (weight grid x weight range x colour range) | This is ASTC's core trade: bits between weights and endpoints. It is the largest single quality step and needs no partition machinery | E3 |
| 3 | Endpoint refit after weight quantisation (least squares), one or two iterations | Cheap, and it recovers most of the quantisation loss | E3 |
| 4 | CEM choice: luminance modes, base+scale, base+offset, blue contraction | More precision for the same bits on common content | E3 |
| 5 | Two partitions by clustering and pattern ranking | The next largest step, on edges and multi-colour blocks | E4 |
| 6 | Dual plane | Decorrelated alpha and normal maps | E4 |
| 7 | Three and four partitions, larger candidate sets, weight realignment | Diminishing returns; this is the medium-to-thorough gap, about 0.25 dB on Kodak in astcenc's own reference numbers (section 1.3) | E5 |
| 8 | Mode shortlists and early-out thresholds | Speed at equal quality | E5, E6 |

The ordering is a hypothesis until the first harvest (7.6) measures how much of astcenc's quality comes from one-partition search alone.

### 7.3 Design rules for our encoder

- **Exact evaluation with the decoder's primitives.** The final choice among candidates is made on the squared error of the block decoded by `astc::reference` functions (or their proven-equal fast twins), in the integer domain of the target decode mode. Estimates only rank candidates. This is what makes EC2 hold by construction.
- **Target decode mode is an input.** `decode_unorm8: bool` chooses whether candidate error is measured after the top-8-bits rule or on the 16-bit value. Quality is always reported against the matching oracle cell.
- **The decoder's envelope caps the encoder.** The encoder emits only what `parse.rs` accepts as legal, and a debug assertion parses every emitted block.
- **Symbolic first.** The search works on `SymbolicBlock`; `pack.rs` is the only code that touches bits.
- **Frozen constants.** Preset limits and thresholds live in `src/astc/encode/tuning.rs`, overridable only with the existing dev-only `tuning` feature, as the BCn encoder does.
- **Clean implementation, not a translation.** Write from the spec, the HPG 2012 paper and astcenc's documented behaviour. See decision 2 in section 12 before translating any astcenc source file or table.

### 7.4 Presets

| Our preset | Partitions | Pattern candidates (2/3/4) | Mode shortlist | Candidates kept | Refinement | Dual plane |
|---|---|---|---|---|---|---|
| `Fastest` | 1, then 2 if the one-partition error is high | 2 / - / - | small | 2 | 1 | only when a channel is clearly uncorrelated |
| `Fast` | up to 2 | 2 / - / - | medium | 3 | 2 | correlation gate |
| `Medium` | up to 2 (3 once E5 lands) | 2 / 2 / - | large | 3 | 3 | correlation gate |
| `Thorough` | up to 4 | 3 / 2 / 2 | near full | 4 | 4 | near always |
| `Exhaustive` | up to 4 | all ranked | full | 8 | 4 | always |

These start from astcenc's documented limits (section 7.1) and are then **measured, not copied**: each limit is swept on the corpus and frozen where the equal-quality speed ratio is best. The names correspond loosely to astcenc's; the README states the measured gap per preset.

### 7.5 Determinism

- No hash maps, no RNG, no time, no thread-dependent order. k-means starts from fixed seeds chosen by a deterministic rule (farthest point from the mean), with a fixed iteration count.
- Floats: `f32` with a fixed operation order; no fused multiply-add; SIMD twins use the same association order as scalar (fixed-pattern horizontal sums), so results are bit-identical. Where an integer formulation costs nothing (final error evaluation), use integers.
- No libm: sine/cosine tables are baked literals; square root is our own routine.
- Ties broken by a total order: lower error, then lower mode index, then lower seed.
- Test: `tests/astc_determinism.rs` with frozen FNV-1a hashes per (image, footprint, preset, profile), run with `simd` on and off, and on x86_64, aarch64 and wasm32 so a platform difference fails loudly.

### 7.6 The first harvest (end of E3)

Before any partition work, run the full quality harness and write `docs/artifacts/astc-encode-quality.md` with per-image tables. Then, following the encoder skill ("bound a quality gap with the reference's own knobs before digging"):

1. Run astcenc with `-partitioncountlimit 1` and dual-plane disabled (`-2planelimitcorrelation 0`; astcenc `main` documents that a zero cutoff disables dual-plane, so check that 5.7.1 behaves the same and otherwise use the lowest cutoff it honours) at each preset. Our E3 encoder is compared with **that**, like for like. A gap here is a search or arithmetic defect in E3 and must be closed before E4.
2. The difference between that astcenc run and full astcenc is the budget E4 and E5 can win.
3. Confirm or replace the numbers in section 2.3 from these two measurements, and record the decision in the ledger.

---

## 8. Risks and unknowns

| # | Risk or unknown | Why it matters | Mitigation |
|---|---|---|---|
| R1 | **"Bit-exact with astcenc" is not one thing.** Output depends on the profile and on the output type (8-bit means `decode_unorm8`; float16 means `decode_float16`), and astcenc uses two different error colours in float16 (section 1.3). | A decoder can match one cell and be off by one on a third of pixels in another, as both existing Rust crates are. | The six-cell matrix of DC1 is the definition. The API exposes the same choice astcenc makes (output type selects the decode mode). Brick 0.4 scores the reference against itself before our decoder is scored. |
| R2 | **HDR exactness.** LNS to FP16, UNORM16 to FP16 with round-toward-zero, the `0x7BFF` clamp, HDR void-extent constants. | Float rounding differences would make "exact" impossible to state. | Do every conversion in integer arithmetic and compare raw half bit patterns; the decoder contains no float operation before the optional widening to `f32`, which is exact. HDR void-extent constants that are Inf or NaN are undefined in the spec: brick 0.4 measures whether astcenc's ISA builds agree on them (5.7.1 fixed an F16C conversion bug in this area, see `Docs/ChangeLog-5x.md`). If they agree, match them; if not, pass the stored bits through unchanged, exclude that class from DC1 by name, and document it. |
| R3 | **Partition function subtleties.** The `small_block` threshold (texel count under 31), the `partitions == 3` shifts, the 8-bit truncation of the squared seeds, z terms in 3D. | A wrong map still gives a plausible image; errors appear only on some seeds and footprints. | DC4 is exhaustive: all seeds, all counts, all footprints against the oracle. Transcribe the spec's C with the same integer widths (`u8` seeds, `u32` hash, wrapping operations written explicitly). |
| R4 | **ISE edge cases.** Truncated final trit/quint blocks, the interleaved bit layout, bit reversal for weights, bit counts. | Hidden by lengths that are multiples of 5 or 3 and by power-of-two ranges. | D2 covers every range and every length; E1 gates the encoder's ISE with a round trip and with the oracle's `decode_ise`. |
| R5 | **sRGB rules.** `0x80` expansion on all four channels, top 8 bits, no linearisation on output. | The astcenc comment and code disagree; the crate's sRGB policy ("stored bytes, no linearisation") must stay true. | Follow the spec and the code. DC1 `LdrSrgb x U8` on content with alpha gradients. Document in `docs/formats.md` that for ASTC the sRGB tag changes the decode arithmetic and the output is still sRGB-encoded bytes. |
| R6 | **Performance cliffs.** Per-block parse cost dominates at 4x4 (16 texels per 16 bytes); infill and partition lookups dominate at 12x12; partition and infill tables total hundreds of KB if built eagerly; `f32` output is 16 bytes per pixel; first-use table construction can look like a slow first decode. | A decoder that is fast on one footprint and slow on another fails DS1. | Measure four footprints, never one. Tables are built lazily per (footprint, grid) and per (footprint, count, seed) in a caller-visible context with a stated memory bound; report table-build time separately from steady-state. Fast paths are chosen from a block-kind histogram of the corpus, not by guess. Frame-size sweep before any cache-layout work (`codec-cache-tiles` rule). |
| R7 | **Encoder quality and time.** astcenc has been tuned since 2011 (its copyright line) and is SIMD throughout; a scalar-first encoder may land well under the section 2.3 targets or take far longer than estimated. | This is the largest schedule risk. | Numeric gates per preset; the like-for-like harvest of 7.6 before partition work; the equal-quality speed ratio so that speed and quality cannot be traded silently; a fallback cut line (section 11) that ships the decoder alone. The README states the measured gap. |
| R8 | **Self-consistent but illegal encoder output.** | Our decoder reads back what our encoder wrote, while a GPU or astcenc does not. | EC1 against astcenc on a conformance matrix as a standing gate; the oracle's `symbolic` dump on our bytes; adversarial content; a debug assertion that parses every emitted block. |
| R9 | **DDS layout refactor.** The 4x4 assumption is spread over pitch, pitch height, mip size and array stride. | A regression here breaks existing BCn users, on the attacker-controlled header path. | A separate brick (C3) with a byte-identical gate before any ASTC code is added: all existing tests, the frozen encode hashes, `parser_robustness`, and a new test that every existing fixture reports the same offsets and sizes for every subresource. Checked arithmetic throughout, as the file already does. |
| R10 | **The DXGI ASTC values are a convention, not a Microsoft format.** | A tool may disagree. | Use the values image-dds uses (133..=187), gate on its 14 files, say so in `docs/formats.md`. |
| R11 | **The oracle is moving.** astcenc `main` describes a 6.0.0 that is not tagged; it changes return codes for bad blocks and reworks internals. FFmpeg 9.1 links whatever libastcenc the system has. | "Exact with astcenc" needs a version. | Pin 5.7.1 by commit. Brick 0.4 lists 5.7.1-versus-main decode differences. Re-pin when 6.0.0 is tagged and re-run DC1 and DC2; say the version in every claim. |
| R12 | **MSRV 1.73 and `core`-only constraints.** No float `const fn`, no `LazyLock`, no `f32::sqrt` in `core`. | Table generation and encoder maths must work around them. | Section 3.3 (integer `const fn`, baked tables with a regeneration test), own square root, CI job on 1.73. |
| R13 | **Package size and CI.** `tests/fixtures/` ships in the crate; astcenc cannot be built in CI. | A large golden set bloats the crate; too small a set weakens CI. | 1.5 MB budget, hash-based goldens, big sets outside the package with skip-when-absent tests, `cargo package --list` check in I3. |
| R14 | **Licence.** astcenc is Apache-2.0; rusty_dds is MIT. | Translated astcenc code or tables would need Apache-2.0 notices and would change the crate's licence expression. | Decision 2 in section 12. Default: write from the spec and the paper, harvest our own mode tables, and keep astcenc as an oracle only. The oracle tool includes astcenc headers at build time in WSL and is not in the package. |
| R15 | **Determinism across platforms.** | A float that differs on aarch64 or wasm changes encoder output. | Section 7.5; frozen-hash test on three targets. |
| R16 | **rff gaps.** No float pixel format, no stream profile field, rusty_dds must be on crates.io first, and the family publishes as one set. | HDR ASTC cannot pass through rff in the first step. | I4 is LDR only; HDR waits on a float `PixelFormat` (decision 8). |
| R17 | **Unknown: how much of the corpus can public material cover.** | If public sets hit few census cells, the directed generator carries the conformance claim. | Brick 0.3 prints the census for public material alone before the generator is written, so its scope is known. |
| R18 | **Unknown: whether `-2planelimitcorrelation 0` disables dual-plane in 5.7.1.** | Needed for the like-for-like harvest. | Check in 7.6 step 1; fall back to the lowest cutoff 5.7.1 honours. |

---

## 9. API sketch

All additions. Names are proposals; shapes follow the existing decode functions.

### 9.1 Core types (`rusty_dds::astc`)

```rust
/// A legal ASTC block footprint. Only the 14 2D and 10 3D sizes can be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AstcFootprint { /* private */ }

impl AstcFootprint {
    pub const B4X4: Self;   // ... through B12X12, and B3X3X3 ... B6X6X6
    pub const ALL_2D: &'static [AstcFootprint];
    pub const ALL_3D: &'static [AstcFootprint];
    pub const fn new_2d(w: u32, h: u32) -> Option<Self>;
    pub const fn new_3d(w: u32, h: u32, d: u32) -> Option<Self>;
    pub const fn width(self) -> u32;
    pub const fn height(self) -> u32;
    pub const fn depth(self) -> u32;      // 1 for 2D
    pub const fn texels(self) -> u32;
}

/// Mirrors astcenc's four profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AstcProfile { LdrSrgb, Ldr, HdrRgbLdrA, Hdr }

/// What a surface decode saw. Illegal blocks are not errors: they decode to the
/// reference error colour, as the spec requires, and are counted here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct AstcDecodeReport {
    pub blocks: u64,
    pub illegal_blocks: u64,
    /// Blocks with an HDR endpoint mode or HDR void-extent decoded under an LDR profile.
    pub hdr_in_ldr_blocks: u64,
}

/// Parsed fields of one block, for tools and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AstcBlockInfo { /* kind, weight grid, ranges, partitions, seed, CEMs, dual plane */ }
```

### 9.2 Decode (free functions on raw block data)

```rust
/// Bytes needed for a surface. `None` on overflow.
pub fn payload_len(width: u32, height: u32, depth: u32, fp: AstcFootprint) -> Option<usize>;
pub fn block_rows(height: u32, fp: AstcFootprint) -> u32;

/// 8-bit output. `LdrSrgb` or `Ldr` only; an HDR profile returns `UnsupportedFormat`
/// (the spec makes 8-bit HDR decode invalid, and this crate does not tone-map).
/// `Ldr` uses the `decode_unorm8` rule. Output bytes are as stored (no linearisation).
pub fn decode_rgba8(data: &[u8], width: u32, height: u32, fp: AstcFootprint,
                    profile: AstcProfile) -> Result<ImageRgba8, Error>;
pub fn decode_rgba8_into(data: &[u8], width: u32, height: u32, depth: u32, fp: AstcFootprint,
                         profile: AstcProfile, out: &mut [u8]) -> Result<AstcDecodeReport, Error>;

/// Half-float output as raw IEEE 754 binary16 bit patterns (`decode_float16`). All profiles.
/// This is the surface that is bit-exact with astcenc's float16 output.
pub fn decode_rgba_f16_into(data: &[u8], width: u32, height: u32, depth: u32, fp: AstcFootprint,
                            profile: AstcProfile, out: &mut [u16]) -> Result<AstcDecodeReport, Error>;
/// The same values widened exactly to `f32`.
pub fn decode_rgba_f32_into(data: &[u8], width: u32, height: u32, depth: u32, fp: AstcFootprint,
                            profile: AstcProfile, out: &mut [f32]) -> Result<AstcDecodeReport, Error>;

/// The job-system seam: `rows` are block rows of one 2D slice; `out` receives only those rows.
pub fn decode_block_rows_into(data: &[u8], width: u32, height: u32, fp: AstcFootprint,
                              profile: AstcProfile, rows: core::ops::Range<u32>,
                              out: &mut [u8]) -> Result<AstcDecodeReport, Error>;
pub fn decode_block_rows_f16_into(/* same, out: &mut [u16] */) -> Result<AstcDecodeReport, Error>;
pub fn decode_block_rows_f32_into(/* same, out: &mut [f32] */) -> Result<AstcDecodeReport, Error>;

/// One block. Total: any 16 bytes produce defined texels.
pub fn decode_block_rgba8(block: &[u8; 16], fp: AstcFootprint, profile: AstcProfile,
                          out: &mut [[u8; 4]]) -> AstcBlockStatus;
pub fn decode_block_f16(block: &[u8; 16], fp: AstcFootprint, profile: AstcProfile,
                        out: &mut [[u16; 4]]) -> AstcBlockStatus;
pub fn inspect_block(block: &[u8; 16], fp: AstcFootprint) -> AstcBlockInfo;

/// The frozen scalar implementation, public for tests and benches (like `decode::reference`).
pub mod reference { /* same signatures */ }
```

Errors: short payload gives `Error::TruncatedData`; a wrong `out` length gives `Error::OutOfBounds`; zero dimensions give `Error::InvalidField`. This answers the issue's "a truncated block stream (must error)".

### 9.3 Encode

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AstcPreset { Fastest, Fast, Medium, Thorough, Exhaustive }

#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct AstcEncodeOptions {
    pub footprint: AstcFootprint,
    pub profile: AstcProfile,          // LdrSrgb or Ldr until E7
    pub preset: AstcPreset,            // default Medium
    /// Optimise for the `decode_unorm8` rounding rule (astcenc's `-decode_unorm8`). Default false.
    pub decode_unorm8: bool,
    pub channel_weights: [f32; 4],     // default [1.0; 4] (astcenc's `-cw`)
    pub alpha_weight: bool,            // scale colour error by alpha (astcenc's `-a`; FFmpeg's `alpha_weight`). Default false
}
impl AstcEncodeOptions {
    pub const fn new(footprint: AstcFootprint, profile: AstcProfile) -> Self;
    pub const fn with_preset(self, preset: AstcPreset) -> Self;   // and the other with_* builders
}

pub fn encode_rgba8(pixels: &[u8], width: u32, height: u32,
                    opts: &AstcEncodeOptions) -> Result<Vec<u8>, Error>;
pub fn encode_rgba8_into(pixels: &[u8], width: u32, height: u32,
                         opts: &AstcEncodeOptions, out: &mut [u8]) -> Result<(), Error>;
/// Caller-threaded seam. Output is identical however the rows are split.
pub fn encode_block_rows_into(pixels: &[u8], width: u32, height: u32, opts: &AstcEncodeOptions,
                              rows: core::ops::Range<u32>, out: &mut [u8]) -> Result<(), Error>;
// E7: encode_rgba_f32(...) for the HDR profiles.
```

Unsupported option combinations (an HDR profile before E7, a 3D footprint before E8, a preset not yet built) return `Error::UnsupportedFormat`; they never fall back silently.

### 9.4 Containers

```rust
/// A raw `.astc` file, borrowed (like `DdsView`).
pub struct AstcFile<'a> { pub footprint: AstcFootprint, pub width: u32, pub height: u32,
                          pub depth: u32, pub data: &'a [u8] }
impl<'a> AstcFile<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error>;
    pub fn write<W: std::io::Write>(w: &mut W, footprint: AstcFootprint, width: u32, height: u32,
                                    depth: u32, payload: &[u8]) -> Result<(), Error>;
    pub fn surface(&self) -> AstcSurface<'a>;
}

/// One ASTC subresource from any container, with what the container knows about it.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct AstcSurface<'a> {
    pub footprint: AstcFootprint, pub width: u32, pub height: u32, pub depth: u32,
    /// From the container tag. `.astc` has no tag: `None`.
    pub srgb: Option<bool>,
    /// `Some(true)` only where the container says so (KTX2 `SFLOAT` formats).
    pub hdr: Option<bool>,
    pub data: &'a [u8],
}
impl AstcSurface<'_> {
    pub fn decode_rgba8(&self, profile: AstcProfile) -> Result<ImageRgba8, Error>;
    pub fn decode_rgba_f32(&self, profile: AstcProfile) -> Result<ImageRgbaF32, Error>;
    pub fn default_profile(&self) -> AstcProfile;   // srgb -> LdrSrgb; hdr -> Hdr; untagged -> LdrSrgb (FFmpeg's choice)
    pub fn gpu_format(&self) -> GpuFormat;
}

pub mod ktx {
    pub struct KtxView<'a>  { /* KTX 1.0 */ }
    pub struct Ktx2View<'a> { /* KTX 2.0 */ }
    // parse(bytes), levels(), layers(), faces(), astc_surface(level, layer, face) -> Result<AstcSurface, Error>
    // write_ktx1(w, &AstcSurface-like description, payloads per level)
}
```

### 9.5 The DDS path (`DdsBase`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct AstcContent { pub footprint: AstcFootprint, pub srgb: bool }

impl<D: AsRef<[u8]>> DdsBase<D> {
    /// Classify an ASTC DDS (DXGI 133..=187). Separate from `decode_content()`, as BC6H is,
    /// so `DecodeContent::ALL_LDR` and the BCn encode matrix are untouched.
    pub fn astc_content(&self) -> Result<AstcContent, Error>;
    pub fn astc_surface(&self, id: SubresourceId) -> Result<AstcSurface<'_>, Error>;
}
impl Dds {
    /// Like `encode_bc6h_uf16`: a dedicated entry point for a format outside `DecodeContent`.
    pub fn encode_astc_from_rgba8(pixels: &[u8], width: u32, height: u32,
                                  opts: &AstcEncodeOptions) -> Result<Dds, Error>;
}
impl DecodeOptions {
    /// Override the profile used for ASTC surfaces (for example HDR data in a `_UNORM`-tagged DDS).
    pub const fn astc_profile(self, profile: AstcProfile) -> Self;
}
```

Existing methods gain ASTC routing, with no signature change:

| Method | ASTC behaviour |
|---|---|
| `decode_rgba8`, `_into`, `_with`, `decode_block_rows_into`, `block_rows` | Profile `LdrSrgb` for `_UNORM_SRGB`, else `Ldr`. 8-bit rule. Block rows are `footprint.height()` pixel rows, not 4. |
| `decode_rgba_f32`, `_into`, `decode_block_rows_f32_into`, `block_rows_f32` | sRGB-tagged: the 8-bit result as an exact `x / 255` (the crate's rule for formats defined at 8 bits; the spec says sRGB always returns 8-bit UNORM). Linear-tagged: profile `Hdr` in `decode_float16`, which decodes LDR blocks identically and HDR blocks instead of erroring; the spec permits this substitution and FFmpeg makes the same choice for linear KTX. |
| `gpu_format`, `upload_plan_compressed` | `GpuFormat { dxgi_name: "ASTC_6X6_UNORM", wgpu_name: "Astc { block: B6x6, channel: Unorm }", vulkan_name: "VK_FORMAT_ASTC_6x6_UNORM_BLOCK", block_bytes: 16, block_width: 6, block_height: 6, compressed: true }` and the sRGB variants. `bytes_per_row` and `rows_per_image` already derive from `block_width`/`block_height`. |
| `upload_plan_decoded_rgba8` | Unchanged shape. |
| `get_pitch`, `get_pitch_height`, mip and array-stride helpers | Block-dimension aware. The public `DataFormat` trait gains a provided method `get_block_dimensions() -> Option<(u32, u32)>` (default derived from `get_block_size`), which is additive. |

Cargo features: a new `astc` feature, on by default, gates the module. ASTC decode additionally needs `decode`; ASTC encode needs `encode`. `default-features = false, features = ["decode"]` users compile exactly what they compile today.

**Semver.** Everything is additive, and the enums and structs that grow are already `#[non_exhaustive]` (`DxgiFormat`, `GpuFormat`, `UploadPath`, `Error`, `DecodeOptions`). One observable behaviour change: a DDS whose DX10 header names DXGI 133..=187 used to be rejected (the enum stops at 132) and now parses and decodes. Brick C3 records exactly which error it returned before. The crate is 0.10.x, so the release is **0.11.0**.

### 9.6 rff wiring (remade_ffmpeg_rs), after rusty_dds 0.11.0 is published

Goal: `rff -i tex.astc out.png` and `rff -i img.png -c:v astc out.astc`, mirroring FFmpeg 9.1 (section 1.5).

| Piece | Change |
|---|---|
| `crates/rff-core/src/media.rs` | `CodecId::Astc`, name `"astc"`, `MediaType::Video`. |
| New `crates/rff-codec-astc` | Decoder and encoder over `rusty_dds::astc`, modelled on `rff-codec-webp`. Decoder: packet is the raw block payload; dimensions and footprint come from the stream's 16-byte `.astc` header in `extradata` (what FFmpeg publishes); output `PixelFormat::Rgba`; option `dec_profile` (`ldr`, `ldr-srgb`; default `ldr-srgb` for untagged input, as FFmpeg). Encoder: `accepted_pixel_formats` `[Rgba, Rgb24]`; `configure` reads `block_size` (default `8x8`), `quality` (0 to 100, default 60, mapped to the nearest preset: under 5 `Fastest`, under 35 `Fast`, under 80 `Medium`, under 99.5 `Thorough`, else `Exhaustive`; presets not yet built clamp down with a logged note), `profile` (default `ldr-srgb`), `alpha_weight` (default 1). Threading over block rows lives here. |
| New `crates/rff-format-astc` | Demuxer and muxer for `.astc`: probe on the magic `13 AB A1 5C`; `MuxCaps::single(&[CodecId::Astc]).image()`. |
| New `crates/rff-format-ktx` | KTX 1.0 demuxer and muxer for ASTC, with FFmpeg's limits (2D, one face, one mip on write) and its `srgb` muxer option. The sRGB/linear tag travels in `Stream.color` transfer. |
| `crates/rff/src/lib.rs`, `crates/rff/Cargo.toml`, root `Cargo.toml` | Register the codec and the two formats; add workspace members and `rusty_dds = { version = "0.11", default-features = false, features = ["decode", "encode", "astc", "simd"] }`. |
| `crates/rff/src/transcode.rs` | Add `Some("astc") | Some("ktx") => Some((CodecId::Astc, false))` to the `required` table so `out.astc` implies the codec. |
| Tests | Round trip PNG to ASTC to PNG equals the library; `.astc` written by rff decodes identically in astcenc; if an FFmpeg 9.1 build with `--enable-libastcenc` is available, cross-decode both ways. |

House rules that apply: build and test the CLI with `-p rff-cli` and check the exe's mtime before trusting an A/B; the family publishes as one set; new crates hit the crates.io new-crate rate limit (three new crates here). HDR ASTC through rff needs a float `PixelFormat` first (decision 8).

---

## 10. Release and README plan

### 10.1 What the README may claim, and only after which measurement

| Claim | Allowed when | Evidence file |
|---|---|---|
| "ASTC decode: LDR and HDR, 14 2D and 10 3D footprints, sRGB and linear, 8-bit and half-float" | DC1 to DC6 pass | `docs/artifacts/astc-conformance.md`, `astc-census.md` |
| "Byte-identical to ARM astcenc 5.7.1 on N blocks in six decode cells" (with N) | DC1 and DC2 pass; N is the count from the run | same |
| "The two existing pure-Rust ASTC decoders differ from astcenc by 1 on X% of pixels" | Re-measured in `bench/astc` on at least three textures, versions named | `docs/artifacts/astc-decode-vs-baselines.md` |
| Decode speed versus `astc-decode`, `texture2ddecoder` and astcenc | DS1 at least; numbers from pinned ABBA under rusty_alloc, per footprint, with the null arm | same |
| "Pure-Rust ASTC encoder" and any "first" wording | EC1 to EC3 pass; the crates.io survey of section 1.4 repeated on release day. Word it as "to our knowledge", with the date | `docs/artifacts/astc-encode-quality.md` |
| Encoder quality versus astcenc per preset | The per-footprint, per-image tables exist; state the gap as measured, including where we lose | same |
| Encoder speed versus astcenc | The equal-quality ratio against both `astcenc-none` and `astcenc-avx2`, both stated | `docs/artifacts/astc-encode-speed.md` |
| "No C, builds for wasm" | The wasm and no-`simd` CI jobs are green | CI |
| FFmpeg comparison ("FFmpeg 9.1 wraps libastcenc") | Cite the FFmpeg Changelog URL | section 1.5 |

Things the README must say plainly: which astcenc version "exact" refers to; that 8-bit linear output follows `decode_unorm8`; that the DXGI ASTC values are a convention; what is not there yet (HDR encode, 3D encode, KTX2 write or supercompression, `Thorough`/`Exhaustive` if they are after the cut).

README edits: the status block, the capability table (new rows: ASTC decode, ASTC encode, `.astc`/KTX containers), the performance snapshot (a short ASTC table linking to the artifacts), the feature table (`astc`), the platform table (wasm), and a quick-start snippet for `astc::decode_rgba8` and `AstcFile::parse`.

### 10.2 `docs/formats.md`

- Add "Supported (ASTC matrix)": rows for 2D LDR, 2D HDR, 3D, with columns Decode, Encode, `.astc`, KTX 1.0, KTX 2.0, DDS, Compressed upload; plus the profile and output-type table from DC1 and the error-colour rules of section 1.3.
- Change the "Explicitly unsupported" row from "ASTC / ETC / PVRTC" to "ETC / PVRTC".
- Add the sRGB note of R5 and the DDS convention note of R10.
- Add `astc` to the feature-gate table.
- Also update `docs/plans/texture-pipeline.md` where it lists ASTC as excluded.

### 10.3 CHANGELOG (house style: what changed, the number, the command that reproduces it)

```
## 0.11.0 - <date>

**ASTC: decode, encode and containers, in pure Rust** (Remade-With-Rust/rusty_dds#6).

### Added — ASTC decode
### Added — ASTC encode (presets ...)
### Added — `.astc`, KTX 1.0 and KTX 2.0 (read) containers; ASTC inside DDS
### Changed — DDS layout maths is block-dimension aware (byte-identical for every existing format)
```

### 10.4 Release mechanics

1. Commits use Conventional Commits; release-plz opens the release PR on push to `master`.
2. release-plz treats a `feat` on a 0.x crate as a patch bump, and the open release PR (#9) is for 0.10.2. To get 0.11.0, add a `release-plz.toml` with `features_always_increment_minor = true` for this package (https://release-plz.dev/docs/config) in the same PR series, and confirm the release PR then proposes 0.11.0. Avoid a hand bump: in this org a hand-bumped version has caused the next release PR to re-bump for the changelog alone.
3. Before merging the release PR: `cargo package --list` (no corpora, no `tools/`, fixtures within budget), MSRV 1.73 build, no-default-features build, wasm build, `cargo doc` with `--cfg docsrs`.
4. After publish: download the crates.io tarball and confirm `src/astc/` is in it before saying "released".
5. Update `Cargo.toml`: description mentions ASTC and KTX; keywords (five allowed) proposed as `dds`, `astc`, `ktx`, `texture`, `bc7`.
6. `THIRD-PARTY-NOTICES.md`: list the test material that is committed (with licences) and, if decision 2 goes that way, any astcenc-derived file.
7. Reply on issue #6 with the measured numbers and the version, using the `github-replies` skill; ArtCraft asked for that.

---

## 11. Effort and cut line

Focused working days for one builder, with the bricks of section 5.

| Phase | Bricks | Days | New Rust (approx. lines, with tests) |
|---|---|---|---|
| 0 Rig | 0.1 to 0.5 | 2.5 | 600 plus 500 C++ |
| D Decoder | D1 to D11 | 8 | 3,750 |
| C Containers | C1, C2, C3, C4a | 3.5 | 1,300 |
| E Encoder, first part | E1 to E4 | 11 | 4,000 |
| P Performance, first part | P1 | 2 | 500 |
| I Integration and release | I1 to I3 | 2.5 | 600 |
| **First release total** | | **about 30** | **about 10,750** |
| After the cut: rff wiring | I4 | 2.5 | 900 (in remade_ffmpeg_rs) |
| After the cut: `Thorough`, `Exhaustive` | E5, E6 | 5 | 1,200 |
| After the cut: SIMD | P2, P3, P4 | 7 | 1,800 |
| After the cut: KTX 2.0 write | C4b | 1.5 | 300 |
| After the cut: HDR encode | E7 | 5 | 1,200 |
| After the cut: 3D encode | E8 | 2 | 400 |

The encoder estimates carry the most uncertainty (R7); treat E3 and E4 as plus or minus 50%.

### Recommended cut line for the first release (0.11.0)

**In:** the full decoder (LDR and HDR, 2D and 3D, six decode cells, error colours), `.astc`, KTX 1.0 read and write, KTX 2.0 read, ASTC inside DDS, a conformant LDR encoder with `Fastest`, `Fast` and `Medium`, scalar performance to DS1, the rusty_dds API, docs, and the issue #6 reply.

**Out, in this order afterwards:** rff wiring (needs the published crate; start it the day 0.11.0 is out), `Thorough` and `Exhaustive`, SIMD for decoder then encoder, KTX 2.0 write, HDR encode, 3D encode.

**Fallback cut, agreed in advance.** ArtCraft's issue asks for decode only. If the E3 harvest (section 7.6) shows the `Medium` target is more than a week away, ship 0.11.0 with phases 0, D, C, P1 and I (about 18 days) and move the encoder to 0.12.0. The module boundaries make this a packaging choice: `pack.rs` and `encode/` are behind `feature = "encode"` and are simply not announced.

---

## 12. Decisions for Tim

1. **Module or crate.** This plan puts ASTC in `rusty_dds::astc` as asked, with a hard boundary so it can move. The alternative is a separate `rusty_astc` crate (zero dependencies, `no_std` + `alloc` enforced by the compiler, its own crates.io page and keyword) that rusty_dds depends on and re-exports, the way `bcdec_rs` sits under BCn. It would let rff depend on the small crate and would make the `no_std` claim structural. Cost: a workspace or a second repo, and coupled releases. Decide before brick D1 names anything public.
2. **Licence posture for the encoder.** Default in this plan: write from the spec and the HPG paper, keep astcenc as an oracle, harvest our own tables; the crate stays MIT. The faster route is to translate astcenc modules and tables (Apache-2.0), which requires notices and changes the crate's licence expression. Also confirm whether any ASTC intellectual-property notice from the Khronos specification needs repeating in our docs; this plan did not research that.
3. **Cut line.** The recommended cut (decoder plus three-preset encoder) or the fallback (decoder and containers first).
4. **Feature default.** `astc` on by default (proposed), or opt-in.
5. **HDR tagging where the container cannot say.** KTX 1.0 and the DXGI convention have no HDR ASTC tag. Proposed: read linear-tagged data with the `Hdr` profile on the float path and the `Ldr` profile on the 8-bit path; on write, use the linear tag for HDR. FFmpeg refuses to write HDR to KTX 1.0; we could do the same.
6. **KTX 2.0 supercompression.** Zstandard needs a decompressor dependency (the house `rusty_zstd`). Proposed: scheme 0 only in the first release, Zstandard behind an optional feature later.
7. **Oracle version.** Pin 5.7.1 now and re-pin when 6.0.0 is tagged (proposed), or wait for the tag.
8. **rff and HDR.** Adding a float `PixelFormat` (and possibly a stream profile field) to `rff-core` is needed before HDR ASTC can pass through rff. Out of scope here unless wanted.
9. **Error colour on the `f32` path.** Proposed: match astcenc (NaN for illegal blocks). Some consumers would prefer magenta; that would be an option on `DecodeOptions`.
10. **Preset targets.** The numbers in section 2.3 are proposals to be confirmed at the E3 harvest.

---

## Appendix A: how the scout numbers were taken

All in WSL Ubuntu, astcenc 5.7.1 built as in section 1.3, files in `/root/astcws/scout`.

```sh
A=/root/astcws/rel571/inst/bin/astcenc-avx2
IMG=/root/astcws/astc-encoder/Test/Images/Khronos/LDR-RGB/ldr-rgb-diffuse.png
for b in 4x4 6x6 8x8 12x12; do $A -cl $IMG d_$b.astc $b -medium -j 8 -silent; done
$A -dl d_6x6.astc d_6x6.png -j 1        # prints "Decoding rate"
$A -ds d_6x6.astc s_6x6.png -j 4 -silent
$A -tl $IMG t.png 6x6 -medium -j 1      # prints PSNR and "Coding rate"
# Rust baselines: /root/astcws/scout/rsbase (astc-decode =0.3.1, texture2ddecoder =0.1.2, png 0.17)
./rsbase/target/release/rsbase d_6x6.astc d_6x6.png   # MT/s and mismatch count against the PNG
```

They are single unpinned runs under the system allocator in a VM. Redo every one of them under section 6.6 before quoting it.

## Appendix B: references

Specification and formats
- Khronos Data Format Specification, ASTC: https://registry.khronos.org/DataFormat/specs/1.3/dataformat.1.3.html#ASTC , https://registry.khronos.org/DataFormat/specs/1.4/dataformat.1.4.html#ASTC , source https://github.com/KhronosGroup/DataFormat/blob/main/astc.txt
- ASTC decode-mode extension: https://registry.khronos.org/OpenGL/extensions/EXT/EXT_texture_compression_astc_decode_mode.txt
- GL ASTC enums: https://registry.khronos.org/OpenGL/extensions/KHR/KHR_texture_compression_astc_hdr.txt , https://registry.khronos.org/OpenGL/extensions/OES/OES_texture_compression_astc.txt
- Vulkan formats: https://github.com/KhronosGroup/Vulkan-Headers/blob/main/include/vulkan/vulkan_core.h
- KTX 1.0: https://registry.khronos.org/KTX/specs/1.0/ktxspec.v1.html ; KTX 2.0: https://registry.khronos.org/KTX/specs/2.0/ktxspec.v2.html
- DXGI ASTC convention as implemented: https://github.com/image-rs/image-dds/blob/main/src/header.rs

ARM astcenc
- Repository: https://github.com/ARM-software/astc-encoder (release 5.7.1: https://github.com/ARM-software/astc-encoder/releases/tag/5.7.1)
- Format overview: https://github.com/ARM-software/astc-encoder/blob/main/Docs/FormatOverview.md
- File format: https://github.com/ARM-software/astc-encoder/blob/main/Docs/FileFormat.md
- Encoding guidance: https://github.com/ARM-software/astc-encoder/blob/main/Docs/Encoding.md
- Building: https://github.com/ARM-software/astc-encoder/blob/main/Docs/Building.md ; Testing: https://github.com/ARM-software/astc-encoder/blob/main/Docs/Testing.md
- Change logs: https://github.com/ARM-software/astc-encoder/blob/main/Docs/ChangeLog-5x.md , https://github.com/ARM-software/astc-encoder/blob/main/Docs/ChangeLog-6x.md
- Sources cited: https://github.com/ARM-software/astc-encoder/tree/main/Source (`astcenc_symbolic_physical.cpp`, `astcenc_decompress_symbolic.cpp`, `astcenc_color_unquantize.cpp`, `astcenc_image.cpp`, `astcenc_entry.cpp`, `astcenc_internal.h`, `astcenc_preset_configs.h`, `astcenccli_toplevel_help.cpp`)
- Paper: https://www.highperformancegraphics.org/previous/www_2012/media/Papers/HPG2012_Papers_Nystad.pdf

FFmpeg 9.1
- Changelog: https://github.com/FFmpeg/FFmpeg/blob/master/Changelog
- https://github.com/FFmpeg/FFmpeg/blob/master/libavcodec/libastcenc.c , https://github.com/FFmpeg/FFmpeg/blob/master/libavcodec/libastcdec.c
- https://github.com/FFmpeg/FFmpeg/blob/master/libavformat/astcdec.c , https://github.com/FFmpeg/FFmpeg/blob/master/libavformat/astcenc.c
- https://github.com/FFmpeg/FFmpeg/blob/master/libavformat/ktxdec.c , https://github.com/FFmpeg/FFmpeg/blob/master/libavformat/ktxenc.c

Rust crates surveyed
- https://crates.io/crates/astc-decode (https://github.com/wwylele/astc-decode)
- https://crates.io/crates/texture2ddecoder (https://github.com/UniversalGameExtraction/texture2ddecoder)
- https://crates.io/crates/dds (https://github.com/image-rs/image-dds ; ASTC decode at https://docs.rs/crate/dds/latest/source/src/decode/astc.rs)
- https://crates.io/crates/astcenc-rs , https://crates.io/crates/astcenc-sys , https://crates.io/crates/ctt , https://crates.io/crates/intel_tex_2 , https://crates.io/crates/basis-universal , https://crates.io/crates/basisu , https://crates.io/crates/ktx2

Test material
- https://github.com/google/astc-codec/tree/master/src/decoder/testdata
- https://github.com/image-rs/image-dds/tree/main/test-data/images/astc
- https://github.com/KhronosGroup/KTX-Software/tree/main/tests/resources
- https://github.com/KhronosGroup/VK-GL-CTS/blob/main/framework/common/tcuAstcUtil.cpp
- Kodak: http://r0k.us/graphics/kodak/

Tooling
- release-plz configuration: https://release-plz.dev/docs/config
- Issue: https://github.com/Remade-With-Rust/rusty_dds/issues/6

---

## 13. Decisions log (2026-11-21)

Tim's answers to section 12, as given to the builder. They are the plan's own defaults. If a phase proves one unworkable, the builder stops and reports instead of switching.

| # | Question | Decision |
|---|---|---|
| 1 | Module or crate | Module `rusty_dds::astc` inside rusty_dds, with a hard internal boundary (nothing under `src/astc/` depends on the DDS half except `Error`) so it can be split out later. |
| 2 | Licence posture | Clean implementation; the crate stays MIT. **No astcenc source is translated.** The Khronos Data Format Specification is the normative reference. astcenc (Apache-2.0) is used only as a black-box oracle: its binaries, its static library linked into `tools/astc-oracle` (built in WSL, never part of the package or the dependency graph), and per-field dumps from that tool. astcenc source may be read for algorithm ideas; code and tables are never copied. Encoder tables (mode shortlists, thresholds) are harvested from our own corpus runs. |
| 3 | Cut line | The recommended 0.11.0 cut first (section 11), then the post-release order: `Thorough`/`Exhaustive`, SIMD (fenced, scalar oracles kept), KTX 2.0 write, HDR encode, 3D encode. rff wiring (I4) is out of this builder's scope: it needs the published crate. Pre-agreed fallback: if the E3 harvest puts `Medium` quality more than a week away, finish decoder plus containers first and continue the encoder after. |
| 4 | release-plz | Add `release-plz.toml` with `features_always_increment_minor = true`, in its own small commit. No hand version bump. |
| 5 | Feature default | `astc` is on by default. |
| 6 | HDR tagging in KTX 1.0 / DDS | As section 12 item 5 proposes: linear-tagged data reads with `Hdr` on the float path and `Ldr` on the 8-bit path; HDR is written with the linear tag. Documented in `docs/formats.md`. |
| 7 | KTX 2.0 Zstandard | Deferred, unless it is cheap with a pure-Rust zstd; the house crate `rusty_zstd` may be used if suitable. |
| 8 | Oracle version | astcenc 5.7.1, commit `0affb51aaf0e0de3a8b038fa32487d59754a1eed`. **Follow-up:** re-pin and re-run DC1/DC2 when 6.0.0 is tagged. |
| 9 | rff float `PixelFormat` | Out of scope. |
| 10 | `f32` error path | Match astcenc exactly in each of the six DC1 cells: NaN (`0xFFFF` half) for illegal blocks and HDR void-extent in LDR profiles; magenta for HDR endpoint modes in LDR profiles; magenta in every 8-bit cell. |

Builder conventions on top of the plan:

- Benches, bins and examples that run the encoder use `rusty_alloc-api` as `#[global_allocator]` in their own roots (standalone harness crates under `bench/`), never in the library.
- No C in the build: no `build.rs`, no `-sys` dependency. `tools/astc-oracle` is outside the crate's dependency graph and excluded from the package.
- The crate's unsafe/SIMD policy is kept as found: `forbid(unsafe_code)` without `simd`; with it, `unsafe` only in `#[target_feature]` kernels with a scalar twin, a `SAFETY:` comment and a `*_matches_scalar` test.
