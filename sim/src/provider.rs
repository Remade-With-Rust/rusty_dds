//! The **stack swap point**: everything a streaming engine asks a DDS stack for.
//!
//! Phase 0 ships `RustyProvider` only. Phase 2 adds `DxTexProvider` over a C ABI
//! shim; the trait is deliberately shaped around a neutral coordinate
//! ([`SubId`]) and borrowed block bytes so a `ScratchImage`-backed
//! implementation fits without reshaping the seam.

use std::fmt;

use rusty_dds::{DdsView, SubresourceId};

// ------------------------------------------------------------------- errors

#[derive(Debug)]
pub struct SimError(pub String);

impl fmt::Display for SimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SimError {}

impl From<rusty_dds::Error> for SimError {
    fn from(e: rusty_dds::Error) -> Self {
        SimError(format!("rusty_dds: {e}"))
    }
}

impl From<std::io::Error> for SimError {
    fn from(e: std::io::Error) -> Self {
        SimError(format!("io: {e}"))
    }
}

pub type SimResult<T> = Result<T, SimError>;

// -------------------------------------------------------------- coordinates

/// Subresource coordinate. Sim-owned on purpose: the seam must not be shaped by
/// the library under test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct SubId {
    pub mip: u32,
    pub layer: u32,
    pub face: u32,
}

impl SubId {
    pub const fn mip(mip: u32) -> Self {
        Self {
            mip,
            layer: 0,
            face: 0,
        }
    }
}

impl From<SubId> for SubresourceId {
    fn from(s: SubId) -> Self {
        SubresourceId::new(s.mip, s.layer, s.face)
    }
}

/// What the renderer needs to create the GPU resource.
#[derive(Debug, Clone)]
pub struct TextureDesc {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub mips: u32,
    pub layers: u32,
    /// DXGI enumerator name, e.g. `"BC7_UNorm"` — stringly typed so the seam
    /// stays free of any graphics-API crate (same policy as `rusty_dds::upload`).
    pub dxgi_name: &'static str,
    pub vulkan_name: &'static str,
    pub block_bytes: u32,
    pub compressed: bool,
}

/// Block bytes plus the pitches a buffer→image copy needs.
pub struct SubresourceBytes<'a> {
    pub bytes: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub bytes_per_row: u32,
    pub rows_per_image: u32,
}

// -------------------------------------------------------------------- traits

pub trait TextureProvider: Send + Sync {
    fn name(&self) -> &'static str;

    /// Parse container bytes into a provider-owned handle. This is the call the
    /// Stream profile measures as `parse_ms`.
    ///
    /// The buffer is moved in and comes back out through
    /// [`OpenTexture::reclaim`] when the texture is dropped.
    fn open(&self, bytes: Vec<u8>) -> SimResult<Box<dyn OpenTexture>>;
}

pub trait OpenTexture: Send + Sync {
    fn desc(&self) -> &TextureDesc;

    /// Stream profile: block bytes for one subresource, no decode in either arm.
    fn subresource(&self, id: SubId) -> SimResult<SubresourceBytes<'_>>;

    /// Transcode profile (Phase 4). Present on the seam from the start so the
    /// DirectXTex arm is written against the final shape.
    fn decode_rgba8(&self, id: SubId) -> SimResult<Vec<u8>>;

    /// Bytes this handle holds resident, for the pool's accounting.
    fn resident_bytes(&self) -> u64;

    /// Give the payload buffer back when the texture is dropped, so the pool can
    /// hand it to the next open instead of faulting in a fresh one.
    ///
    /// Returning `None` is always correct; it just forfeits the reuse.
    fn reclaim(self: Box<Self>) -> Option<Vec<u8>> {
        None
    }
}

// -------------------------------------------------------------------- rusty

pub struct RustyProvider;

impl TextureProvider for RustyProvider {
    fn name(&self) -> &'static str {
        "rusty_dds"
    }

    fn open(&self, bytes: Vec<u8>) -> SimResult<Box<dyn OpenTexture>> {
        // Borrowing parse: the header is read, the payload is not copied. The
        // owning `Dds::read` allocated a second 1.33 MiB buffer per open, and
        // ~87% of that call was the OS faulting in pages we then overwrote.
        let dds = DdsView::parse(&bytes)?;
        let fmt = dds.gpu_format()?;
        let surf = dds.surface(SubresourceId::new(0, 0, 0))?;
        let desc = TextureDesc {
            width: surf.width,
            height: surf.height,
            depth: surf.depth,
            mips: dds.get_num_mipmap_levels(),
            layers: dds.get_num_array_layers(),
            dxgi_name: fmt.dxgi_name,
            vulkan_name: fmt.vulkan_name,
            block_bytes: fmt.block_bytes,
            compressed: fmt.compressed,
        };
        // The view borrows `bytes`, so the texture owns the bytes and re-derives
        // a view per query. `DdsView::parse` reads only the header and allocates
        // nothing, so that is far cheaper than keeping a copied payload alive.
        drop(dds);
        Ok(Box::new(RustyTexture { bytes, desc }))
    }
}

