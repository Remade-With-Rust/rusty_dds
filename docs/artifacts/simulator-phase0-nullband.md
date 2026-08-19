# Simulator board — Phase 0 (null arm, no GPU backend)

Profile `stream`, renderer `null`. Phase 0 measures the CPU streaming path only: container parse, subresource slicing, upload-plan construction and the staging copy. There is no swapchain, so GPU columns are absent by construction rather than omitted — the D3D11 backend lands in Phase 1.

## Gates

- **Comparability**: PASS — every run pins the same pack, tier, worker count, frame count, pool budget, machine and binary.
- **Work-count parity**: PASS — all 14 runs share `trace_hash = 621fda12d5fc784d`. Every frame requested the same subresources and handed the renderer the same bytes.

## Run

| scenario | tier | workers | textures | pack | peak demand | pool budget | frames/arm | reps | arms | affinity |
|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| `traverse` | medium | 4 | 192 | 48.0 MiB | 1.4 MiB | 0.7 MiB | 10500 | 7 | a, a2 | `0x3c` + high priority |

## Metrics, each against its own null band

| metric | `a` | `a2` | delta | null band | verdict |
|---|---:|---:|---:|---:|---|
| Run CPU time | 0.5469 s | 0.5156 s | +6.06% | ±38.71% | inside the noise |
| Streaming CPU, total | 276.7 ms | 287.3 ms | -3.69% | ±18.26% | inside the noise |
| Container parse, total | 71.179 ms | 72.857 ms | -2.30% | ±13.54% | inside the noise |
| Staging copy, total | 63.139 ms | 65.660 ms | -3.84% | ±8.71% | inside the noise |
| Frame cost, median | 0.0081 ms | 0.0083 ms | -2.41% | ±11.39% | inside the noise |
| Frame cost, p99 | 0.2786 ms | 0.2809 ms | -0.82% | ±19.54% | inside the noise |
| Frame cost, p99.9 | 0.4409 ms | 0.4197 ms | +5.05% | ±34.33% | inside the noise |
| Frame cost, max | 0.8087 ms | 0.9211 ms | -12.20% | ±94.20% | inside the noise |
| Hitches (> 1 ms) | 0.0000 | 0.0000 | NaN% | ±0.00% | identical (both zero) |
| Peak working set | 37.625 MiB | 37.785 MiB | -0.42% | ±1.51% | inside the noise |
| Allocations | 218825.0 | 218825.0 | +0.00% | ±0.00% | inside the noise |
| Uploaded (parity check) | 52.027 MiB | 52.027 MiB | +0.00% | ±0.00% | inside the noise |

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
