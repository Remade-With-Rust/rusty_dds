//! The Direct3D 11 viewport.
//!
//! A conventional PC renderer shape on purpose: immediate context, per-object
//! constant buffer, `UpdateSubresource` for streamed mips, and
//! `SetResourceMinLOD` so sampling reflects what the streamer has actually
//! delivered. That last call is what makes the with/without difference *visible*
//! — a texture whose top mips have not arrived is sampled at the coarsest mip
//! that has, exactly as it would be in a shipping engine.
//!
//! GPU time comes from D3D11's own disjoint/timestamp query pair. A disjoint
//! result reports `NaN` rather than zero: a clock change invalidates the
//! measurement, and zero is a plausible-looking lie.

use std::collections::HashMap;

use windows::core::{Interface, PCSTR};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCompile, D3DCOMPILE_OPTIMIZATION_LEVEL3};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter3, IDXGIDevice, IDXGIFactory1, IDXGISwapChain,
    DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_QUERY_VIDEO_MEMORY_INFO, DXGI_SWAP_CHAIN_DESC,
    DXGI_SWAP_EFFECT_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};

use crate::gpu::scene::{Quad, View};
use crate::gpu::window::Window;
use crate::gpu::{Api, GpuLimits, GpuTimings, Viewport, ViewportConfig};
use crate::provider::{SimError, SimResult, SubId, SubresourceBytes, TextureDesc};

const SHADER: &str = r#"
cbuffer CB : register(b0) { float4x4 mvp; };
struct VSOut { float4 pos : SV_POSITION; float2 uv : TEXCOORD0; };
VSOut vs_main(float3 p : POSITION, float2 uv : TEXCOORD0) {
    VSOut o;
    o.pos = mul(mvp, float4(p, 1.0));
    o.uv = uv;
    return o;
}
Texture2D tex : register(t0);
SamplerState smp : register(s0);
float4 ps_main(VSOut i) : SV_Target {
    return float4(tex.Sample(smp, i.uv).rgb, 1.0);
}
"#;

#[repr(C)]
#[derive(Clone, Copy)]
struct Vertex {
    pos: [f32; 3],
    uv: [f32; 2],
}

const QUAD: [Vertex; 6] = [
    Vertex { pos: [-0.5, -0.5, 0.0], uv: [0.0, 1.0] },
    Vertex { pos: [-0.5, 0.5, 0.0], uv: [0.0, 0.0] },
    Vertex { pos: [0.5, 0.5, 0.0], uv: [1.0, 0.0] },
    Vertex { pos: [-0.5, -0.5, 0.0], uv: [0.0, 1.0] },
    Vertex { pos: [0.5, 0.5, 0.0], uv: [1.0, 0.0] },
    Vertex { pos: [0.5, -0.5, 0.0], uv: [1.0, 1.0] },
];

struct QuerySet {
    disjoint: ID3D11Query,
    begin: ID3D11Query,
    end: ID3D11Query,
}

struct GpuTexture {
    texture: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
    mips: u32,
    /// VRAM this resource occupies, for the cache budget.
    bytes: u64,
    /// Frame it was last drawn or uploaded to, for LRU trimming.
    last_used: u64,
    /// Still streamed by the pool? `false` means evicted, and therefore a
    /// candidate for trimming — but never freed on the spot.
    live: bool,
}

/// Full-chain size of a texture, for the cache budget. Approximate on purpose:
/// it only has to rank resources against each other and against a ceiling.
fn texture_bytes(desc: &TextureDesc) -> u64 {
    let bpb = desc.block_bytes.max(1) as u64;
    let (bw, bh) = if desc.compressed { (4u64, 4u64) } else { (1, 1) };
    let mut total = 0u64;
    for m in 0..desc.mips.max(1) {
        let w = (desc.width >> m).max(1) as u64;
        let h = (desc.height >> m).max(1) as u64;
        total += w.div_ceil(bw) * h.div_ceil(bh) * bpb;
    }
    total * desc.layers.max(1) as u64
}

