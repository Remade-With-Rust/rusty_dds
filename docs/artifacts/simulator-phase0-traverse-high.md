# Simulator board — Phase 0 (null arm, no GPU backend)

Profile `stream`, renderer `null`. Phase 0 measures the CPU streaming path only: container parse, subresource slicing, upload-plan construction and the staging copy. There is no swapchain, so GPU columns are absent by construction rather than omitted — the D3D11 backend lands in Phase 1.

## Gates

- **Comparability**: PASS — every run pins the same pack, tier, worker count, frame count, pool budget, machine and binary.
- **Work-count parity**: PASS — all 14 runs share `trace_hash = 169a7205afde5605`. Every frame requested the same subresources and handed the renderer the same bytes.

## Run

| scenario | tier | workers | textures | pack | peak demand | pool budget | frames/arm | reps | arms | affinity |
|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| `traverse` | high | 4 | 192 | 192.0 MiB | 20.1 MiB | 13.1 MiB | 10500 | 7 | a, a2 | `0x3c` + high priority |

## Metrics, each against its own null band

| metric | `a` | `a2` | delta | null band | verdict |
|---|---:|---:|---:|---:|---|
| Run CPU time | 2.203 s | 2.203 s | +0.00% | ±15.91% | inside the noise |
| Streaming CPU, total | 1790.8 ms | 1808.6 ms | -0.98% | ±13.04% | inside the noise |
| Container parse, total | 410.2 ms | 410.8 ms | -0.16% | ±13.54% | inside the noise |
| Staging copy, total | 855.6 ms | 866.5 ms | -1.26% | ±9.07% | inside the noise |
| Frame cost, median | 0.0111 ms | 0.0113 ms | -1.77% | ±14.02% | inside the noise |
| Frame cost, p99 | 1.167 ms | 1.206 ms | -3.21% | ±20.74% | inside the noise |
| Frame cost, p99.9 | 1.552 ms | 1.568 ms | -1.05% | ±22.95% | inside the noise |
| Frame cost, max | 2.050 ms | 2.060 ms | -0.49% | ±36.29% | inside the noise |
| Hitches (> 1 ms) | 426.0 | 418.0 | +1.91% | ±125.11% | inside the noise |
| Peak working set | 132.6 MiB | 132.6 MiB | -0.03% | ±0.46% | inside the noise |
| Allocations | 263112.0 | 263113.0 | -0.00% | ±0.00% | inside the noise |
| Uploaded (parity check) | 822.2 MiB | 822.2 MiB | +0.00% | ±0.00% | inside the noise |

## Reading this board

Both arms are **the same build** — `a` and `a2` differ only in their label. Every row should therefore read *inside the noise*, and the `null band` column is the real output: it is the smallest difference each metric can resolve on this machine, and no later phase may report anything narrower than it.

`Uploaded` is a parity check rather than a result: in the stream profile both arms hand the renderer byte-identical data, so any spread there means the runs are not comparable. Hitches count frames costing more than 1 ms on the streaming path — Phase 0 has no present, so it cannot yet use the definition studios use (a frame that missed its deadline).

---

Reproduce:

```sh
cd sim
cargo run --release --bin sim -- cook --tier medium --textures 192 --out pack/medium192
cargo run --release --bin sim -- verify --pack pack/medium192
cargo run --release --bin sim -- bench --pack pack/medium192 --scenario traverse --arms a,a2 --reps 7 --out runs/traverse
cargo run --release --bin sim -- board --runs runs/traverse
```
