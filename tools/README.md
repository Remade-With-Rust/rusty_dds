# tools/ (not published)

Competitive **Microsoft DirectXTex** decode/encode harness sources used for local
bake-offs live here when present (`dxtex_decode_bench/`) but are **not** part of
the public rusty_dds tree — this crate stays pure Rust on GitHub / crates.io.

Published boards in [`docs/artifacts/`](../docs/artifacts/) were measured against
that optional peer. Reproducing them requires a private checkout of the harness
plus [microsoft/DirectXTex](https://github.com/microsoft/DirectXTex) under
`third_party/DirectXTex/` (also gitignored).
