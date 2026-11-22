//! KTX 1.0 (<https://registry.khronos.org/KTX/specs/1.0/ktxspec.v1.html>)
//! and KTX 2.0 (<https://registry.khronos.org/KTX/specs/2.0/ktxspec.v2.html>)
//! for ASTC payloads.
//!
//! KTX 1.0 carries the format as a GL internal format (`0x93B0..=0x93BD`
//! linear 2D, `0x93D0..=0x93DD` sRGB 2D, `0x93C0..=0x93C9` / `0x93E0..=0x93E9`
//! 3D). It has no HDR tag. KTX 2.0 carries a Vulkan format (`UNORM`, `SRGB`
//! or `SFLOAT` per footprint), or `VK_FORMAT_UNDEFINED` with an ASTC Data
//! Format Descriptor. Supercompression scheme 0 (none) is read; other schemes
//! return [`Error::UnsupportedFormat`].

use super::{slice, surface_len, AstcSurface};
use crate::astc::AstcFootprint;
use crate::Error;

const KTX1_ID: [u8; 12] = [
    0xAB, 0x4B, 0x54, 0x58, 0x20, 0x31, 0x31, 0xBB, 0x0D, 0x0A, 0x1A, 0x0A,
];
const KTX2_ID: [u8; 12] = [
    0xAB, 0x4B, 0x54, 0x58, 0x20, 0x32, 0x30, 0xBB, 0x0D, 0x0A, 0x1A, 0x0A,
];

const GL_RGBA: u32 = 0x1908;
const GL_ASTC_2D: u32 = 0x93B0;
const GL_ASTC_3D: u32 = 0x93C0;
const GL_SRGB_ASTC_2D: u32 = 0x93D0;
const GL_SRGB_ASTC_3D: u32 = 0x93E0;

/// The GL internal format of an ASTC footprint (`GL_COMPRESSED_RGBA_ASTC_*`
/// or `GL_COMPRESSED_SRGB8_ALPHA8_ASTC_*`).
pub fn gl_internal_format(fp: AstcFootprint, srgb: bool) -> u32 {
    if fp.is_3d() {
        let i = AstcFootprint::ALL_3D
            .iter()
            .position(|&f| f == fp)
            .unwrap_or(0) as u32;
        i + if srgb { GL_SRGB_ASTC_3D } else { GL_ASTC_3D }
    } else {
        let i = AstcFootprint::ALL_2D
            .iter()
            .position(|&f| f == fp)
            .unwrap_or(0) as u32;
        i + if srgb { GL_SRGB_ASTC_2D } else { GL_ASTC_2D }
    }
}

/// Footprint and sRGB flag of a GL internal format, if it is ASTC.
pub fn from_gl_internal_format(f: u32) -> Option<(AstcFootprint, bool)> {
    let pick2 = |base: u32| {
        AstcFootprint::ALL_2D
            .get(f.wrapping_sub(base) as usize)
            .copied()
    };
    let pick3 = |base: u32| {
        AstcFootprint::ALL_3D
            .get(f.wrapping_sub(base) as usize)
            .copied()
    };
    match f {
        0x93B0..=0x93BD => pick2(GL_ASTC_2D).map(|p| (p, false)),
        0x93D0..=0x93DD => pick2(GL_SRGB_ASTC_2D).map(|p| (p, true)),
        0x93C0..=0x93C9 => pick3(GL_ASTC_3D).map(|p| (p, false)),
        0x93E0..=0x93E9 => pick3(GL_SRGB_ASTC_3D).map(|p| (p, true)),
        _ => None,
    }
}

/// Encoding of an ASTC Vulkan format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VkAstcKind {
    Unorm,
    Srgb,
    Sfloat,
}

/// The `VkFormat` of an ASTC footprint and encoding.
pub fn vk_format(fp: AstcFootprint, kind: VkAstcKind) -> u32 {
    if fp.is_3d() {
        let i = AstcFootprint::ALL_3D
            .iter()
            .position(|&f| f == fp)
            .unwrap_or(0) as u32;
        1_000_288_000 + 3 * i + kind as u32
    } else {
        let i = AstcFootprint::ALL_2D
            .iter()
            .position(|&f| f == fp)
            .unwrap_or(0) as u32;
        match kind {
            VkAstcKind::Unorm => 157 + 2 * i,
            VkAstcKind::Srgb => 158 + 2 * i,
            VkAstcKind::Sfloat => 1_000_066_000 + i,
        }
    }
}