pub struct RustyTexture {
    bytes: Vec<u8>,
    desc: TextureDesc,
}

impl OpenTexture for RustyTexture {
    fn desc(&self) -> &TextureDesc {
        &self.desc
    }

    fn subresource(&self, id: SubId) -> SimResult<SubresourceBytes<'_>> {
        let dds = DdsView::parse(&self.bytes)?;
        let plan = dds.upload_plan_compressed(id.into())?;
        let end = plan
            .data_offset
            .checked_add(plan.data_len)
            .ok_or_else(|| SimError("upload plan range overflow".into()))?;
        // `dds.data` borrows `self.bytes`, so the slice outlives the view.
        let bytes = dds
            .data
            .get(plan.data_offset..end)
            .ok_or_else(|| SimError("upload plan range outside payload".into()))?;
        Ok(SubresourceBytes {
            bytes,
            width: plan.width,
            height: plan.height,
            bytes_per_row: plan.bytes_per_row,
            rows_per_image: plan.rows_per_image,
        })
    }

    fn decode_rgba8(&self, id: SubId) -> SimResult<Vec<u8>> {
        Ok(DdsView::parse(&self.bytes)?.decode_rgba8(id.into())?.pixels)
    }

    fn resident_bytes(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn reclaim(self: Box<Self>) -> Option<Vec<u8>> {
        Some(self.bytes)
    }
}

/// What an arm label asks for.
///
/// An arm names two independent things — which DDS stack, and which allocator —
/// because those are two of the four variables the demo exists to separate. The
/// allocator half cannot be honoured at runtime (`#[global_allocator]` is a
/// compile-time choice), so it selects a *binary*: `sim` or `sim-ra`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arm {
    pub stack: Stack,
    /// `true` when the label ends in `+ra`.
    pub wants_rusty_alloc: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stack {
    RustyDds,
    DirectXTex,
}

impl Stack {
    pub fn name(self) -> &'static str {
        match self {
            Stack::RustyDds => "rusty_dds",
            Stack::DirectXTex => "DirectXTex",
        }
    }
}

/// Parse an arm label.
///
/// | label | stack | allocator | binary |
/// |---|---|---|---|
/// | `a`, `a2`, `rusty` | rusty_dds | system | `sim` |
/// | `rusty+ra` | rusty_dds | rusty_alloc | `sim-ra` |
/// | `dxtex` | DirectXTex | system | `sim` |
/// | `dxtex+ra` | DirectXTex | rusty_alloc | `sim-ra` |
///
/// `a` and `a2` are the null pair: the same build under two labels.
pub fn parse_arm(arm: &str) -> SimResult<Arm> {
    let (base, wants_rusty_alloc) = match arm.strip_suffix("+ra") {
        Some(b) => (b, true),
        None => (arm, false),
    };
    let stack = match base {
        "a" | "a2" | "rusty" => Stack::RustyDds,
        "b" | "dxtex" => Stack::DirectXTex,
        other => {
            return Err(SimError(format!(
                "unknown arm `{other}` — expected a, a2, rusty, dxtex, optionally suffixed `+ra`"
            )))
        }
    };
    Ok(Arm {
        stack,
        wants_rusty_alloc,
    })
}

/// Arm label → provider. `peer` selects which DirectXTex path the peer arm uses
/// and is ignored by the rusty arm.
pub fn provider_for(arm: &str, peer: &str) -> SimResult<Box<dyn TextureProvider>> {
    match parse_arm(arm)?.stack {
        Stack::RustyDds => Ok(Box::new(RustyProvider)),
        Stack::DirectXTex => {
            #[cfg(feature = "dxtex")]
            {
                let p = crate::dxtex::Peer::parse(peer).ok_or_else(|| {
                    SimError(format!("unknown DirectXTex peer `{peer}` — loader or scratch"))
                })?;
                Ok(Box::new(crate::dxtex::DxTexProvider::new(p)?))
            }
            #[cfg(not(feature = "dxtex"))]
            {
                let _ = peer;
                Err(SimError(
                    "arm `dxtex` needs the `dxtex` feature and the DirectXTex shim —                      build it with `cargo build --release --features dxtex` (see sim/shim/README.md)"
                        .into(),
                ))
            }
        }
    }
}
