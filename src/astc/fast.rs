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

/// Per-surface decode state; caller-owned, no global state.
pub(crate) struct FastCtx {
    fp: AstcFootprint,
    texels: usize,
    /// Grids seen so far, keyed by (w, h, d).
    grids: Vec<((u8, u8, u8), Grid)>,
    /// Partition maps for counts 2..=4, seed 0..1024, built on first use.
    parts: Vec<Option<Box<[u8]>>>,
}

impl FastCtx {
    pub(crate) fn new(fp: AstcFootprint) -> Self {
        Self {
            fp,
            texels: fp.texels() as usize,
            grids: Vec::new(),
            parts: (0..3 * 1024).map(|_| None).collect(),
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
        let s = parse::parse(block, fp);
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
