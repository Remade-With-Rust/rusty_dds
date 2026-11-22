//! Per-footprint encoder tables, built once per encode call: the legal block
//! modes, the infill table of every weight grid they use, and the
//! deduplicated partitionings. No global state; the caller owns the context.

use alloc::vec::Vec;

use crate::astc::block_mode::{self, ModeClass};
use crate::astc::footprint::AstcFootprint;
use crate::astc::partition::select_partition;
pub(crate) use crate::astc::tables::{build_grid, Grid};

/// A legal block mode for the footprint.
#[derive(Clone, Copy)]
pub(crate) struct Mode {
    pub mode: u16,
    pub grid: u16,
    /// Weight range 0..=11.
    pub range: u8,
    pub dual: bool,
    /// Colour range per (partitions - 1, class value count / 2 - 1), 0xFF if
    /// the configuration does not fit (more than 18 values or too few bits).
    pub crange: [[u8; 4]; 4],
}

/// A distinct, non-degenerate partitioning.
pub(crate) struct Partitioning {
    pub seed: u16,
    pub labels: Vec<u8>,
    /// Texels in partition 1, as a bitset (texel i is bit i % 64 of word i / 64).
    pub bits: [u64; 4],
}

pub(crate) struct Ctx {
    pub texels: usize,
    pub grids: Vec<Grid>,
    pub modes: Vec<Mode>,
    /// Two-partition patterns, deduplicated, both partitions non-empty.
    pub part2: Vec<Partitioning>,
    /// Three- and four-partition patterns (built only when a preset uses
    /// them), every partition non-empty, exact duplicates removed.
    pub part3: Vec<Partitioning>,
    pub part4: Vec<Partitioning>,
    /// Per-fit working buffers, reused across blocks (the context is owned
    /// by one encode call; nothing here outlives it).
    pub scratch: core::cell::RefCell<Scratch>,
}

/// Reusable per-fit buffers of the mode search.
pub(crate) struct Scratch {
    pub dec_err: Vec<f32>,
    pub dec1: Vec<[f32; 64]>,
    pub dec2: Vec<[f32; 64]>,
    pub refined: Vec<bool>,
}

fn build_parts(fp: AstcFootprint, count: u32) -> Vec<Partitioning> {
    let texels = fp.texels() as usize;
    let small = texels < 31;
    let mut out: Vec<Partitioning> = Vec::new();
    for seed in 0..1024u32 {
        let mut labels = Vec::with_capacity(texels);
        for z in 0..fp.depth() {
            for y in 0..fp.height() {
                for x in 0..fp.width() {
                    labels.push(select_partition(seed, x, y, z, count, small));
                }
            }
        }
        let mut seen = [false; 4];
        for &l in &labels {
            seen[l as usize] = true;
        }
        if seen.iter().take(count as usize).any(|s| !s) {
            continue;
        }
        if out.iter().any(|p| p.labels == labels) {
            continue;
        }
        out.push(Partitioning {
            seed: seed as u16,
            labels,
            bits: [0; 4],
        });
    }
    out
}

impl Ctx {
    pub(crate) fn new(fp: AstcFootprint, max_partitions: u8) -> Self {
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
            let mut crange = [[0xFFu8; 4]; 4];
            for (pi, row) in crange.iter_mut().enumerate() {
                let parts = pi as u32 + 1;
                let config = if parts == 1 { 17 } else { 29 } + if g.dual_plane { 2 } else { 0 };
                let avail = 128u32.saturating_sub(config + g.weight_bits());
                for (vi, slot) in row.iter_mut().enumerate() {
                    let values = 2 * (vi as u32 + 1) * parts;
                    if values <= 18 {
                        if let Some(r) = crate::astc::quant::color_range(values, avail) {
                            *slot = r as u8;
                        }
                    }
                }
            }
            modes.push(Mode {
                mode: m as u16,
                grid: gi as u16,
                range: g.range,
                dual: g.dual_plane,
                crange,
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
                let mut bits = [0u64; 4];
                for (i, &l) in labels.iter().enumerate() {
                    if l == 1 {
                        bits[i / 64] |= 1 << (i % 64);
                    }
                }
                part2.push(Partitioning {
                    seed: seed as u16,
                    labels,
                    bits,
                });
            }
        }
        let part3 = if max_partitions >= 3 { build_parts(fp, 3) } else { Vec::new() };
        let part4 = if max_partitions >= 4 { build_parts(fp, 4) } else { Vec::new() };
        let ng = grids.len();
        Ctx {
            texels,
            grids,
            modes,
            part2,
            part3,
            part4,
            scratch: core::cell::RefCell::new(Scratch {
                dec_err: alloc::vec![-1.0; ng],
                dec1: alloc::vec![[0.0; 64]; ng],
                dec2: alloc::vec![[0.0; 64]; ng],
                refined: alloc::vec![false; ng],
            }),
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
            let ctx = Ctx::new(fp, 2);
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