pub struct D3d11Viewport {
    window: Window,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    swap: IDXGISwapChain,
    rtv: Option<ID3D11RenderTargetView>,
    adapter: Option<IDXGIAdapter3>,

    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    layout: ID3D11InputLayout,
    vb: ID3D11Buffer,
    cb: ID3D11Buffer,
    sampler: ID3D11SamplerState,

    /// Two query sets, written on frame N and read on frame N+1. Reading the
    /// current frame's timestamps would either stall the pipeline or, worse,
    /// silently report stale numbers.
    queries: [QuerySet; 2],
    query_at: usize,
    have_pending: bool,

    textures: HashMap<u32, GpuTexture>,
    gpu_bytes: u64,
    frame_counter: u64,
}

fn compile(source: &str, entry: &str, target: &str) -> SimResult<Vec<u8>> {
    let entry_z = format!("{entry}\0");
    let target_z = format!("{target}\0");
    let mut blob = None;
    let mut errors = None;
    // SAFETY: all pointers reference live, correctly-sized local data; the two
    // out-params are Option<Interface> slots the API fills.
    let hr = unsafe {
        D3DCompile(
            source.as_ptr() as *const _,
            source.len(),
            None,
            None,
            None,
            PCSTR(entry_z.as_ptr()),
            PCSTR(target_z.as_ptr()),
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut blob,
            Some(&mut errors),
        )
    };
    if hr.is_err() {
        let msg = errors
            .map(|e| {
                // SAFETY: the error blob, when present, holds a NUL-terminated string.
                unsafe {
                    let p = e.GetBufferPointer() as *const u8;
                    let n = e.GetBufferSize();
                    String::from_utf8_lossy(std::slice::from_raw_parts(p, n)).into_owned()
                }
            })
            .unwrap_or_else(|| format!("{hr:?}"));
        return Err(SimError(format!("HLSL {entry} failed: {msg}")));
    }
    let blob = blob.ok_or_else(|| SimError("HLSL produced no bytecode".into()))?;
    // SAFETY: a successful compile yields a blob of the reported size.
    Ok(unsafe {
        std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize())
            .to_vec()
    })
}

fn dxgi_format(name: &str) -> DXGI_FORMAT {
    match name {
        "BC1_UNorm" => DXGI_FORMAT_BC1_UNORM,
        "BC1_UNorm_sRGB" => DXGI_FORMAT_BC1_UNORM_SRGB,
        "BC2_UNorm" => DXGI_FORMAT_BC2_UNORM,
        "BC3_UNorm" => DXGI_FORMAT_BC3_UNORM,
        "BC3_UNorm_sRGB" => DXGI_FORMAT_BC3_UNORM_SRGB,
        "BC4_UNorm" => DXGI_FORMAT_BC4_UNORM,
        "BC4_SNorm" => DXGI_FORMAT_BC4_SNORM,
        "BC5_UNorm" => DXGI_FORMAT_BC5_UNORM,
        "BC5_SNorm" => DXGI_FORMAT_BC5_SNORM,
        "BC6H_UF16" => DXGI_FORMAT_BC6H_UF16,
        "BC7_UNorm" => DXGI_FORMAT_BC7_UNORM,
        "BC7_UNorm_sRGB" => DXGI_FORMAT_BC7_UNORM_SRGB,
        "B8G8R8A8_UNorm" => DXGI_FORMAT_B8G8R8A8_UNORM,
        _ => DXGI_FORMAT_R8G8B8A8_UNORM,
    }
}

