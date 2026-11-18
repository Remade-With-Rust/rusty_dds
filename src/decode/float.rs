//! Full-precision RGBA `f32` decode for everything that is not BC6H.
//!
//! [`crate::DdsBase::decode_rgba_f32`] used to accept BC6H alone and fail
//! closed on every other format, so a material tool that wanted an HDR texture,
//! a 16-bit height map or a packed normal had nowhere to go. This module is the
//! rest of that matrix:
//!
//! * **Uncompressed layouts** ([`PlainFormat`]): 32- and 16-bit float, 16- and
//!   8-bit UNORM/SNORM, 10:10:10:2, 11:11:10 float, 9:9:9:E5, the 5:6:5 /
//!   5:5:5:1 / 4:4:4:4 packs and the legacy luminance formats.
//! * **BC4 and BC5** decoded straight to `f32` from their endpoints, so the
//!   interpolated levels keep the precision the 8-bit path rounds away.
//! * **BC1, BC2, BC3, BC7, RGBA8 and BGRA8** through the existing 8-bit kernels
//!   and an exact `x / 255` widen. Those formats define their result at 8 bits,
//!   so nothing is lost.
//!
//! # Conventions
//!
//! * Float formats are returned **unclamped** (negative, > 1, Inf and NaN pass
//!   through).
//! * UNORM is `x / (2^n - 1)`, correctly rounded, so `0 → 0.0` and
//!   `max → 1.0` exactly.
//! * SNORM is `x / (2^(n-1) - 1)` in **`-1.0..=1.0`**, with the most negative
//!   code clamped to `-1.0` as D3D specifies. (`dds` 0.2 remaps SNORM into
//!   `0..1`; this crate returns the value a GPU samples.)
//! * Channels a format does not carry read as `0.0`, and a missing alpha as
//!   `1.0` — again what a GPU returns. Single-channel formats can be expanded to
//!   grey instead with [`crate::DecodeOptions`]. Luminance formats (`L8`, `L16`,
//!   `A8L8`, `A4L4`) are grey by definition.

use super::bc6h::half_to_f32;
use crate::content::DecodeContent;
use crate::error::Error;
use crate::format::{D3DFormat, DxgiFormat};

/// How a one-channel format is expanded to RGBA.
///
/// The default, [`SingleChannel::Red`], is what a GPU returns when it samples a
/// BC4 or `R8`/`R16`/`R32_FLOAT` texture: `(r, 0, 0, 1)`. [`SingleChannel::Grey`]
/// replicates the channel, `(r, r, r, 1)`, which is the DirectXTex and `dds`
/// crate convention and what a height or roughness map looks like in a viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[non_exhaustive]
pub enum SingleChannel {
    /// `(r, 0, 0, 1)` — GPU sampling semantics. The default.
    #[default]
    Red,
    /// `(r, r, r, 1)` — greyscale, as DirectXTex and `dds` 0.2 return BC4.
    Grey,
}

/// Options for [`crate::DdsBase::decode_rgba8_with`] and
/// [`crate::DdsBase::decode_rgba_f32_with`].
///
/// ```
/// use rusty_dds::{DecodeOptions, SingleChannel};
/// let grey = DecodeOptions::new().single_channel(SingleChannel::Grey);
/// assert_eq!(grey.single_channel, SingleChannel::Grey);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[non_exhaustive]
pub struct DecodeOptions {
    /// How BC4 and the other one-channel formats fill green and blue.
    pub single_channel: SingleChannel,
}

impl DecodeOptions {
    /// The defaults: identical output to `decode_rgba8` / `decode_rgba_f32`.
    pub const fn new() -> Self {
        DecodeOptions {
            single_channel: SingleChannel::Red,
        }
    }

    /// Choose how one-channel formats expand.
    pub const fn single_channel(mut self, mode: SingleChannel) -> Self {
        self.single_channel = mode;
        self
    }

    pub(crate) fn grey(self) -> bool {
        self.single_channel == SingleChannel::Grey
    }
}

/// Uncompressed layouts the `f32` path decodes. Internal: the public surface is
/// `decode_rgba_f32` succeeding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlainFormat {
    Rgba32F,
    Rgb32F,
    Rg32F,
    R32F,
    Rgba16F,
    Rg16F,
    R16F,
    Rgba16Un,
    Rg16Un,
    R16Un,
    Rgba16Sn,
    Rg16Sn,
    R16Sn,
    Rgba8Un,
    Rgba8Sn,
    Bgra8,
    Bgrx8,
    Rgbx8,
    Bgr8,
    Rg8Un,
    Rg8Sn,
    R8Un,
    R8Sn,
    A8Un,
    Rgb10A2,
    Bgr10A2,
    Rg11B10F,
    Rgb9E5,
    B5G6R5,
    B5G5R5A1,
    B5G5R5X1,
    B4G4R4A4,
    B4G4R4X4,
    L8,
    L16,
    A8L8,
    A4L4,
}

