//! The scalar reference decoder: a direct transcription of the Khronos
//! specification, written for reading, with no caches and no SIMD.
//!
//! This is the oracle every fast path is gated against (byte-identical), in
//! the same role as [`crate::reference`] plays for BCn. It is frozen once the
//! conformance gates pass; speed work happens in the surface drivers.
//!
//! Output rules (the six cells, matching ARM astcenc 5.7.1 bit for bit):
//!
//! | profile      | 8-bit output (`decode_unorm8`)  | half-float output (`decode_float16`) |
//! |--------------|---------------------------------|--------------------------------------|
//! | `LdrSrgb`    | top 8 bits                      | top 8 bits, as `unorm16(c * 257)`    |
//! | `Ldr`        | top 8 bits                      | `C / 65536`, round toward zero       |
//! | `HdrRgbLdrA` | invalid (rejected by the API)   | LDR lanes as `Ldr`, HDR lanes LNS    |
//! | `Hdr`        | invalid (rejected by the API)   | LDR lanes as `Ldr`, HDR lanes LNS    |
//!
//! Error colours: an illegal block, or an HDR void-extent block under an LDR
//! profile, decodes to magenta in 8-bit output and to `0xFFFF` (NaN) in every
//! half-float channel. An HDR endpoint mode under an LDR profile turns only
//! that partition's texels magenta, in both outputs (`0x3C00, 0, 0x3C00, 0x3C00`
//! as halves).

use super::cem;
use super::footprint::AstcFootprint;
use super::infill::{self, GRID_PAD};
use super::parse::{self, BlockKind, IllegalReason};
use super::partition::select_partition;
use super::texel::{lerp, lns_to_f16, unorm16_to_f16};
use super::{AstcBlockStatus, AstcProfile};

use super::AstcDecodeReport;
use crate::{Error, ImageRgba8, ImageRgbaF32};

super::decode::surface_api!(super::decode::Reference);

/// Output flavour of the per-texel conversion.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Out {
    /// 8-bit UNORM (`decode_unorm8`, or any sRGB decode). Values 0..=255.
    U8,
    /// binary16 bit patterns (`decode_float16`).
    F16,
}

const MAGENTA_U8: [u16; 4] = [0xFF, 0, 0xFF, 0xFF];
const NAN_F16: [u16; 4] = [0xFFFF; 4];

