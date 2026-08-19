//! API-agnostic GPU upload planning (no wgpu / ash / windows deps).
//!
//! Path A — **compressed**: pass DDS block bytes with BCn row pitches.
//! Path B — **decoded**: tightly packed RGBA8 after [`crate::Dds::decode_rgba8`].
//!
//! Pitches follow the same rules as wgpu `ImageDataLayout` /
//! `vkCmdCopyBufferToImage` for tightly packed (or block-row) uploads.

use crate::error::Error;
use crate::format::{D3DFormat, DxgiFormat};
use crate::surface::SubresourceId;
use crate::DdsBase;

/// Recommended GPU texture format name (stringly typed — no graphics API crate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct GpuFormat {
    /// DXGI enum name, e.g. `"BC7_UNorm_sRGB"`.
    pub dxgi_name: &'static str,
    /// Typical wgpu `TextureFormat` discriminant name, e.g. `"Bc7RgbaUnormSrgb"`.
    pub wgpu_name: &'static str,
    /// Vulkan `VkFormat` enumerator name, e.g. `"VK_FORMAT_BC7_SRGB_BLOCK"`.
    pub vulkan_name: &'static str,
    /// Bytes per block (compressed) or per pixel (uncompressed).
    pub block_bytes: u32,
    /// Block width in texels (`4` for BCn, `1` for RGBA8).
    pub block_width: u32,
    /// Block height in texels.
    pub block_height: u32,
    pub compressed: bool,
}

/// How to feed one subresource to a GPU copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UploadPath {
    /// Keep DDS payload bytes; GPU samples BCn / native format.
    Compressed,
    /// CPU-decoded tightly packed RGBA8 (`Rgba8Unorm` / `VK_FORMAT_R8G8B8A8_UNORM`).
    DecodedRgba8,
}

/// One mip/layer/face ready for `write_texture` / buffer→image copy.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UploadPlan {
    pub id: SubresourceId,
    pub path: UploadPath,
    pub format: GpuFormat,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    /// Byte offset into [`Dds::data`] for [`UploadPath::Compressed`].
    /// For [`UploadPath::DecodedRgba8`], always `0` (buffer is the decode output).
    pub data_offset: usize,
    /// Byte length of the upload region.
    pub data_len: usize,
    /// `bytes_per_row` / `VkBufferImageCopy.bufferRowLength` pitch in **bytes**.
    /// BCn: bytes for one row of blocks. RGBA8: `width * 4`.
    pub bytes_per_row: u32,
    /// Rows per depth slice: block rows for BCn, texel rows for RGBA8.
    pub rows_per_image: u32,
}

impl<D: AsRef<[u8]>> DdsBase<D> {
    /// Map this DDS to a [`GpuFormat`] for compressed GPU upload, if supported.
    pub fn gpu_format(&self) -> Result<GpuFormat, Error> {
        if let Some(dxgi) = self.get_dxgi_format() {
            return gpu_format_from_dxgi(dxgi);
        }
        if let Some(d3d) = self.get_d3d_format() {
            return gpu_format_from_d3d(d3d);
        }
        Err(Error::UnsupportedFormat)
    }

    /// Plan a **compressed** upload of one subresource (Path A).
    pub fn upload_plan_compressed(&self, id: SubresourceId) -> Result<UploadPlan, Error> {
        let format = self.gpu_format()?;
        // `surface()` is `subresource_range()` + `mip_dimensions()`, so calling
        // both walked the mip chain twice for one query. `mip_dimensions` is a
        // shift; the range walk is the expensive half, and once is enough.
        let range = self.subresource_range(id)?;
        let (width, height, depth) = self.mip_dimensions(id.mip)?;
        let (bytes_per_row, rows_per_image) = compressed_pitches(format, width, height)?;
        Ok(UploadPlan {
            id,
            path: UploadPath::Compressed,
            format,
            width,
            height,
            depth,
            data_offset: range.start,
            data_len: range.end - range.start,
            bytes_per_row,
            rows_per_image,
        })
    }

