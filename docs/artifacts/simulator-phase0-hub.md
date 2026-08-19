# Simulator board — Phase 0 (null arm, no GPU backend)

Profile `stream`, renderer `null`. Phase 0 measures the CPU streaming path only: container parse, subresource slicing, upload-plan construction and the staging copy. There is no swapchain, so GPU columns are absent by construction rather than omitted — the D3D11 backend lands in Phase 1.

## Gates

- **Comparability**: PASS — every run pins the same pack, tier, worker count, frame count, pool budget, machine and binary.
- **Work-count parity**: PASS — all 14 runs share `trace_hash = 96a5191441ba5dfa`. Every frame requested the same subresources and handed the renderer the same bytes.

## Run

| scenario | tier | workers | textures | pack | peak demand | pool budget | frames/arm | reps | arms | affinity |
|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| `hub` | medium | 4 | 192 | 48.0 MiB | 1.2 MiB | 0.6 MiB | 17700 | 7 | a, a2 | `0x3c` + high priority |

## Metrics, each against its own null band

| metric | `a` | `a2` | delta | null band | verdict |
|---|---:|---:|---:|---:|---|
| Run CPU time | 0.6875 s | 0.6719 s | +2.33% | ±24.39% | inside the noise |
| Streaming CPU, total | 308.9 ms | 300.9 ms | +2.66% | ±10.10% | inside the noise |
| Container parse, total | 82.235 ms | 80.120 ms | +2.64% | ±7.56% | inside the noise |
| Staging copy, total | 24.627 ms | 23.579 ms | +4.44% | ±7.08% | inside the noise |
| Frame cost, median | 0.0096 ms | 0.0092 ms | +4.35% | ±6.67% | inside the noise |
| Frame cost, p99 | 0.2289 ms | 0.2216 ms | +3.29% | ±11.79% | inside the noise |
| Frame cost, p99.9 | 0.3270 ms | 0.3166 ms | +3.28% | ±21.04% | inside the noise |
| Frame cost, max | 0.6596 ms | 0.5692 ms | +15.88% | ±93.18% | inside the noise |
| Hitches (> 1 ms) | 0.0000 | 0.0000 | NaN% | ±0.00% | identical (both zero) |
| Peak working set | 38.422 MiB | 38.391 MiB | +0.08% | ±1.91% | inside the noise |
| Allocations | 295728.0 | 295728.0 | +0.00% | ±0.00% | inside the noise |
| Uploaded (parity check) | 9.910 MiB | 9.910 MiB | +0.00% | ±0.00% | inside the noise |

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