impl PlainFormat {
    pub(crate) fn from_dxgi(f: DxgiFormat) -> Option<PlainFormat> {
        use DxgiFormat::*;
        use PlainFormat as P;
        Some(match f {
            R32G32B32A32_Float => P::Rgba32F,
            R32G32B32_Float => P::Rgb32F,
            R32G32_Float => P::Rg32F,
            R32_Float => P::R32F,
            R16G16B16A16_Float => P::Rgba16F,
            R16G16_Float => P::Rg16F,
            R16_Float => P::R16F,
            R16G16B16A16_UNorm => P::Rgba16Un,
            R16G16_UNorm => P::Rg16Un,
            R16_UNorm => P::R16Un,
            R16G16B16A16_SNorm => P::Rgba16Sn,
            R16G16_SNorm => P::Rg16Sn,
            R16_SNorm => P::R16Sn,
            R8G8B8A8_SNorm => P::Rgba8Sn,
            B8G8R8X8_UNorm | B8G8R8X8_Typeless | B8G8R8X8_UNorm_sRGB => P::Bgrx8,
            R8G8_UNorm => P::Rg8Un,
            R8G8_SNorm => P::Rg8Sn,
            R8_UNorm => P::R8Un,
            R8_SNorm => P::R8Sn,
            A8_UNorm => P::A8Un,
            R10G10B10A2_UNorm => P::Rgb10A2,
            R11G11B10_Float => P::Rg11B10F,
            R9G9B9E5_SharedExp => P::Rgb9E5,
            B5G6R5_UNorm => P::B5G6R5,
            B5G5R5A1_UNorm => P::B5G5R5A1,
            B4G4R4A4_UNorm => P::B4G4R4A4,
            _ => return None,
        })
    }

    /// Legacy (non-DX10) headers. D3D9 names list channels from the most
    /// significant bit, so `A8R8G8B8` is B, G, R, A in memory.
    pub(crate) fn from_d3d(f: D3DFormat) -> Option<PlainFormat> {
        use D3DFormat::*;
        use PlainFormat as P;
        Some(match f {
            A32B32G32R32F => P::Rgba32F,
            G32R32F => P::Rg32F,
            R32F => P::R32F,
            A16B16G16R16F => P::Rgba16F,
            G16R16F => P::Rg16F,
            R16F => P::R16F,
            A16B16G16R16 => P::Rgba16Un,
            G16R16 => P::Rg16Un,
            A8B8G8R8 => P::Rgba8Un,
            A8R8G8B8 => P::Bgra8,
            X8R8G8B8 => P::Bgrx8,
            X8B8G8R8 => P::Rgbx8,
            R8G8B8 => P::Bgr8,
            A2B10G10R10 => P::Rgb10A2,
            A2R10G10B10 => P::Bgr10A2,
            R5G6B5 => P::B5G6R5,
            A1R5G5B5 => P::B5G5R5A1,
            X1R5G5B5 => P::B5G5R5X1,
            A4R4G4B4 => P::B4G4R4A4,
            X4R4G4B4 => P::B4G4R4X4,
            A8 => P::A8Un,
            L8 => P::L8,
            L16 => P::L16,
            A8L8 => P::A8L8,
            A4L4 => P::A4L4,
            _ => return None,
        })
    }

    pub(crate) fn bytes_per_pixel(self) -> usize {
        use PlainFormat as P;
        match self {
            P::Rgba32F => 16,
            P::Rgb32F => 12,
            P::Rg32F | P::Rgba16F | P::Rgba16Un | P::Rgba16Sn => 8,
            P::Bgr8 => 3,
            P::R32F
            | P::Rg16F
            | P::Rg16Un
            | P::Rg16Sn
            | P::Rgba8Un
            | P::Rgba8Sn
            | P::Bgra8
            | P::Bgrx8
            | P::Rgbx8
            | P::Rgb10A2
            | P::Bgr10A2
            | P::Rg11B10F
            | P::Rgb9E5 => 4,
            P::R16F
            | P::R16Un
            | P::R16Sn
            | P::Rg8Un
            | P::Rg8Sn
            | P::B5G6R5
            | P::B5G5R5A1
            | P::B5G5R5X1
            | P::B4G4R4A4
            | P::B4G4R4X4
            | P::L16
            | P::A8L8 => 2,
            P::R8Un | P::R8Sn | P::A8Un | P::L8 | P::A4L4 => 1,
        }
    }

}