impl D3d11Viewport {
    pub fn new(cfg: &ViewportConfig) -> SimResult<D3d11Viewport> {
        let window = Window::new(&cfg.title, cfg.x, cfg.y, cfg.width, cfg.height)
            .map_err(|e| SimError(format!("window: {e}")))?;

        let desc = DXGI_SWAP_CHAIN_DESC {
            BufferDesc: DXGI_MODE_DESC {
                Width: cfg.width,
                Height: cfg.height,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                RefreshRate: DXGI_RATIONAL {
                    Numerator: 0,
                    Denominator: 1,
                },
                ..Default::default()
            },
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            OutputWindow: window.hwnd,
            Windowed: true.into(),
            SwapEffect: DXGI_SWAP_EFFECT_DISCARD,
            ..Default::default()
        };

        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        let mut swap: Option<IDXGISwapChain> = None;
        let levels = [D3D_FEATURE_LEVEL_11_0];

        // SAFETY: descriptor and out-params are valid; HWND outlives the swapchain
        // because the window is owned by this struct.
        unsafe {
            D3D11CreateDeviceAndSwapChain(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                windows::Win32::Foundation::HMODULE::default(),
                D3D11_CREATE_DEVICE_FLAG(0),
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&desc),
                Some(&mut swap),
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(|e| SimError(format!("D3D11CreateDeviceAndSwapChain: {e}")))?;
        }
        let device = device.ok_or_else(|| SimError("no D3D11 device".into()))?;
        let context = context.ok_or_else(|| SimError("no D3D11 context".into()))?;
        let swap = swap.ok_or_else(|| SimError("no swapchain".into()))?;

        // The adapter, for VRAM budget reporting.
        let adapter = (|| -> windows::core::Result<IDXGIAdapter3> {
            let dxgi: IDXGIDevice = device.cast()?;
            // SAFETY: a live IDXGIDevice always has a parent adapter.
            let ad = unsafe { dxgi.GetAdapter()? };
            ad.cast::<IDXGIAdapter3>()
        })()
        .ok();
        let _factory: Option<IDXGIFactory1> =
            // SAFETY: documented factory entry point.
            unsafe { CreateDXGIFactory1().ok() };

        let vs_code = compile(SHADER, "vs_main", "vs_5_0")?;
        let ps_code = compile(SHADER, "ps_main", "ps_5_0")?;

        // SAFETY: every descriptor below is fully initialised and every buffer
        // pointer references live local data for the duration of the call.
        unsafe {
            let mut vs = None;
            device
                .CreateVertexShader(&vs_code, None, Some(&mut vs))
                .map_err(|e| SimError(format!("CreateVertexShader: {e}")))?;
            let mut ps = None;
            device
                .CreatePixelShader(&ps_code, None, Some(&mut ps))
                .map_err(|e| SimError(format!("CreatePixelShader: {e}")))?;

            let elems = [
                D3D11_INPUT_ELEMENT_DESC {
                    SemanticName: PCSTR(b"POSITION\0".as_ptr()),
                    SemanticIndex: 0,
                    Format: DXGI_FORMAT_R32G32B32_FLOAT,
                    InputSlot: 0,
                    AlignedByteOffset: 0,
                    InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                    InstanceDataStepRate: 0,
                },
                D3D11_INPUT_ELEMENT_DESC {
                    SemanticName: PCSTR(b"TEXCOORD\0".as_ptr()),
                    SemanticIndex: 0,
                    Format: DXGI_FORMAT_R32G32_FLOAT,
                    InputSlot: 0,
                    AlignedByteOffset: 12,
                    InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                    InstanceDataStepRate: 0,
                },
            ];
            let mut layout = None;
            device
                .CreateInputLayout(&elems, &vs_code, Some(&mut layout))
                .map_err(|e| SimError(format!("CreateInputLayout: {e}")))?;

            let vb_desc = D3D11_BUFFER_DESC {
                ByteWidth: std::mem::size_of_val(&QUAD) as u32,
                Usage: D3D11_USAGE_IMMUTABLE,
                BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32,
                ..Default::default()
            };
            let vb_data = D3D11_SUBRESOURCE_DATA {
                pSysMem: QUAD.as_ptr() as *const _,
                ..Default::default()
            };
            let mut vb = None;
            device
                .CreateBuffer(&vb_desc, Some(&vb_data), Some(&mut vb))
                .map_err(|e| SimError(format!("CreateBuffer(vb): {e}")))?;

            let cb_desc = D3D11_BUFFER_DESC {
                ByteWidth: 64,
                Usage: D3D11_USAGE_DYNAMIC,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                ..Default::default()
            };
            let mut cb = None;
            device
                .CreateBuffer(&cb_desc, None, Some(&mut cb))
                .map_err(|e| SimError(format!("CreateBuffer(cb): {e}")))?;

            let samp_desc = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_ANISOTROPIC,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MaxAnisotropy: 8,
                MinLOD: 0.0,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            let mut sampler = None;
            device
                .CreateSamplerState(&samp_desc, Some(&mut sampler))
                .map_err(|e| SimError(format!("CreateSamplerState: {e}")))?;

            let q = |kind| -> SimResult<ID3D11Query> {
                let d = D3D11_QUERY_DESC {
                    Query: kind,
                    MiscFlags: 0,
                };
                let mut out = None;
                device
                    .CreateQuery(&d, Some(&mut out))
                    .map_err(|e| SimError(format!("CreateQuery: {e}")))?;
                out.ok_or_else(|| SimError("CreateQuery returned nothing".into()))
            };
            let mut sets = Vec::with_capacity(2);
            for _ in 0..2 {
                sets.push(QuerySet {
                    disjoint: q(D3D11_QUERY_TIMESTAMP_DISJOINT)?,
                    begin: q(D3D11_QUERY_TIMESTAMP)?,
                    end: q(D3D11_QUERY_TIMESTAMP)?,
                });
            }
            let mut sets = sets.into_iter();
            let queries = [sets.next().unwrap(), sets.next().unwrap()];

            let mut vp = D3d11Viewport {
                window,
                device,
                context,
                swap,
                rtv: None,
                adapter,
                vs: vs.unwrap(),
                ps: ps.unwrap(),
                layout: layout.unwrap(),
                vb: vb.unwrap(),
                cb: cb.unwrap(),
                sampler: sampler.unwrap(),
                queries,
                query_at: 0,
                have_pending: false,
                textures: HashMap::new(),
                gpu_bytes: 0,
                frame_counter: 0,
            };
            vp.make_rtv()?;
            Ok(vp)
        }
    }

