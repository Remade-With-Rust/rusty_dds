//! The DirectXTex arm, over the C ABI shim in `shim/`.
//!
//! # Peer fairness
//!
//! At runtime there are two honest DirectXTex peers, and a benchmark that picks
//! only one is picking a side:
//!
//! * [`Peer::Loader`] — `GetMetadataFromDDSMemory` + `ComputePitch`, pointing
//!   into the caller's buffer. This is the `DDSTextureLoader` shape, and it is
//!   what a shipping engine uses to feed the GPU. **Default**, because
//!   comparing against the slower path would flatter us.
//! * [`Peer::Scratch`] — `LoadFromDDSMemory` into a `ScratchImage`. DirectXTex's
//!   own container API, which copies.
//!
//! The peer is recorded in every run manifest and printed on the board.

use std::os::raw::{c_int, c_uchar};

use crate::provider::{OpenTexture, SimError, SimResult, SubId, SubresourceBytes, TextureDesc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Peer {
    #[default]
    Loader,
    Scratch,
}

impl Peer {
    pub fn parse(s: &str) -> Option<Peer> {
        match s {
            "loader" | "ddstextureloader" => Some(Peer::Loader),
            "scratch" | "scratchimage" => Some(Peer::Scratch),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Peer::Loader => "loader",
            Peer::Scratch => "scratch",
        }
    }

    fn raw(self) -> c_int {
        match self {
            Peer::Loader => 0,
            Peer::Scratch => 1,
        }
    }
}

#[repr(C)]
struct DxtTextureOpaque {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct DxtDesc {
    width: u32,
    height: u32,
    depth: u32,
    mips: u32,
    layers: u32,
    dxgi_format: u32,
    block_bytes: u32,
    compressed: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct DxtSub {
    data: *const c_uchar,
    len: usize,
    width: u32,
    height: u32,
    row_pitch: u32,
    rows: u32,
}

extern "C" {
    fn dxt_open(
        bytes: *const c_uchar,
        len: usize,
        peer: c_int,
        out: *mut *mut DxtTextureOpaque,
    ) -> c_int;
    fn dxt_desc(tex: *const DxtTextureOpaque, out: *mut DxtDesc) -> c_int;
    fn dxt_subresource(
        tex: *const DxtTextureOpaque,
        mip: u32,
        layer: u32,
        face: u32,
        out: *mut DxtSub,
    ) -> c_int;
    fn dxt_decode_rgba8(
        tex: *const DxtTextureOpaque,
        mip: u32,
        layer: u32,
        face: u32,
        out: *mut *mut c_uchar,
        out_len: *mut usize,
    ) -> c_int;
    fn dxt_decode_rgba_f32(
        tex: *const DxtTextureOpaque,
        mip: u32,
        layer: u32,
        face: u32,
        out: *mut *mut f32,
        out_floats: *mut usize,
    ) -> c_int;
    fn dxt_free(p: *mut c_uchar);
    fn dxt_close(tex: *mut DxtTextureOpaque);
    fn dxt_resident_bytes(tex: *const DxtTextureOpaque) -> u64;
}

fn check(rc: c_int, what: &str) -> SimResult<()> {
    match rc {
        0 => Ok(()),
        1 => Err(SimError(format!("DirectXTex: {what}: parse failed"))),
        2 => Err(SimError(format!("DirectXTex: {what}: out of range"))),
        3 => Err(SimError(format!("DirectXTex: {what}: unsupported format"))),
        4 => Err(SimError(format!("DirectXTex: {what}: allocation failed"))),
        other => Err(SimError(format!("DirectXTex: {what}: code {other}"))),
    }
}

pub struct DxTexProvider {
    peer: Peer,
}

impl DxTexProvider {
    pub fn new(peer: Peer) -> SimResult<DxTexProvider> {
        Ok(DxTexProvider { peer })
    }
}

impl crate::provider::TextureProvider for DxTexProvider {
    fn name(&self) -> &'static str {
        "DirectXTex"
    }

    fn open(&self, bytes: Vec<u8>) -> SimResult<Box<dyn OpenTexture>> {
        let mut handle: *mut DxtTextureOpaque = std::ptr::null_mut();
        // SAFETY: `bytes` is a live allocation for the call and, because the
        // returned texture owns the Vec below, for the handle's whole lifetime —
        // which the loader path requires, as it points into it rather than copying.
        let rc = unsafe { dxt_open(bytes.as_ptr(), bytes.len(), self.peer.raw(), &mut handle) };
        check(rc, "open")?;
        if handle.is_null() {
            return Err(SimError("DirectXTex: open returned no handle".into()));
        }

        let mut desc = DxtDesc::default();
        // SAFETY: `handle` is non-null and freshly returned by dxt_open.
        let rc = unsafe { dxt_desc(handle, &mut desc) };
        if let Err(e) = check(rc, "desc") {
            // SAFETY: handle is live and not yet closed.
            unsafe { dxt_close(handle) };
            return Err(e);
        }

        Ok(Box::new(DxTexTexture {
            handle,
            _bytes: bytes,
            desc: TextureDesc {
                width: desc.width,
                height: desc.height,
                depth: desc.depth.max(1),
                mips: desc.mips,
                layers: desc.layers.max(1),
                dxgi_name: dxgi_name(desc.dxgi_format),
                vulkan_name: vulkan_name(desc.dxgi_format),
                block_bytes: desc.block_bytes,
                compressed: desc.compressed != 0,
            },
        }))
    }
}

pub struct DxTexTexture {
    handle: *mut DxtTextureOpaque,
    /// Borrowed by the shim on the loader path; must outlive `handle`. `Drop`
    /// closes the handle before this field is dropped.
    _bytes: Vec<u8>,
    desc: TextureDesc,
}

// SAFETY: after `open` the handle is only ever read. The streaming pool hands a
// texture to at most one worker per frame (it is moved into the job and moved
// back on merge), so no two threads touch one handle concurrently.
unsafe impl Send for DxTexTexture {}
unsafe impl Sync for DxTexTexture {}

impl Drop for DxTexTexture {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: closed exactly once; `Drop::drop` runs before `_bytes` is
            // released, so the shim never sees a dangling base pointer.
            unsafe { dxt_close(self.handle) };
            self.handle = std::ptr::null_mut();
        }
    }
}

