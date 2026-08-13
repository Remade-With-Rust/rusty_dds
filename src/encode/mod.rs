//! CPU encode: RGBA8 → DDS payload for the same LDR content set as decode.
//!
//! Round-trip gate: `decode(encode(rgba))` — bit-exact for RGBA/BGRA; PSNR /
//! max-abs for lossy BCn (see `tests/encode_matrix.rs`).

mod bc6h;
mod blocks;
mod harvest;
mod mips;

pub use blocks::EncodeQuality;

use crate::content::{slice_payload_bytes, DecodeContent, ImageRgba8};
use crate::error::Error;
use crate::format::DxgiFormat;
use crate::header::Caps2;
use crate::header10::{AlphaMode, D3D10ResourceDimension};
use crate::surface::SubresourceId;
use crate::{Dds, NewDxgiParams};

/// How to lay out encoded subresources — mirrors decode matrix contexts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodeLayout {
    pub content: DecodeContent,
    pub width: u32,
    pub height: u32,
    /// Volume depth; `1` for 2D / array / cubemap.
    pub depth: u32,
    pub mipmap_levels: u32,
    /// Non-cubemap array length, or `6 * cube_count` when [`Self::is_cubemap`].
    pub array_layers: u32,
    pub is_cubemap: bool,
    /// BC4/5 search effort. Default [`EncodeQuality::Quality`].
    pub quality: blocks::EncodeQuality,
}

impl EncodeLayout {
    pub fn flat_2d(content: DecodeContent, width: u32, height: u32) -> Self {
        Self {
            content,
            width,
            height,
            depth: 1,
            mipmap_levels: 1,
            array_layers: 1,
            is_cubemap: false,
            quality: blocks::EncodeQuality::Quality,
        }
    }

    pub fn with_mips(mut self, levels: u32) -> Self {
        self.mipmap_levels = levels.max(1);
        self
    }

    pub fn with_array(mut self, layers: u32) -> Self {
        self.array_layers = layers.max(1);
        self
    }

    pub fn with_depth(mut self, depth: u32) -> Self {
        self.depth = depth.max(1);
        self
    }

    pub fn with_quality(mut self, quality: blocks::EncodeQuality) -> Self {
        self.quality = quality;
        self
    }

    pub fn cubemap(mut self) -> Self {
        self.is_cubemap = true;
        if self.array_layers < 6 {
            self.array_layers = 6;
        }
        self
    }

    fn dxgi(self) -> DxgiFormat {
        match self.content {
            DecodeContent::Bc1 => DxgiFormat::BC1_UNorm,
            DecodeContent::Bc2 => DxgiFormat::BC2_UNorm,
            DecodeContent::Bc3 => DxgiFormat::BC3_UNorm,
            DecodeContent::Bc4UNorm => DxgiFormat::BC4_UNorm,
            DecodeContent::Bc4SNorm => DxgiFormat::BC4_SNorm,
            DecodeContent::Bc5UNorm => DxgiFormat::BC5_UNorm,
            DecodeContent::Bc5SNorm => DxgiFormat::BC5_SNorm,
            DecodeContent::Bc7 => DxgiFormat::BC7_UNorm,
            DecodeContent::Rgba8 => DxgiFormat::R8G8B8A8_UNorm,
            DecodeContent::Bgra8 => DxgiFormat::B8G8R8A8_UNorm,
        }
    }

    fn validate(&self) -> Result<(), Error> {
        if self.width == 0 || self.height == 0 || self.depth == 0 {
            return Err(Error::InvalidField("zero image dimension".into()));
        }
        if self.mipmap_levels == 0 || self.array_layers == 0 {
            return Err(Error::InvalidField("zero mip/array count".into()));
        }
        if self.is_cubemap {
            if self.array_layers % 6 != 0 {
                return Err(Error::InvalidField(
                    "cubemap array_layers must be a multiple of 6".into(),
                ));
            }
            if self.depth != 1 {
                return Err(Error::InvalidField("cubemap cannot be a volume".into()));
            }
        }
        if self.depth > 1 && self.array_layers != 1 {
            return Err(Error::InvalidField(
                "volume textures cannot also be arrays".into(),
            ));
        }
        Ok(())
    }

    /// Expected mip-0 RGBA8 byte length for this layout.
    pub fn source_rgba8_len(&self) -> Result<usize, Error> {
        self.validate()?;
        let faces_or_layers = self.array_layers;
        (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|n| n.checked_mul(self.depth as usize))
            .and_then(|n| n.checked_mul(faces_or_layers as usize))
            .and_then(|n| n.checked_mul(4))
            .ok_or(Error::OutOfBounds)
    }
}

