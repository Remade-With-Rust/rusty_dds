# ASTC encode quality vs astcenc

Encoder under test: `rusty_dds::astc::encode_rgba8`, LDR linear profile, branch `feat/astc-perf` at
`687b7d3` (Fastest, Fast and Thorough are byte-identical to `feat/astc` `f0ed691`; on that commit
Medium measured -0.391 / -0.340 / -0.308 / -0.455 / -0.426 / -0.369). Reference: ARM astcenc 5.7.1 (`astcenc-none`, `-cl <fp> -<preset> -j 8`), at the
same preset name. Machine: WSL2 on an Intel Core i7-14650HX. Date: 2026-11-21.

## Method

- Images: Kodak 01, 03, 05, 08, 13, 15, 19 and 23 (768x512, opaque).
- Metric: RGB PSNR of each encode against its source. Both encodes are decoded by
  `rusty_dds::astc`, which is bit-exact with astcenc's own decoder (`astc-conformance.md`).
- Gap = ours minus astcenc, in dB, so a positive gap means we are better. The tables give the
  mean over the 8 images and the worst single image.
- EC1: every block we emitted in these runs was decoded by astcenc 5.7.1 in all six decode
  cells and matched our decode. Bad blocks: **0**.
- Command: `astc_harness quality <preset> 4x4,5x5,6x6,8x8,10x10,12x12 ldr "" <images>`. The
  MT/s column is our single-thread wall rate during the sweep, for orientation only; speed
  claims are in `astc-encode-speed.md`.

## Results

| Preset | 4x4 | 5x5 | 6x6 | 8x8 | 10x10 | 12x12 | worst image | target (plan 2.3) |
|---|---|---|---|---|---|---|---|---|
| Fastest | +0.080 | +0.095 | -0.121 | -0.141 | -0.156 | +0.057 | -0.741 | mean >= -0.30: **pass** |
| Fast | -0.031 | +0.089 | +0.043 | +0.027 | +0.034 | +0.185 | -0.541 | mean >= -0.30: **pass** |
| Medium | -0.407 | -0.351 | -0.311 | -0.456 | -0.426 | -0.377 | -0.711 | mean >= -0.50: **pass** |
| Thorough | -0.384 | -0.293 | -0.313 | -0.425 | -0.458 | -0.460 | -0.658 | none set |

The worst-image floor (at least -1.5 dB) holds for every preset.

Absolute means (dB, ours / astcenc):

| Preset | 4x4 | 6x6 | 8x8 | 12x12 |
|---|---|---|---|---|
| Fastest | 43.691 / 43.611 | 36.566 / 36.687 | 32.458 / 32.599 | 28.630 / 28.573 |
| Fast | 43.989 / 44.020 | 36.872 / 36.829 | 32.807 / 32.779 | 28.867 / 28.682 |
| Medium | 44.223 / 44.630 | 37.062 / 37.374 | 32.996 / 33.453 | 29.042 / 29.419 |
| Thorough | 44.510 / 44.894 | 37.295 / 37.608 | 33.261 / 33.686 | 29.300 / 29.760 |

## HDR (first version) and 3D

HDR encode (`encode_rgba_f32`, one partition, endpoint modes 11/14/15) at Medium, log2 RMSE
over RGB, ours / astcenc (`-ch`/`-cH -medium`): 0.448 / 0.219 at 4x4, 0.670 / 0.496 at 6x6,
and 0.928 / 0.757 at 8x8. HDR quality is not claimed yet. EC1 is 0 bad blocks for both HDR
profiles, and for all ten 3D footprints (`encode_volume_rgba8`, 256x256x6 volume)
(`astc_harness ec1x`).
