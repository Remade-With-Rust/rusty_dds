# ASTC build ledger

One line per brick: state, gate result, commit. Refuted ideas go in the second table with their measurement. The plan is `docs/plans/astc.md`.

| Brick | State | Gate result | Commit |
|---|---|---|---|
| plan | done | decisions recorded (plan section 13) | 66fc0ca |
| release-plz | done | `features_always_increment_minor = true` | 229c4d9 |
| 0.1 oracle workspace | done | astcenc 5.7.1 (`0affb51`) built in WSL; `-version` prints 5.7.1 | (rig) |
| 0.2 oracle tool | done | `tools/astc-oracle`: decode/info/ise/modes/partitions; decodes equal astcenc CLI output on corpus files | 42d15fb |
| 0.3 corpus + census | done | 956 natural files (corpus.sh); census script in the harness | 42d15fb, fd15e76 |
| 0.4 reference vs itself | done | avx2 == none in all six cells; 5.7.1 == main@22ba8a0 bytes (main returns BAD_BLOCK for error strips) | 85f6f06 |
| 0.5 bench harness | done | bench/astc under rusty_alloc, ABBA + null arm; baselines reproduce the scout pattern (27-37% pixels off by 1) | fd15e76 |
| D1 block modes | done | DC3 49,152/49,152 | 42d15fb |
| D2 ISE | done | 21 ranges x lengths 1..=64, 348,000 sequences equal | 42d15fb |
| D3 parse | done | per-field equal on 24 x 30,000 legal-biased blocks + DC2 | 42d15fb |
| D4 unquantisation | done | colour values and weights equal (per-field sweep) | 42d15fb |
| D5a/b endpoints | done | decoded endpoints + HDR flags equal in `ldr` and `hdr` profiles | 42d15fb |
| D6 partitions + infill | done | DC4 73,728/73,728; per-texel weights equal | 42d15fb |
| D7 texels + error colours | done | DC1/DC2 0 diffs, six cells; sNaN quieting of HDR void constants matched | 42d15fb |
| D8 surface driver | done | 127x128, 1x1, 3D cropping tests; rows compose the surface | 92689cb |
| D9 3D | done | DC1-DC4 on the 10 3D footprints | 42d15fb |
| D10 robustness | done | `arbitrary_blocks_never_panic` (10^6/footprint in release); fuzz target `astc_decode` | 92689cb, fd15e76 |
| D11 goldens + census | done | golden_v1.bin (432 KiB, oracle output hashes); census 2,888/2,888 cells >= 32 | 92689cb |
| C1 .astc | done | astcenc negatives rejected; read/write round trip; astcenc reads ours | 099d88e |
| C2 KTX 1.0 | done | astcenc .ktx read; ours read by astcenc; KTX-Software array_7 | 099d88e |
| C3 DDS | done | layout snapshot (4,063 lines) unchanged across the refactor; image-dds 14/14; dds 0.2 reads ours | 433b27a, c0c372d, c54dba9 |
| C4a KTX 2.0 read | done | KTX-Software files; zstd via `ktx2-zstd` (rusty_zstd 0.5) | 099d88e |
| E1 pack | done | parse(pack(parse(b))) == parse(b) on > 50,000 legal blocks; ISE encode round trip all ranges/lengths | ef446cf |
| E2/E3 encoder | first cut | EC1 0 bad blocks; EC2 standing test; harvest below | ef446cf |

## E3 harvest (2026-11-21)

Kodak 01, 03, 08, 15; 4x4 / 6x6 / 8x8 / 12x12; mean PSNR gap ours - astcenc 5.7.1 at the same preset name, PSNR on our decode (bit-exact with astcenc's).

| Comparison | 4x4 | 6x6 | 8x8 | 12x12 |
|---|---|---|---|---|
| Fastest vs astcenc `-fastest -partitioncountlimit 1 -2planelimitcorrelation 0` (like for like) | +0.07 | -0.03 | +0.11 | +0.26 |
| Fastest vs astcenc `-fastest` | -1.71 | -0.44 | -0.22 | +0.23 |
| Fast vs `-fast` | -0.37 | +0.21 | +0.34 | +0.60 |
| Medium vs `-medium` | -0.50 | -0.20 | -0.28 | -0.12 |

Decision: the section 2.3 quality targets stand; Medium is within reach, so the fallback cut (decoder-only 0.11.0) is not taken. The open problem is speed (Fast 6x6 0.10 MT/s single-thread vs astcenc-none ~3.6).

## Refuted or deferred ideas

| Idea | Measurement | Verdict |
|---|---|---|
| Rank every mode by its own quantised weights | 0.041 MT/s Fast 6x6; per-grid residual + analytic noise term gives 0.111 MT/s at -0.04 dB | replaced |
| Endpoint class chosen by estimate alone | base+scale picked on lines far from the origin: kodim03 4x4 Fastest 36.3 dB vs 42.4 after adding the class model error and evaluating each kept mode in every class | fixed |
| Precomputed per-mode colour range table | byte-identical, no measurable speed change (mode ranking no longer the bottleneck) | kept (simpler), not a win |
