# Fuzzing `rusty_dds`

Two halves, on purpose.

| | `tests/parser_robustness.rs` | `fuzz/` (this directory) |
|---|---|---|
| Toolchain | stable | nightly |
| C/C++ in the graph | none | `libfuzzer-sys` links LLVM's libFuzzer |
| Runs in `cargo test` | yes, every time | never |
| Input generation | deterministic mutation + arbitrary bytes | coverage-guided |
| Purpose | regression floor, reproducible from a seed | open-ended search |

Both halves drive the same `tests/common/driver.rs`, so a public entry point
cannot be covered by one and missed by the other.

This crate is a **standalone workspace** and is listed in the parent's
`exclude`, so `cargo build`, `cargo test` and `cargo package` at the repo root
never resolve it. That is what keeps the crate's "no C toolchain required"
property true while still offering real coverage-guided fuzzing to anyone who
wants it.

## Running

```sh
cargo install cargo-fuzz
cargo +nightly fuzz run parse
cargo +nightly fuzz run read_limited
cargo +nightly fuzz run encode_roundtrip

# Time-boxed, e.g. for a nightly CI job
cargo +nightly fuzz run parse -- -max_total_time=600

# Seed the corpus from the real fixtures - far better than starting from noise
cargo +nightly fuzz run parse ../tests/fixtures
```

## Targets

- **`parse`** — the untrusted-input surface: `Dds::read`, then every metadata,
  subresource, upload-plan and decode call reachable from the parsed value.
  Contract: `Ok` or `Err`, never a panic.
- **`read_limited`** — that the byte budget is a real ceiling, not advisory, and
  that it can only ever reject on *size*, never on structure.
- **`encode_roundtrip`** — that the encoder never emits a payload its own
  decoder rejects, at any layout, quality or `Rdo` strength.

## When a target finds something

1. `cargo +nightly fuzz fmt <target> <artifact>` to see the input.
2. Copy the artifact from `fuzz/artifacts/<target>/` into
   `tests/fixtures/regressions/`.
3. Fix the bug.
4. Commit the input **and** the fix together — `regression_corpus_is_clean`
   replays it on every `cargo test` from then on, so it cannot come back.

## Found so far

The first run of the stable harness found four unchecked-arithmetic defects on
the untrusted path, all now fixed and covered:

- `get_texture_size` — `pitch * row_height * depth` overflowed on a hostile
  `width`/`height`/`depth`.
- `DxgiFormat::get_pitch` / `D3DFormat::get_pitch` — the same, one layer down.
- `get_array_stride` — accumulated a wrapped stride, **and** looped
  `mip_map_count` times, so a header claiming `0xFFFF_FFFF` mips spun for
  billions of iterations on every metadata query. Now closed-form once the mip
  size bottoms out.
- `get_min_mipmap_size_in_bytes` — `bpp + 7` overflowed on a raw
  `rgb_bit_count` header field.

In a release build these wrapped silently rather than panicking, which is the
worse outcome: a wrapped size goes on to slice the payload.
