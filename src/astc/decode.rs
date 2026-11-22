//! Surface drivers: walk the blocks of a surface (row-major x, then y, then
//! z), decode each with a block decoder, and write the cropped texels.
//!
//! The drivers are generic over the block decoder so the public API and the
//! frozen [`super::reference`] module share one driver and differ only in the
//! block kernel.

use core::ops::Range;

use super::footprint::{block_rows, payload_len, AstcFootprint};
use super::reference::Out;
use super::texel::f16_to_f32;
use super::{AstcBlockStatus, AstcDecodeReport, AstcProfile};
use crate::Error;

/// A block decoder: `[u8; 16]` to texels in (z, y, x) order. Stateful, so a
/// kernel can keep per-surface caches; one instance decodes one surface.
pub(crate) trait BlockDecoder {
    fn new(fp: AstcFootprint) -> Self;
    fn decode(
        &mut self,
        block: &[u8; 16],
        fp: AstcFootprint,
        profile: AstcProfile,
        out_kind: Out,
        out: &mut [[u16; 4]],
    ) -> AstcBlockStatus;
}

/// The reference kernel (frozen oracle).
pub(crate) struct Reference;
impl BlockDecoder for Reference {
    fn new(_: AstcFootprint) -> Self {
        Reference
    }
    #[inline]
    fn decode(
        &mut self,
        block: &[u8; 16],
        fp: AstcFootprint,
        profile: AstcProfile,
        out_kind: Out,
        out: &mut [[u16; 4]],
    ) -> AstcBlockStatus {
        super::reference::decode_block(block, fp, profile, out_kind, out)
    }
}

/// The fast scalar kernel, byte-identical to [`Reference`].
pub(crate) struct Fast(super::fast::FastCtx);
impl BlockDecoder for Fast {
    fn new(fp: AstcFootprint) -> Self {
        Fast(super::fast::FastCtx::new(fp))
    }
    #[inline]
    fn decode(
        &mut self,
        block: &[u8; 16],
        _fp: AstcFootprint,
        profile: AstcProfile,
        out_kind: Out,
        out: &mut [[u16; 4]],
    ) -> AstcBlockStatus {
        self.0.decode(block, profile, out_kind, out)
    }
}

/// Destination texel type.
pub(crate) trait Sink: Copy + Default {
    const KIND: Out;
    fn from_value(v: u16) -> Self;
}
impl Sink for u8 {
    const KIND: Out = Out::U8;
    #[inline]
    fn from_value(v: u16) -> Self {
        v as u8
    }
}
impl Sink for u16 {
    const KIND: Out = Out::F16;
    #[inline]
    fn from_value(v: u16) -> Self {
        v
    }
}
impl Sink for f32 {
    const KIND: Out = Out::F16;
    #[inline]
    fn from_value(v: u16) -> Self {
        f16_to_f32(v)
    }
}

fn validate(
    data: &[u8],
    width: u32,
    height: u32,
    depth: u32,
    fp: AstcFootprint,
    profile: AstcProfile,
    kind: Out,
) -> Result<(), Error> {
    if width == 0 || height == 0 || depth == 0 {
        return Err(Error::InvalidField(
            "ASTC surface has a zero dimension".into(),
        ));
    }
    if kind == Out::U8 && matches!(profile, AstcProfile::Hdr | AstcProfile::HdrRgbLdrA) {
        // 8-bit decode in HDR operation is invalid per the specification.
        return Err(Error::UnsupportedFormat);
    }
    let need = payload_len(width, height, depth, fp).ok_or(Error::SizeLimitExceeded {
        limit: usize::MAX,
        at_least: usize::MAX,
    })?;
    if data.len() < need {
        return Err(Error::TruncatedData);
    }
    Ok(())
}

fn texel_count(width: u32, height: u32, depth: u32) -> Result<usize, Error> {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(depth as usize))
        .and_then(|n| n.checked_mul(4))
        .ok_or(Error::OutOfBounds)
}

fn tally(r: &mut AstcDecodeReport, st: AstcBlockStatus) {
    r.blocks += 1;
    match st {
        AstcBlockStatus::Ok => {}
        AstcBlockStatus::Illegal(_) => r.illegal_blocks += 1,
        AstcBlockStatus::HdrInLdr => r.hdr_in_ldr_blocks += 1,
    }
}