    fn make_rtv(&mut self) -> SimResult<()> {
        // SAFETY: swapchain buffer 0 always exists after creation.
        unsafe {
            let back: ID3D11Texture2D = self
                .swap
                .GetBuffer(0)
                .map_err(|e| SimError(format!("GetBuffer: {e}")))?;
            let mut rtv = None;
            self.device
                .CreateRenderTargetView(&back, None, Some(&mut rtv))
                .map_err(|e| SimError(format!("CreateRenderTargetView: {e}")))?;
            self.rtv = rtv;
        }
        Ok(())
    }

    fn vram(&self) -> (f64, f64) {
        let Some(ad) = &self.adapter else {
            return (0.0, 0.0);
        };
        let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
        // SAFETY: `info` is a valid out-param for the local memory segment.
        let ok = unsafe { ad.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info) };
        if ok.is_err() {
            return (0.0, 0.0);
        }
        const MB: f64 = (1 << 20) as f64;
        (
            info.CurrentUsage as f64 / MB,
            info.Budget as f64 / MB,
        )
    }
}

impl Viewport for D3d11Viewport {
    fn api(&self) -> Api {
        Api::D3D11
    }

    fn ensure_texture(&mut self, texture: u32, desc: &TextureDesc) -> SimResult<()> {
        // Re-marking an existing resource live is the whole point of the cache:
        // the pool evicts and re-requests the same textures constantly, and
        // recreating them here is what hung the driver.
        if let Some(gt) = self.textures.get_mut(&texture) {
            gt.live = true;
            gt.last_used = self.frame_counter;
            return Ok(());
        }
        let td = D3D11_TEXTURE2D_DESC {
            Width: desc.width,
            Height: desc.height,
            MipLevels: desc.mips,
            ArraySize: desc.layers.max(1),
            Format: dxgi_format(desc.dxgi_name),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            ..Default::default()
        };
        // SAFETY: descriptor fully initialised; no initial data, mips arrive by
        // UpdateSubresource as the streamer delivers them.
        unsafe {
            let mut tex = None;
            self.device
                .CreateTexture2D(&td, None, Some(&mut tex))
                .map_err(|e| SimError(format!("CreateTexture2D: {e}")))?;
            let tex = tex.ok_or_else(|| SimError("CreateTexture2D returned nothing".into()))?;
            let mut srv = None;
            self.device
                .CreateShaderResourceView(&tex, None, Some(&mut srv))
                .map_err(|e| SimError(format!("CreateShaderResourceView: {e}")))?;
            let bytes = texture_bytes(desc);
            self.gpu_bytes += bytes;
            self.textures.insert(
                texture,
                GpuTexture {
                    texture: tex,
                    srv: srv.ok_or_else(|| SimError("no SRV".into()))?,
                    mips: desc.mips,
                    bytes,
                    last_used: self.frame_counter,
                    live: true,
                },
            );
        }
        Ok(())
    }