/// What `decode_rgba_f32` dispatches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum F32Kind {
    Bc6h { signed: bool },
    Ldr(DecodeContent),
    Plain(PlainFormat),
}

impl F32Kind {
    /// Pixel rows per block row.
    pub(crate) fn block_height(self) -> u32 {
        match self {
            F32Kind::Bc6h { .. } => 4,
            F32Kind::Ldr(DecodeContent::Rgba8 | DecodeContent::Bgra8) => 1,
            F32Kind::Ldr(_) => 4,
            F32Kind::Plain(_) => 1,
        }
    }

    /// Payload bytes of one block row of `width` pixels.
    pub(crate) fn row_bytes(self, width: u32) -> Result<usize, Error> {
        let w = width as usize;
        let (units, per) = match self {
            F32Kind::Bc6h { .. } => (w.div_ceil(4), 16),
            F32Kind::Ldr(DecodeContent::Rgba8 | DecodeContent::Bgra8) => (w, 4),
            F32Kind::Ldr(k) => (w.div_ceil(4), k.block_bytes().unwrap_or(16)),
            F32Kind::Plain(p) => (w, p.bytes_per_pixel()),
        };
        units.checked_mul(per).ok_or(Error::OutOfBounds)
    }

    /// Payload bytes of one whole slice.
    pub(crate) fn slice_bytes(self, width: u32, height: u32) -> Result<usize, Error> {
        if width == 0 || height == 0 {
            return Err(Error::InvalidField("zero image dimension".into()));
        }
        let rows = height.div_ceil(self.block_height()) as usize;
        self.row_bytes(width)?
            .checked_mul(rows)
            .ok_or(Error::OutOfBounds)
    }
}

// ---- exact UNORM widening without a divide ------------------------------
//
// `x / (2^n - 1)` rounded once is the target. A divide per channel is what
// made the first version of this module 2-3x slower than `dds` 0.2 on the
// packed formats: scalar `divss` is ~4 cycles a channel, and a `[f32; 256]`
// table lookup is a gather the vectoriser will not form. These are the
// multiply forms (Fabian Giesen, "Exact UNORM8 to float", 2024; the same
// constants `dds` uses), each proved equal to the correctly rounded quotient
// for EVERY input by `unorm_multiplies_equal_division_exhaustively`.

#[inline(always)]
pub(crate) fn n8(x: u8) -> f32 {
    (x as f32 * 3.0) * (1.0 / 765.0)
}

#[inline(always)]
fn n16(x: u16) -> f32 {
    const C0: f32 = 1.0 / 65536.0;
    const C1: f32 = (1.0 + 65536.0) / 65536.0 / 65536.0 / 65536.0;
    let t = x as f32;
    (t * C0) + (t * C1)
}

/// Lane form inlined in the `Rgb10A2` arm; kept as the tested definition.
#[cfg(test)]
#[inline(always)]
fn n10(x: u32) -> f32 {
    (x as f32 * 85.0) * (1.0 / (1023.0 * 85.0))
}

#[inline(always)]
fn n6(x: u32) -> f32 {
    (x as f32 * 5.0) * (1.0 / (63.0 * 5.0))
}

#[inline(always)]
fn n5(x: u32) -> f32 {
    (x as f32 * 3.0) * (1.0 / (31.0 * 3.0))
}

#[inline(always)]
fn n4(x: u32) -> f32 {
    (x as f32 * 3.0) * (1.0 / (15.0 * 3.0))
}

#[cfg(test)]
#[inline(always)]
fn n2(x: u32) -> f32 {
    x as f32 * (1.0 / 3.0)
}

/// SNORM keeps its divide: it vectorises in the flat loops below, and no
/// shipping texture pipeline leans on SNORM uncompressed.
#[inline(always)]
fn sn16(x: u16) -> f32 {
    ((x as i16).max(-32767)) as f32 / 32767.0
}

#[inline(always)]
fn sn8(x: u8) -> f32 {
    ((x as i8).max(-127)) as f32 / 127.0
}

#[inline(always)]
fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

