//! The fast scalar block decoder (plan P1). Same output as
//! [`super::reference`], byte for byte (tested exhaustively over the golden
//! sequences and random blocks); faster by caching per surface what the
//! reference recomputes per texel:
//!
//! - infill as precomputed taps per weight grid (identity for full grids);
//! - partition maps per (count, seed), built on first use;
//! - output conversion in specialised loops.
//!
//! Parsing is shared with the reference ([`super::parse::parse`]); void-extent
//! and illegal blocks go through the reference unchanged.

use alloc::boxed::Box;
use alloc::vec::Vec;

use super::cem;
use super::footprint::AstcFootprint;
use super::parse::{self, BlockKind};
use super::partition::select_partition;
use super::reference::{decode_block, Out};
use super::tables::{build_grid, Grid};
use super::texel::{lerp, lns_to_f16, unorm16_to_f16};
use super::{AstcBlockStatus, AstcProfile};

/// 8-bit output of LDR interpolation from 8-bit endpoints `c0`, `c1`.
///
/// With `X = c0 (64 - w) + c1 w` (at most 255 * 64 = 16_320):
/// - linear: endpoints expand to `257 c`, and
///   `(257 X + 32) >> 6 = 4 X + ((X + 32) >> 6)`, whose top 8 bits are the
///   output; every intermediate fits 16 bits unsigned (at most 65_535);
/// - sRGB: endpoints expand to `256 c + 128`, and
///   `(256 X + 128 * 64 + 32) >> 6 = 4 X + 128`.
///
/// Identical to `lerp(e0, e1, w) >> 8` on the expanded endpoints (tested).
#[inline(always)]
pub(crate) fn u8_from_8bit(c0: u16, c1: u16, w: u8, srgb: bool) -> u16 {
    let w = w as u32;
    let x = c0 as u32 * (64 - w) + c1 as u32 * w;
    if srgb {
        (((x << 2) + 128) >> 8) as u16
    } else {
        (((x << 2) + ((x + 32) >> 6)) >> 8) as u16
    }
}

/// Per-surface decode state; caller-owned, no global state.
pub(crate) struct FastCtx {
    fp: AstcFootprint,
    texels: usize,
    /// Grids seen so far, keyed by (w, h, d).
    grids: Vec<((u8, u8, u8), Grid)>,
    /// Partition maps for counts 2..=4, seed 0..1024, built on first use.
    parts: Vec<Option<Box<[u8]>>>,
    /// Every block mode classified for the footprint.
    modes: Box<[parse::ModeInfo]>,
}

impl FastCtx {
    pub(crate) fn new(fp: AstcFootprint) -> Self {
        Self {
            fp,
            texels: fp.texels() as usize,
            grids: Vec::new(),
            parts: (0..3 * 1024).map(|_| None).collect(),
            modes: (0..2048u16).map(|m| parse::mode_info(m, fp)).collect(),
        }
    }

    fn grid(&mut self, w: u8, h: u8, d: u8) -> usize {
        if let Some(i) = self.grids.iter().position(|(k, _)| *k == (w, h, d)) {
            return i;
        }
        self.grids.push(((w, h, d), build_grid(self.fp, w as u32, h as u32, d as u32)));
        self.grids.len() - 1
    }

    fn partition_map(&mut self, count: u32, seed: u32) -> &[u8] {
        let k = (count as usize - 2) * 1024 + (seed as usize & 1023);
        if self.parts[k].is_none() {
            let fp = self.fp;
            let small = self.texels < 31;
            let mut v = Vec::with_capacity(self.texels);
            for z in 0..fp.depth() {
                for y in 0..fp.height() {
                    for x in 0..fp.width() {
                        v.push(select_partition(seed, x, y, z, count, small));
                    }
                }
            }
            self.parts[k] = Some(v.into_boxed_slice());
        }
        self.parts[k].as_deref().unwrap_or(&[])
    }