impl OpenTexture for DxTexTexture {
    fn desc(&self) -> &TextureDesc {
        &self.desc
    }

    fn subresource(&self, id: SubId) -> SimResult<SubresourceBytes<'_>> {
        let mut sub = DxtSub::default();
        // SAFETY: handle live; `sub` is a valid out-param.
        let rc = unsafe { dxt_subresource(self.handle, id.mip, id.layer, id.face, &mut sub) };
        check(rc, "subresource")?;
        if sub.data.is_null() {
            return Err(SimError("DirectXTex: subresource returned no data".into()));
        }
        // SAFETY: the shim returns a pointer into either the borrowed file bytes
        // or the ScratchImage, both owned by `self` and outliving the borrow.
        let bytes = unsafe { std::slice::from_raw_parts(sub.data, sub.len) };
        Ok(SubresourceBytes {
            bytes,
            width: sub.width,
            height: sub.height,
            bytes_per_row: sub.row_pitch,
            rows_per_image: sub.rows,
        })
    }

    fn decode_rgba8(&self, id: SubId) -> SimResult<Vec<u8>> {
        let mut out: *mut c_uchar = std::ptr::null_mut();
        let mut len: usize = 0;
        // SAFETY: handle live; both out-params valid.
        let rc =
            unsafe { dxt_decode_rgba8(self.handle, id.mip, id.layer, id.face, &mut out, &mut len) };
        check(rc, "decode_rgba8")?;
        if out.is_null() {
            return Err(SimError("DirectXTex: decode returned no buffer".into()));
        }
        // SAFETY: the shim allocated `len` bytes at `out`; copied out and freed
        // through the shim's own deallocator, never Rust's.
        let v = unsafe { std::slice::from_raw_parts(out, len) }.to_vec();
        // SAFETY: `out` came from dxt_decode_rgba8 and is freed exactly once.
        unsafe { dxt_free(out) };
        Ok(v)
    }

    fn decode_rgba_f32(&self, id: SubId) -> SimResult<Vec<f32>> {
        let mut out: *mut f32 = std::ptr::null_mut();
        let mut len: usize = 0;
        // SAFETY: handle live; both out-params valid.
        let rc = unsafe {
            dxt_decode_rgba_f32(self.handle, id.mip, id.layer, id.face, &mut out, &mut len)
        };
        check(rc, "decode_rgba_f32")?;
        if out.is_null() {
            return Err(SimError("DirectXTex: HDR decode returned no buffer".into()));
        }
        // SAFETY: the shim allocated `len` floats at `out`; copied out and freed
        // through the shim's own deallocator, never Rust's.
        let v = unsafe { std::slice::from_raw_parts(out, len) }.to_vec();
        // SAFETY: `out` came from dxt_decode_rgba_f32 and is freed exactly once.
        unsafe { dxt_free(out as *mut c_uchar) };
        Ok(v)
    }

    fn resident_bytes(&self) -> u64 {
        // SAFETY: handle live.
        unsafe { dxt_resident_bytes(self.handle) }
    }

    fn reclaim(mut self: Box<Self>) -> Option<Vec<u8>> {
        // Close the handle first: on the loader path it points into `_bytes`,
        // and handing that buffer back while the shim still referenced it would
        // be a use-after-free.
        if !self.handle.is_null() {
            // SAFETY: closed exactly once; `Drop` sees a null handle afterwards.
            unsafe { dxt_close(self.handle) };
            self.handle = std::ptr::null_mut();
        }
        Some(std::mem::take(&mut self._bytes))
    }
}

