# Simulator board — Phase 0 (null arm, no GPU backend)

Profile `stream`, renderer `null`. Phase 0 measures the CPU streaming path only: container parse, subresource slicing, upload-plan construction and the staging copy. There is no swapchain, so GPU columns are absent by construction rather than omitted — the D3D11 backend lands in Phase 1.

## Gates

- **Comparability**: PASS — every run pins the same pack, tier, worker count, frame count, pool budget and machine; and within each arm, the same binary, allocator, stack and peer.
- **Work-count parity**: PASS — all 14 runs share `trace_hash = fe8e3f78b734e734`. Every frame requested the same subresources and handed the renderer the same bytes.

## Run

| scenario | tier | workers | textures | pack | peak demand | pool budget | frames/arm | reps | arms | affinity |
|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| `traverse` | high | 4 | 192 | 192.0 MiB | 20.1 MiB | 13.1 MiB | 10500 | 7 | dxtex, rusty | `0x3c` + high priority |

## Metrics, each against its own null band

| metric | `dxtex` | `rusty` | delta (`dxtex` vs `rusty`) | null band | verdict |
|---|---:|---:|---:|---:|---|
| Run CPU time | 0.8594 s | 0.8906 s | -3.51% | ±31.91% | inside the noise |
| Streaming CPU, total | 497.9 ms | 508.0 ms | -1.98% | ±17.64% | inside the noise |
| Container parse, total | 2.121 ms | 1.699 ms | +24.85% | ±17.96% | **outside the band** |
| Staging copy, total | 88.423 ms | 89.647 ms | -1.36% | ±13.60% | inside the noise |
| Frame cost, median | 0.0111 ms | 0.0110 ms | +0.91% | ±8.41% | inside the noise |
| Frame cost, p99 | 0.5801 ms | 0.5917 ms | -1.96% | ±14.13% | inside the noise |
| Frame cost, p99.9 | 0.8442 ms | 0.8919 ms | -5.35% | ±27.73% | inside the noise |
| Frame cost, max | 1.264 ms | 3.329 ms | -62.04% | ±741.81% | inside the noise |
| Hitches (> 1 ms) | 2.000 | 5.000 | -60.00% | ±800.00% | inside the noise |
| Peak working set | 156.3 MiB | 156.3 MiB | +0.02% | ±0.88% | inside the noise |
| Allocations | 45162.0 | 45162.0 | +0.00% | ±0.00% | inside the noise |
| Uploaded (parity check) | 822.2 MiB | 822.2 MiB | +0.00% | ±0.00% | inside the noise |

## Reading this board

Both arms are **the same build** — `a` and `a2` differ only in their label. Every row should therefore read *inside the noise*, and the `null band` column is the real output: it is the smallest difference each metric can resolve on this machine, and no later phase may report anything narrower than it.

`Uploaded` is a parity check rather than a result: in the stream profile both arms hand the renderer byte-identical data, so any spread there means the runs are not comparable. Hitches count frames costing more than 1 ms on the streaming path — Phase 0 has no present, so it cannot yet use the definition studios use (a frame that missed its deadline).

**Attention** — these rows landed outside their own band on a null comparison, which means the band is understated, the machine was not quiet, or the metric is unstable. Do not build a Phase 2 claim on them until a quiet re-run says otherwise:

- Container parse, total (band ±17.96%)

---

Reproduce:

```sh
cd sim
cargo run --release --bin sim -- cook --tier medium --textures 192 --out pack/medium192
cargo run --release --bin sim -- verify --pack pack/medium192
cargo run --release --bin sim -- bench --pack pack/medium192 --scenario traverse --arms a,a2 --reps 7 --out runs/traverse
cargo run --release --bin sim -- board --runs runs/traverse
```
