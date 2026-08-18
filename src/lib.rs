// The MIT License (MIT)
//
// Copyright (c) 2018 Michael Dilger
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
// THE SOFTWARE.

//! The main entry point for this library is the [`Dds`] type.
//!
//! # Features
//!
//! | Feature | Default | Provides |
//! |---------|---------|----------|
//! | `decode` | yes | [`Dds::decode_rgba8`], `bcdec_rs` BCn kernels |
//! | `encode` | yes | [`Dds::encode_from_rgba8`], [`EncodeLayout`] |
//!
//! Container parse/compose, surfaces, content classification, and GPU upload
//! plans are always available. Use `default-features = false, features = ["decode"]`
//! for loaders / WASM that never encode.

#![cfg_attr(docsrs, feature(doc_cfg))]
// The memory-safety claim is compiler-enforced, not asserted: without the
// `simd` feature the crate cannot contain a single `unsafe` block. With it,
// `unsafe` is confined to the `#[target_feature]` AVX2 kernels in
// `encode::blocks::simd`, each proven bit-exact against its scalar twin and
// reachable only behind a runtime CPU check.
#![cfg_attr(not(feature = "simd"), forbid(unsafe_code))]

#[macro_use]
extern crate bitflags;

mod error;
pub use error::*;

mod format;
pub use format::{D3DFormat, DataFormat, DxgiFormat, FourCC, PixelFormat, PixelFormatFlags};

mod header;
pub use header::{Caps, Caps2, Header, HeaderFlags};

mod header10;
pub use header10::{AlphaMode, D3D10ResourceDimension, Header10, MiscFlag};

mod surface;
pub use surface::{CubemapFace, SubresourceId, SurfaceView, SurfaceViewMut};

mod content;
pub use content::{DecodeContent, HdrDecodeContent, ImageRgba8, ImageRgbaF32};

#[cfg(feature = "decode")]
mod decode;
#[cfg(feature = "decode")]
pub use decode::reference;

#[cfg(feature = "encode")]
mod encode;
#[cfg(feature = "encode")]
pub use encode::{max_abs_diff, psnr_rgba8, EncodeLayout, EncodeQuality, Rdo};

mod upload;
pub use upload::{GpuFormat, UploadPath, UploadPlan};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::fmt;
use std::io::{Read, Write};

/// This is the main DirectDraw Surface file structure
#[derive(Clone)]
pub struct Dds {
    // magic is implicit
    pub header: Header,
    pub header10: Option<Header10>,
    pub data: Vec<u8>,
}

/// Parameters for Dds::new_d3d()
#[derive(Debug, Clone)]
pub struct NewD3dParams {
    pub height: u32,
    pub width: u32,
    pub depth: Option<u32>,
    pub format: D3DFormat,
    pub mipmap_levels: Option<u32>,
    pub caps2: Option<Caps2>,
}

/// Parameters for Dds::new_dxgi()
#[derive(Debug, Clone)]
pub struct NewDxgiParams {
    pub height: u32,
    pub width: u32,
    pub depth: Option<u32>,
    pub format: DxgiFormat,
    pub mipmap_levels: Option<u32>,
    pub array_layers: Option<u32>,
    pub caps2: Option<Caps2>,
    pub is_cubemap: bool,
    pub resource_dimension: D3D10ResourceDimension,
    pub alpha_mode: AlphaMode,
}

impl Dds {
    const MAGIC: u32 = 0x20534444; // b"DDS " in little endian

    /// Create a new DirectDraw Surface with a D3DFormat
    pub fn new_d3d(params: NewD3dParams) -> Result<Dds, Error> {
        let size = match get_texture_size(
            params.format.get_pitch(params.width),
            None,
            params.format.get_pitch_height(),
            params.height,
            params.depth,
        ) {
            Some(s) => s,
            None => return Err(Error::UnsupportedFormat),
        };

        let mml = params.mipmap_levels.unwrap_or(1);
        let min_mipmap_size = match params.format.get_minimum_mipmap_size_in_bytes() {
            Some(mms) => mms,
            None => return Err(Error::UnsupportedFormat),
        };
        let array_stride = get_array_stride(size, min_mipmap_size, mml);

        let data_size = array_stride;

        Ok(Dds {
            header: Header::new_d3d(
                params.height,
                params.width,
                params.depth,
                params.format,
                params.mipmap_levels,
                params.caps2,
            )?,
            header10: None,
            data: vec![0; data_size as usize],
        })
    }

