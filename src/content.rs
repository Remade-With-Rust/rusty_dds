//! LDR content classification shared by decode, encode, and upload plans.

use crate::error::Error;
use crate::format::{D3DFormat, DxgiFormat};
use crate::Dds;

/// Tightly packed RGBA8 image (row-major).
///
/// For volumes (`depth > 1`), slices are stacked in order `z = 0 .. depth-1`,
/// each `width * height * 4` bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRgba8 {
    pub width: u32,
    pub height: u32,
    /// Always ≥ 1. `1` for 2D / cubemap faces / array layers.
    pub depth: u32,
    pub pixels: Vec<u8>,
}

impl ImageRgba8 {
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        self.pixel3(x, y, 0)
    }

    pub fn pixel3(&self, x: u32, y: u32, z: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height || z >= self.depth {
            return None;
        }
        let slice = (self.width * self.height * 4) as usize;
        let i = z as usize * slice + ((y * self.width + x) * 4) as usize;
        Some([
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ])
    }
}

/// Public LDR format classification (decode + encode matrix).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecodeContent {
    Bc1,
    Bc2,
    Bc3,
    Bc4UNorm,
    Bc4SNorm,
    Bc5UNorm,
    Bc5SNorm,
    Bc7,
    Rgba8,
    Bgra8,
}

impl DecodeContent {
    pub const ALL_LDR: &'static [DecodeContent] = &[
        DecodeContent::Bc1,
        DecodeContent::Bc2,
        DecodeContent::Bc3,
        DecodeContent::Bc4UNorm,
        DecodeContent::Bc4SNorm,
        DecodeContent::Bc5UNorm,
        DecodeContent::Bc5SNorm,
        DecodeContent::Bc7,
        DecodeContent::Rgba8,
        DecodeContent::Bgra8,
    ];

    pub fn name(self) -> &'static str {
        match self {
            DecodeContent::Bc1 => "bc1",
            DecodeContent::Bc2 => "bc2",
            DecodeContent::Bc3 => "bc3",
            DecodeContent::Bc4UNorm => "bc4u",
            DecodeContent::Bc4SNorm => "bc4s",
            DecodeContent::Bc5UNorm => "bc5u",
            DecodeContent::Bc5SNorm => "bc5s",
            DecodeContent::Bc7 => "bc7",
            DecodeContent::Rgba8 => "rgba8",
            DecodeContent::Bgra8 => "bgra8",
        }
    }

    /// Parse CLI / docs names (`bc1`, `bc7`, `rgba8`, …).
    pub fn from_name(s: &str) -> Option<DecodeContent> {
        DecodeContent::ALL_LDR
            .iter()
            .copied()
            .find(|c| c.name().eq_ignore_ascii_case(s))
    }

    pub fn block_bytes(self) -> Option<usize> {
        match self {
            DecodeContent::Bc1 | DecodeContent::Bc4UNorm | DecodeContent::Bc4SNorm => Some(8),
            DecodeContent::Bc2
            | DecodeContent::Bc3
            | DecodeContent::Bc5UNorm
            | DecodeContent::Bc5SNorm
            | DecodeContent::Bc7 => Some(16),
            DecodeContent::Rgba8 | DecodeContent::Bgra8 => None,
        }
    }
}

impl Dds {
    /// Classify this DDS for LDR RGBA8 decode/encode, if supported.
    pub fn decode_content(&self) -> Result<DecodeContent, Error> {
        if let Some(dxgi) = self.get_dxgi_format() {
            return dxgi_decode_content(dxgi);
        }
        if let Some(d3d) = self.get_d3d_format() {
            return d3d_decode_content(d3d);
        }
        Err(Error::UnsupportedFormat)
    }
}

#[cfg_attr(
    not(any(feature = "decode", feature = "encode")),
    allow(dead_code)
)]
pub(crate) fn slice_payload_bytes(
    kind: DecodeContent,
    width: u32,
    height: u32,
) -> Result<usize, Error> {
    if width == 0 || height == 0 {
        return Err(Error::InvalidField("zero image dimension".into()));
    }
    match kind.block_bytes() {
        Some(block_bytes) => {
            let bx = (width as usize + 3) / 4;
            let by = (height as usize + 3) / 4;
            bx.checked_mul(by)
                .and_then(|n| n.checked_mul(block_bytes))
                .ok_or(Error::OutOfBounds)
        }
        None => (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or(Error::OutOfBounds),
    }
}

pub(crate) fn dxgi_decode_content(format: DxgiFormat) -> Result<DecodeContent, Error> {
    use DxgiFormat::*;
    Ok(match format {
        BC1_Typeless | BC1_UNorm | BC1_UNorm_sRGB => DecodeContent::Bc1,
        BC2_Typeless | BC2_UNorm | BC2_UNorm_sRGB => DecodeContent::Bc2,
        BC3_Typeless | BC3_UNorm | BC3_UNorm_sRGB => DecodeContent::Bc3,
        BC4_Typeless | BC4_UNorm => DecodeContent::Bc4UNorm,
        BC4_SNorm => DecodeContent::Bc4SNorm,
        BC5_Typeless | BC5_UNorm => DecodeContent::Bc5UNorm,
        BC5_SNorm => DecodeContent::Bc5SNorm,
        BC7_Typeless | BC7_UNorm | BC7_UNorm_sRGB => DecodeContent::Bc7,
        R8G8B8A8_Typeless | R8G8B8A8_UNorm | R8G8B8A8_UNorm_sRGB | R8G8B8A8_UInt => {
            DecodeContent::Rgba8
        }
        B8G8R8A8_Typeless | B8G8R8A8_UNorm | B8G8R8A8_UNorm_sRGB => DecodeContent::Bgra8,
        _ => return Err(Error::UnsupportedFormat),
    })
}

fn d3d_decode_content(format: D3DFormat) -> Result<DecodeContent, Error> {
    use D3DFormat::*;
    Ok(match format {
        DXT1 => DecodeContent::Bc1,
        DXT2 | DXT3 => DecodeContent::Bc2,
        DXT4 | DXT5 => DecodeContent::Bc3,
        _ => return Err(Error::UnsupportedFormat),
    })
}
