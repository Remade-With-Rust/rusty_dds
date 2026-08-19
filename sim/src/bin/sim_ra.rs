//! `sim-ra` — the harness on **rusty_alloc**.
//!
//! The allocator arm. Same program as `sim`; only the global allocator differs,
//! which is exactly the variable this binary exists to isolate.
//!
//! `rusty_alloc` is pinned `>=0.4.0`: its own release notes declare 0.3.2 and
//! earlier unsound on every target.

#[cfg(feature = "alloc-counters")]
#[global_allocator]
static ALLOC: rusty_dds_sim::metrics::CountingAlloc<rusty_alloc_api::RustyAlloc> =
    rusty_dds_sim::metrics::CountingAlloc(rusty_alloc_api::RustyAlloc);

fn main() {
    rusty_dds_sim::cli::main("rusty_alloc");
}