/// Decode one block into `out[..fp.texels()]`, texels in (z, y, x) order.
/// U8 values are 0..=255; F16 values are half bit patterns. Total: any 16
/// bytes produce defined texels.
// One index walks several parallel per-texel / per-channel arrays (and in the encoder fixes the floating-point operation order); iterator chains would obscure that.
#[allow(clippy::needless_range_loop)]
pub(crate) fn decode_block(
    block: &[u8; 16],
    fp: AstcFootprint,
    profile: AstcProfile,
    out_kind: Out,
    out: &mut [[u16; 4]],
) -> AstcBlockStatus {
    let texels = fp.texels() as usize;
    let out = &mut out[..texels];
    let ldr_profile = matches!(profile, AstcProfile::Ldr | AstcProfile::LdrSrgb);
    let error = if out_kind == Out::U8 {
        MAGENTA_U8
    } else {
        NAN_F16
    };
    let s = parse::parse(block, fp);
    let convert = |c: u16, lns: bool| -> u16 {
        match out_kind {
            Out::U8 => c >> 8,
            Out::F16 => {
                if profile == AstcProfile::LdrSrgb {
                    unorm16_to_f16((c >> 8) * 257)
                } else if lns {
                    lns_to_f16(c)
                } else {
                    unorm16_to_f16(c)
                }
            }
        }
    };
    match s.kind {
        BlockKind::Illegal(r) => {
            out.fill(error);
            return AstcBlockStatus::Illegal(r);
        }
        BlockKind::VoidHdr => {
            if ldr_profile {
                out.fill(error);
                return AstcBlockStatus::HdrInLdr;
            }
            let px = match out_kind {
                // FP16 constants pass through, except that a signalling NaN is
                // quieted (bit 9 set, payload kept). The specification leaves
                // Inf/NaN constants undefined; this is what astcenc produces
                // (its FP16 -> FP32 -> FP16 round trip quiets them).
                Out::F16 => s.void_color.map(quiet_nan),
                // 8-bit HDR decode is invalid; the API never asks for it. Keep
                // the function total with the error colour.
                Out::U8 => MAGENTA_U8,
            };
            out.fill(px);
            return AstcBlockStatus::Ok;
        }
        BlockKind::VoidLdr => {
            let c = s.void_color;
            let px = [
                convert(c[0], false),
                convert(c[1], false),
                convert(c[2], false),
                convert(c[3], false),
            ];
            out.fill(px);
            return AstcBlockStatus::Ok;
        }
        BlockKind::Normal => {}
    }

    // Endpoints per partition.
    let parts = s.partitions as usize;
    let mut eps = [cem::Endpoints {
        e0: [0; 4],
        e1: [0; 4],
        lns: [false; 4],
        hdr_in_ldr: false,
    }; 4];
    let mut off = 0;
    let mut any_hdr_in_ldr = false;
    for p in 0..parts {
        let n = cem::value_count(s.cem[p]);
        let raw = cem::decode(s.cem[p], &s.color_values[off..off + n]);
        off += n;
        eps[p] = cem::expand(&raw, profile);
        any_hdr_in_ldr |= eps[p].hdr_in_ldr;
    }

    // Weights per plane, then infill.
    let pw = s.plane_weight_count();
    let mut g1 = [0u8; GRID_PAD];
    let mut g2 = [0u8; GRID_PAD];
    if s.dual_plane {
        for i in 0..pw {
            g1[i] = s.weights[2 * i];
            g2[i] = s.weights[2 * i + 1];
        }
    } else {
        g1[..pw].copy_from_slice(&s.weights[..pw]);
    }
    let (gw, gh, gd) = (s.grid[0] as u32, s.grid[1] as u32, s.grid[2] as u32);
    let mut w1 = [0u8; 216];
    let mut w2 = [0u8; 216];
    infill::infill(fp, gw, gh, gd, &g1, &mut w1);
    if s.dual_plane {
        infill::infill(fp, gw, gh, gd, &g2, &mut w2);
    }

    let small = texels < 31;
    let ccs = if s.dual_plane {
        s.plane2_component as usize
    } else {
        4
    };
    let mut i = 0;
    for z in 0..fp.depth() {
        for y in 0..fp.height() {
            for x in 0..fp.width() {
                let p = if parts > 1 {
                    select_partition(s.partition_seed as u32, x, y, z, parts as u32, small) as usize
                } else {
                    0
                };
                let e = &eps[p];
                let mut px = [0u16; 4];
                for c in 0..4 {
                    let w = if c == ccs { w2[i] } else { w1[i] };
                    px[c] = convert(lerp(e.e0[c], e.e1[c], w), e.lns[c]);
                }
                out[i] = px;
                i += 1;
            }
        }
    }
    if any_hdr_in_ldr {
        AstcBlockStatus::HdrInLdr
    } else {
        AstcBlockStatus::Ok
    }
}

/// Quiet a binary16 signalling NaN; every other pattern is unchanged.
#[inline]
fn quiet_nan(h: u16) -> u16 {
    if h & 0x7C00 == 0x7C00 && h & 0x03FF != 0 {
        h | 0x0200
    } else {
        h
    }
}

/// Decode one block to 8-bit RGBA (`decode_unorm8`; sRGB uses its own
/// expansion). `out` must hold at least `fp.texels()` entries. An HDR profile
/// decodes like `Ldr` would for LDR content and gives magenta for HDR content.
pub fn decode_block_rgba8(
    block: &[u8; 16],
    fp: AstcFootprint,
    profile: AstcProfile,
    out: &mut [[u8; 4]],
) -> AstcBlockStatus {
    let mut tmp = [[0u16; 4]; 216];
    let n = (fp.texels() as usize).min(out.len());
    let st = decode_block(block, fp, profile, Out::U8, &mut tmp);
    for (o, t) in out[..n].iter_mut().zip(tmp.iter()) {
        *o = [t[0] as u8, t[1] as u8, t[2] as u8, t[3] as u8];
    }
    st
}

/// Decode one block to binary16 RGBA bit patterns (`decode_float16`).
/// `out` must hold at least `fp.texels()` entries.
pub fn decode_block_f16(
    block: &[u8; 16],
    fp: AstcFootprint,
    profile: AstcProfile,
    out: &mut [[u16; 4]],
) -> AstcBlockStatus {
    let mut tmp = [[0u16; 4]; 216];
    let n = (fp.texels() as usize).min(out.len());
    let st = decode_block(block, fp, profile, Out::F16, &mut tmp);
    out[..n].copy_from_slice(&tmp[..n]);
    st
}

/// The illegal-encoding reason of a block, if any.
pub fn illegal_reason(block: &[u8; 16], fp: AstcFootprint) -> Option<IllegalReason> {
    match parse::parse(block, fp).kind {
        BlockKind::Illegal(r) => Some(r),
        _ => None,
    }
}
