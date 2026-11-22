# ASTC decode speed vs baselines

Decoder under test: `rusty_dds::astc::decode_rgba8_into` (the fast scalar kernel with the SSE2
8-bit output twin, features `decode,astc,simd`), branch `feat/astc` at `f0ed691`.
Baselines: `astc-decode` 0.3.1 and `texture2ddecoder` 0.1.2 (the two pure-Rust ASTC decoders
on crates.io), and ARM astcenc 5.7.1 `astcenc-avx2 -dl -j 1`.
Machine: Intel Core i7-14650HX, WSL2 (kernel 6.6.87.2), single thread pinned with
`taskset -c 2`. Date: 2026-11-21.

## Method

- Inputs: four 2048x2048 natural-content textures. `m1` to `m3` are mosaics of twelve Kodak
  images each (`astc_harness mosaic`); `m4` is the scout texture. Each was encoded with
  `astcenc-avx2 -cl ... -medium` at 4x4, 6x6, 8x8 and 12x12.
- Rust arms: `bench/astc` (`astc_bench --rounds 11`). It is one binary under rusty_alloc, so
  every arm shares the compiler, flags and allocator. Output is 8-bit RGBA into a buffer
  allocated once, with no I/O in the timed region. The arms are interleaved in-process with
  the order reversed every round (ABBA), and each sample is K decodes with K set so that a
  sample is at least 50 ms. The table reports the median over 11 rounds. The clock is the
  thread CPU clock (`CLOCK_THREAD_CPUTIME_ID`), because the host was shared with other jobs.
- Null arm: our decoder a second time under another name. Its spread from the first arm is
  the noise floor, at most 6% here.
- Correctness: each baseline's output is compared with ours, which is bit-exact with astcenc
  `decode_unorm8` (see `astc-conformance.md`). Neither baseline is bit-exact with astcenc; the
  differing-pixel counts are in the table.
- astcenc: the median over 11 runs of `2048 * 2048 / (child CPU time x Decoding time / Total time)`.
  This is its own decode share applied to its CPU time, which leaves out PNG I/O
  (`.agent-scratch/dbench.sh` on the dev box).

## Results (MT/s, higher is better)

| Input | Footprint | rusty_dds | null arm | astc-decode | texture2ddecoder | astcenc-avx2 | vs faster Rust crate | vs astcenc-avx2 | baseline pixels differing (astc-decode / t2d) |
|---|---|---|---|---|---|---|---|---|---|
| m1 | 4x4 | 101.0 | 100.3 | 35.2 | 36.9 | 56.8 | 2.74x | 1.78x | 1,521,216 / 1,519,500 |
| m2 | 4x4 | 89.4 | 88.4 | 32.5 | 32.0 | 61.4 | 2.75x | 1.46x | 1,425,050 / 1,423,025 |
| m3 | 4x4 | 98.2 | 98.9 | 32.7 | 34.7 | 61.2 | 2.83x | 1.60x | 1,522,826 / 1,520,873 |
| m4 | 4x4 | 139.2 | 139.1 | 61.7 | 52.8 | 80.4 | 2.26x | 1.73x | 664,556 / 663,933 |
| m1 | 6x6 | 107.9 | 112.9 | 45.8 | 48.0 | 61.0 | 2.25x | 1.77x | 1,720,954 / 1,717,903 |
| m2 | 6x6 | 123.9 | 125.2 | 49.4 | 53.2 | 74.3 | 2.33x | 1.67x | 1,634,984 / 1,632,337 |
| m3 | 6x6 | 113.3 | 110.0 | 46.0 | 49.9 | 74.1 | 2.27x | 1.53x | 1,717,721 / 1,714,754 |
| m4 | 6x6 | 135.5 | 134.6 | 62.9 | 61.5 | 81.1 | 2.15x | 1.67x | 1,477,028 / 1,476,945 |
| m1 | 8x8 | 125.1 | 118.0 | 52.9 | 54.6 | 86.2 | 2.29x | 1.45x | 1,671,825 / 1,668,545 |
| m2 | 8x8 | 142.2 | 142.6 | 62.6 | 66.5 | 87.1 | 2.14x | 1.63x | 1,627,921 / 1,624,442 |
| m3 | 8x8 | 115.4 | 116.8 | 51.2 | 55.5 | 75.3 | 2.08x | 1.53x | 1,716,945 / 1,713,605 |
| m4 | 8x8 | 150.5 | 151.2 | 72.3 | 71.4 | 85.9 | 2.08x | 1.75x | 1,331,219 / 1,331,087 |
| m1 | 12x12 | 146.1 | 146.6 | 70.2 | 69.1 | 78.2 | 2.08x | 1.87x | 1,645,094 / 1,640,591 |
| m2 | 12x12 | 149.5 | 150.1 | 71.6 | 71.2 | 101.5 | 2.09x | 1.47x | 1,592,858 / 1,588,761 |
| m3 | 12x12 | 165.9 | 158.8 | 81.3 | 74.9 | 78.9 | 2.04x | 2.10x | 1,695,639 / 1,691,702 |
| m4 | 12x12 | 181.9 | 187.0 | 95.2 | 87.5 | 95.9 | 1.91x | 1.90x | 1,414,810 / 1,414,744 |

Each texture has 4,194,304 pixels.

## Gates (plan section 2.4)

| Gate | Criterion | Result |
|---|---|---|
| DS1 | Never slower than `astc-decode` or `texture2ddecoder` on any measured footprint | **pass**: 1.91x to 2.83x |
| DS2 | At least 1.5x the faster Rust crate, and at least 1.0x `astcenc-avx2 -j 1` | **pass**: at least 1.91x and at least 1.45x |
| DS3 (performance phase) | At least 2.0x `astcenc-avx2 -j 1` | **not met**: 1.45x to 2.10x (one cell of 16 reaches 2.0x) |