/// Decode block rows `rows` of z-slice-block `bzi` into `out`, whose first
/// texel row is texel row `row0` of that slab. `out_rows` texel rows and
/// `out_slices` texel slices are written.
#[allow(clippy::too_many_arguments)]
fn decode_region<D: BlockDecoder, T: Sink>(
    data: &[u8],
    width: u32,
    height: u32,
    depth: u32,
    fp: AstcFootprint,
    profile: AstcProfile,
    rows: Range<u32>,
    out: &mut [T],
) -> AstcDecodeReport {
    let (nbx, nby, nbz) = fp.block_counts(width, height, depth);
    let (bw, bh, bd) = (fp.width(), fp.height(), fp.depth());
    let y0 = rows.start * bh;
    let y1 = (rows.end * bh).min(height);
    let out_h = (y1 - y0) as usize;
    let mut report = AstcDecodeReport::default();
    let mut tmp = [[0u16; 4]; 216];
    let mut dec = D::new(fp);
    for bz in 0..nbz {
        for by in rows.clone() {
            for bx in 0..nbx {
                let idx =
                    ((bz as usize * nby as usize + by as usize) * nbx as usize + bx as usize) * 16;
                let mut blk = [0u8; 16];
                blk.copy_from_slice(&data[idx..idx + 16]);
                let st = dec.decode(&blk, fp, profile, T::KIND, &mut tmp);
                tally(&mut report, st);
                // Copy the visible part of the block row by row: the crop is
                // decided once per block, not per texel.
                let xs = (bx * bw) as usize;
                let vis_w = (width - bx * bw).min(bw) as usize;
                for tz in 0..bd {
                    let z = bz * bd + tz;
                    if z >= depth {
                        break;
                    }
                    for ty in 0..bh {
                        let y = by * bh + ty;
                        if y >= height {
                            break;
                        }
                        let row = (z as usize * out_h + (y - y0) as usize) * width as usize + xs;
                        let src = &tmp[((tz * bh + ty) * bw) as usize..][..vis_w];
                        let dst = &mut out[row * 4..(row + vis_w) * 4];
                        for (d, px) in dst.chunks_exact_mut(4).zip(src) {
                            d[0] = T::from_value(px[0]);
                            d[1] = T::from_value(px[1]);
                            d[2] = T::from_value(px[2]);
                            d[3] = T::from_value(px[3]);
                        }
                    }
                }
            }
        }
    }
    report
}