/// DXGI enumerator names for the formats this harness cooks. Only the names the
/// renderer will need; anything else reports the numeric value's absence rather
/// than guessing.
fn dxgi_name(fmt: u32) -> &'static str {
    match fmt {
        71 => "BC1_UNorm",
        72 => "BC1_UNorm_sRGB",
        74 => "BC2_UNorm",
        77 => "BC3_UNorm",
        78 => "BC3_UNorm_sRGB",
        80 => "BC4_UNorm",
        81 => "BC4_SNorm",
        83 => "BC5_UNorm",
        84 => "BC5_SNorm",
        95 => "BC6H_UF16",
        96 => "BC6H_SF16",
        98 => "BC7_UNorm",
        99 => "BC7_UNorm_sRGB",
        28 => "R8G8B8A8_UNorm",
        87 => "B8G8R8A8_UNorm",
        _ => "unknown",
    }
}

fn vulkan_name(fmt: u32) -> &'static str {
    match fmt {
        71 => "VK_FORMAT_BC1_RGBA_UNORM_BLOCK",
        72 => "VK_FORMAT_BC1_RGBA_SRGB_BLOCK",
        74 => "VK_FORMAT_BC2_UNORM_BLOCK",
        77 => "VK_FORMAT_BC3_UNORM_BLOCK",
        78 => "VK_FORMAT_BC3_SRGB_BLOCK",
        80 => "VK_FORMAT_BC4_UNORM_BLOCK",
        81 => "VK_FORMAT_BC4_SNORM_BLOCK",
        83 => "VK_FORMAT_BC5_UNORM_BLOCK",
        84 => "VK_FORMAT_BC5_SNORM_BLOCK",
        95 => "VK_FORMAT_BC6H_UFLOAT_BLOCK",
        96 => "VK_FORMAT_BC6H_SFLOAT_BLOCK",
        98 => "VK_FORMAT_BC7_UNORM_BLOCK",
        99 => "VK_FORMAT_BC7_SRGB_BLOCK",
        28 => "VK_FORMAT_R8G8B8A8_UNORM",
        87 => "VK_FORMAT_B8G8R8A8_UNORM",
        _ => "unknown",
    }
}
