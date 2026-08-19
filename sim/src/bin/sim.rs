//! `sim` — the harness on the **system allocator**.
//!
//! Identical to `sim-ra` in every respect except the global allocator, which is
//! a compile-time choice and therefore cannot be a runtime flag. Both link the
//! same `cli` out of the library, so the allocator is the only variable between
//! them.

#[cfg(feature = "alloc-counters")]
#[global_allocator]
static ALLOC: rusty_dds_sim::metrics::CountingAlloc<std::alloc::System> =
    rusty_dds_sim::metrics::CountingAlloc(std::alloc::System);

fn main() {
    rusty_dds_sim::cli::main("system");
}
