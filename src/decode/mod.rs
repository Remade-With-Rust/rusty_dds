//! CPU decode of DDS subresources to tightly packed RGBA8.
//!
//! **sRGB policy:** channel bytes are returned as stored. Formats tagged `_sRGB`
//! are **not** converted to linear — callers that need linear light must convert.
//!
//! Block compression (BC1–BC5, BC7) uses the pure-Rust [`bcdec_rs`] crate (MIT),
//! the same decompression core as `image_dds`. Uncompressed layouts are handled
//! in-house.

mod bc6h;
mod bcn;
mod uncompressed;
pub mod reference;

use crate::content::{
    slice_payload_bytes, DecodeContent, HdrDecodeContent, ImageRgba8, ImageRgbaF32,
};
use crate::error::Error;
use crate::surface::SubresourceId;
use crate::Dds;

impl Dds {
    /// Decode one subresource to tightly packed RGBA8.
    ///
    /// Volumes (`depth > 1`) decode every depth slice and stack them in
    /// [`ImageRgba8::pixels`]. sRGB-tagged formats return stored bytes without
    /// linearization.
    pub fn decode_rgba8(&self, id: SubresourceId) -> Result<ImageRgba8, Error> {
        let surf = self.surface(id)?;
        let kind = self.decode_content()?;
        let pixels = decode_surface_pixels(kind, surf.data, surf.width, surf.height, surf.depth)?;
        Ok(ImageRgba8 {
            width: surf.width,
            height: surf.height,
            depth: surf.depth,
            pixels,
        })
    }
}

impl Dds {
    /// Decode one HDR subresource (BC6H) to tightly packed RGBA `f32`
    /// (`A = 1.0`). Volumes decode every depth slice, stacked. LDR content
    /// stays on [`Dds::decode_rgba8`]; each API fails closed on the other's
    /// formats.
    pub fn decode_rgba_f32(&self, id: SubresourceId) -> Result<ImageRgbaF32, Error> {
        let kind = self.hdr_decode_content()?;
        let signed = kind == HdrDecodeContent::Bc6hSf16;
        let surf = self.surface(id)?;
        let slice_bytes = {
            let bx = (surf.width as usize + 3) / 4;
            let by = (surf.height as usize + 3) / 4;
            bx.checked_mul(by)
                .and_then(|n| n.checked_mul(16))
                .ok_or(Error::OutOfBounds)?
        };
        let need = slice_bytes
            .checked_mul(surf.depth as usize)
            .ok_or(Error::OutOfBounds)?;
        if surf.data.len() < need {
            return Err(Error::TruncatedData);
        }
        let mut pixels = Vec::with_capacity(
            (surf.width as usize)
                .saturating_mul(surf.height as usize)
                .saturating_mul(surf.depth as usize)
                .saturating_mul(4),
        );
        for z in 0..surf.depth as usize {
            let start = z * slice_bytes;
            pixels.extend(bc6h::decode_bc6h(
                &surf.data[start..start + slice_bytes],
                surf.width,
                surf.height,
                signed,
            )?);
        }
        Ok(ImageRgbaF32 {
            width: surf.width,
            height: surf.height,
            depth: surf.depth,
            pixels,
        })
    }
}

pub(crate) fn decode_surface_pixels(
    kind: DecodeContent,
    data: &[u8],
    width: u32,
    height: u32,
    depth: u32,
) -> Result<Vec<u8>, Error> {
    if depth == 0 {
        return Err(Error::InvalidField("zero depth".into()));
    }
    if depth == 1 {
        return decode_slice(kind, data, width, height);
    }
    let slice_bytes = slice_payload_bytes(kind, width, height)?;
    let need = slice_bytes
        .checked_mul(depth as usize)
        .ok_or(Error::OutOfBounds)?;
    if data.len() < need {
        return Err(Error::TruncatedData);
    }
    let mut out = Vec::with_capacity(
        (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(depth as usize)
            .saturating_mul(4),
    );
    for z in 0..depth as usize {
        let start = z * slice_bytes;
        out.extend(decode_slice(
            kind,
            &data[start..start + slice_bytes],
            width,
            height,
        )?);
    }
    Ok(out)
}

fn decode_slice(
    kind: DecodeContent,
    data: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<u8>, Error> {
    match kind {
        DecodeContent::Bc1 => bcn::decode_bc1(data, width, height),
        DecodeContent::Bc2 => bcn::decode_bc2(data, width, height),
        DecodeContent::Bc3 => bcn::decode_bc3(data, width, height),
        DecodeContent::Bc4UNorm => bcn::decode_bc4(data, width, height, false),
        DecodeContent::Bc4SNorm => bcn::decode_bc4(data, width, height, true),
        DecodeContent::Bc5UNorm => bcn::decode_bc5(data, width, height, false),
        DecodeContent::Bc5SNorm => bcn::decode_bc5(data, width, height, true),
        DecodeContent::Bc7 => bcn::decode_bc7(data, width, height),
        DecodeContent::Rgba8 => uncompressed::decode_rgba8(data, width, height),
        DecodeContent::Bgra8 => uncompressed::decode_bgra8(data, width, height),
    }
}