/// Footprint and encoding of a `VkFormat`, if it is ASTC.
pub fn from_vk_format(f: u32) -> Option<(AstcFootprint, VkAstcKind)> {
    match f {
        157..=184 => {
            let i = (f - 157) as usize;
            let kind = if i % 2 == 0 {
                VkAstcKind::Unorm
            } else {
                VkAstcKind::Srgb
            };
            AstcFootprint::ALL_2D.get(i / 2).map(|&p| (p, kind))
        }
        1_000_066_000..=1_000_066_013 => AstcFootprint::ALL_2D
            .get((f - 1_000_066_000) as usize)
            .map(|&p| (p, VkAstcKind::Sfloat)),
        1_000_288_000..=1_000_288_029 => {
            let i = (f - 1_000_288_000) as usize;
            let kind = [VkAstcKind::Unorm, VkAstcKind::Srgb, VkAstcKind::Sfloat][i % 3];
            AstcFootprint::ALL_3D.get(i / 3).map(|&p| (p, kind))
        }
        _ => None,
    }
}

fn mip(v: u32, level: u32) -> u32 {
    (v.checked_shr(level).unwrap_or(0)).max(1)
}

/// A parsed KTX 1.0 file holding ASTC data, borrowed.
#[derive(Debug, Clone)]
pub struct KtxView<'a> {
    pub footprint: AstcFootprint,
    pub srgb: bool,
    pub width: u32,
    pub height: u32,
    /// 1 for 2D.
    pub depth: u32,
    /// Array layers (at least 1).
    pub layers: u32,
    /// 1, or 6 for a cube map.
    pub faces: u32,
    /// Mip levels (at least 1).
    pub levels: u32,
    data: &'a [u8],
    /// Byte offset of each level's first image.
    level_offsets: Vec<usize>,
}

