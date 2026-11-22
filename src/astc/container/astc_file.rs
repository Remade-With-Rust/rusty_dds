//! The raw `.astc` file (ARM astcenc `Docs/FileFormat.md`): a 16-byte header
//! (magic `13 AB A1 5C`, block x/y/z as bytes, image x/y/z as 24-bit
//! little-endian) followed by 16 bytes per block. No profile, no mips.

use super::{slice, surface_len, AstcSurface};
use crate::astc::AstcFootprint;
use crate::Error;

const MAGIC: [u8; 4] = [0x13, 0xAB, 0xA1, 0x5C];

/// A raw `.astc` file, borrowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct AstcFile<'a> {
    pub footprint: AstcFootprint,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    /// Exactly the payload the dimensions imply; trailing bytes are ignored.
    pub data: &'a [u8],
}

impl<'a> AstcFile<'a> {
    /// Parse a `.astc` file. The payload length is validated against the
    /// header before anything is returned; a short payload is
    /// [`Error::TruncatedData`].
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.len() < 16 {
            return Err(Error::ShortFile);
        }
        if bytes[..4] != MAGIC {
            return Err(Error::BadMagicNumber);
        }
        let footprint = AstcFootprint::new_3d(bytes[4] as u32, bytes[5] as u32, bytes[6] as u32)
            .ok_or_else(|| Error::InvalidField(format!("ASTC block size {}x{}x{}", bytes[4], bytes[5], bytes[6])))?;
        let dim = |o: usize| bytes[o] as u32 | (bytes[o + 1] as u32) << 8 | (bytes[o + 2] as u32) << 16;
        let (width, height, depth) = (dim(7), dim(10), dim(13));
        if footprint.is_3d() && depth == 0 {
            return Err(Error::InvalidField("ASTC 3D image with zero depth".into()));
        }
        let len = surface_len(footprint, width, height, depth)?;
        let data = slice(bytes, 16, len)?;
        Ok(Self {
            footprint,
            width,
            height,
            depth,
            data,
        })
    }

    /// Write a `.astc` file. `payload` must be exactly the block data the
    /// dimensions imply. Dimensions must fit 24 bits.
    pub fn write<W: std::io::Write>(
        w: &mut W,
        footprint: AstcFootprint,
        width: u32,
        height: u32,
        depth: u32,
        payload: &[u8],
    ) -> Result<(), Error> {
        for v in [width, height, depth] {
            if v == 0 || v > 0xFF_FFFF {
                return Err(Error::InvalidField(format!(".astc dimension {v} out of range")));
            }
        }
        if payload.len() != surface_len(footprint, width, height, depth)? {
            return Err(Error::OutOfBounds);
        }
        let mut h = [0u8; 16];
        h[..4].copy_from_slice(&MAGIC);
        h[4] = footprint.width() as u8;
        h[5] = footprint.height() as u8;
        h[6] = footprint.depth() as u8;
        for (i, v) in [width, height, depth].iter().enumerate() {
            h[7 + 3 * i] = *v as u8;
            h[8 + 3 * i] = (*v >> 8) as u8;
            h[9 + 3 * i] = (*v >> 16) as u8;
        }
        w.write_all(&h)?;
        w.write_all(payload)?;
        Ok(())
    }

    /// The file as a container-neutral surface (no sRGB or HDR tag).
    pub fn surface(&self) -> AstcSurface<'a> {
        AstcSurface::new(self.footprint, self.width, self.height, self.depth, None, None, self.data)
    }
}
