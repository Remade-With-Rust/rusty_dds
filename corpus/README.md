# Proxy cook corpus (CC0)

Real PBR maps for encode speed/quality vs DirectXTex — **not** Star Citizen / Cry assets.

| Source | License | Notes |
|--------|---------|-------|
| [ambientCG](https://ambientcg.com/) | [CC0](https://creativecommons.org/publicdomain/zero/1.0/) | Materials downloaded as `1K-PNG` zips |

## Layout

```text
corpus/
  README.md
  manifest.json       # cook roles + paths (committed)
  fetch_ambientcg.py  # download + unpack
  raw/                # PNGs (gitignored; fetch locally)
```

## Fetch

```bash
python corpus/fetch_ambientcg.py
```

Requires network. Re-running skips files that already exist.

## Roles (Cry-shaped mix)

| Role | Map | Target BCn |
|------|-----|------------|
| albedo | `*Color.png` | BC1, BC7 |
| albedo_alpha | `*Color.png` (opaque→synthetic A later) | BC3, BC7 |
| normal | `*NormalGL.png` | BC5 |
| mask | `*Roughness.png` | BC4 |

## Contents (after fetch)

4 ambientCG materials × Color / NormalGL / Roughness = **12 PNGs** (~1024²):

| Asset | Category | Roles |
|-------|----------|-------|
| Bricks097 | brick | albedo, normal, mask |
| Metal063 | metal | albedo, normal, mask |
| Rock064 | rock | albedo, normal, mask |
| Wood095 | wood | albedo, normal, mask |

`raw/` is gitignored (~20MB PNGs + zips). Commit `manifest.json` + `fetch_ambientcg.py` only.

## Harvest vs DirectXTex

```bash
python corpus/fetch_ambientcg.py
cargo run --release --example harvest_corpus_vs_dxtex
```

Writes `docs/artifacts/corpus-vs-directxtex.{json,md}` (encode µs + round-trip PSNR).

## Real .tif / CryTIF corpora (encoder campaign, gitignored)

| Dir | Source | License / provenance | Contents |
|-----|--------|---------------------|----------|
| `raw_crytif/` | [CRYTEK/GameSDK](https://github.com/CRYTEK/GameSDK) (`release` branch, `Libs/UI/...`) | Crytek GameSDK sample-project assets, downloaded for local benchmarking only (not redistributed) | 16 genuine CryTIF-pipeline `.tif` (RGBA UI textures, alpha-heavy; 512²–2048×1024) |
| `raw_tif/` | [USC-SIPI image database](https://sipi.usc.edu/database/) | Research-use image database, local benchmarking only | 10 real `.tiff` (big-endian): color (mandrill/peppers/airplane/aerials) + grayscale Brodatz textures |

Fetch (re-run skips existing): see git history of this file / `bench_encode_corpus.rs` header.
Roles: CryTIF RGBA → BC1/BC3/BC7; SIPI color → BC1/BC3/BC7; SIPI gray → BC4U/BC4S/BC1.

## Encoder A/B gate

```bash
cargo run --release --example bench_encode_corpus   # table + target/encode_corpus_bench.json
```

Per-case round-trip PSNR + payload FNV (byte-identity gate for speed bricks) in one
deterministic pass; encode wall best-of-N in a separate pass (`RUSTY_DDS_ITERS`,
`RUSTY_DDS_FILTER` to subset; pin the process externally for A/B verdicts).