impl<'a> KtxView<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.len() < 64 {
            return Err(Error::ShortFile);
        }
        if bytes[..12] != KTX1_ID {
            return Err(Error::BadMagicNumber);
        }
        let be = match u32::from_le_bytes(bytes[12..16].try_into().unwrap()) {
            0x0403_0201 => false,
            0x0102_0304 => true,
            _ => return Err(Error::InvalidField("KTX endianness".into())),
        };
        let rd = |o: usize| -> u32 {
            let b: [u8; 4] = bytes[o..o + 4].try_into().unwrap();
            if be {
                u32::from_be_bytes(b)
            } else {
                u32::from_le_bytes(b)
            }
        };
        let (gl_type, gl_internal) = (rd(16), rd(28));
        if gl_type != 0 {
            return Err(Error::UnsupportedFormat);
        }
        let (footprint, srgb) =
            from_gl_internal_format(gl_internal).ok_or(Error::UnsupportedFormat)?;
        let width = rd(36);
        let height = rd(40).max(1);
        let depth = rd(44).max(1);
        let layers = rd(48).max(1);
        let faces = rd(52);
        let levels = rd(56).max(1);
        let kvd = rd(60) as usize;
        if width == 0 {
            return Err(Error::InvalidField("KTX width 0".into()));
        }
        if faces != 1 && faces != 6 {
            return Err(Error::InvalidField(format!("KTX numberOfFaces {faces}")));
        }
        if levels > 32 {
            return Err(Error::InvalidField(format!(
                "KTX numberOfMipmapLevels {levels}"
            )));
        }
        let non_array_cube = rd(48) == 0 && faces == 6;
        let mut off = 64usize.checked_add(kvd).ok_or(Error::TruncatedData)?;
        let mut level_offsets = Vec::with_capacity(levels as usize);
        for l in 0..levels {
            let (w, h, d) = (mip(width, l), mip(height, l), mip(depth, l));
            let one = surface_len(footprint, w, h, d)?;
            let image_size = rd_at(bytes, off, be)? as usize;
            let expect = if non_array_cube {
                one
            } else {
                one.checked_mul(layers as usize)
                    .and_then(|v| v.checked_mul(faces as usize))
                    .ok_or(Error::OutOfBounds)?
            };
            if image_size != expect {
                return Err(Error::InvalidField(format!(
                    "KTX level {l} imageSize {image_size}, layout needs {expect}"
                )));
            }
            off = off.checked_add(4).ok_or(Error::TruncatedData)?;
            level_offsets.push(off);
            // ASTC images are multiples of 16 bytes, so face and mip padding is 0.
            let total = one
                .checked_mul(layers as usize)
                .and_then(|v| v.checked_mul(faces as usize))
                .ok_or(Error::OutOfBounds)?;
            slice(bytes, off, total)?;
            off += total;
        }
        Ok(Self {
            footprint,
            srgb,
            width,
            height,
            depth,
            layers,
            faces,
            levels,
            data: bytes,
            level_offsets,
        })
    }

    /// One subresource.
    pub fn astc_surface(
        &self,
        level: u32,
        layer: u32,
        face: u32,
    ) -> Result<AstcSurface<'a>, Error> {
        if level >= self.levels || layer >= self.layers || face >= self.faces {
            return Err(Error::OutOfBounds);
        }
        let (w, h, d) = (
            mip(self.width, level),
            mip(self.height, level),
            mip(self.depth, level),
        );
        let one = surface_len(self.footprint, w, h, d)?;
        let idx = (layer as usize) * self.faces as usize + face as usize;
        let off = self.level_offsets[level as usize] + idx * one;
        let data = slice(self.data, off, one)?;
        Ok(AstcSurface::new(
            self.footprint,
            w,
            h,
            d,
            Some(self.srgb),
            None,
            data,
        ))
    }

    /// Write a little-endian KTX 1.0 file with one face and no array layers:
    /// `levels[i]` is mip level `i`'s payload. HDR data is written with the
    /// linear tag (KTX 1.0 has no HDR ASTC format).
    pub fn write<W: std::io::Write>(
        w: &mut W,
        footprint: AstcFootprint,
        width: u32,
        height: u32,
        depth: u32,
        srgb: bool,
        levels: &[&[u8]],
    ) -> Result<(), Error> {
        if levels.is_empty() || levels.len() > 32 {
            return Err(Error::InvalidField("KTX needs 1..=32 levels".into()));
        }
        for (l, p) in levels.iter().enumerate() {
            let l = l as u32;
            if p.len() != surface_len(footprint, mip(width, l), mip(height, l), mip(depth, l))? {
                return Err(Error::OutOfBounds);
            }
        }
        let mut h = Vec::with_capacity(64);
        h.extend_from_slice(&KTX1_ID);
        for v in [
            0x0403_0201u32,
            0, // glType
            1, // glTypeSize
            0, // glFormat
            gl_internal_format(footprint, srgb),
            GL_RGBA,
            width,
            height,
            if depth > 1 { depth } else { 0 },
            0, // numberOfArrayElements
            1, // numberOfFaces
            levels.len() as u32,
            0, // bytesOfKeyValueData
        ] {
            h.extend_from_slice(&v.to_le_bytes());
        }
        w.write_all(&h)?;
        for p in levels {
            w.write_all(&(p.len() as u32).to_le_bytes())?;
            w.write_all(p)?;
        }
        Ok(())
    }
}

fn rd_at(b: &[u8], off: usize, be: bool) -> Result<u32, Error> {
    let s: [u8; 4] = slice(b, off, 4)?.try_into().unwrap();
    Ok(if be {
        u32::from_be_bytes(s)
    } else {
        u32::from_le_bytes(s)
    })
}

fn le32(b: &[u8], off: usize) -> Result<u32, Error> {
    Ok(u32::from_le_bytes(slice(b, off, 4)?.try_into().unwrap()))
}

fn le64(b: &[u8], off: usize) -> Result<u64, Error> {
    Ok(u64::from_le_bytes(slice(b, off, 8)?.try_into().unwrap()))
}

