# Simulator board — Phase 0 (null arm, no GPU backend)

Profile `stream`, renderer `null`. Phase 0 measures the CPU streaming path only: container parse, subresource slicing, upload-plan construction and the staging copy. There is no swapchain, so GPU columns are absent by construction rather than omitted — the D3D11 backend lands in Phase 1.

## Gates

- **Comparability**: PASS — every run pins the same pack, tier, worker count, frame count, pool budget and machine; and within each arm, the same binary, allocator, stack and peer.
- **Work-count parity**: PASS — all 28 runs share `trace_hash = fe8e3f78b734e734`. Every frame requested the same subresources and handed the renderer the same bytes.

## Run

| scenario | tier | workers | textures | pack | peak demand | pool budget | frames/arm | reps | arms | affinity |
|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| `traverse` | high | 4 | 192 | 192.0 MiB | 20.1 MiB | 13.1 MiB | 10500 | 7 | dxtex, dxtex+ra, rusty, rusty+ra | `0x3c` + high priority |

## Metrics, each against its own null band

| metric | `dxtex` | `dxtex+ra` | `rusty` | `rusty+ra` | delta (`dxtex` vs `dxtex+ra`) | null band | verdict |
|---|---:|---:|---:|---:|---:|---:|---|
| Run CPU time | 1.016 s | 0.9062 s | 1.078 s | 0.9062 s | +12.07% | ±146.67% | inside the noise |
| Streaming CPU, total | 636.9 ms | 552.8 ms | 666.9 ms | 514.1 ms | +15.22% | ±284.69% | inside the noise |
| Container parse, total | 3.253 ms | 3.390 ms | 2.513 ms | 2.115 ms | -4.04% | ±143.28% | inside the noise |
| Staging copy, total | 115.3 ms | 115.7 ms | 123.3 ms | 117.2 ms | -0.32% | ±54.59% | inside the noise |
| Frame cost, median | 0.0130 ms | 0.0123 ms | 0.0139 ms | 0.0121 ms | +5.69% | ±18.58% | inside the noise |
| Frame cost, p99 | 0.6410 ms | 0.5249 ms | 0.6985 ms | 0.4727 ms | +22.12% | ±367.77% | inside the noise |
| Frame cost, p99.9 | 0.9059 ms | 0.7021 ms | 0.9585 ms | 0.7533 ms | +29.03% | ±1759.51% | inside the noise |
| Frame cost, max | 2.127 ms | 1.276 ms | 1.524 ms | 1.076 ms | +66.77% | ±12335.66% | inside the noise |
| Hitches (> 1 ms) | 4.000 | 1.000 | 7.000 | 1.000 | +300.00% | ±60700.00% | inside the noise |
| Peak working set | 156.3 MiB | 260.5 MiB | 156.8 MiB | 259.8 MiB | -40.01% | ±13.54% | **outside the band** |
| Allocations | 45162.0 | 45164.0 | 45163.0 | 45163.0 | -0.00% | ±0.01% | inside the noise |
| Uploaded (parity check) | 822.2 MiB | 822.2 MiB | 822.2 MiB | 822.2 MiB | +0.00% | ±0.00% | inside the noise |

## Reading this board

Both arms are **the same build** — `a` and `a2` differ only in their label. Every row should therefore read *inside the noise*, and the `null band` column is the real output: it is the smallest difference each metric can resolve on this machine, and no later phase may report anything narrower than it.

`Uploaded` is a parity check rather than a result: in the stream profile both arms hand the renderer byte-identical data, so any spread there means the runs are not comparable. Hitches count frames costing more than 1 ms on the streaming path — Phase 0 has no present, so it cannot yet use the definition studios use (a frame that missed its deadline).

**Attention** — these rows landed outside their own band on a null comparison, which means the band is understated, the machine was not quiet, or the metric is unstable. Do not build a Phase 2 claim on them until a quiet re-run says otherwise:

- Peak working set (band ±13.54%)

---

Reproduce:

```sh
cd sim
cargo run --release --bin sim -- cook --tier medium --textures 192 --out pack/medium192
cargo run --release --bin sim -- verify --pack pack/medium192
cargo run --release --bin sim -- bench --pack pack/medium192 --scenario traverse --arms a,a2 --reps 7 --out runs/traverse
cargo run --release --bin sim -- board --runs runs/traverse
```
