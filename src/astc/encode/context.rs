//! Per-footprint encoder tables, built once per encode call: the legal block
//! modes, the infill table of every weight grid they use, and the
//! deduplicated partitionings. No global state; the caller owns the context.

use alloc::vec::Vec;

use crate::astc::block_mode::{self, ModeClass};
use crate::astc::footprint::AstcFootprint;
use crate::astc::partition::select_partition;

/// Infill of one weight grid: for each texel up to four (stored-weight index,
/// contribution 0..=16) pairs, so that the texel's weight is
/// `(sum(grid[idx] * contrib) + 8) >> 4`, exactly the specification's infill.
pub(crate) struct Grid {
    pub w: u8,
    pub h: u8,
    pub d: u8,
    /// Stored weights per plane.
    pub count: usize,
    /// `taps[texel]` = (number of taps, [(index, contribution); 4]).
    pub taps: Vec<(u8, [(u8, u8); 4])>,
    /// Transpose: for each stored weight, the texels it feeds and how much.
    pub feeds: Vec<Vec<(u8, u8)>>,
    /// The grid equals the footprint (infill is the identity).
    pub full: bool,
}

/// A legal block mode for the footprint.
#[derive(Clone, Copy)]
pub(crate) struct Mode {
    pub mode: u16,
    pub grid: u16,
    /// Weight range 0..=11.
    pub range: u8,
    pub dual: bool,
    pub weight_bits: u8,
}

/// A distinct, non-degenerate partitioning.
pub(crate) struct Partitioning {
    pub seed: u16,
    pub labels: Vec<u8>,
}

pub(crate) struct Ctx {
    pub texels: usize,
    pub grids: Vec<Grid>,
    pub modes: Vec<Mode>,
    /// Two-partition patterns, deduplicated, both partitions non-empty.
    pub part2: Vec<Partitioning>,
}

fn build_grid(fp: AstcFootprint, gw: u32, gh: u32, gd: u32) -> Grid {
    let (bw, bh, bd) = (fp.width(), fp.height(), fp.depth());
    let ds = (1024 + bw / 2) / (bw - 1);
    let dt = (1024 + bh / 2) / (bh - 1);
    let dr = if bd > 1 { (1024 + bd / 2) / (bd - 1) } else { 0 };
    let count = (gw * gh * gd) as usize;
    let mut taps = Vec::with_capacity(fp.texels() as usize);
    for r in 0..bd {
        for t in 0..bh {
            for s in 0..bw {
                let gs = (ds * s * (gw - 1) + 32) >> 6;
                let gt = (dt * t * (gh - 1) + 32) >> 6;
                let (js, fs) = (gs >> 4, gs & 0xF);
                let (jt, ft) = (gt >> 4, gt & 0xF);
                let raw: [(u32, u32); 4] = if bd == 1 {
                    let v0 = js + jt * gw;
                    let w11 = (fs * ft + 8) >> 4;
                    [
                        (v0, 16 + w11 - fs - ft),
                        (v0 + 1, fs - w11),
                        (v0 + gw, ft - w11),
                        (v0 + gw + 1, w11),
                    ]
                } else {
                    let gr = (dr * r * (gd - 1) + 32) >> 6;
                    let (jr, fr) = (gr >> 4, gr & 0xF);
                    let wh = gw * gh;
                    let v0 = js + jt * gw + jr * wh;
                    let (s1, s2, w) = if fs > ft && ft > fr {
                        (1, gw, [16 - fs, fs - ft, ft - fr, fr])
                    } else if ft >= fs && fs > fr {
                        (gw, 1, [16 - ft, ft - fs, fs - fr, fr])
                    } else if fs > fr && fr >= ft {
                        (1, wh, [16 - fs, fs - fr, fr - ft, ft])
                    } else if fr >= fs && fs > ft {
                        (wh, 1, [16 - fr, fr - fs, fs - ft, ft])
                    } else if ft > fr && fr >= fs {
                        (gw, wh, [16 - ft, ft - fr, fr - fs, fs])
                    } else {
                        (wh, gw, [16 - fr, fr - ft, ft - fs, fs])
                    };
                    [
                        (v0, w[0]),
                        (v0 + s1, w[1]),
                        (v0 + s1 + s2, w[2]),
                        (v0 + wh + gw + 1, w[3]),
                    ]
                };
                // Merge duplicate indices and drop zero contributions; a zero
                // tap may point past the grid, a non-zero one never does.
                let mut out = [(0u8, 0u8); 4];
                let mut n = 0usize;
                for &(idx, c) in raw.iter() {
                    if c == 0 {
                        continue;
                    }
                    debug_assert!((idx as usize) < count);
                    if let Some(e) = out[..n].iter_mut().find(|e| e.0 as u32 == idx) {
                        e.1 += c as u8;
                    } else {
                        out[n] = (idx as u8, c as u8);
                        n += 1;
                    }
                }
                taps.push((n as u8, out));
            }
        }
    }
    let mut feeds = alloc::vec![Vec::new(); count];
    for (t, &(n, tp)) in taps.iter().enumerate() {
        for &(idx, c) in &tp[..n as usize] {
            feeds[idx as usize].push((t as u8, c));
        }
    }
    Grid {
        w: gw as u8,
        h: gh as u8,
        d: gd as u8,
        count,
        taps,
        feeds,
        full: gw == bw && gh == bh && gd == bd,
    }
}

