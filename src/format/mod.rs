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

mod pixel_format;
pub use self::pixel_format::{FourCC, PixelFormat, PixelFormatFlags};

mod d3d;
pub use self::d3d::D3DFormat;

mod dxgi;
pub use self::dxgi::DxgiFormat;

pub trait DataFormat {
    /// This gets the number of bytes required to store one row of data
    fn get_pitch(&self, width: u32) -> Option<u32>;

    /// This gets the height of each row of data. Normally it is 1, but for block
    /// compressed textures each row is one block high (4 pixels for BCn).
    fn get_pitch_height(&self) -> u32 {
        match self.get_block_dimensions() {
            Some((_, h)) => h,
            None => 1,
        }
    }

    /// Width and height in pixels of one compressed block, for compressed
    /// formats. BCn blocks are 4x4, which is the default for any format with a
    /// block size; ASTC formats report their footprint.
    fn get_block_dimensions(&self) -> Option<(u32, u32)> {
        self.get_block_size().map(|_| (4, 4))
    }

    /// This gets the number of bits required to store a single pixel.  It is
    /// only defined for uncompressed formats
    fn get_bits_per_pixel(&self) -> Option<u8>;

    /// This gets a block compression format's block size, and is only defined
    /// for compressed formats
    fn get_block_size(&self) -> Option<u32>;

    /// Get the fourcc code for this format, if known
    fn get_fourcc(&self) -> Option<FourCC>;

    /// Returns true if the DX10 extention is required to use this format.
    fn requires_extension(&self) -> bool;

    /// This gets the minimum mipmap size in bytes. Even if they go all the way
    /// down to 1x1, there is a minimum number of bytes based on bits per pixel
    /// or blocksize.
    fn get_minimum_mipmap_size_in_bytes(&self) -> Option<u32> {
        if let Some(bpp) = self.get_bits_per_pixel() {
            Some((bpp as u32).div_ceil(8))
        } else {
            self.get_block_size()
        }
    }
}

/// The header's format, held by value.
///
/// [`crate::Dds::get_format`] boxes the same information into a
/// `Box<dyn DataFormat>` — a heap allocation on *every* call. That accessor sits
/// underneath `get_pitch`, `get_pitch_height`, `get_bits_per_pixel` and
/// `get_min_mipmap_size_in_bytes`, all of which sit underneath every subresource
/// offset computation, so one `upload_plan_compressed` was costing twelve
/// allocations. This is the allocation-free equivalent for that path; the boxed
/// accessor stays for callers who want type erasure.
///
/// The delegation below covers the five methods `DataFormat` requires plus
/// `get_block_dimensions`, which `DxgiFormat` overrides for ASTC. Neither
/// concrete type overrides the other provided methods (`get_pitch_height`,
/// `get_minimum_mipmap_size_in_bytes`), so inheriting them here is
/// behaviour-identical to dispatching through `dyn DataFormat`. If either
/// concrete type ever overrides one, this enum must delegate it too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FormatOf {
    Dxgi(DxgiFormat),
    D3d(D3DFormat),
}

impl DataFormat for FormatOf {
    fn get_pitch(&self, width: u32) -> Option<u32> {
        match self {
            FormatOf::Dxgi(f) => f.get_pitch(width),
            FormatOf::D3d(f) => f.get_pitch(width),
        }
    }

    fn get_bits_per_pixel(&self) -> Option<u8> {
        match self {
            FormatOf::Dxgi(f) => f.get_bits_per_pixel(),
            FormatOf::D3d(f) => f.get_bits_per_pixel(),
        }
    }

    fn get_block_size(&self) -> Option<u32> {
        match self {
            FormatOf::Dxgi(f) => f.get_block_size(),
            FormatOf::D3d(f) => f.get_block_size(),
        }
    }

    fn get_block_dimensions(&self) -> Option<(u32, u32)> {
        match self {
            FormatOf::Dxgi(f) => f.get_block_dimensions(),
            FormatOf::D3d(f) => f.get_block_dimensions(),
        }
    }

    fn get_fourcc(&self) -> Option<FourCC> {
        match self {
            FormatOf::Dxgi(f) => f.get_fourcc(),
            FormatOf::D3d(f) => f.get_fourcc(),
        }
    }

    fn requires_extension(&self) -> bool {
        match self {
            FormatOf::Dxgi(f) => f.requires_extension(),
            FormatOf::D3d(f) => f.requires_extension(),
        }
    }
}
