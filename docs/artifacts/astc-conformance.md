# ASTC decoder conformance

Reference: ARM astcenc **5.7.1** (`0affb51aaf0e0de3a8b038fa32487d59754a1eed`), `none` ISA
build, through `tools/astc-oracle` (its static library behind a batch front end).
Decoder under test: `rusty_dds::astc` scalar reference (`src/astc/reference.rs`).
Machine: WSL2 Ubuntu 26.04 on the dev box, gcc 15.2. Date: 2026-11-21.

"Bit-exact" is defined over six cells: (sRGB LDR, linear LDR) x (8-bit, half-float) plus
(HDR RGB + LDR alpha, HDR) x half-float. Half-float output is compared as raw binary16 bit
patterns. 8-bit output follows `decode_unorm8` (astcenc selects it from a `U8` output image).

## Gates

| ID | Criterion | Result | Command |
|---|---|---|---|
| DC3 | Block-mode table vs the oracle, 2048 modes x 24 footprints | **49,152 / 49,152** equal | `astc_harness modes` |
| DC4 | Partition maps vs the oracle, 3 counts x 1024 seeds x 24 footprints | **73,728 / 73,728** equal | `astc_harness partitions` |
| D2 | Integer sequence decode, 21 ranges x every length 1..=64 | **348,000** sequences equal | `astc_harness ise 500` |
| D3-D6 | Per-field (kind, partitions, seed, CEMs, ranges, plane-2 component, grid, colour values, weights, decoded endpoints and HDR flags, per-texel weights, partition of every texel), `ldr` and `hdr` profiles | 0 differing fields, 24 footprints x 30,000 legal-biased blocks | `astc_harness sweep mix:30000` |
| DC1 | Natural corpus (956 astcenc-encoded files, every footprint, `-cl/-cs/-ch/-cH`, `-fastest/-fast/-medium/-thorough`), every block in all six cells | **0** differing bytes (2,671,729 unique blocks) | `astc_harness dc2 ...` |
| DC2 | 10^7 uniform random + 10^7 mutated (1..=8 bit flips of corpus blocks) + 10^6 legal-biased blocks per footprint, all six cells | **0** differing bytes over 506,671,729 blocks (3.04 x 10^9 block decodes across the six cells) | `astc_harness dc2 10000000 /root/astcws/corpus ...` |
| DC5 | Census: every reachable class cell has >= 32 bit-exact blocks | **2,888 / 2,888** cells (per-footprint structural cells enumerated exactly from the mode table and bit budget, plus 59 endpoint sub-mode cells) | see `astc-census.md` |
| 0.4 | astcenc-avx2 vs astcenc-none (5.7.1), and 5.7.1 vs astcenc `main` @22ba8a0 | 0 differing bytes in all six cells (main reports `ASTCENC_ERR_BAD_BLOCK` for strips with error blocks; bytes identical) | `ASTC_ORACLE=... astc_harness dc2 16384 ...` |
| Third-party | google/astc-codec testdata (30 files, 14 footprints) | 0 differing bytes | `astc_harness dc2 0 <testdata>` |
| DC6 | No panic: `tests/astc` robustness (10^6 arbitrary blocks per footprint in release, every block mode x partition count); libFuzzer `astc_decode` | robustness test passes; libFuzzer 3,380,934 executions in 3,601 s (1 CPU-hour), 0 crashes, 5,039 new units, peak RSS 458 MB | `cargo test --release --lib astc`; `cargo +nightly fuzz run astc_decode -- -max_total_time=3600` |
| Golden (CI) | `tests/fixtures/astc/golden_v1.bin`: astcenc's output hashed per 16 blocks, 6,144 blocks x 24 footprints x 6 cells | passes in `cargo test` | `cargo test --lib astc::tests::golden` |

## Behaviour notes (all matched to astcenc)

- Illegal blocks, and HDR void-extent blocks under an LDR profile: magenta in 8-bit output,
  `0xFFFF` in every half-float channel.
- HDR endpoint modes under an LDR profile: that partition's texels decode as magenta in both
  outputs (`0x3C00, 0, 0x3C00, 0x3C00` as halves); other partitions decode normally.
- sRGB expands all four channels as `(c << 8) | 0x80`; the half-float sRGB cell is
  `unorm16_to_f16((C >> 8) * 257)`, round toward zero.
- HDR void-extent constants pass through, except that a signalling NaN is quieted (bit 9 set,
  payload kept); astcenc's FP16 -> FP32 -> FP16 round trip does this, and the AVX2/F16C build
  agrees with the scalar build.
- HDR modes without coded alpha default to 1.0: HDR `0x780` under `Hdr`, LDR `0xFF` under
  `HdrRgbLdrA`.

## Reproduce

```sh
# WSL / Linux, after building the oracle (tools/astc-oracle/README.md):
cd tools/astc-harness && cargo build --release
./target/release/astc_harness modes
./target/release/astc_harness partitions
./target/release/astc_harness ise 500
./target/release/astc_harness dc2 10000000 /path/to/corpus census.md all
```

The corpus is regenerated with `tools/astc-harness/corpus.sh` (astcenc 5.7.1 over its own
`Test/Images` sets); it is not committed.