impl Ctx {
    pub(crate) fn new(fp: AstcFootprint) -> Self {
        let texels = fp.texels() as usize;
        let mut grids: Vec<Grid> = Vec::new();
        let mut modes = Vec::new();
        for m in 0..2048u32 {
            let g = match block_mode::classify(m, fp) {
                ModeClass::Grid(g) if block_mode::check(&g, fp).is_ok() => g,
                _ => continue,
            };
            let gi = match grids.iter().position(|x| x.w == g.w && x.h == g.h && x.d == g.d) {
                Some(i) => i,
                None => {
                    grids.push(build_grid(fp, g.w as u32, g.h as u32, g.d as u32));
                    grids.len() - 1
                }
            };
            // One mode per (grid, range, dual): the lowest index.
            if modes
                .iter()
                .any(|x: &Mode| x.grid as usize == gi && x.range == g.range && x.dual == g.dual_plane)
            {
                continue;
            }
            modes.push(Mode {
                mode: m as u16,
                grid: gi as u16,
                range: g.range,
                dual: g.dual_plane,
                weight_bits: g.weight_bits() as u8,
            });
        }
        let small = texels < 31;
        let mut part2: Vec<Partitioning> = Vec::new();
        for seed in 0..1024u32 {
            let mut labels = Vec::with_capacity(texels);
            for z in 0..fp.depth() {
                for y in 0..fp.height() {
                    for x in 0..fp.width() {
                        labels.push(select_partition(seed, x, y, z, 2, small));
                    }
                }
            }
            let ones = labels.iter().filter(|&&l| l == 1).count();
            if ones == 0 || ones == texels {
                continue;
            }
            // Deduplicate, including the complement (same split, swapped labels).
            let dup = part2.iter().any(|p| {
                p.labels == labels || p.labels.iter().zip(&labels).all(|(a, b)| *a != *b)
            });
            if !dup {
                part2.push(Partitioning {
                    seed: seed as u16,
                    labels,
                });
            }
        }
        Ctx {
            texels,
            grids,
            modes,
            part2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::astc::infill::{infill, GRID_PAD};
    use crate::astc::testgen::Rng;

    /// The tap tables reproduce the reference infill for every grid of every
    /// footprint, with random stored weights.
    #[test]
    fn taps_equal_reference_infill() {
        let mut rng = Rng::new(42);
        for &fp in AstcFootprint::ALL_2D.iter().chain(AstcFootprint::ALL_3D) {
            let ctx = Ctx::new(fp);
            assert!(!ctx.modes.is_empty() && !ctx.part2.is_empty());
            for g in &ctx.grids {
                for _ in 0..4 {
                    let mut grid = [0u8; GRID_PAD];
                    for v in grid[..g.count].iter_mut() {
                        *v = rng.below(65) as u8;
                    }
                    let mut want = [0u8; 216];
                    infill(fp, g.w as u32, g.h as u32, g.d as u32, &grid, &mut want);
                    for (t, &(n, tp)) in g.taps.iter().enumerate() {
                        let s: u32 = tp[..n as usize].iter().map(|&(i, c)| grid[i as usize] as u32 * c as u32).sum();
                        assert_eq!(((s + 8) >> 4) as u8, want[t], "{fp} grid {}x{}x{} texel {t}", g.w, g.h, g.d);
                    }
                }
            }
        }
    }
}