/// A parsed KTX 2.0 file holding ASTC data, borrowed.
#[derive(Debug, Clone)]
pub struct Ktx2View<'a> {
    pub footprint: AstcFootprint,
    pub srgb: bool,
    /// The format is `SFLOAT` (or the DFD says float samples): HDR content.
    pub hdr: bool,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub layers: u32,
    pub faces: u32,
    pub levels: u32,
    /// `(byteOffset, byteLength)` of each level.
    level_index: Vec<(usize, usize)>,
    data: &'a [u8],
}

const KHR_DF_MODEL_ASTC: u32 = 162;
const KHR_DF_TRANSFER_SRGB: u32 = 2;

impl<'a> Ktx2View<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.len() < 80 {
            return Err(Error::ShortFile);
        }
        if bytes[..12] != KTX2_ID {
            return Err(Error::BadMagicNumber);
        }
        let vk = le32(bytes, 12)?;
        let width = le32(bytes, 20)?;
        let height = le32(bytes, 24)?.max(1);
        let depth = le32(bytes, 28)?.max(1);
        let layers = le32(bytes, 32)?.max(1);
        let faces = le32(bytes, 36)?;
        let levels = le32(bytes, 40)?.max(1);
        let scheme = le32(bytes, 44)?;
        if width == 0 || (faces != 1 && faces != 6) || levels > 32 {
            return Err(Error::InvalidField("KTX2 header geometry".into()));
        }
        let (footprint, srgb, hdr) = match from_vk_format(vk) {
            Some((fp, k)) => (fp, k == VkAstcKind::Srgb, k == VkAstcKind::Sfloat),
            None if vk == 0 => Self::astc_from_dfd(bytes)?,
            None => return Err(Error::UnsupportedFormat),
        };
        if scheme != 0 {
            return Err(Error::UnsupportedFormat);
        }
        let mut level_index = Vec::with_capacity(levels as usize);
        for l in 0..levels {
            let base = 80 + 24 * l as usize;
            let off = usize::try_from(le64(bytes, base)?).map_err(|_| Error::OutOfBounds)?;
            let len = usize::try_from(le64(bytes, base + 8)?).map_err(|_| Error::OutOfBounds)?;
            let (w, h, d) = (mip(width, l), mip(height, l), mip(depth, l));
            let need = surface_len(footprint, w, h, d)?
                .checked_mul(layers as usize)
                .and_then(|v| v.checked_mul(faces as usize))
                .ok_or(Error::OutOfBounds)?;
            if len != need {
                return Err(Error::InvalidField(format!(
                    "KTX2 level {l} length {len}, layout needs {need}"
                )));
            }
            slice(bytes, off, len)?;
            level_index.push((off, len));
        }
        Ok(Self {
            footprint,
            srgb,
            hdr,
            width,
            height,
            depth,
            layers,
            faces,
            levels,
            level_index,
            data: bytes,
        })
    }

    /// `VK_FORMAT_UNDEFINED` with an ASTC basic Data Format Descriptor.
    fn astc_from_dfd(bytes: &[u8]) -> Result<(AstcFootprint, bool, bool), Error> {
        let off = le32(bytes, 48)? as usize;
        let len = le32(bytes, 52)? as usize;
        let dfd = slice(bytes, off, len)?;
        // dfdTotalSize, then the first descriptor block.
        if dfd.len() < 4 + 24 + 16 {
            return Err(Error::UnsupportedFormat);
        }
        let w2 = le32(dfd, 4 + 8)?;
        let model = w2 & 0xFF;
        let transfer = (w2 >> 16) & 0xFF;
        if model != KHR_DF_MODEL_ASTC {
            return Err(Error::UnsupportedFormat);
        }
        let w3 = le32(dfd, 4 + 12)?;
        let (bx, by, bz) = (
            (w3 & 0xFF) + 1,
            ((w3 >> 8) & 0xFF) + 1,
            ((w3 >> 16) & 0xFF) + 1,
        );
        let fp = AstcFootprint::new_3d(bx, by, bz).ok_or(Error::UnsupportedFormat)?;
        // First sample: qualifier bits in the top byte of its first word; 0x80 = float.
        let s0 = le32(dfd, 4 + 24)?;
        let hdr = (s0 >> 24) & 0x80 != 0;
        Ok((fp, transfer == KHR_DF_TRANSFER_SRGB, hdr))
    }

    /// One subresource.
    pub fn astc_surface(
        &self,
        level: u32,
        layer: u32,
        face: u32,
    ) -> Result<AstcSurface<'a>, Error> {
        if level >= self.levels || layer >= self.layers || face >= self.faces {
            return Err(Error::OutOfBounds);
        }
        let (w, h, d) = (
            mip(self.width, level),
            mip(self.height, level),
            mip(self.depth, level),
        );
        let one = surface_len(self.footprint, w, h, d)?;
        let (off, _) = self.level_index[level as usize];
        let idx = layer as usize * self.faces as usize + face as usize;
        let data = slice(self.data, off + idx * one, one)?;
        Ok(AstcSurface::new(
            self.footprint,
            w,
            h,
            d,
            Some(self.srgb),
            Some(self.hdr),
            data,
        ))
    }
}