    /// Decode one block; identical to [`decode_block`].
    pub(crate) fn decode(
        &mut self,
        block: &[u8; 16],
        profile: AstcProfile,
        out_kind: Out,
        out: &mut [[u16; 4]],
    ) -> AstcBlockStatus {
        let fp = self.fp;
        let b = u128::from_le_bytes(*block);
        let mode = (b & 0x7FF) as u16;
        let s = parse::parse_with(b, mode, fp, self.modes[mode as usize]);
        if s.kind != BlockKind::Normal {
            return decode_block(block, fp, profile, out_kind, out);
        }
        let n = self.texels;
        let parts = s.partitions as usize;
        let mut eps = [cem::Endpoints {
            e0: [0; 4],
            e1: [0; 4],
            lns: [false; 4],
            hdr_in_ldr: false,
        }; 4];
        let mut off = 0;
        let mut any_hdr_in_ldr = false;
        for (p, ep) in eps.iter_mut().enumerate().take(parts) {
            let k = cem::value_count(s.cem[p]);
            let raw = cem::decode(s.cem[p], &s.color_values[off..off + k]);
            off += k;
            *ep = cem::expand(&raw, profile);
            any_hdr_in_ldr |= ep.hdr_in_ldr;
        }
        // Texel weights through the grid's taps.
        let gi = self.grid(s.grid[0], s.grid[1], s.grid[2]);
        let g = &self.grids[gi].1;
        let pw = g.count;
        let mut w1 = [0u8; 216];
        let mut w2 = [0u8; 216];
        let stride = if s.dual_plane { 2 } else { 1 };
        if g.full {
            for t in 0..n {
                w1[t] = s.weights[t * stride];
                if s.dual_plane {
                    w2[t] = s.weights[t * stride + 1];
                }
            }
        } else if !s.dual_plane {
            // One plane: the taps read the stored weights in place (no staging
            // copy), and plane 2 is not interpolated: `w2` is only read for
            // channel `ccs`, and `ccs` is 4 (none) here.
            for (t, &(nt, tp)) in g.taps.iter().enumerate() {
                let mut a = 8u32;
                for &(k, c) in &tp[..nt as usize] {
                    a += s.weights[k as usize] as u32 * c as u32;
                }
                w1[t] = (a >> 4) as u8;
            }
        } else {
            let mut g1 = [0u8; 64];
            let mut g2 = [0u8; 64];
            for i in 0..pw {
                g1[i] = s.weights[i * stride];
                if s.dual_plane {
                    g2[i] = s.weights[i * stride + 1];
                }
            }
            for (t, &(nt, tp)) in g.taps.iter().enumerate() {
                let mut a = 8u32;
                let mut b = 8u32;
                for &(k, c) in &tp[..nt as usize] {
                    a += g1[k as usize] as u32 * c as u32;
                    b += g2[k as usize] as u32 * c as u32;
                }
                w1[t] = (a >> 4) as u8;
                w2[t] = (b >> 4) as u8;
            }
        }
        let ccs = if s.dual_plane { s.plane2_component as usize } else { 4 };
        let pmap: &[u8] = if parts > 1 {
            self.partition_map(parts as u32, s.partition_seed as u32)
        } else {
            &[]
        };
        let out = &mut out[..n];
        match out_kind {
            Out::U8 if matches!(profile, AstcProfile::Ldr | AstcProfile::LdrSrgb) => {
                // 8-bit output under an LDR profile: every endpoint is an 8-bit
                // value c expanded as 257 c (linear) or 256 c + 128 (sRGB),
                // so the interpolation reduces exactly to 16-bit integers (see
                // `u8_from_8bit`); the SIMD twin runs two texels per register.
                let srgb = profile == AstcProfile::LdrSrgb;
                let ends: Ends = core::array::from_fn(|p| (eps[p].e0.map(|v| v >> 8), eps[p].e1.map(|v| v >> 8)));
                let pm: &[u8] = if parts > 1 { pmap } else { &[] };
                #[cfg(feature = "simd")]
                super::simd::u8_texels(&ends, pm, &w1, &w2, ccs, srgb, n, out);
                #[cfg(not(feature = "simd"))]
                u8_texels_scalar(&ends, pm, &w1, &w2, ccs, srgb, n, out);
            }
            Out::U8 => {
                for t in 0..n {
                    let e = &eps[if parts > 1 { pmap[t] as usize } else { 0 }];
                    let mut px = [0u16; 4];
                    for c in 0..4 {
                        let w = if c == ccs { w2[t] } else { w1[t] };
                        px[c] = lerp(e.e0[c], e.e1[c], w) >> 8;
                    }
                    out[t] = px;
                }
            }
            Out::F16 => {
                let srgb = profile == AstcProfile::LdrSrgb;
                for t in 0..n {
                    let e = &eps[if parts > 1 { pmap[t] as usize } else { 0 }];
                    let mut px = [0u16; 4];
                    for c in 0..4 {
                        let w = if c == ccs { w2[t] } else { w1[t] };
                        let v = lerp(e.e0[c], e.e1[c], w);
                        px[c] = if srgb {
                            unorm16_to_f16((v >> 8) * 257)
                        } else if e.lns[c] {
                            lns_to_f16(v)
                        } else {
                            unorm16_to_f16(v)
                        };
                    }
                    out[t] = px;
                }
            }
        }
        if any_hdr_in_ldr {
            AstcBlockStatus::HdrInLdr
        } else {
            AstcBlockStatus::Ok
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u8_from_8bit_equals_the_16bit_lerp() {
        for c0 in 0..=255u16 {
            for c1 in 0..=255u16 {
                for w in 0..=64u8 {
                    let lin = lerp(c0 * 257, c1 * 257, w) >> 8;
                    assert_eq!(u8_from_8bit(c0, c1, w, false), lin);
                    let srgb = lerp((c0 << 8) | 0x80, (c1 << 8) | 0x80, w) >> 8;
                    assert_eq!(u8_from_8bit(c0, c1, w, true), srgb);
                }
            }
        }
    }
}

/// Endpoint pairs (8-bit values, four channels) per partition.
pub(crate) type Ends = [([u16; 4], [u16; 4]); 4];

/// Scalar oracle: 8-bit output for `n` texels.
#[allow(clippy::too_many_arguments)]
pub(crate) fn u8_texels_scalar(
    ends: &Ends,
    pmap: &[u8],
    w1: &[u8; 216],
    w2: &[u8; 216],
    ccs: usize,
    srgb: bool,
    n: usize,
    out: &mut [[u16; 4]],
) {
    for t in 0..n {
        let (a, b) = &ends[if pmap.is_empty() { 0 } else { pmap[t] as usize }];
        out[t] = core::array::from_fn(|c| {
            let w = if c == ccs { w2[t] } else { w1[t] };
            u8_from_8bit(a[c], b[c], w, srgb)
        });
    }
}

/// Weights of texel `t` per channel (plane 2 on channel `ccs`).
#[inline(always)]
#[cfg_attr(not(feature = "simd"), allow(dead_code))]
pub(crate) fn wv(w1: &[u8; 216], w2: &[u8; 216], ccs: usize, t: usize) -> [u16; 4] {
    let a = w1[t] as u16;
    let b = w2[t] as u16;
    [
        if ccs == 0 { b } else { a },
        if ccs == 1 { b } else { a },
        if ccs == 2 { b } else { a },
        if ccs == 3 { b } else { a },
    ]
}