impl Dds {
    /// Encode tightly packed RGBA8 pixels into a new DDS matching [`EncodeLayout`].
    ///
    /// Source layout (mip 0 only; extra mips are box-filtered):
    /// - 2D: `width * height * 4`
    /// - Array: layers stacked
    /// - Cubemap: faces in DirectX order, then cubes
    /// - Volume: depth slices stacked (`z` major after rows)
    pub fn encode_from_rgba8(pixels: &[u8], layout: EncodeLayout) -> Result<Dds, Error> {
        layout.validate()?;
        let need = layout.source_rgba8_len()?;
        if pixels.len() < need {
            return Err(Error::TruncatedData);
        }

        let caps2 = if layout.is_cubemap {
            Some(Caps2::CUBEMAP | Caps2::CUBEMAP_ALLFACES)
        } else {
            None
        };
        let mut dds = Dds::new_dxgi(NewDxgiParams {
            height: layout.height,
            width: layout.width,
            depth: if layout.depth > 1 {
                Some(layout.depth)
            } else {
                None
            },
            format: layout.dxgi(),
            mipmap_levels: Some(layout.mipmap_levels),
            array_layers: Some(layout.array_layers),
            caps2,
            is_cubemap: layout.is_cubemap,
            resource_dimension: if layout.depth > 1 {
                D3D10ResourceDimension::Texture3D
            } else {
                D3D10ResourceDimension::Texture2D
            },
            alpha_mode: AlphaMode::Straight,
        })?;

        let layer_pixels = (layout.width as usize)
            .saturating_mul(layout.height as usize)
            .saturating_mul(layout.depth as usize)
            .saturating_mul(4);
        let physical = dds.physical_slice_count();

        for phys in 0..physical {
            let src0 = &pixels[phys as usize * layer_pixels..(phys as usize + 1) * layer_pixels];
            let (layer, face) = if layout.is_cubemap {
                (phys / 6, phys % 6)
            } else {
                (phys, 0)
            };

            let mut mip_rgba = src0.to_vec();
            let mut mw = layout.width;
            let mut mh = layout.height;
            let mut md = layout.depth;

            for mip in 0..layout.mipmap_levels {
                let id = SubresourceId::new(mip, layer, face);
                let surf = dds.surface_mut(id)?;
                encode_surface(
                    layout.content,
                    layout.quality,
                    &mip_rgba,
                    mw,
                    mh,
                    md,
                    surf.data,
                )?;
                if mip + 1 < layout.mipmap_levels {
                    mip_rgba = mips::downsample_rgba8(&mip_rgba, mw, mh, md)?;
                    mw = (mw / 2).max(1);
                    mh = (mh / 2).max(1);
                    md = (md / 2).max(1);
                }
            }
        }

        Ok(dds)
    }
}

impl Dds {
    /// Encode tightly packed RGBA `f32` pixels (alpha ignored) as a 2D
    /// BC6H_UF16 DDS (mode 11: single subset, 10-bit endpoints, 4-bit
    /// indices). Negative / NaN inputs clamp to 0, values above the half
    /// range clamp to 65504. Round-trips through [`Dds::decode_rgba_f32`].
    pub fn encode_bc6h_uf16(pixels: &[f32], width: u32, height: u32) -> Result<Dds, Error> {
        let need = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or(Error::OutOfBounds)?;
        if width == 0 || height == 0 {
            return Err(Error::InvalidField("zero image dimension".into()));
        }
        if pixels.len() < need {
            return Err(Error::TruncatedData);
        }
        let mut dds = Dds::new_dxgi(NewDxgiParams {
            height,
            width,
            depth: None,
            format: DxgiFormat::BC6H_UF16,
            mipmap_levels: None,
            array_layers: None,
            caps2: None,
            is_cubemap: false,
            resource_dimension: D3D10ResourceDimension::Texture2D,
            alpha_mode: AlphaMode::Opaque,
        })?;
        let mut data = std::mem::take(&mut dds.data);
        bc6h::encode_slice_uf16(pixels, width, height, &mut data)?;
        dds.data = data;
        Ok(dds)
    }
}

impl ImageRgba8 {
    /// Encode this image as a single-layer 2D (or volume if `depth > 1`) DDS.
    pub fn encode_dds(&self, content: DecodeContent) -> Result<Dds, Error> {
        let layout = EncodeLayout {
            content,
            width: self.width,
            height: self.height,
            depth: self.depth,
            mipmap_levels: 1,
            array_layers: 1,
            is_cubemap: false,
            quality: blocks::EncodeQuality::Quality,
        };
        Dds::encode_from_rgba8(&self.pixels, layout)
    }
}

