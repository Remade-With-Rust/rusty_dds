# Migrating from crates.io `ddsfile`

`rusty_dds` is a hard rename of the PistonDevelopers/`ddsfile` container lineage
plus decode / encode / upload. There is **no** `ddsfile` compatibility facade
crate — swap the dependency and import path.

## Container API

| `ddsfile` | `rusty_dds` |
|-----------|-------------|
| `ddsfile::Dds` | `rusty_dds::Dds` |
| `Dds::read` / `write` | same |
| `Header` / `Header10` / formats | same module layout under `rusty_dds` |

Parse A/B: `cargo bench --bench parse_ab` compares this crate to crates.io
`ddsfile` 0.5.2 on committed fixtures.

## What you gain

- `SubresourceId` / `surface()` fail-closed ranges
- `decode_rgba8` / `encode_from_rgba8` (features `decode` / `encode`)
- `upload_plan_compressed` / `upload_plan_decoded_rgba8`

## Cargo.toml

```toml
# before
ddsfile = "0.5"

# after
rusty_dds = "0.1"   # or { path = "…" } until published
# optional: rusty_dds = { version = "0.1", default-features = false, features = ["decode"] }
```

## Not a drop-in for `image_dds`

`image_dds` is a higher-level image↔DDS converter (and may pull `intel_tex_2` for
encode). `rusty_dds` is the container + pure-Rust LDR matrix + GPU pitch plans.
Use both if you need their encode quality presets; do not expect identical APIs.
