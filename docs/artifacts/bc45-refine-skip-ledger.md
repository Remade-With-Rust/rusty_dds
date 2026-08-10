# BC4/5 neighborhood search-skip ledger

## Harvest
- Tool: `cargo run --release --example harvest_bc45_refine_skip`
- Sweep: `python target/sweep_bc45_refine_skip.py`
- CSV: `docs/artifacts/bc45-refine-skip-harvest.csv`
- Scope: ambientCG proxy normals/masks, blocks that reach ±N after LS
- Rows: 511 781 · refine wins: 70.5% · total SSE gain: 3 461 991
- Note: harvest rows are almost entirely busy (`n_unique>5`); simple blocks early-exit before ±N.

## Feature
`score = null_err * 16 / span` (span-normalized LS residual)

| T (skip if score≤T) | Skip rate | Gain kept |
|--|--|--|
| 6 | 6.4% | 99.55% |
| 7 | 14.3% | 98.57% |
| 8 | 26.7% | 96.13% |
| 9 | 35.5% | 93.76% |
| **10** | **43.7%** | **90.95%** |

## Shipped
- Skip neighborhood when `null_err ≤ 4` or `score ≤ 10`
- Busy blocks keep axis-aligned ±N when not skipped
- Unique-pairs: skip if dual err ≤4; full exhaust only for ≤4 uniques (5-uniques only if err >16)
- Harvest env `RUSTY_DDS_BC45_REFINE_HARVEST` is observe-only (never skips)
- `EncodeQuality::Fast` skips unique-pairs + neighborhood entirely (dual + LS only)

## Encode parallelism
- Strip-parallel `encode_image` when block count ≥ 4096 (same threshold as BC7 decode)
- DirectXTex corpus peer uses `TEX_COMPRESS_DEFAULT` (no `TEX_COMPRESS_PARALLEL`) — wall-clock board is MT rusty vs ST DX