    /// Create a new DirectDraw Surface with a DxgiFormat
    pub fn new_dxgi(params: NewDxgiParams) -> Result<Dds, Error> {
        let arraysize = params.array_layers.unwrap_or(1);

        let size = match get_texture_size(
            params.format.get_pitch(params.width),
            None,
            params.format.get_pitch_height(),
            params.height,
            params.depth,
        ) {
            Some(s) => s,
            None => return Err(Error::UnsupportedFormat),
        };

        let mml = params.mipmap_levels.unwrap_or(1);
        let min_mipmap_size = match params.format.get_minimum_mipmap_size_in_bytes() {
            Some(mms) => mms,
            None => return Err(Error::UnsupportedFormat),
        };
        let array_stride = get_array_stride(size, min_mipmap_size, mml);

        let data_size = arraysize
            .checked_mul(array_stride)
            .ok_or(Error::OutOfBounds)?;

        let arraysize = if params.is_cubemap {
            arraysize / 6
        } else {
            arraysize
        };
        let header10 = Header10::new(
            params.format,
            params.is_cubemap,
            params.resource_dimension,
            arraysize,
            params.alpha_mode,
        );

        Ok(Dds {
            header: Header::new_dxgi(
                params.height,
                params.width,
                params.depth,
                params.format,
                params.mipmap_levels,
                params.array_layers,
                params.caps2,
            )?,
            header10: Some(header10),
            data: vec![0; data_size as usize],
        })
    }

    /// Read a DDS file, accepting a payload of any length.
    ///
    /// The payload is read to end-of-stream with no cap, so the peak allocation
    /// is whatever the reader yields. That is the right behaviour for a trusted
    /// file on disk and the wrong one for bytes arriving from a network, a
    /// user upload, or a mod archive — for those, use [`Dds::read_limited`],
    /// which fails closed at a byte budget you choose.
    pub fn read<R: Read>(r: R) -> Result<Dds, Error> {
        Self::read_inner(r, None)
    }

    /// Read a DDS file, refusing a payload larger than `max_data_len` bytes.
    ///
    /// The limit covers the **payload only** — the 128-byte header (148 with a
    /// DX10 header) is read first and is not counted. Exceeding it returns
    /// [`Error::SizeLimitExceeded`] without buffering the overrun, so a hostile
    /// or corrupt stream cannot force an unbounded allocation.
    ///
    /// ```
    /// use rusty_dds::{Dds, Error};
    ///
    /// let mut bytes = Vec::new();
    /// Dds::new_dxgi(rusty_dds::NewDxgiParams {
    ///     height: 64, width: 64, depth: None,
    ///     format: rusty_dds::DxgiFormat::BC1_UNorm,
    ///     mipmap_levels: None, array_layers: None, caps2: None, is_cubemap: false,
    ///     resource_dimension: rusty_dds::D3D10ResourceDimension::Texture2D,
    ///     alpha_mode: rusty_dds::AlphaMode::Straight,
    /// })?.write(&mut bytes)?;
    ///
    /// assert!(Dds::read_limited(&bytes[..], 8 * 1024).is_ok());
    /// assert!(matches!(
    ///     Dds::read_limited(&bytes[..], 16),
    ///     Err(Error::SizeLimitExceeded { .. })
    /// ));
    /// # Ok::<(), Error>(())
    /// ```
    pub fn read_limited<R: Read>(r: R, max_data_len: usize) -> Result<Dds, Error> {
        Self::read_inner(r, Some(max_data_len))
    }

    fn read_inner<R: Read>(mut r: R, max_data_len: Option<usize>) -> Result<Dds, Error> {
        let magic = r.read_u32::<LittleEndian>()?;
        if magic != Self::MAGIC {
            return Err(Error::BadMagicNumber);
        }

        let header = Header::read(&mut r)?;

        let header10 = if header.spf.fourcc == Some(FourCC(<FourCC>::DX10)) {
            Some(Header10::read(&mut r)?)
        } else {
            None
        };

        let mut data: Vec<u8> = Vec::new();
        match max_data_len {
            None => {
                r.read_to_end(&mut data)?;
            }
            Some(limit) => {
                // Read one byte past the budget: if the reader still had bytes
                // to give, the payload is over the limit and we stop there
                // rather than buffering the rest.
                let mut capped = r.take(limit as u64 + 1);
                capped.read_to_end(&mut data)?;
                if data.len() > limit {
                    return Err(Error::SizeLimitExceeded {
                        limit,
                        // Only ever `limit + 1` here — the true length is
                        // unknown by construction, and that is the point.
                        at_least: data.len(),
                    });
                }
            }
        }
        Ok(Dds {
            header,
            header10,
            data,
        })
    }