impl Ktx2View<'_> {
    /// Write a KTX 2.0 file (no supercompression) holding one 2D or 3D ASTC
    /// texture: `levels[i]` is mip level `i`'s payload, one layer, one face.
    /// The Data Format Descriptor is the basic ASTC descriptor (model
    /// `KHR_DF_MODEL_ASTC`, BT.709 primaries, linear or sRGB transfer, one
    /// 128-bit `ASTC_DATA` sample; signed + float qualifiers and bounds
    /// -1.0..1.0 for `SFLOAT`), and a `KTXwriter` key is recorded. Levels are
    /// stored smallest first, 16-byte aligned, as the specification orders them.
    pub fn write<W: std::io::Write>(
        w: &mut W,
        footprint: AstcFootprint,
        width: u32,
        height: u32,
        depth: u32,
        kind: VkAstcKind,
        levels: &[&[u8]],
    ) -> Result<(), Error> {
        if levels.is_empty() || levels.len() > 32 || width == 0 || height == 0 || depth == 0 {
            return Err(Error::InvalidField(
                "KTX2 needs 1..=32 levels and non-zero size".into(),
            ));
        }
        for (l, p) in levels.iter().enumerate() {
            let l = l as u32;
            if p.len() != surface_len(footprint, mip(width, l), mip(height, l), mip(depth, l))? {
                return Err(Error::OutOfBounds);
            }
        }
        let n = levels.len();
        // Data Format Descriptor.
        let mut dfd: Vec<u32> = Vec::with_capacity(11);
        dfd.push(44); // dfdTotalSize
        dfd.push(0); // vendorId KHRONOS (0), descriptorType BASICFORMAT (0)
        dfd.push(2 | (40 << 16)); // versionNumber 2, descriptorBlockSize 40
        let transfer = if kind == VkAstcKind::Srgb {
            KHR_DF_TRANSFER_SRGB
        } else {
            1
        };
        dfd.push(KHR_DF_MODEL_ASTC | (1 << 8) | (transfer << 16)); // model, BT709, transfer, flags 0
        dfd.push(
            (footprint.width() - 1)
                | ((footprint.height() - 1) << 8)
                | ((footprint.depth() - 1) << 16),
        );
        dfd.push(16); // bytesPlane0
        dfd.push(0); // bytesPlane4..7
        let qualifiers = if kind == VkAstcKind::Sfloat {
            0xC0u32
        } else {
            0
        };
        dfd.push((127 << 16) | (qualifiers << 24)); // bitOffset 0, bitLength 127, ASTC_DATA
        dfd.push(0); // sample position
        let (lower, upper) = if kind == VkAstcKind::Sfloat {
            ((-1.0f32).to_bits(), 1.0f32.to_bits())
        } else {
            (0, u32::MAX)
        };
        dfd.push(lower);
        dfd.push(upper);
        // Key/value data: KTXwriter.
        let kv_key = b"KTXwriter\0rusty_dds\0";
        let mut kvd = Vec::new();
        kvd.extend_from_slice(&(kv_key.len() as u32).to_le_bytes());
        kvd.extend_from_slice(kv_key);
        while kvd.len() % 4 != 0 {
            kvd.push(0);
        }
        let index_end = 80 + 24 * n;
        let dfd_off = index_end;
        let kvd_off = dfd_off + 44;
        let mut data_off = kvd_off + kvd.len();
        // Level offsets, smallest level first, each 16-byte aligned.
        let mut offs = alloc::vec![0usize; n];
        for l in (0..n).rev() {
            data_off = (data_off + 15) & !15;
            offs[l] = data_off;
            data_off += levels[l].len();
        }
        let mut h = Vec::with_capacity(index_end);
        h.extend_from_slice(&KTX2_ID);
        for v in [
            vk_format(footprint, kind),
            1, // typeSize
            width,
            height,
            if depth > 1 { depth } else { 0 },
            0, // layerCount
            1, // faceCount
            n as u32,
            0, // supercompressionScheme
            dfd_off as u32,
            44,
            kvd_off as u32,
            kvd.len() as u32,
        ] {
            h.extend_from_slice(&v.to_le_bytes());
        }
        h.extend_from_slice(&0u64.to_le_bytes()); // sgdByteOffset
        h.extend_from_slice(&0u64.to_le_bytes()); // sgdByteLength
        for (l, p) in levels.iter().enumerate() {
            h.extend_from_slice(&(offs[l] as u64).to_le_bytes());
            h.extend_from_slice(&(p.len() as u64).to_le_bytes());
            h.extend_from_slice(&(p.len() as u64).to_le_bytes());
        }
        for v in &dfd {
            h.extend_from_slice(&v.to_le_bytes());
        }
        h.extend_from_slice(&kvd);
        w.write_all(&h)?;
        let mut pos = h.len();
        for l in (0..n).rev() {
            let pad = offs[l] - pos;
            w.write_all(&[0u8; 16][..pad])?;
            w.write_all(levels[l])?;
            pos = offs[l] + levels[l].len();
        }
        Ok(())
    }
}