    /// Plan a **decoded RGBA8** upload (Path B).
    ///
    /// `data_offset` is `0`; `data_len` is `width * height * depth * 4`.
    /// Call [`Self::decode_rgba8`] for the bytes. Format is always RGBA8 UNORM
    /// (stored sRGB bytes are not linearized — same policy as decode).
    pub fn upload_plan_decoded_rgba8(&self, id: SubresourceId) -> Result<UploadPlan, Error> {
        // Ensure we can decode this content (even if gpu_format fails for exotic forms).
        let _ = self.decode_content()?;
        let surf = self.surface(id)?;
        let pixels = (surf.width as usize)
            .checked_mul(surf.height as usize)
            .and_then(|n| n.checked_mul(surf.depth as usize))
            .and_then(|n| n.checked_mul(4))
            .ok_or(Error::OutOfBounds)?;
        Ok(UploadPlan {
            id,
            path: UploadPath::DecodedRgba8,
            format: GPU_RGBA8_UNORM,
            width: surf.width,
            height: surf.height,
            depth: surf.depth,
            data_offset: 0,
            data_len: pixels,
            bytes_per_row: surf
                .width
                .checked_mul(4)
                .ok_or(Error::OutOfBounds)?,
            rows_per_image: surf.height,
        })
    }
}

fn compressed_pitches(format: GpuFormat, width: u32, height: u32) -> Result<(u32, u32), Error> {
    if width == 0 || height == 0 {
        return Err(Error::InvalidField("zero image dimension".into()));
    }
    let blocks_x = width.div_ceil(format.block_width);
    let blocks_y = height.div_ceil(format.block_height);
    let bytes_per_row = blocks_x
        .checked_mul(format.block_bytes)
        .ok_or(Error::OutOfBounds)?;
    Ok((bytes_per_row, blocks_y))
}

const GPU_RGBA8_UNORM: GpuFormat = GpuFormat {
    dxgi_name: "R8G8B8A8_UNorm",
    wgpu_name: "Rgba8Unorm",
    vulkan_name: "VK_FORMAT_R8G8B8A8_UNORM",
    block_bytes: 4,
    block_width: 1,
    block_height: 1,
    compressed: false,
};

fn bc(dxgi: &'static str, wgpu: &'static str, vk: &'static str, block_bytes: u32) -> GpuFormat {
    GpuFormat {
        dxgi_name: dxgi,
        wgpu_name: wgpu,
        vulkan_name: vk,
        block_bytes,
        block_width: 4,
        block_height: 4,
        compressed: true,
    }
}

