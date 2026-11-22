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

## HDR (branch `perf/astc-hdr-quality`, 2026-11-22)

HDR encode (`encode_rgba_f32`, profile `HdrRgbLdrA`, astcenc's `-ch`) at Medium against astcenc
5.7.1 `-ch -medium`. Command: `astch hdrq medium 4x4,6x6,8x8 <images>` (tools/astc-harness). Metrics
over RGB on our bit-exact decode of each file: log2 RMSE (`log_rmse`, inputs clamped at 1e-4) and
mPSNR with astcenc's tone-map operator, fstops -10 to +10. Gates on every encode: EC1 (every block
decodes identically in astcenc's six decode cells) and `astcenc -dh` of our file equal to our own
half-float decode texel for texel. Both: 0 bad blocks / 0 differing texels, every row.

Images: astcenc's HDRIHaven set (arboretum, bellparkpier, canarywharf, eveningroad, riverwalk),
Small/hdr-rgb-00 and Data/Tiles/hdr.hdr (8x8 pixels; at 4x4 all three encoders are exact).

| image | fp | 0.11 mPSNR | now mPSNR | astcenc mPSNR | gap now (dB) | 0.11 log2 RMSE | now | astcenc |
|---|---|---|---|---|---|---|---|---|
| arboretum | 4x4 | 44.200 | 44.823 | 45.679 | -0.86 | 0.0722 | 0.0671 | 0.0606 |
| arboretum | 6x6 | 36.746 | 37.054 | 37.965 | -0.91 | 0.1715 | 0.1653 | 0.1486 |
| arboretum | 8x8 | 32.726 | 32.940 | 33.706 | -0.77 | 0.2755 | 0.2685 | 0.2452 |
| bellparkpier | 4x4 | 54.374 | 55.119 | 55.972 | -0.85 | 0.0222 | 0.0204 | 0.0185 |
| bellparkpier | 6x6 | 48.187 | 48.606 | 49.376 | -0.77 | 0.0456 | 0.0434 | 0.0397 |
| bellparkpier | 8x8 | 44.323 | 44.715 | 45.526 | -0.81 | 0.0717 | 0.0684 | 0.0622 |
| canarywharf | 4x4 | 47.067 | 48.561 | 49.544 | -0.98 | 0.0518 | 0.0435 | 0.0388 |
| canarywharf | 6x6 | 42.138 | 42.566 | 43.219 | -0.65 | 0.0916 | 0.0872 | 0.0808 |
| canarywharf | 8x8 | 38.906 | 39.219 | 39.786 | -0.57 | 0.1337 | 0.1289 | 0.1206 |
| eveningroad | 4x4 | 50.562 | 51.096 | 52.254 | -1.16 | 0.0344 | 0.0323 | 0.0283 |
| eveningroad | 6x6 | 45.888 | 46.049 | 46.742 | -0.69 | 0.0588 | 0.0577 | 0.0534 |
| eveningroad | 8x8 | 42.877 | 42.998 | 43.643 | -0.65 | 0.0835 | 0.0823 | 0.0764 |
| riverwalk | 4x4 | 31.931 | 33.312 | 34.807 | -1.50 | 0.3041 | 0.2539 | 0.2099 |
| riverwalk | 6x6 | 27.909 | 28.188 | 28.624 | -0.44 | 0.4857 | 0.4686 | 0.4462 |
| riverwalk | 8x8 | 25.185 | 25.323 | 25.732 | -0.41 | 0.6753 | 0.6640 | 0.6363 |
| hdr-rgb-00 | 4x4 | 28.395 | 31.466 | 34.146 | -2.68 | 0.4483 | 0.2995 | 0.2187 |
| hdr-rgb-00 | 6x6 | 25.059 | 26.164 | 27.518 | -1.35 | 0.6703 | 0.5759 | 0.4958 |
| hdr-rgb-00 | 8x8 | 22.509 | 23.049 | 24.138 | -1.09 | 0.9279 | 0.8590 | 0.7565 |
| Tiles/hdr | 6x6 | 14.315 | 21.810 | 28.178 | -6.37 | 2.7480 | 0.7649 | 0.2277 |
| Tiles/hdr | 8x8 | 10.613 | 17.456 | 21.108 | -3.65 | 4.1191 | 1.3898 | 0.7359 |

What changed: endpoint values damaged by colour quantisation are repaired (coordinate search over
neighbouring levels), endpoint mode 7 (RGB base + scale) is tried beside mode 11, and two-partition
blocks are searched. The gap to astcenc closed by 0.3 to 1.5 dB on the photographic images (3 dB on
hdr-rgb-00 4x4) but **has not closed**: we still trail astcenc by 0.4 to 1.5 dB mPSNR on every
photographic image and footprint, 1.1 to 2.7 dB on hdr-rgb-00, more on the 8x8-pixel tile. HDR
quality is still not claimed as parity. astcenc also uses dual-plane HDR blocks (4 to 12% of
arboretum's), mode 7 in two-partition blocks and three partitions, none of which we search yet.
HDR encode does more work per block now (more candidates): the harness's single-run times went from
about 1 to 2 s to about 4 to 14 s per HDRIHaven image at Medium on a shared machine. That is an
orientation, not a measurement; no HDR speed is claimed.

3D: EC1 is 0 bad blocks for all ten 3D footprints (`encode_volume_rgba8`, 256x256x6 volume)
(`astc_harness ec1x`).