    fn upload(&mut self, texture: u32, id: SubId, sub: &SubresourceBytes<'_>) -> SimResult<()> {
        let frame = self.frame_counter;
        let Some(gt) = self.textures.get_mut(&texture) else {
            return Ok(());
        };
        gt.last_used = frame;
        let gt = &*gt;
        let subresource = id.mip + (id.layer + id.face) * gt.mips;
        // SAFETY: `sub.bytes` is live for the call and at least
        // `bytes_per_row * rows_per_image` long; the subresource index is inside
        // the texture created in `ensure_texture`.
        unsafe {
            self.context.UpdateSubresource(
                &gt.texture,
                subresource,
                None,
                sub.bytes.as_ptr() as *const _,
                sub.bytes_per_row,
                sub.bytes_per_row * sub.rows_per_image,
            );
        }
        Ok(())
    }

    fn set_min_lod(&mut self, texture: u32, min_lod: u32) {
        if let Some(gt) = self.textures.get(&texture) {
            // SAFETY: the resource is live and owned by this viewport.
            unsafe {
                self.context
                    .SetResourceMinLOD(&gt.texture, min_lod as f32);
            }
        }
    }

    fn release_texture(&mut self, texture: u32) {
        // Deliberately does not free. See the trait docs and GpuLimits.
        if let Some(gt) = self.textures.get_mut(&texture) {
            gt.live = false;
        }
    }

    fn trim(&mut self, limits: &GpuLimits) -> usize {
        if self.gpu_bytes <= limits.max_gpu_texture_bytes {
            return 0;
        }
        // Oldest evicted resources first, and never one the pool still streams
        // or that was drawn this frame.
        let mut cands: Vec<(u64, u32)> = self
            .textures
            .iter()
            .filter(|(_, gt)| !gt.live && gt.last_used < self.frame_counter)
            .map(|(id, gt)| (gt.last_used, *id))
            .collect();
        cands.sort_unstable();

        let mut freed = 0;
        for (_, id) in cands.into_iter().take(limits.max_destroys_per_frame) {
            if self.gpu_bytes <= limits.max_gpu_texture_bytes {
                break;
            }
            if let Some(gt) = self.textures.remove(&id) {
                self.gpu_bytes = self.gpu_bytes.saturating_sub(gt.bytes);
                freed += 1;
            }
        }
        freed
    }

    fn gpu_bytes(&self) -> u64 {
        self.gpu_bytes
    }

    fn frame(&mut self, view: &View, visible: &[Quad]) -> SimResult<GpuTimings> {
        let Some(rtv) = self.rtv.clone() else {
            return Ok(GpuTimings::default());
        };
        let (w, h) = (self.window.width as f32, self.window.height as f32);

        self.frame_counter += 1;
        let this_set = self.query_at;
        let prev_set = 1 - self.query_at;

        // SAFETY: every resource referenced is owned by this struct and live;
        // the constant buffer is mapped WRITE_DISCARD and written exactly 64 bytes.
        unsafe {
            self.context.Begin(&self.queries[this_set].disjoint);
            self.context.End(&self.queries[this_set].begin);

            let vp = D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: w,
                Height: h,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            self.context.RSSetViewports(Some(&[vp]));
            self.context.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            self.context
                .ClearRenderTargetView(&rtv, &[0.05, 0.06, 0.08, 1.0]);

            self.context
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.context.IASetInputLayout(&self.layout);
            let stride = std::mem::size_of::<Vertex>() as u32;
            let offset = 0u32;
            self.context.IASetVertexBuffers(
                0,
                1,
                Some(&Some(self.vb.clone())),
                Some(&stride),
                Some(&offset),
            );
            self.context.VSSetShader(&self.vs, None);
            self.context.PSSetShader(&self.ps, None);
            self.context.VSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            self.context.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));