fn encode_surface(
    content: DecodeContent,
    quality: blocks::EncodeQuality,
    rgba: &[u8],
    width: u32,
    height: u32,
    depth: u32,
    out: &mut [u8],
) -> Result<(), Error> {
    let slice_rgba = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(Error::OutOfBounds)?;
    let slice_out = slice_payload_bytes(content, width, height)?;
    let need_rgba = slice_rgba
        .checked_mul(depth as usize)
        .ok_or(Error::OutOfBounds)?;
    let need_out = slice_out
        .checked_mul(depth as usize)
        .ok_or(Error::OutOfBounds)?;
    if rgba.len() < need_rgba || out.len() < need_out {
        return Err(Error::TruncatedData);
    }
    blocks::with_quality(quality, || {
        for z in 0..depth as usize {
            encode_slice(
                content,
                &rgba[z * slice_rgba..(z + 1) * slice_rgba],
                width,
                height,
                &mut out[z * slice_out..(z + 1) * slice_out],
            )?;
        }
        Ok(())
    })
}

fn encode_slice(
    content: DecodeContent,
    rgba: &[u8],
    width: u32,
    height: u32,
    out: &mut [u8],
) -> Result<(), Error> {
    match content {
        DecodeContent::Rgba8 => {
            let n = slice_payload_bytes(content, width, height)?;
            out[..n].copy_from_slice(&rgba[..n]);
            Ok(())
        }
        DecodeContent::Bgra8 => {
            let n = slice_payload_bytes(content, width, height)?;
            for (dst, src) in out[..n].chunks_exact_mut(4).zip(rgba[..n].chunks_exact(4)) {
                dst[0] = src[2];
                dst[1] = src[1];
                dst[2] = src[0];
                dst[3] = src[3];
            }
            Ok(())
        }
        DecodeContent::Bc1 => {
            blocks::encode_image(rgba, width, height, 8, blocks::encode_bc1, out)
        }
        DecodeContent::Bc2 => {
            blocks::encode_image(rgba, width, height, 16, blocks::encode_bc2, out)
        }
        DecodeContent::Bc3 => {
            blocks::encode_image(rgba, width, height, 16, blocks::encode_bc3, out)
        }
        DecodeContent::Bc4UNorm => encode_bc4_surface(rgba, width, height, false, out),
        DecodeContent::Bc4SNorm => encode_bc4_surface(rgba, width, height, true, out),
        DecodeContent::Bc5UNorm => encode_bc5_surface(rgba, width, height, false, out),
        DecodeContent::Bc5SNorm => encode_bc5_surface(rgba, width, height, true, out),
        DecodeContent::Bc7 => {
            blocks::encode_image(rgba, width, height, 16, blocks::encode_bc7_mode6, out)
        }
    }
}

/// Near-flat masks/normals: global channel span ≤2 → dual min/max only.
fn encode_bc4_surface(
    rgba: &[u8],
    width: u32,
    height: u32,
    signed: bool,
    out: &mut [u8],
) -> Result<(), Error> {
    let flat = blocks::channel_span(rgba, width, height, 0) <= 2;
    if flat {
        blocks::encode_image(
            rgba,
            width,
            height,
            8,
            |p, o| blocks::encode_bc4_flat(p, signed, o),
            out,
        )
    } else {
        blocks::encode_image(
            rgba,
            width,
            height,
            8,
            |p, o| blocks::encode_bc4(p, signed, o),
            out,
        )
    }
}

fn encode_bc5_surface(
    rgba: &[u8],
    width: u32,
    height: u32,
    signed: bool,
    out: &mut [u8],
) -> Result<(), Error> {
    let flat = blocks::channel_span(rgba, width, height, 0) <= 2
        && blocks::channel_span(rgba, width, height, 1) <= 2;
    if flat {
        blocks::encode_image(
            rgba,
            width,
            height,
            16,
            |p, o| blocks::encode_bc5_flat(p, signed, o),
            out,
        )
    } else {
        blocks::encode_image(
            rgba,
            width,
            height,
            16,
            |p, o| blocks::encode_bc5(p, signed, o),
            out,
        )
    }
}

/// Peak signal-to-noise ratio for two equal-length RGBA8 buffers (dB).
pub fn psnr_rgba8(a: &[u8], b: &[u8]) -> Option<f64> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let mut sse = 0.0f64;
    for (&x, &y) in a.iter().zip(b.iter()) {
        let d = x as f64 - y as f64;
        sse += d * d;
    }
    if sse == 0.0 {
        return Some(f64::INFINITY);
    }
    let mse = sse / a.len() as f64;
    Some(10.0 * (255.0f64 * 255.0 / mse).log10())
}

/// Max absolute per-byte error.
pub fn max_abs_diff(a: &[u8], b: &[u8]) -> Option<u8> {
    if a.len() != b.len() {
        return None;
    }
    Some(
        a.iter()
            .zip(b.iter())
            .map(|(&x, &y)| x.abs_diff(y))
            .max()
            .unwrap_or(0),
    )
}
