# DirectXTex benches (official Microsoft DDS peer)

Peer: [microsoft/DirectXTex](https://github.com/microsoft/DirectXTex)

| Binary | Protocol |
|--------|----------|
| `dxtex_decode_bench` | `LoadFromDDSMemory` + `Decompress`/`Convert` → RGBA8 |
| `dxtex_encode_bench` | RGBA8 → `Compress` / `Convert` (`BC7` = `TEX_COMPRESS_BC7_QUICK`) |
| `dxtex_roundtrip` | One-shot Compress→Decompress for the headful TIFF demo |

## One-time setup

```bat
git clone --depth 1 https://github.com/microsoft/DirectXTex.git ..\..\third_party\DirectXTex
```

From an **x64 Native Tools** / `vcvars64` shell:

```bat
cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=Release
cmake --build build
```

## Run side-by-side

From repo root:

```bat
cargo run --release --example bench_baselines
```

Writes `docs/artifacts/decode-vs-baselines.*` and `encode-vs-baselines.*`.

Headful TIFF demo (uses `dxtex_roundtrip` when present):

```bat
cargo run --release --example demo_tiff_side_by_side
```
