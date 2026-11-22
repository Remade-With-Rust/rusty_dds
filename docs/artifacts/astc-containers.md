# ASTC containers and ASTC inside DDS: gates

Date 2026-11-21. Oracle: astcenc 5.7.1 (`0affb51`) CLI and decoder. Third-party files are
fetched, never committed: astcenc `Test/Data`, image-rs/image-dds `test-data/images/astc`,
KhronosGroup/KTX-Software `tests/resources/{ktx,ktx2}` (Git LFS objects via
media.githubusercontent.com).

Command: `tools/astc-harness`: `astc_harness containers` and `astc_harness dds` (WSL).

| Brick | Check | Result |
|---|---|---|
| C1 `.astc` | astcenc's `negative_block_size/huge/magic/overflow/short.astc` | all rejected with an error, no panic |
| C1 | astcenc-written `.astc` (6x5 linear, 12x12 sRGB, the 1x1 test file) decode through `AstcFile` to astcenc's own PNG | equal |
| C1 | `AstcFile::write` reproduces the input bytes; astcenc decodes the written file | equal |
| C1 | payload one byte short | `Error::TruncatedData` |
| C2 KTX 1.0 | astcenc-written `.ktx` (6x5 linear, 10x8 sRGB, 4x4 linear) read, sRGB tag correct, level 0 equals astcenc's PNG | equal |
| C2 | `KtxView::write` output read back and decoded by astcenc | equal |
| C2/C4a | KTX-Software `astc_8x8_unorm_array_7.ktx` and `.ktx2`: same payload per layer (7 layers); layer 0 vs astcenc | equal |
| C4a KTX 2.0 | `ktx_app_astc_8x8.ktx2` (1024x1024, 11 levels, sRGB), `r8g8b8a8_srgb_mip_astc.ktx2` (64x64, 7 levels): level 0 rewrapped as KTX 1.0 and decoded by astcenc | equal |
| C4a + zstd | `Desk_astc_hdr6x6.ktx2` (supercompression scheme 2, SFLOAT 6x6, 644x874) inflated with `ktx2-zstd` (rusty_zstd 0.5); payload decoded in all six cells against the oracle | 0 differences |
| C3 refactor | `tests/layout_snapshot.rs`: 4,063 layout lines (every fixture plus every DXGI format x odd/POT sizes x mips/arrays/cubes/volumes), recorded before the change | unchanged |
| C3 DDS | image-dds's 14 ASTC DDS files (129x66, 8 mip levels, every 2D footprint): parse, the exact mip chain accounts for every payload byte, level 0 equals astcenc decoding the payload | 14 / 14 |
| C3 DDS | DDS files written by `Dds::new_dxgi` with ASTC formats (6x6 64x48; 8x5 37x29 with 3 mips; 12x12 100x100) read by the `dds` 0.2 crate | pixels equal astc-decode 0.3.1 on the same payload (the decoder the `dds` crate uses) |
| Robustness | `tests/astc_containers.rs`: every truncation, every bit flip in the first 128 bytes, 3,000 seeded arbitrary payloads behind each magic | no panic |

Behaviour change: a DDS whose DX10 header names DXGI 133..=187 used to fail with
`InvalidField("dxgi_format")`; it now parses and decodes. The DXGI ASTC values are a
convention (image-dds and the tools it interoperates with), not a Microsoft-defined format.
