// The MIT License (MIT)
//
// Copyright (c) 2018 Michael Dilger
// Copyright (c) 2026 Remade With Rust / Mata Network
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

//! Typed views into a DDS payload: mip / array layer / cubemap face.

use crate::error::Error;
use crate::header::Caps2;
use crate::header10::MiscFlag;
use crate::DdsBase;
use std::ops::Range;

/// DirectX cubemap face order (face index 0..5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum CubemapFace {
    PositiveX = 0,
    NegativeX = 1,
    PositiveY = 2,
    NegativeY = 3,
    PositiveZ = 4,
    NegativeZ = 5,
}

impl CubemapFace {
    pub const ALL: [CubemapFace; 6] = [
        CubemapFace::PositiveX,
        CubemapFace::NegativeX,
        CubemapFace::PositiveY,
        CubemapFace::NegativeY,
        CubemapFace::PositiveZ,
        CubemapFace::NegativeZ,
    ];

    pub fn from_index(index: u32) -> Result<CubemapFace, Error> {
        match index {
            0 => Ok(CubemapFace::PositiveX),
            1 => Ok(CubemapFace::NegativeX),
            2 => Ok(CubemapFace::PositiveY),
            3 => Ok(CubemapFace::NegativeY),
            4 => Ok(CubemapFace::PositiveZ),
            5 => Ok(CubemapFace::NegativeZ),
            _ => Err(Error::OutOfBounds),
        }
    }

    pub fn index(self) -> u32 {
        self as u32
    }
}

/// Identifies one mip / layer / face subresource inside [`DdsBase::data`](crate::DdsBase::data).
///
/// - `mip` — mip level (0 = largest)
/// - `layer` — array layer, or cube index for cubemaps
/// - `face` — cubemap face 0..5 ([`CubemapFace`]); must be `0` for non-cubemaps
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SubresourceId {
    pub mip: u32,
    pub layer: u32,
    pub face: u32,
}

impl SubresourceId {
    pub const fn new(mip: u32, layer: u32, face: u32) -> Self {
        Self { mip, layer, face }
    }

    pub const fn mip_layer(mip: u32, layer: u32) -> Self {
        Self {
            mip,
            layer,
            face: 0,
        }
    }

    pub const fn cubemap(mip: u32, layer: u32, face: CubemapFace) -> Self {
        Self {
            mip,
            layer,
            face: face as u32,
        }
    }
}

/// Borrowed view of one subresource's bytes and mip dimensions.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct SurfaceView<'a> {
    pub id: SubresourceId,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub data: &'a [u8],
}

impl<D: AsRef<[u8]>> DdsBase<D> {
    /// True when this DDS is a cubemap (DX10 `TEXTURECUBE` or legacy `Caps2::CUBEMAP`).
    pub fn is_cubemap(&self) -> bool {
        if let Some(ref h10) = self.header10 {
            h10.misc_flag.contains(MiscFlag::TEXTURECUBE)
        } else {
            self.header.caps2.contains(Caps2::CUBEMAP)
        }
    }

    /// Number of selectable `SubresourceId::layer` values.
    ///
    /// Cubemap: cube count. Non-cubemap: array layer count (or `1`).
    pub fn subresource_layer_count(&self) -> u32 {
        if self.is_cubemap() {
            self.cube_count()
        } else if let Some(ref h10) = self.header10 {
            h10.array_size.max(1)
        } else {
            1
        }
    }

    /// Cubemap face count (`6`) or `1` for non-cubemaps.
    pub fn subresource_face_count(&self) -> u32 {
        if self.is_cubemap() {
            6
        } else {
            1
        }
    }

    /// Number of cube maps stored (DX10 `array_size`, or `1` for legacy cubemaps).
    pub fn cube_count(&self) -> u32 {
        if !self.is_cubemap() {
            return 0;
        }
        if let Some(ref h10) = self.header10 {
            h10.array_size.max(1)
        } else {
            1
        }
    }

    /// Total physical mip-chains in `data` (array layers, or cubes × 6 faces).
    pub fn physical_slice_count(&self) -> u32 {
        if self.is_cubemap() {
            self.cube_count().saturating_mul(6)
        } else if let Some(ref h10) = self.header10 {
            h10.array_size.max(1)
        } else {
            1
        }
    }

