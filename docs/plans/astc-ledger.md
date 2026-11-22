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

## Refuted or deferred ideas

| Idea | Measurement | Verdict |
|---|---|---|