#[inline(always)]
fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// An unsigned 11- or 10-bit float (5-bit exponent, bias 15, no sign) as a
/// half bit pattern: same exponent, mantissa left-aligned.
#[inline(always)]
fn small_float(bits: u32, mantissa_bits: u32) -> f32 {
    half_to_f32((bits << (10 - mantissa_bits)) as u16)
}

/// Widen RGBA8 bytes to floats, one channel at a time (a flat loop the
/// compiler vectorises).
#[inline]
pub(crate) fn widen_u8(src: &[u8], dst: &mut [f32]) {
    for (d, &s) in dst.iter_mut().zip(src) {
        *d = n8(s);
    }
}

/// Write one stored channel `r` as `(r, 0, 0, 1)` or `(r, r, r, 1)`. Two
/// loops, not a branch inside one, so each vectorises.
#[inline(always)]
fn single<T: Copy>(src: &[T], dst: &mut [f32], grey: bool, f: impl Fn(T) -> f32) {
    if grey {
        for (d, &s) in dst.chunks_exact_mut(4).zip(src) {
            let r = f(s);
            d.copy_from_slice(&[r, r, r, 1.0]);
        }
    } else {
        for (d, &s) in dst.chunks_exact_mut(4).zip(src) {
            d.copy_from_slice(&[f(s), 0.0, 0.0, 1.0]);
        }
    }
}

/// Little-endian `u16`s / `u32`s / `f32`s of a byte row.
fn words16(src: &[u8]) -> impl Iterator<Item = u16> + '_ {
    src.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]]))
}