    /// Width / height / depth of a mip level (each at least 1).
    pub fn mip_dimensions(&self, mip: u32) -> Result<(u32, u32, u32), Error> {
        if mip >= self.get_num_mipmap_levels() {
            return Err(Error::OutOfBounds);
        }
        let width = (self.header.width >> mip).max(1);
        let height = (self.header.height >> mip).max(1);
        let depth = (self.header.depth.unwrap_or(1) >> mip).max(1);
        Ok((width, height, depth))
    }

    /// Byte range of one subresource inside [`DdsBase::data`](crate::DdsBase::data).
    pub fn subresource_range(&self, id: SubresourceId) -> Result<Range<usize>, Error> {
        let (offset, size) = self.subresource_offset_and_size(id)?;
        let start = offset as usize;
        let end = start
            .checked_add(size as usize)
            .ok_or(Error::OutOfBounds)?;
        if end > self.data.as_ref().len() {
            return Err(Error::TruncatedData);
        }
        Ok(start..end)
    }

    /// Borrowed view of one subresource.
    pub fn surface(&self, id: SubresourceId) -> Result<SurfaceView<'_>, Error> {
        let range = self.subresource_range(id)?;
        let (width, height, depth) = self.mip_dimensions(id.mip)?;
        Ok(SurfaceView {
            id,
            width,
            height,
            depth,
            data: &self.data.as_ref()[range],
        })
    }

    fn subresource_offset_and_size(&self, id: SubresourceId) -> Result<(u32, u32), Error> {
        self.validate_subresource_id(id)?;

        let physical = self.physical_slice_index(id)?;
        let array_stride = self.get_array_stride()?;
        let (mip_offset, mip_size) = self.mip_offset_and_size_in_chain(id.mip)?;

        let offset = physical
            .checked_mul(array_stride)
            .and_then(|base| base.checked_add(mip_offset))
            .ok_or(Error::OutOfBounds)?;

        Ok((offset, mip_size))
    }

    fn validate_subresource_id(&self, id: SubresourceId) -> Result<(), Error> {
        if id.mip >= self.get_num_mipmap_levels() {
            return Err(Error::OutOfBounds);
        }
        if id.layer >= self.subresource_layer_count() {
            return Err(Error::OutOfBounds);
        }
        if self.is_cubemap() {
            if id.face >= 6 {
                return Err(Error::OutOfBounds);
            }
        } else if id.face != 0 {
            return Err(Error::OutOfBounds);
        }
        Ok(())
    }

    fn physical_slice_index(&self, id: SubresourceId) -> Result<u32, Error> {
        if self.is_cubemap() {
            id.layer
                .checked_mul(6)
                .and_then(|base| base.checked_add(id.face))
                .ok_or(Error::OutOfBounds)
        } else {
            Ok(id.layer)
        }
    }

    /// Mip offset/size within one physical slice, matching [`Dds::get_array_stride`]'s chain.
    fn mip_offset_and_size_in_chain(&self, mip: u32) -> Result<(u32, u32), Error> {
        let levels = self.get_num_mipmap_levels();
        if mip >= levels {
            return Err(Error::OutOfBounds);
        }
        let mut current = self
            .get_main_texture_size()
            .ok_or(Error::UnsupportedFormat)?;
        let min_size = self.get_min_mipmap_size_in_bytes();
        let mut offset = 0_u32;
        for level in 0..levels {
            if level == mip {
                return Ok((offset, current));
            }
            offset = offset.checked_add(current).ok_or(Error::OutOfBounds)?;
            current /= 4;
            if current < min_size {
                current = min_size;
            }
        }
        Err(Error::OutOfBounds)
    }
}

/// Mutable payload access. A [`crate::DdsView`] over `&[u8]` cannot satisfy
/// `AsMut`, so these are available only when the payload is owned.
impl<D: AsRef<[u8]> + AsMut<[u8]>> DdsBase<D> {
    /// Mutable borrowed view of one subresource's bytes (dimensions unchanged).
    pub fn surface_mut(&mut self, id: SubresourceId) -> Result<SurfaceViewMut<'_>, Error> {
        let range = self.subresource_range(id)?;
        let (width, height, depth) = self.mip_dimensions(id.mip)?;
        Ok(SurfaceViewMut {
            id,
            width,
            height,
            depth,
            data: &mut self.data.as_mut()[range],
        })
    }
}

/// Mutable borrowed view of one subresource.
#[derive(Debug)]
#[non_exhaustive]
pub struct SurfaceViewMut<'a> {
    pub id: SubresourceId,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub data: &'a mut [u8],
}