    /// Write to a DDS file
    pub fn write<W: Write>(&self, w: &mut W) -> Result<(), Error> {
        w.write_u32::<LittleEndian>(Self::MAGIC)?;
        self.header.write(w)?;
        if let Some(ref header10) = self.header10 {
            header10.write(w)?;
        }
        w.write_all(&self.data)?;
        Ok(())
    }

    /// Attempt to get the format of this DDS, presuming it is a D3DFormat.
    pub fn get_d3d_format(&self) -> Option<D3DFormat> {
        // FIXME: some d3d formats are equivalent to some dxgi formats.
        //    but we dont have a try_from() between them yet.
        //    Right now we will yield None if the format is dxgi, but
        //    later on we should try to convert.

        D3DFormat::try_from_pixel_format(&self.header.spf)
    }

    /// Attempt to get the format of this DDS, presuming it is a DxgiFormat.
    pub fn get_dxgi_format(&self) -> Option<DxgiFormat> {
        // FIXME: some d3d formats are equivalent to some dxgi formats.
        //    but we dont have a try_from() between them yet.
        //    Right now we will yield None if the format is d3d, but
        //    later on we should try to convert.
        if let Some(ref h10) = self.header10 {
            Some(h10.dxgi_format)
        } else {
            DxgiFormat::try_from_pixel_format(&self.header.spf)
        }
    }

    /// Get the format of the DDS as a trait (type-erasure)
    pub fn get_format(&self) -> Option<Box<dyn DataFormat>> {
        if let Some(dxgi) = self.get_dxgi_format() {
            Some(Box::new(dxgi))
        } else if let Some(d3d) = self.get_d3d_format() {
            Some(Box::new(d3d))
        } else {
            None
        }
    }

    pub fn get_width(&self) -> u32 {
        self.header.width
    }

    pub fn get_height(&self) -> u32 {
        self.header.height
    }

    pub fn get_depth(&self) -> u32 {
        self.header.depth.unwrap_or(1)
    }

    pub fn get_bits_per_pixel(&self) -> Option<u32> {
        // Try format first
        if let Some(format) = self.get_format() {
            if let Some(bpp) = format.get_bits_per_pixel() {
                return Some(bpp as u32);
            }
        }
        // Fall back to pixel_format rgb_bit_count field
        if let Some(bpp) = self.header.spf.rgb_bit_count {
            return Some(bpp);
        }
        None
    }

    pub fn get_pitch(&self) -> Option<u32> {
        // Try format first
        if let Some(format) = self.get_format() {
            if let Some(pitch) = format.get_pitch(self.header.width) {
                return Some(pitch);
            }
        }
        // Then try header.pitch
        if let Some(pitch) = self.header.pitch {
            return Some(pitch);
        }

        // Then try to calculate it ourselves
        if let Some(bpp) = self.get_bits_per_pixel() {
            // Both operands come from the file; a header that overflows here is
            // not describing a pitch we can honour.
            return bpp
                .checked_mul(self.get_width())
                .and_then(|n| n.checked_add(7))
                .map(|n| n / 8);
        }
        None
    }

    pub fn get_pitch_height(&self) -> u32 {
        if let Some(format) = self.get_format() {
            format.get_pitch_height()
        } else {
            1
        }
    }

    pub fn get_main_texture_size(&self) -> Option<u32> {
        get_texture_size(
            self.get_pitch(),
            self.header.linear_size,
            self.get_pitch_height(),
            self.header.height,
            self.header.depth,
        )
    }

    pub fn get_array_stride(&self) -> Result<u32, Error> {
        let size = match self.get_main_texture_size() {
            Some(s) => s,
            None => return Err(Error::UnsupportedFormat),
        };
        let mml = self.get_num_mipmap_levels();
        let min_mipmap_size = self.get_min_mipmap_size_in_bytes();
        Ok(get_array_stride(size, min_mipmap_size, mml))
    }

    pub fn get_num_array_layers(&self) -> u32 {
        if let Some(ref h10) = self.header10 {
            h10.array_size
        } else if self.header.caps2.contains(Caps2::CUBEMAP) {
            6
        } else {
            1 // just the 1 layer
        }
    }

    pub fn get_num_mipmap_levels(&self) -> u32 {
        if let Some(mmc) = self.header.mip_map_count {
            mmc
        } else {
            1 // just the main image
        }
    }

    pub fn get_min_mipmap_size_in_bytes(&self) -> u32 {
        if let Some(format) = self.get_format() {
            if let Some(min) = format.get_minimum_mipmap_size_in_bytes() {
                return min;
            }
        }
        if let Some(bpp) = self.get_bits_per_pixel() {
            // `bpp` can be the raw `rgb_bit_count` header field, so it is not
            // bounded by any real format; saturate rather than overflow.
            bpp.saturating_add(7) / 8
        } else {
            1
        }
    }