/// Convert `dst.len() / 4` consecutive pixels (any number of whole rows).
/// `src` holds at least that many pixels' bytes.
pub(crate) fn convert_row(fmt: PlainFormat, src: &[u8], dst: &mut [f32], grey: bool) {
    use PlainFormat as P;
    let n = dst.len() / 4;
    let src = &src[..n * fmt.bytes_per_pixel()];
    // Each arm is its own loop so the compiler sees one stride. The formats
    // that store all four channels are converted channel-at-a-time (one flat
    // loop over `dst`), which is the shape that vectorises.
    match fmt {
        P::Rgba32F => {
            // Output bytes ARE the input bytes on a little-endian target, so
            // this is a memcpy — and only a real `memcpy` reaches the
            // streaming-store path that skips the read-for-ownership of a
            // destination too big for cache. The element loop below pays it:
            // over the same whole-surface call it measured 1.60 vs 0.86 ms at
            // 1024^2 and 40.0 vs 26.0 ms at 4096^2 (this arm ~ `dds` 0.2's
            // `read_exact`). The unsafe arm lives under `simd`, the crate's one
            // `unsafe` gate; without it the safe loop is the fallback.
            #[cfg(all(feature = "simd", target_endian = "little"))]
            {
                let bytes = n * 16;
                assert!(src.len() >= bytes && dst.len() * 4 == bytes);
                // SAFETY: both ranges are `bytes` long (asserted), they cannot
                // overlap (`&[u8]` and `&mut [f32]` are distinct borrows), every
                // bit pattern is a valid `f32`, and a byte copy has no
                // alignment requirement on either side.
                unsafe {
                    core::ptr::copy_nonoverlapping(src.as_ptr(), dst.as_mut_ptr().cast::<u8>(), bytes);
                }
                return;
            }
            // A pixel is 16 bytes in and 16 bytes out: copy it whole so the
            // loop lowers to one 128-bit move per pixel.
            #[allow(unreachable_code)]
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(16)) {
                let mut px = [0f32; 4];
                for (c, v) in px.iter_mut().enumerate() {
                    *v = f32::from_le_bytes([s[4 * c], s[4 * c + 1], s[4 * c + 2], s[4 * c + 3]]);
                }
                d.copy_from_slice(&px);
            }
        }
        P::Rgba16F => {
            for (d, s) in dst.iter_mut().zip(words16(src)) {
                *d = half_to_f32(s);
            }
        }
        P::Rgba16Un => {
            for (d, s) in dst.iter_mut().zip(words16(src)) {
                *d = n16(s);
            }
        }
        P::Rgba16Sn => {
            for (d, s) in dst.iter_mut().zip(words16(src)) {
                *d = sn16(s);
            }
        }
        P::Rgba8Un => widen_u8(src, dst),
        P::Rgba8Sn => {
            for (d, &s) in dst.iter_mut().zip(src) {
                *d = sn8(s);
            }
        }
        P::Rgb32F => {
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(12)) {
                d.copy_from_slice(&[
                    f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
                    f32::from_le_bytes([s[4], s[5], s[6], s[7]]),
                    f32::from_le_bytes([s[8], s[9], s[10], s[11]]),
                    1.0,
                ]);
            }
        }
        P::Rg32F => {
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(8)) {
                d.copy_from_slice(&[
                    f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
                    f32::from_le_bytes([s[4], s[5], s[6], s[7]]),
                    0.0,
                    1.0,
                ]);
            }
        }
        P::R32F => {
            let f = |s: &[u8]| f32::from_le_bytes([s[0], s[1], s[2], s[3]]);
            if grey {
                for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                    let r = f(s);
                    d.copy_from_slice(&[r, r, r, 1.0]);
                }
            } else {
                for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                    d.copy_from_slice(&[f(s), 0.0, 0.0, 1.0]);
                }
            }
        }
        P::Rg16F => {
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                d.copy_from_slice(&[half_to_f32(u16_at(s, 0)), half_to_f32(u16_at(s, 2)), 0.0, 1.0]);
            }
        }
        P::R16F => single_words(src, dst, grey, half_to_f32),
        P::Rg16Un => {
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                d.copy_from_slice(&[n16(u16_at(s, 0)), n16(u16_at(s, 2)), 0.0, 1.0]);
            }
        }
        P::R16Un => single_words(src, dst, grey, n16),
        P::Rg16Sn => {
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                d.copy_from_slice(&[sn16(u16_at(s, 0)), sn16(u16_at(s, 2)), 0.0, 1.0]);
            }
        }
        P::R16Sn => single_words(src, dst, grey, sn16),
        P::Bgra8 => {
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                d.copy_from_slice(&[n8(s[2]), n8(s[1]), n8(s[0]), n8(s[3])]);
            }
        }
        P::Bgrx8 => {
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                d.copy_from_slice(&[n8(s[2]), n8(s[1]), n8(s[0]), 1.0]);
            }
        }
        P::Bgr8 => {
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(3)) {
                d.copy_from_slice(&[n8(s[2]), n8(s[1]), n8(s[0]), 1.0]);
            }
        }
        P::Rgbx8 => {
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                d.copy_from_slice(&[n8(s[0]), n8(s[1]), n8(s[2]), 1.0]);
            }
        }
        P::Rg8Un => {
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(2)) {
                d.copy_from_slice(&[n8(s[0]), n8(s[1]), 0.0, 1.0]);
            }
        }
        P::Rg8Sn => {
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(2)) {
                d.copy_from_slice(&[sn8(s[0]), sn8(s[1]), 0.0, 1.0]);
            }
        }
        P::R8Un => single(src, dst, grey, n8),
        P::R8Sn => single(src, dst, grey, sn8),
        P::A8Un => {
            for (d, &s) in dst.chunks_exact_mut(4).zip(src) {
                d.copy_from_slice(&[0.0, 0.0, 0.0, n8(s)]);
            }
        }
        P::Rgb10A2 | P::Bgr10A2 => {
            // Written as four independent lanes (shift, mask, convert, two
            // multiplies) so it vectorises across the pixel; `n10`/`n2` in lane
            // form, same constants, same exactness.
            let (lo, hi) = if fmt == P::Bgr10A2 { (20, 0) } else { (0, 20) };
            const K0: [f32; 4] = [85.0, 85.0, 85.0, 1.0];
            const K1: [f32; 4] = [
                1.0 / (1023.0 * 85.0),
                1.0 / (1023.0 * 85.0),
                1.0 / (1023.0 * 85.0),
                1.0 / 3.0,
            ];
            let shift = [lo, 10u32, hi, 30];
            let mask = [0x3ffu32, 0x3ff, 0x3ff, 0x3];
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                let v = u32_at(s, 0);
                for c in 0..4 {
                    d[c] = (((v >> shift[c]) & mask[c]) as f32 * K0[c]) * K1[c];
                }
            }
        }
        P::Rg11B10F => {
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                let v = u32_at(s, 0);
                d.copy_from_slice(&[
                    small_float(v & 0x7ff, 6),
                    small_float((v >> 11) & 0x7ff, 6),
                    small_float(v >> 22, 5),
                    1.0,
                ]);
            }
        }
        P::Rgb9E5 => {
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                let v = u32_at(s, 0);
                let e = v >> 27;
                // 2^(e - 15 - 9): e - 24 + 127 is in 103..=134, always a normal
                // exponent, so the scale and every product are exact.
                let scale = f32::from_bits((e + 103) << 23);
                d.copy_from_slice(&[
                    (v & 0x1ff) as f32 * scale,
                    ((v >> 9) & 0x1ff) as f32 * scale,
                    ((v >> 18) & 0x1ff) as f32 * scale,
                    1.0,
                ]);
            }
        }
        P::B5G6R5 => {
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(2)) {
                let v = u16_at(s, 0) as u32;
                d.copy_from_slice(&[n5(v >> 11), n6((v >> 5) & 0x3f), n5(v & 0x1f), 1.0]);
            }
        }
        P::B5G5R5A1 | P::B5G5R5X1 => {
            let opaque = fmt == P::B5G5R5X1;
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(2)) {
                let v = u16_at(s, 0) as u32;
                let a = if opaque { 1.0 } else { (v >> 15) as f32 };
                d.copy_from_slice(&[n5((v >> 10) & 0x1f), n5((v >> 5) & 0x1f), n5(v & 0x1f), a]);
            }
        }
        P::B4G4R4A4 | P::B4G4R4X4 => {
            let opaque = fmt == P::B4G4R4X4;
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(2)) {
                let v = u16_at(s, 0) as u32;
                let a = if opaque { 1.0 } else { n4(v >> 12) };
                d.copy_from_slice(&[n4((v >> 8) & 0xf), n4((v >> 4) & 0xf), n4(v & 0xf), a]);
            }
        }
        P::L8 => single(src, dst, true, n8),
        P::L16 => single_words(src, dst, true, n16),
        P::A8L8 => {
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(2)) {
                let l = n8(s[0]);
                d.copy_from_slice(&[l, l, l, n8(s[1])]);
            }
        }
        P::A4L4 => {
            for (d, &s) in dst.chunks_exact_mut(4).zip(src) {
                let l = n4((s & 0xf) as u32);
                d.copy_from_slice(&[l, l, l, n4((s >> 4) as u32)]);
            }
        }
    }
}