fn gpu_format_from_dxgi(format: DxgiFormat) -> Result<GpuFormat, Error> {
    use DxgiFormat::*;
    Ok(match format {
        BC1_Typeless | BC1_UNorm => bc(
            "BC1_UNorm",
            "Bc1RgbaUnorm",
            "VK_FORMAT_BC1_RGBA_UNORM_BLOCK",
            8,
        ),
        BC1_UNorm_sRGB => bc(
            "BC1_UNorm_sRGB",
            "Bc1RgbaUnormSrgb",
            "VK_FORMAT_BC1_RGBA_SRGB_BLOCK",
            8,
        ),
        BC2_Typeless | BC2_UNorm => bc(
            "BC2_UNorm",
            "Bc2RgbaUnorm",
            "VK_FORMAT_BC2_UNORM_BLOCK",
            16,
        ),
        BC2_UNorm_sRGB => bc(
            "BC2_UNorm_sRGB",
            "Bc2RgbaUnormSrgb",
            "VK_FORMAT_BC2_SRGB_BLOCK",
            16,
        ),
        BC3_Typeless | BC3_UNorm => bc(
            "BC3_UNorm",
            "Bc3RgbaUnorm",
            "VK_FORMAT_BC3_UNORM_BLOCK",
            16,
        ),
        BC3_UNorm_sRGB => bc(
            "BC3_UNorm_sRGB",
            "Bc3RgbaUnormSrgb",
            "VK_FORMAT_BC3_SRGB_BLOCK",
            16,
        ),
        BC4_Typeless | BC4_UNorm => {
            bc("BC4_UNorm", "Bc4RUnorm", "VK_FORMAT_BC4_UNORM_BLOCK", 8)
        }
        BC4_SNorm => bc("BC4_SNorm", "Bc4RSnorm", "VK_FORMAT_BC4_SNORM_BLOCK", 8),
        BC5_Typeless | BC5_UNorm => {
            bc("BC5_UNorm", "Bc5RgUnorm", "VK_FORMAT_BC5_UNORM_BLOCK", 16)
        }
        BC5_SNorm => bc("BC5_SNorm", "Bc5RgSnorm", "VK_FORMAT_BC5_SNORM_BLOCK", 16),
        // BC6H was missing from this table entirely, which meant a format this
        // crate can both decode and encode could not be handed to a GPU at all.
        // Every consumer of `gpu_format` — the upload planner included — failed
        // closed on HDR content with `UnsupportedFormat`.
        BC6H_Typeless | BC6H_UF16 => bc(
            "BC6H_UF16",
            "Bc6hRgbUfloat",
            "VK_FORMAT_BC6H_UFLOAT_BLOCK",
            16,
        ),
        BC6H_SF16 => bc(
            "BC6H_SF16",
            "Bc6hRgbFloat",
            "VK_FORMAT_BC6H_SFLOAT_BLOCK",
            16,
        ),
        BC7_Typeless | BC7_UNorm => bc(
            "BC7_UNorm",
            "Bc7RgbaUnorm",
            "VK_FORMAT_BC7_UNORM_BLOCK",
            16,
        ),
        BC7_UNorm_sRGB => bc(
            "BC7_UNorm_sRGB",
            "Bc7RgbaUnormSrgb",
            "VK_FORMAT_BC7_SRGB_BLOCK",
            16,
        ),
        R8G8B8A8_Typeless | R8G8B8A8_UNorm | R8G8B8A8_UInt => GPU_RGBA8_UNORM,
        R8G8B8A8_UNorm_sRGB => GpuFormat {
            dxgi_name: "R8G8B8A8_UNorm_sRGB",
            wgpu_name: "Rgba8UnormSrgb",
            vulkan_name: "VK_FORMAT_R8G8B8A8_SRGB",
            block_bytes: 4,
            block_width: 1,
            block_height: 1,
            compressed: false,
        },
        B8G8R8A8_Typeless | B8G8R8A8_UNorm => GpuFormat {
            dxgi_name: "B8G8R8A8_UNorm",
            wgpu_name: "Bgra8Unorm",
            vulkan_name: "VK_FORMAT_B8G8R8A8_UNORM",
            block_bytes: 4,
            block_width: 1,
            block_height: 1,
            compressed: false,
        },
        B8G8R8A8_UNorm_sRGB => GpuFormat {
            dxgi_name: "B8G8R8A8_UNorm_sRGB",
            wgpu_name: "Bgra8UnormSrgb",
            vulkan_name: "VK_FORMAT_B8G8R8A8_SRGB",
            block_bytes: 4,
            block_width: 1,
            block_height: 1,
            compressed: false,
        },
        _ => return Err(Error::UnsupportedFormat),
    })
}

fn gpu_format_from_d3d(format: D3DFormat) -> Result<GpuFormat, Error> {
    use D3DFormat::*;
    Ok(match format {
        DXT1 => bc(
            "BC1_UNorm",
            "Bc1RgbaUnorm",
            "VK_FORMAT_BC1_RGBA_UNORM_BLOCK",
            8,
        ),
        DXT2 | DXT3 => bc(
            "BC2_UNorm",
            "Bc2RgbaUnorm",
            "VK_FORMAT_BC2_UNORM_BLOCK",
            16,
        ),
        DXT4 | DXT5 => bc(
            "BC3_UNorm",
            "Bc3RgbaUnorm",
            "VK_FORMAT_BC3_UNORM_BLOCK",
            16,
        ),
        _ => return Err(Error::UnsupportedFormat),
    })
}