/// Rewrite a Zstandard-supercompressed KTX 2.0 file (scheme 2) as an
/// uncompressed one (scheme 0) that [`Ktx2View::parse`] reads. Each level is
/// inflated with the pure-Rust `rusty_zstd`, bounded by the size the header
/// geometry implies (a level that inflates to any other size is an error).
/// Uncompressed input is returned unchanged.
#[cfg(feature = "ktx2-zstd")]
#[cfg_attr(docsrs, doc(cfg(feature = "ktx2-zstd")))]
pub fn ktx2_inflate(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    if bytes.len() < 80 || bytes[..12] != KTX2_ID {
        return Err(Error::BadMagicNumber);
    }
    let scheme = le32(bytes, 44)?;
    if scheme == 0 {
        return Ok(bytes.to_vec());
    }
    if scheme != 2 {
        return Err(Error::UnsupportedFormat);
    }
    let vk = le32(bytes, 12)?;
    let width = le32(bytes, 20)?;
    let height = le32(bytes, 24)?.max(1);
    let depth = le32(bytes, 28)?.max(1);
    let layers = le32(bytes, 32)?.max(1);
    let faces = le32(bytes, 36)?;
    let levels = le32(bytes, 40)?.max(1);
    if width == 0 || (faces != 1 && faces != 6) || levels > 32 {
        return Err(Error::InvalidField("KTX2 header geometry".into()));
    }
    let footprint = match from_vk_format(vk) {
        Some((fp, _)) => fp,
        None if vk == 0 => Ktx2View::astc_from_dfd(bytes)?.0,
        None => return Err(Error::UnsupportedFormat),
    };
    let index_end = 80 + 24 * levels as usize;
    // Keep everything up to the end of the metadata (index, DFD, KVD).
    let mut meta_end = index_end;
    for (o, l) in [(48usize, 52usize), (56, 60)] {
        let end = (le32(bytes, o)? as usize)
            .checked_add(le32(bytes, l)? as usize)
            .ok_or(Error::OutOfBounds)?;
        meta_end = meta_end.max(end);
    }
    let mut out = slice(bytes, 0, meta_end)?.to_vec();
    out[44..48].copy_from_slice(&0u32.to_le_bytes());
    // No supercompression global data for scheme 0.
    out[64..80].copy_from_slice(&[0u8; 16]);
    let opts = rusty_zstd::DecompressOptions::default();
    // Smallest level first, as the specification orders them; 16-byte aligned.
    for l in (0..levels).rev() {
        let base = 80 + 24 * l as usize;
        let off = usize::try_from(le64(bytes, base)?).map_err(|_| Error::OutOfBounds)?;
        let len = usize::try_from(le64(bytes, base + 8)?).map_err(|_| Error::OutOfBounds)?;
        let need = surface_len(footprint, mip(width, l), mip(height, l), mip(depth, l))?
            .checked_mul(layers as usize)
            .and_then(|v| v.checked_mul(faces as usize))
            .ok_or(Error::OutOfBounds)?;
        let src = slice(bytes, off, len)?;
        while out.len() % 16 != 0 {
            out.push(0);
        }
        let new_off = out.len() as u64;
        // Inflate straight onto the end of the output: no per-level buffer to
        // copy from. The cap counts only this level's bytes, and the frame's
        // back-references cannot reach the bytes already in `out`.
        let got = rusty_zstd::decompress_into_with_limit(&mut out, src, opts, need)
            .map_err(|e| Error::InvalidField(format!("KTX2 level {l} zstd: {e:?}")))?;
        if got != need {
            return Err(Error::InvalidField(format!(
                "KTX2 level {l} inflates to {got}, layout needs {need}"
            )));
        }
        out[base..base + 8].copy_from_slice(&new_off.to_le_bytes());
        out[base + 8..base + 16].copy_from_slice(&(need as u64).to_le_bytes());
        out[base + 16..base + 24].copy_from_slice(&(need as u64).to_le_bytes());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_tables_round_trip() {
        for &fp in AstcFootprint::ALL_2D.iter().chain(AstcFootprint::ALL_3D) {
            for srgb in [false, true] {
                assert_eq!(
                    from_gl_internal_format(gl_internal_format(fp, srgb)),
                    Some((fp, srgb))
                );
            }
            for k in [VkAstcKind::Unorm, VkAstcKind::Srgb, VkAstcKind::Sfloat] {
                assert_eq!(from_vk_format(vk_format(fp, k)), Some((fp, k)));
            }
        }
        assert_eq!(vk_format(AstcFootprint::B4X4, VkAstcKind::Unorm), 157);
        assert_eq!(vk_format(AstcFootprint::B12X12, VkAstcKind::Srgb), 184);
        assert_eq!(gl_internal_format(AstcFootprint::B12X12, true), 0x93DD);
    }

    #[test]
    fn ktx2_write_round_trip() {
        for &(fp, w, h, d, kind) in &[
            (AstcFootprint::B6X5, 37u32, 21u32, 1u32, VkAstcKind::Unorm),
            (AstcFootprint::B8X8, 64, 64, 1, VkAstcKind::Srgb),
            (AstcFootprint::B4X4, 5, 3, 1, VkAstcKind::Sfloat),
            (AstcFootprint::B4X4X4, 9, 9, 6, VkAstcKind::Unorm),
        ] {
            let levels: Vec<Vec<u8>> = (0..3)
                .map(|l| {
                    let n = surface_len(fp, mip(w, l), mip(h, l), mip(d, l)).unwrap();
                    (0..n).map(|i| (i * 7 + l as usize) as u8).collect()
                })
                .collect();
            let refs: Vec<&[u8]> = levels.iter().map(|v| &v[..]).collect();
            let mut out = Vec::new();
            Ktx2View::write(&mut out, fp, w, h, d, kind, &refs).unwrap();
            let v = Ktx2View::parse(&out).unwrap();
            assert_eq!(
                (v.footprint, v.width, v.height, v.depth, v.levels),
                (fp, w, h, d, 3)
            );
            assert_eq!(v.srgb, kind == VkAstcKind::Srgb);
            assert_eq!(v.hdr, kind == VkAstcKind::Sfloat);
            for l in 0..3 {
                assert_eq!(
                    v.astc_surface(l, 0, 0).unwrap().data,
                    &levels[l as usize][..]
                );
            }
            // The DFD alone (vkFormat cleared) identifies the same format.
            let mut undef = out.clone();
            undef[12..16].copy_from_slice(&0u32.to_le_bytes());
            let u = Ktx2View::parse(&undef).unwrap();
            assert_eq!((u.footprint, u.srgb, u.hdr), (fp, v.srgb, v.hdr));
        }
    }
}