    /// This gets a reference to the data at the given `array_layer` (which should be
    /// 0 for textures with just one image).
    pub fn get_data(&self, array_layer: u32) -> Result<&[u8], Error> {
        let (offset, size) = self.get_offset_and_size(array_layer)?;
        let offset = offset as usize;
        let size = size as usize;
        let end = offset.checked_add(size).ok_or(Error::OutOfBounds)?;
        self.data.get(offset..end).ok_or(Error::OutOfBounds)
    }

    /// This gets a reference to the data at the given `array_layer` (which should be
    /// 0 for textures with just one image).
    pub fn get_mut_data(&mut self, array_layer: u32) -> Result<&mut [u8], Error> {
        let (offset, size) = self.get_offset_and_size(array_layer)?;
        let offset = offset as usize;
        let size = size as usize;
        let end = offset.checked_add(size).ok_or(Error::OutOfBounds)?;
        self.data.get_mut(offset..end).ok_or(Error::OutOfBounds)
    }

    fn get_offset_and_size(&self, array_layer: u32) -> Result<(u32, u32), Error> {
        // Verify request bounds
        if array_layer >= self.get_num_array_layers() {
            return Err(Error::OutOfBounds);
        }
        let array_stride = self.get_array_stride()?;
        let offset = array_layer
            .checked_mul(array_stride)
            .ok_or(Error::OutOfBounds)?;

        Ok((offset, array_stride))
    }
}

/// Bytes for one mip-0 surface, or `None` when the header's own fields cannot
/// describe one.
///
/// Every input here is an attacker-controlled `u32` straight out of the file,
/// so the arithmetic is checked throughout: an overflow means the header is
/// describing a texture that cannot exist, and the honest answer is `None`
/// (which callers turn into [`Error::UnsupportedFormat`]), not a wrapped size
/// that would then be used to slice the payload.
fn get_texture_size(
    pitch: Option<u32>,
    linear_size: Option<u32>,
    pitch_height: u32,
    height: u32,
    depth: Option<u32>,
) -> Option<u32> {
    let depth = depth.unwrap_or(1);

    if let Some(ls) = linear_size {
        return Some(ls);
    }
    let pitch = pitch?;
    // A zero pitch height would divide by zero; a format that reports one is
    // not describing a layout we can compute.
    if pitch_height == 0 {
        return None;
    }
    let row_height = height.checked_add(pitch_height - 1)? / pitch_height;
    pitch.checked_mul(row_height)?.checked_mul(depth)
}

/// Total bytes of one mip chain.
///
/// `mipmap_levels` comes from the file, so it can be up to `u32::MAX`. Once the
/// mip size has bottomed out at `min_mipmap_size` every remaining level
/// contributes exactly that much, so the tail is computed in closed form rather
/// than iterated — otherwise a header claiming `mip_map_count = 0xFFFF_FFFF`
/// would spin for billions of iterations on every metadata query. Accumulation
/// saturates for the same reason `get_texture_size` is checked: a wrapped
/// stride would be used to index the payload.
fn get_array_stride(texture_size: u32, min_mipmap_size: u32, mipmap_levels: u32) -> u32 {
    let mut stride: u32 = 0;
    let mut current_mipsize: u32 = texture_size;
    let mut level: u32 = 0;
    while level < mipmap_levels {
        stride = stride.saturating_add(current_mipsize);
        level += 1;
        current_mipsize /= 4;
        if current_mipsize <= min_mipmap_size {
            let remaining = mipmap_levels - level;
            return stride.saturating_add(remaining.saturating_mul(min_mipmap_size));
        }
    }
    stride
}

impl fmt::Debug for Dds {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f, "Dds:")?;
        if let Some(d3dformat) = self.get_d3d_format() {
            writeln!(f, "  Format: {:?}", d3dformat)?;
        } else if let Some(dxgiformat) = self.get_dxgi_format() {
            writeln!(f, "  Format: {:?}", dxgiformat)?;
        } else if let Some(ref fourcc) = self.header.spf.fourcc {
            writeln!(f, "  Format: FOURCC={:?} (Unknown)", fourcc)?;
        } else {
            writeln!(f, "  Format UNSPECIFIED")?;
        }
        write!(f, "{:?}", self.header)?;
        if let Some(ref h10) = self.header10 {
            write!(f, "{:?}", h10)?;
        }
        writeln!(f, "  (data elided)")?;
        Ok(())
    }
}
