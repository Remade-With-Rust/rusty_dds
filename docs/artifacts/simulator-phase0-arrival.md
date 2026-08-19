# Simulator board — Phase 0 (null arm, no GPU backend)

Profile `stream`, renderer `null`. Phase 0 measures the CPU streaming path only: container parse, subresource slicing, upload-plan construction and the staging copy. There is no swapchain, so GPU columns are absent by construction rather than omitted — the D3D11 backend lands in Phase 1.

## Gates

- **Comparability**: PASS — every run pins the same pack, tier, worker count, frame count, pool budget, machine and binary.
- **Work-count parity**: PASS — all 14 runs share `trace_hash = 18283b61e2ff0158`. Every frame requested the same subresources and handed the renderer the same bytes.

## Run

| scenario | tier | workers | textures | pack | peak demand | pool budget | frames/arm | reps | arms | affinity |
|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| `arrival` | medium | 4 | 192 | 48.0 MiB | 1.4 MiB | 0.7 MiB | 11700 | 7 | a, a2 | `0x3c` + high priority |

## Metrics, each against its own null band

| metric | `a` | `a2` | delta | null band | verdict |
|---|---:|---:|---:|---:|---|
| Run CPU time | 0.2812 s | 0.2812 s | +0.00% | ±64.29% | inside the noise |
| Streaming CPU, total | 141.4 ms | 143.6 ms | -1.53% | ±26.30% | inside the noise |
| Container parse, total | 44.125 ms | 43.951 ms | +0.39% | ±17.52% | inside the noise |
| Staging copy, total | 20.133 ms | 20.183 ms | -0.25% | ±15.75% | inside the noise |
| Frame cost, median | 0.0057 ms | 0.0057 ms | +0.00% | ±23.64% | inside the noise |
| Frame cost, p99 | 0.1661 ms | 0.1661 ms | +0.00% | ±38.34% | inside the noise |
| Frame cost, p99.9 | 0.4181 ms | 0.4298 ms | -2.72% | ±44.53% | inside the noise |
| Frame cost, max | 5.867 ms | 5.987 ms | -2.01% | ±43.83% | inside the noise |
| Hitches (> 1 ms) | 8.000 | 9.000 | -11.11% | ±25.00% | inside the noise |
| Peak working set | 46.664 MiB | 46.648 MiB | +0.03% | ±2.18% | inside the noise |
| Allocations | 86848.0 | 86848.0 | +0.00% | ±0.00% | inside the noise |
| Uploaded (parity check) | 14.588 MiB | 14.588 MiB | +0.00% | ±0.00% | inside the noise |

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