pub(crate) fn surface_into<D: BlockDecoder, T: Sink>(
    data: &[u8],
    width: u32,
    height: u32,
    depth: u32,
    fp: AstcFootprint,
    profile: AstcProfile,
    out: &mut [T],
) -> Result<AstcDecodeReport, Error> {
    validate(data, width, height, depth, fp, profile, T::KIND)?;
    if out.len() != texel_count(width, height, depth)? {
        return Err(Error::OutOfBounds);
    }
    let rows = 0..block_rows(height, fp);
    Ok(decode_region::<D, T>(
        data, width, height, depth, fp, profile, rows, out,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn rows_into<D: BlockDecoder, T: Sink>(
    data: &[u8],
    width: u32,
    height: u32,
    fp: AstcFootprint,
    profile: AstcProfile,
    rows: Range<u32>,
    out: &mut [T],
) -> Result<AstcDecodeReport, Error> {
    if fp.is_3d() {
        return Err(Error::UnsupportedFormat);
    }
    validate(data, width, height, 1, fp, profile, T::KIND)?;
    let total = block_rows(height, fp);
    if rows.start > rows.end || rows.end > total {
        return Err(Error::OutOfBounds);
    }
    let y0 = rows.start * fp.height();
    let y1 = (rows.end * fp.height()).min(height);
    if out.len() != texel_count(width, y1 - y0, 1)? {
        return Err(Error::OutOfBounds);
    }
    Ok(decode_region::<D, T>(
        data, width, height, 1, fp, profile, rows, out,
    ))
}

/// Generates the public surface API bound to one block decoder.
macro_rules! surface_api {
    ($dec:ty) => {
        /// Decode a 2D surface to 8-bit RGBA. `LdrSrgb` or `Ldr` only (an HDR
        /// profile returns [`Error::UnsupportedFormat`]: 8-bit HDR decode is
        /// invalid in the specification and this crate does not tone-map).
        /// `Ldr` follows the `decode_unorm8` rule. Bytes are as stored: no
        /// sRGB linearisation.
        pub fn decode_rgba8(
            data: &[u8],
            width: u32,
            height: u32,
            fp: AstcFootprint,
            profile: AstcProfile,
        ) -> Result<ImageRgba8, Error> {
            let n = $crate::astc::decode::texel_count_pub(width, height, 1)?;
            let mut pixels = vec![0u8; n];
            decode_rgba8_into(data, width, height, 1, fp, profile, &mut pixels)?;
            Ok(ImageRgba8 {
                width,
                height,
                depth: 1,
                pixels,
            })
        }

        /// Decode a 2D surface to `f32` RGBA: the half-float result widened
        /// exactly. All profiles.
        pub fn decode_rgba_f32(
            data: &[u8],
            width: u32,
            height: u32,
            fp: AstcFootprint,
            profile: AstcProfile,
        ) -> Result<ImageRgbaF32, Error> {
            let n = $crate::astc::decode::texel_count_pub(width, height, 1)?;
            let mut pixels = vec![0f32; n];
            decode_rgba_f32_into(data, width, height, 1, fp, profile, &mut pixels)?;
            Ok(ImageRgbaF32 {
                width,
                height,
                depth: 1,
                pixels,
            })
        }

        /// Decode a surface (2D: `depth == 1`; 3D footprints stack slices
        /// `z = 0..depth`) to 8-bit RGBA into `out` (`width * height * depth *
        /// 4` bytes).
        #[allow(clippy::too_many_arguments)]
        pub fn decode_rgba8_into(
            data: &[u8],
            width: u32,
            height: u32,
            depth: u32,
            fp: AstcFootprint,
            profile: AstcProfile,
            out: &mut [u8],
        ) -> Result<AstcDecodeReport, Error> {
            $crate::astc::decode::surface_into::<$dec, u8>(
                data, width, height, depth, fp, profile, out,
            )
        }

        /// Decode a surface to binary16 RGBA bit patterns (`decode_float16`).
        /// All profiles. This is the output that is bit-exact with astcenc's
        /// half-float output.
        #[allow(clippy::too_many_arguments)]
        pub fn decode_rgba_f16_into(
            data: &[u8],
            width: u32,
            height: u32,
            depth: u32,
            fp: AstcFootprint,
            profile: AstcProfile,
            out: &mut [u16],
        ) -> Result<AstcDecodeReport, Error> {
            $crate::astc::decode::surface_into::<$dec, u16>(
                data, width, height, depth, fp, profile, out,
            )
        }

        /// [`decode_rgba_f16_into`] widened exactly to `f32`.
        #[allow(clippy::too_many_arguments)]
        pub fn decode_rgba_f32_into(
            data: &[u8],
            width: u32,
            height: u32,
            depth: u32,
            fp: AstcFootprint,
            profile: AstcProfile,
            out: &mut [f32],
        ) -> Result<AstcDecodeReport, Error> {
            $crate::astc::decode::surface_into::<$dec, f32>(
                data, width, height, depth, fp, profile, out,
            )
        }

        /// The job-system seam: decode block rows `rows` of a 2D surface into
        /// `out`, which receives only those texel rows (the last block row is
        /// cropped to `height`).
        #[allow(clippy::too_many_arguments)]
        pub fn decode_block_rows_into(
            data: &[u8],
            width: u32,
            height: u32,
            fp: AstcFootprint,
            profile: AstcProfile,
            rows: core::ops::Range<u32>,
            out: &mut [u8],
        ) -> Result<AstcDecodeReport, Error> {
            $crate::astc::decode::rows_into::<$dec, u8>(data, width, height, fp, profile, rows, out)
        }

        /// [`decode_block_rows_into`] with binary16 output.
        #[allow(clippy::too_many_arguments)]
        pub fn decode_block_rows_f16_into(
            data: &[u8],
            width: u32,
            height: u32,
            fp: AstcFootprint,
            profile: AstcProfile,
            rows: core::ops::Range<u32>,
            out: &mut [u16],
        ) -> Result<AstcDecodeReport, Error> {
            $crate::astc::decode::rows_into::<$dec, u16>(
                data, width, height, fp, profile, rows, out,
            )
        }

        /// [`decode_block_rows_into`] with `f32` output.
        #[allow(clippy::too_many_arguments)]
        pub fn decode_block_rows_f32_into(
            data: &[u8],
            width: u32,
            height: u32,
            fp: AstcFootprint,
            profile: AstcProfile,
            rows: core::ops::Range<u32>,
            out: &mut [f32],
        ) -> Result<AstcDecodeReport, Error> {
            $crate::astc::decode::rows_into::<$dec, f32>(
                data, width, height, fp, profile, rows, out,
            )
        }
    };
}
pub(crate) use surface_api;

pub(crate) fn texel_count_pub(width: u32, height: u32, depth: u32) -> Result<usize, Error> {
    texel_count(width, height, depth)
}
