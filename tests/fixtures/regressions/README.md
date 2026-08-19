# Parser regression corpus

Every input that ever panicked, hung, or over-allocated in `Dds::read` or
anything downstream of it goes in this directory, as a raw byte file with any
name. `tests/parser_robustness.rs::regression_corpus_is_clean` replays all of
them on every `cargo test`.

Sources: `tests/parser_robustness.rs` (the always-on structured harness) and
`fuzz/` (cargo-fuzz; see `fuzz/README.md`). When a fuzz run produces an
artifact under `fuzz/artifacts/<target>/`, copy it here and commit it in the
same change as the fix - the fix is not done until the input is pinned.