/// [`single`] over little-endian 16-bit words.
#[inline(always)]
fn single_words(src: &[u8], dst: &mut [f32], grey: bool, f: impl Fn(u16) -> f32) {
    if grey {
        for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(2)) {
            let r = f(u16::from_le_bytes([s[0], s[1]]));
            d.copy_from_slice(&[r, r, r, 1.0]);
        }
    } else {
        for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(2)) {
            d.copy_from_slice(&[f(u16::from_le_bytes([s[0], s[1]])), 0.0, 0.0, 1.0]);
        }
    }
}

/// Correctly rounded `n / d` for every numerator a BC4 palette entry can have.
///
/// Every entry of a BC4 palette is `(a*e0 + b*e1) / (n * 255)` (or `127` for
/// SNORM), and the endpoints are the same form with `(n, 0)` weights. A divide
/// per entry is six-plus `divss` per block, which made the f32 BC4 path SLOWER
/// than `dds` 0.2 at 1024^2 (0.83x under the system allocator). Multiplying by
/// a rounded reciprocal, as `dds` does, is not exact: it is off by one ulp on
/// 1 881 of these numerators. A table of the exact quotients is both.
pub(crate) struct Bc4Tables {
    u7: Vec<f32>, // n / 1785, n in 0..=1785
    u5: Vec<f32>, // n / 1275, n in 0..=1275
    s7: Vec<f32>, // n / 889,  n in -889..=889, offset 889
    s5: Vec<f32>, // n / 635,  n in -635..=635, offset 635
}

impl Bc4Tables {
    pub(crate) fn get() -> &'static Bc4Tables {
        static T: std::sync::OnceLock<Bc4Tables> = std::sync::OnceLock::new();
        T.get_or_init(|| {
            let q = |lo: i32, hi: i32, d: f32| (lo..=hi).map(|n| n as f32 / d).collect::<Vec<f32>>();
            Bc4Tables {
                u7: q(0, 1785, 1785.0),
                u5: q(0, 1275, 1275.0),
                s7: q(-889, 889, 889.0),
                s5: q(-635, 635, 635.0),
            }
        })
    }
}

