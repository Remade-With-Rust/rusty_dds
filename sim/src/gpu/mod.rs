//! The **API swap point**, in its real form: a windowed viewport that owns a
//! swapchain, uploads streamed subresources to the GPU, and measures GPU time
//! with the API's own timestamp queries.
//!
//! `NullRenderer` (in `renderer.rs`) remains the headless path used by the
//! board; this module is what the demo shows. Both consume the same streamed
//! bytes from the same deterministic replay, so a viewport cannot render
//! something the board did not measure.
//!
//! Threading: GPU uploads happen on the **main thread**. Worker threads still do
//! read + parse + the staging copy, exactly as they do headless; the main thread
//! then asks the streamer for the bytes of whatever became resident this frame
//! and issues the GPU copy. That mirrors how engines actually split the work,
//! and it keeps the immediate context single-threaded as D3D11 requires.

pub mod math;
pub mod scene;
#[cfg(windows)]
pub mod window;

#[cfg(feature = "d3d11")]
pub mod d3d11;
#[cfg(feature = "vulkan")]
pub mod vulkan;

use crate::provider::{SimResult, SubId, SubresourceBytes, TextureDesc};

/// Which graphics API a viewport speaks. This is the demo's third axis; the
/// other two are the DDS stack and the allocator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Api {
    D3D11,
    Vulkan,
}

impl Api {
    pub fn parse(s: &str) -> Option<Api> {
        match s {
            "d3d11" | "dx11" | "directx11" => Some(Api::D3D11),
            "vulkan" | "vk" => Some(Api::Vulkan),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Api::D3D11 => "d3d11",
            Api::Vulkan => "vulkan",
        }
    }
}

/// Where to put the window. The four-pane demo tiles these.
#[derive(Debug, Clone)]
pub struct ViewportConfig {
    pub title: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Draw the per-pane caption and live figures into the window itself, so a
    /// screen recording of the grid is self-describing.
    pub overlay: bool,
}

/// What the GPU reported for one frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct GpuTimings {
    /// Milliseconds between the frame's begin and end timestamps. `NaN` when the
    /// query was disjoint (a clock change invalidates the pair) — never zero,
    /// because zero is a plausible-looking lie.
    pub gpu_ms: f64,
    pub present_ms: f64,
    /// Bytes of GPU-local memory the process reports using, when the API can say.
    pub vram_used_mb: f64,
    pub vram_budget_mb: f64,
}

/// Hard limits that keep a live pane from becoming a driver stress test.
///
/// These exist because the first version of this viewport did not have them and
/// hung the machine. The streaming pool is deliberately over-committed, so it
/// evicts and re-requests textures continuously; the viewport was creating and
/// destroying a `ID3D11Texture2D` + SRV on every one of those cycles, on an
/// unthrottled `Present(0)` loop. That is thousands of GPU resource
/// create/destroy pairs per second, and it took the display driver down with it.
///
/// Every limit below is a ceiling, never a target: a pane that stays well under
/// all of them behaves exactly as it would without them.
#[derive(Debug, Clone, Copy)]
pub struct GpuLimits {
    /// Subresource uploads issued in one frame. The rest wait for the next.
    pub max_uploads_per_frame: usize,
    /// Bytes uploaded in one frame.
    pub max_upload_bytes_per_frame: u64,
    /// Ceiling on GPU memory held by streamed textures before the cache trims.
    pub max_gpu_texture_bytes: u64,
    /// GPU textures destroyed in one frame. Destruction is the expensive,
    /// driver-serialising operation, so it is rationed hardest.
    pub max_destroys_per_frame: usize,
    /// A frame slower than this counts as a stall.
    pub frame_abort_ms: f64,
    /// Consecutive stalls before the pane gives up rather than keep hammering.
    pub abort_after_slow_frames: u32,
    /// Wall-clock ceiling for one pane, regardless of frame count.
    pub max_run_secs: f64,
}

impl Default for GpuLimits {
    fn default() -> Self {
        Self {
            // 64/frame at 60 Hz is ~3800 uploads/s — comfortably inside what a
            // driver handles, and enough to keep GPU residency with the pool.
            // The first bounded run peaked at 740 deferred uploads on 24/frame.
            max_uploads_per_frame: 64,
            // Must stay under the Vulkan viewport's 32 MiB staging region.
            max_upload_bytes_per_frame: 24 << 20,
            max_gpu_texture_bytes: 1 << 30,
            max_destroys_per_frame: 2,
            frame_abort_ms: 500.0,
            abort_after_slow_frames: 20,
            max_run_secs: 900.0,
        }
    }
}

/// A live rendering surface. Implemented once per graphics API.
pub trait Viewport {
    fn api(&self) -> Api;

    /// Open a frame: wait until the GPU has finished with the previous one, and
    /// release anything that was deferred because it was still in use.
    ///
    /// **Every resource call below must happen after this.** Creating, updating
    /// or destroying a GPU resource while a submitted command buffer still
    /// references it is undefined behaviour, and on Vulkan it presents as
    /// `ERROR_DEVICE_LOST` — intermittently, under contention, which is the
    /// worst way to find out. D3D11's immediate context serialises for us, so
    /// its implementation is the default no-op.
    fn begin_frame(&mut self) -> SimResult<()> {
        Ok(())
    }

    /// Create the GPU resource backing one streamed texture, if it does not
    /// already exist. Idempotent.
    fn ensure_texture(&mut self, texture: u32, desc: &TextureDesc) -> SimResult<()>;

    /// Upload one subresource that just became resident.
    fn upload(&mut self, texture: u32, id: SubId, sub: &SubresourceBytes<'_>) -> SimResult<()>;

    /// The finest mip currently resident, so sampling reflects residency rather
    /// than showing detail the streamer has not delivered.
    fn set_min_lod(&mut self, texture: u32, min_lod: u32);

    /// Mark a texture as no longer streamed. **This must not free anything.**
    ///
    /// The pool evicts constantly by design; freeing here is what caused the
    /// create/destroy storm. The GPU resource stays cached for reuse, and only
    /// [`Viewport::trim`] ever releases one.
    fn release_texture(&mut self, texture: u32);

    /// Release least-recently-used GPU textures if the cache is over budget.
    /// At most `limits.max_destroys_per_frame` per call. Returns how many went.
    fn trim(&mut self, limits: &GpuLimits) -> usize;

    /// GPU memory currently held by streamed textures.
    fn gpu_bytes(&self) -> u64;

    /// Draw one frame and present.
    fn frame(&mut self, view: &scene::View, visible: &[scene::Quad]) -> SimResult<GpuTimings>;

    /// Pump the window's message queue. `false` means the user closed it.
    fn pump(&mut self) -> bool;

    /// Caption text drawn by the overlay.
    fn set_caption(&mut self, caption: &str);
}
