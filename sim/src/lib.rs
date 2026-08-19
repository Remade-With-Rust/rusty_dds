//! `rusty_dds_sim` — the deterministic DDS texture-streaming harness behind
//! [`docs/plans/simulator-demo.md`](../../docs/plans/simulator-demo.md).
//!
//! **Phase 0 scope: the harness and the null arm.** There is no GPU backend and
//! no DirectXTex arm yet, by design — the plan's Phase 0 exit gate is that
//! `A` vs `A` is flat and that the per-frame hash streams are bit-stable.
//! Adding a second stack before that gate passes would mean reporting a
//! difference against an unmeasured noise floor.
//!
//! The crate is a library so that both front-ends drive the *same* replay
//! driver ([`sim::Sim`]):
//!
//! * `sim` — the headless CLI. Reportable numbers come from here.
//! * `cockpit` — the Dioxus desktop app (feature `ui`). Live view and demo
//!   surface; the UI shares the process, so its timings are indicative.
//!
//! Two seams are already in place for later phases: [`provider::TextureProvider`]
//! (the stack swap: rusty_dds vs DirectXTex) and [`renderer::Renderer`] (the API
//! swap: null vs D3D11 vs Vulkan).

pub mod bench;
pub mod board;
pub mod cli;
#[cfg(feature = "dxtex")]
pub mod dxtex;
#[cfg(any(feature = "d3d11", feature = "vulkan"))]
pub mod gpu;
pub mod hash;
pub mod live;
pub mod metrics;
pub mod os;
pub mod pack;
pub mod panes;
pub mod provider;
pub mod renderer;
pub mod run;
pub mod scenario;
pub mod sim;
pub mod stream;
#[cfg(any(feature = "d3d11", feature = "vulkan"))]
pub mod view;