/// The eight-entry palette of one BC4 channel, in `f32`.
///
/// Each entry is the exact rational `(a*e0 + b*e1) / (n * 255)` (or `127` for
/// SNORM) rounded once: both operands are small integers that `f32` holds
/// exactly, and the quotient comes from a table of correctly rounded divides
/// (see [`Bc4Tables`]). The 8-bit path rounds the same value to the nearest
/// level; this one keeps it.
#[inline]
pub(crate) fn bc4_palette_f32(a0: u8, a1: u8, signed: bool, t: &Bc4Tables) -> [f32; 8] {
    let (e0, e1) = if signed {
        ((a0 as i8 as i32).max(-127), (a1 as i8 as i32).max(-127))
    } else {
        (a0 as i32, a1 as i32)
    };
    let (t7, t5, off7, off5) = if signed {
        (&t.s7[..], &t.s5[..], 889, 635)
    } else {
        (&t.u7[..], &t.u5[..], 0, 0)
    };
    if e0 > e1 {
        let l = |w0: i32, w1: i32| t7[(w0 * e0 + w1 * e1 + off7) as usize];
        [l(7, 0), l(0, 7), l(6, 1), l(5, 2), l(4, 3), l(3, 4), l(2, 5), l(1, 6)]
    } else {
        let l = |w0: i32, w1: i32| t5[(w0 * e0 + w1 * e1 + off5) as usize];
        [
            l(5, 0),
            l(0, 5),
            l(4, 1),
            l(3, 2),
            l(2, 3),
            l(1, 4),
            if signed { -1.0 } else { 0.0 },
            1.0,
        ]
    }
}

/// The definition the table form must equal: one IEEE divide per entry.
#[cfg(test)]
fn bc4_palette_f32_div(a0: u8, a1: u8, signed: bool) -> [f32; 8] {
    let (e0, e1, scale) = if signed {
        ((a0 as i8 as i32).max(-127), (a1 as i8 as i32).max(-127), 127.0f32)
    } else {
        (a0 as i32, a1 as i32, 255.0f32)
    };
    let lerp = |w0: i32, w1: i32, n: f32| (w0 * e0 + w1 * e1) as f32 / (n * scale);
    let (f0, f1) = (e0 as f32 / scale, e1 as f32 / scale);
    if e0 > e1 {
        [f0, f1, lerp(6, 1, 7.0), lerp(5, 2, 7.0), lerp(4, 3, 7.0), lerp(3, 4, 7.0), lerp(2, 5, 7.0), lerp(1, 6, 7.0)]
    } else {
        [f0, f1, lerp(4, 1, 5.0), lerp(3, 2, 5.0), lerp(2, 3, 5.0), lerp(1, 4, 5.0), if signed { -1.0 } else { 0.0 }, 1.0]
    }
}