            let frame = self.frame_counter;
            for quad in visible {
                let Some(gt) = self.textures.get_mut(&quad.texture) else {
                    continue;
                };
                gt.last_used = frame;
                let gt = &*gt;
                let mvp = view.view_proj.mul(quad.model).transposed();
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                if self
                    .context
                    .Map(&self.cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))
                    .is_ok()
                {
                    std::ptr::copy_nonoverlapping(
                        mvp.as_ptr(),
                        mapped.pData as *mut f32,
                        16,
                    );
                    self.context.Unmap(&self.cb, 0);
                }
                self.context
                    .PSSetShaderResources(0, Some(&[Some(gt.srv.clone())]));
                self.context.Draw(QUAD.len() as u32, 0);
            }

            self.context.End(&self.queries[this_set].end);
            self.context.End(&self.queries[this_set].disjoint);

            // Present(1) = wait for one vblank. Present(0) lets the loop submit
            // as fast as the CPU can build frames, which is exactly how the
            // first version of this viewport hung the display driver. The demo
            // wants a watchable, display-paced picture anyway; the reportable
            // numbers come from the headless board, which has no present at all.
            let present_start = std::time::Instant::now();
            let _ = self.swap.Present(1, windows::Win32::Graphics::Dxgi::DXGI_PRESENT(0));
            let present_ms = present_start.elapsed().as_secs_f64() * 1e3;

            // Read the PREVIOUS frame's set, which the GPU has had a whole frame
            // to finish. Readiness is detected by sentinel, not by the return
            // value: `GetData` answers S_FALSE when the data is not ready yet,
            // and windows-rs maps S_FALSE onto `Ok(())`, so `is_ok()` would
            // happily hand back an untouched buffer full of zeros. A zero here
            // is the most plausible-looking lie the harness could tell.
            let gpu_ms = if self.have_pending {
                let mut dj = D3D11_QUERY_DATA_TIMESTAMP_DISJOINT {
                    Frequency: 0,
                    Disjoint: true.into(),
                };
                let mut t0: u64 = u64::MAX;
                let mut t1: u64 = u64::MAX;
                let q = &self.queries[prev_set];
                let _ = self.context.GetData(
                    &q.disjoint,
                    Some(&mut dj as *mut _ as *mut _),
                    std::mem::size_of_val(&dj) as u32,
                    0,
                );
                let _ = self
                    .context
                    .GetData(&q.begin, Some(&mut t0 as *mut _ as *mut _), 8, 0);
                let _ = self
                    .context
                    .GetData(&q.end, Some(&mut t1 as *mut _ as *mut _), 8, 0);

                let ready = dj.Frequency != 0 && t0 != u64::MAX && t1 != u64::MAX;
                if ready && !dj.Disjoint.as_bool() && t1 >= t0 {
                    (t1 - t0) as f64 * 1e3 / dj.Frequency as f64
                } else {
                    f64::NAN
                }
            } else {
                f64::NAN
            };
            self.have_pending = true;
            self.query_at = prev_set;

            let (vram_used_mb, vram_budget_mb) = self.vram();
            Ok(GpuTimings {
                gpu_ms,
                present_ms,
                vram_used_mb,
                vram_budget_mb,
            })
        }
    }

    fn pump(&mut self) -> bool {
        self.window.pump()
    }

    fn set_caption(&mut self, caption: &str) {
        self.window.set_title(caption);
    }
}
