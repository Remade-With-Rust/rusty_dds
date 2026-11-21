//! ASTC containers: the raw `.astc` file, KTX 1.0 and KTX 2.0. Every parser
//! works on a borrowed `&[u8]`, validates sizes with checked arithmetic before
//! it trusts them, and never allocates in proportion to a header field.

mod astc_file;
pub mod ktx;

pub use astc_file::AstcFile;

use super::{AstcFootprint, AstcProfile};
use crate::{Error, ImageRgba8, ImageRgbaF32};

/// One ASTC subresource from any container, with what the container says
/// about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct AstcSurface<'a> {
    pub footprint: AstcFootprint,
    pub width: u32,
    pub height: u32,
    /// 1 for 2D; the slice count of a 3D texture.
    pub depth: u32,
    /// From the container's format tag. `.astc` has no tag: `None`.
    pub srgb: Option<bool>,
    /// `Some(true)` only where the container says so (KTX 2.0 `SFLOAT`
    /// formats). KTX 1.0 and the DDS convention cannot express HDR: `None`.
    pub hdr: Option<bool>,
    /// The block payload of this subresource (exactly its length).
    pub data: &'a [u8],
}

impl<'a> AstcSurface<'a> {
    pub(crate) fn new(
        footprint: AstcFootprint,
        width: u32,
        height: u32,
        depth: u32,
        srgb: Option<bool>,
        hdr: Option<bool>,
        data: &'a [u8],
    ) -> Self {
        Self {
            footprint,
            width,
            height,
            depth,
            srgb,
            hdr,
            data,
        }
    }

    /// The profile for an 8-bit decode: `LdrSrgb` when tagged sRGB or
    /// untagged (FFmpeg's default for a raw `.astc`), `Ldr` when tagged
    /// linear. An `hdr == Some(true)` surface has no 8-bit profile: this
    /// returns `Hdr`, which 8-bit decode rejects.
    pub fn default_profile(&self) -> AstcProfile {
        if self.hdr == Some(true) {
            AstcProfile::Hdr
        } else if self.srgb == Some(false) {
            AstcProfile::Ldr
        } else {
            AstcProfile::LdrSrgb
        }
    }

    /// The profile for a float decode: `LdrSrgb` when tagged sRGB or
    /// untagged; `Hdr` otherwise. A linear tag cannot say whether the data is
    /// HDR, and the `Hdr` profile decodes LDR blocks identically to `Ldr`
    /// while also decoding HDR blocks (the specification permits this
    /// substitution; FFmpeg makes the same choice for linear KTX).
    pub fn default_profile_f32(&self) -> AstcProfile {
        if self.srgb == Some(false) || self.hdr == Some(true) {
            AstcProfile::Hdr
        } else {
            AstcProfile::LdrSrgb
        }
    }

    /// Decode to 8-bit RGBA (`LdrSrgb` or `Ldr`).
    pub fn decode_rgba8(&self, profile: AstcProfile) -> Result<ImageRgba8, Error> {
        let n = super::decode::texel_count_pub(self.width, self.height, self.depth)?;
        let mut pixels = vec![0u8; n];
        super::decode_rgba8_into(
            self.data,
            self.width,
            self.height,
            self.depth,
            self.footprint,
            profile,
            &mut pixels,
        )?;
        Ok(ImageRgba8 {
            width: self.width,
            height: self.height,
            depth: self.depth,
            pixels,
        })
    }

    /// Decode to `f32` RGBA (the half-float result widened exactly).
    pub fn decode_rgba_f32(&self, profile: AstcProfile) -> Result<ImageRgbaF32, Error> {
        let n = super::decode::texel_count_pub(self.width, self.height, self.depth)?;
        let mut pixels = vec![0f32; n];
        super::decode_rgba_f32_into(
            self.data,
            self.width,
            self.height,
            self.depth,
            self.footprint,
            profile,
            &mut pixels,
        )?;
        Ok(ImageRgbaF32 {
            width: self.width,
            height: self.height,
            depth: self.depth,
            pixels,
        })
    }
}

/// Payload length of a subresource, as a container size check.
pub(crate) fn surface_len(fp: AstcFootprint, w: u32, h: u32, d: u32) -> Result<usize, Error> {
    if w == 0 || h == 0 || d == 0 {
        return Err(Error::InvalidField("ASTC surface has a zero dimension".into()));
    }
    if d > 1 && !fp.is_3d() {
        // A stack of 2D slices: each slice is its own block grid.
        let one = super::payload_len(w, h, 1, fp).ok_or(Error::OutOfBounds)?;
        return one.checked_mul(d as usize).ok_or(Error::OutOfBounds);
    }
    super::payload_len(w, h, d, fp).ok_or(Error::OutOfBounds)
}

/// Bounds-checked sub-slice.
pub(crate) fn slice(data: &[u8], off: usize, len: usize) -> Result<&[u8], Error> {
    let end = off.checked_add(len).ok_or(Error::TruncatedData)?;
    data.get(off..end).ok_or(Error::TruncatedData)
}
