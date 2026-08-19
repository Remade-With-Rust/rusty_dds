# Simulator board — Phase 0 (null arm, no GPU backend)

Profile `stream`, renderer `null`. Phase 0 measures the CPU streaming path only: container parse, subresource slicing, upload-plan construction and the staging copy. There is no swapchain, so GPU columns are absent by construction rather than omitted — the D3D11 backend lands in Phase 1.

## Gates

- **Comparability**: PASS — every run pins the same pack, tier, worker count, frame count, pool budget, machine and binary.
- **Work-count parity**: PASS — all 14 runs share `trace_hash = 169a7205afde5605`. Every frame requested the same subresources and handed the renderer the same bytes.

## Run

| scenario | tier | workers | textures | pack | peak demand | pool budget | frames/arm | reps | arms | affinity |
|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| `traverse` | high | 4 | 192 | 192.0 MiB | 20.1 MiB | 13.1 MiB | 10500 | 7 | rusty, rusty+ra | `0x3c` + high priority |

## Metrics, each against its own null band

| metric | `rusty` | `rusty+ra` | delta | null band | verdict |
|---|---:|---:|---:|---:|---|
| Run CPU time | 2.234 s | 1.703 s | +31.19% | ±15.27% | **outside the band** |
| Streaming CPU, total | 1863.2 ms | 1385.8 ms | +34.45% | ±20.10% | **outside the band** |
| Container parse, total | 418.6 ms | 149.1 ms | +180.76% | ±12.39% | **outside the band** |
| Staging copy, total | 894.2 ms | 892.4 ms | +0.21% | ±8.57% | inside the noise |
| Frame cost, median | 0.0118 ms | 0.0100 ms | +18.00% | ±10.53% | **outside the band** |
| Frame cost, p99 | 1.254 ms | 1.161 ms | +7.95% | ±9.29% | inside the noise |
| Frame cost, p99.9 | 1.663 ms | 1.449 ms | +14.76% | ±341.52% | inside the noise |
| Frame cost, max | 2.130 ms | 2.105 ms | +1.18% | ±352.18% | inside the noise |
| Hitches (> 1 ms) | 492.0 | 312.0 | +57.69% | ±27.24% | **outside the band** |
| Peak working set | 135.5 MiB | 258.6 MiB | -47.60% | ±1.31% | **outside the band** |
| Allocations | 263112.0 | 263112.0 | +0.00% | ±0.00% | inside the noise |
| Uploaded (parity check) | 822.2 MiB | 822.2 MiB | +0.00% | ±0.00% | inside the noise |

## Reading this board

Both arms are **the same build** — `a` and `a2` differ only in their label. Every row should therefore read *inside the noise*, and the `null band` column is the real output: it is the smallest difference each metric can resolve on this machine, and no later phase may report anything narrower than it.

`Uploaded` is a parity check rather than a result: in the stream profile both arms hand the renderer byte-identical data, so any spread there means the runs are not comparable. Hitches count frames costing more than 1 ms on the streaming path — Phase 0 has no present, so it cannot yet use the definition studios use (a frame that missed its deadline).

**Attention** — these rows landed outside their own band on a null comparison, which means the band is understated, the machine was not quiet, or the metric is unstable. Do not build a Phase 2 claim on them until a quiet re-run says otherwise:

- Run CPU time (band ±15.27%)
- Streaming CPU, total (band ±20.10%)
- Container parse, total (band ±12.39%)
- Frame cost, median (band ±10.53%)
- Hitches (> 1 ms) (band ±27.24%)
- Peak working set (band ±1.31%)

---

Reproduce:

```sh
cd sim
cargo run --release --bin sim -- cook --tier medium --textures 192 --out pack/medium192
cargo run --release --bin sim -- verify --pack pack/medium192
cargo run --release --bin sim -- bench --pack pack/medium192 --scenario traverse --arms a,a2 --reps 7 --out runs/traverse
cargo run --release --bin sim -- board --runs runs/traverse
```