/// Decode `rows` block rows of BC4 (`two == false`) or BC5 into `dst`, which is
/// `width * height * 4` floats for the pixel rows those block rows cover.
pub(crate) fn decode_bc45_f32(
    data: &[u8],
    width: u32,
    height: u32,
    signed: bool,
    two: bool,
    grey: bool,
    dst: &mut [f32],
) -> Result<(), Error> {
    let w = width as usize;
    let h = height as usize;
    let bx = w.div_ceil(4);
    let by = h.div_ceil(4);
    let block = if two { 16 } else { 8 };
    let need = bx
        .checked_mul(by)
        .and_then(|n| n.checked_mul(block))
        .ok_or(Error::OutOfBounds)?;
    if data.len() < need {
        return Err(Error::TruncatedData);
    }
    if dst.len() != w.checked_mul(h).and_then(|n| n.checked_mul(4)).ok_or(Error::OutOfBounds)? {
        return Err(Error::OutOfBounds);
    }
    let pitch = w * 4;
    let t = Bc4Tables::get();
    for (yb, row_blocks) in data[..need].chunks_exact(bx * block).enumerate() {
        let y0 = yb * 4;
        let rows_here = (h - y0).min(4);
        for (xb, blk) in row_blocks.chunks_exact(block).enumerate() {
            let x0 = xb * 4;
            let cols_here = (w - x0).min(4);
            let pr = bc4_palette_f32(blk[0], blk[1], signed, t);
            let ir = super::bcn::bc4_indices(&blk[..8]);
            if two {
                let pg = bc4_palette_f32(blk[8], blk[9], signed, t);
                let ig = super::bcn::bc4_indices(&blk[8..16]);
                for r in 0..rows_here {
                    let o = (y0 + r) * pitch + x0 * 4;
                    for (c, px) in dst[o..o + cols_here * 4].chunks_exact_mut(4).enumerate() {
                        let sh = 3 * (r * 4 + c);
                        px.copy_from_slice(&[
                            pr[((ir >> sh) & 7) as usize],
                            pg[((ig >> sh) & 7) as usize],
                            0.0,
                            1.0,
                        ]);
                    }
                }
            } else {
                // Eight whole RGBA entries, built once per block: the pixel
                // loop is then one 16-byte copy, with no per-pixel branch on
                // the channel layout.
                let pal: [[f32; 4]; 8] = core::array::from_fn(|k| {
                    let v = pr[k];
                    if grey {
                        [v, v, v, 1.0]
                    } else {
                        [v, 0.0, 0.0, 1.0]
                    }
                });
                for r in 0..rows_here {
                    let o = (y0 + r) * pitch + x0 * 4;
                    for (c, px) in dst[o..o + cols_here * 4].chunks_exact_mut(4).enumerate() {
                        px.copy_from_slice(&pal[((ir >> (3 * (r * 4 + c))) & 7) as usize]);
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every multiply form equals the correctly rounded quotient, for every
    /// input it can see. `f64` division then rounding to `f32` is exact here
    /// (the quotient of two small integers never sits on an `f32` tie).
    #[test]
    fn unorm_multiplies_equal_division_exhaustively() {
        let exact = |x: u32, max: u32| (x as f64 / max as f64) as f32;
        for x in 0..=255u32 {
            assert_eq!(n8(x as u8), exact(x, 255), "n8 {x}");
        }
        for x in 0..=u16::MAX as u32 {
            assert_eq!(n16(x as u16), exact(x, 65535), "n16 {x}");
        }
        for x in 0..1024 {
            assert_eq!(n10(x), exact(x, 1023), "n10 {x}");
        }
        for x in 0..64 {
            assert_eq!(n6(x), exact(x, 63), "n6 {x}");
        }
        for x in 0..32 {
            assert_eq!(n5(x), exact(x, 31), "n5 {x}");
        }
        for x in 0..16 {
            assert_eq!(n4(x), exact(x, 15), "n4 {x}");
        }
        for x in 0..4 {
            assert_eq!(n2(x), exact(x, 3), "n2 {x}");
        }
    }

    #[test]
    fn unorm16_and_snorm16_endpoints() {
        assert_eq!(n16(0), 0.0);
        assert_eq!(n16(65535), 1.0);
        assert_eq!(sn16(0x8000), -1.0);
        assert_eq!(sn16(0x8001), -1.0);
        assert_eq!(sn16(0x7fff), 1.0);
        assert_eq!(sn16(0), 0.0);
        assert_eq!(sn8(0x80), -1.0);
        assert_eq!(sn8(0x7f), 1.0);
    }

    #[test]
    fn small_floats_and_shared_exponent() {
        // 11-bit float 1.0: exponent 15, mantissa 0.
        assert_eq!(small_float(15 << 6, 6), 1.0);
        // 10-bit float 2.0: exponent 16.
        assert_eq!(small_float(16 << 5, 5), 2.0);
        // RGB9E5: mantissa 256 with exponent 16 => 256 * 2^(16-24) = 1.0.
        let v: u32 = 256 | (16 << 27);
        let mut d = [0f32; 4];
        convert_row(PlainFormat::Rgb9E5, &v.to_le_bytes(), &mut d, false);
        assert_eq!(d, [1.0, 0.0, 0.0, 1.0]);
    }

    /// Rounding the f32 palette to the nearest byte must land on the 8-bit
    /// decoder's level (or one either side, where the 8-bit path's fixed-point
    /// rounding breaks a near-tie the other way).
    #[test]
    fn bc4_palette_f32_rounds_to_the_8bit_palette() {
        for signed in [false, true] {
            for a0 in 0..=255u8 {
                for a1 in 0..=255u8 {
                    let f = bc4_palette_f32(a0, a1, signed, Bc4Tables::get());
                    let exact = bc4_palette_f32_div(a0, a1, signed);
                    for k in 0..8 {
                        assert_eq!(
                            f[k].to_bits(),
                            exact[k].to_bits(),
                            "a0={a0} a1={a1} signed={signed} k={k}: table differs from the divide"
                        );
                    }
                    let b = super::super::bcn::bc4_palette_packed(a0, a1, signed).to_le_bytes();
                    for k in 0..8 {
                        let (want, got) = if signed {
                            (b[k] as i8 as f32, f[k] * 127.0)
                        } else {
                            (b[k] as f32, f[k] * 255.0)
                        };
                        assert!(
                            (want - got).abs() <= 0.5 + 1e-3,
                            "a0={a0} a1={a1} signed={signed} k={k}: 8-bit {want}, f32 {got}"
                        );
                    }
                }
            }
        }
    }
}
