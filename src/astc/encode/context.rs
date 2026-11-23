//! Per-footprint encoder tables, built once per encode call: the legal block
//! modes, the infill table of every weight grid they use, and the
//! deduplicated partitionings. No global state; the caller owns the context.

use alloc::vec::Vec;

use crate::astc::block_mode::{self, ModeClass};
use crate::astc::footprint::AstcFootprint;
use crate::astc::partition::PartitionGen;
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

/// A three- or four-partition pattern's partitions as bitsets (texel i is
/// bit i % 64 of word i / 64) and their sizes: the ranking's confusion
/// matrix comes from popcounts of these instead of a per-texel walk.
pub(crate) struct PartBits {
    pub bits: [[u64; 4]; 4],
    pub cnt: [u32; 4],
}

/// Per-label bitsets and counts of a labelling (labels < 4, at most 256 texels).
pub(crate) fn label_bits(labels: &[u8]) -> PartBits {
    let mut bits = [[0u64; 4]; 4];
    let mut cnt = [0u32; 4];
    for (i, &l) in labels.iter().enumerate() {
        let l = (l & 3) as usize;
        bits[l][(i >> 6) & 3] |= 1 << (i & 63);
        cnt[l] += 1;
    }
    PartBits { bits, cnt }
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
    /// `label_bits` of each `part3` / `part4` pattern, same order.
    pub bits3: Vec<PartBits>,
    pub bits4: Vec<PartBits>,
    /// Per-fit working buffers, reused across blocks (the context is owned
    /// by one encode call; nothing here outlives it).
    pub scratch: core::cell::RefCell<Scratch>,
    /// The block fits, reused across blocks the same way.
    pub fits: core::cell::RefCell<super::block::FitPool>,
    /// Set when the CPU supports AVX2: the eight-lane twins of the float
    /// kernels run (`encode::simd`; bit-identical to the scalar).
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    pub(in crate::astc::encode) avx2: Option<super::simd::Avx2>,
    /// Set when the CPU has POPCNT: the pattern ranking uses it.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    pub(in crate::astc::encode) popcnt: Option<super::simd::Popcnt>,
}

/// Reusable per-fit buffers of the mode search.
pub(crate) struct Scratch {
    pub dec_err: Vec<f32>,
    pub dec1: Vec<[f32; 64]>,
    pub dec2: Vec<[f32; 64]>,
    pub refined: Vec<bool>,
}

/// The distinct `count`-partition patterns with every partition non-empty,
/// and their `label_bits` (same order). Labels are below 4, so two
/// labellings are equal exactly when their per-label bitsets are: the
/// duplicate test compares bitsets (four words per label) instead of label
/// vectors.
fn build_parts(fp: AstcFootprint, count: u32) -> (Vec<Partitioning>, Vec<PartBits>) {
    let texels = fp.texels() as usize;
    let small = texels < 31;
    let mut out: Vec<Partitioning> = Vec::new();
    let mut outb: Vec<PartBits> = Vec::new();
    for seed in 0..1024u32 {
        let gen = PartitionGen::new(seed, count, small);
        let mut labels = Vec::with_capacity(texels);
        for z in 0..fp.depth() {
            for y in 0..fp.height() {
                for x in 0..fp.width() {
                    labels.push(gen.label(x, y, z));
                }
            }
        }
        let lb = label_bits(&labels);
        if lb.cnt.iter().take(count as usize).any(|&n| n == 0) {
            continue;
        }
        if outb.iter().any(|q| q.bits == lb.bits) {
            continue;
        }
        out.push(Partitioning {
            seed: seed as u16,
            labels,
            bits: [0; 4],
        });
        outb.push(lb);
    }
    (out, outb)
}

impl Ctx {
    /// Keep only the modes of the footprint's shortlist needed to cover
    /// `coverage` per mille of blocks (2D footprints with a shortlist; 1000
    /// or more keeps every mode). The grids stay; unreferenced ones are
    /// simply never ranked.
    pub(crate) fn restrict_modes(&mut self, fp: AstcFootprint, coverage: u16) {
        if coverage >= 1000 || fp.depth() != 1 {
            return;
        }
        let Some(&(_, _, list)) = super::shortlist::SHORTLIST
            .iter()
            .find(|e| e.0 as u32 == fp.width() && e.1 as u32 == fp.height())
        else {
            return;
        };
        let keep: Vec<u16> = list
            .iter()
            .take_while(|e| e.1 < coverage)
            .map(|e| e.0)
            .collect();
        let kept: Vec<Mode> = self
            .modes
            .iter()
            .copied()
            .filter(|m| keep.contains(&m.mode))
            .collect();
        // Never leave the search empty.
        if !kept.is_empty() {
            self.modes = kept;
        }
    }

    pub(crate) fn new(fp: AstcFootprint, max_partitions: u8) -> Self {
        let texels = fp.texels() as usize;
        let mut grids: Vec<Grid> = Vec::new();
        let mut modes = Vec::new();
        for m in 0..2048u32 {
            let g = match block_mode::classify(m, fp) {
                ModeClass::Grid(g) if block_mode::check(&g, fp).is_ok() => g,
                _ => continue,
            };
            let gi = match grids
                .iter()
                .position(|x| x.w == g.w && x.h == g.h && x.d == g.d)
            {
                Some(i) => i,
                None => {
                    grids.push(build_grid(fp, g.w as u32, g.h as u32, g.d as u32));
                    grids.len() - 1
                }
            };
            // One mode per (grid, range, dual): the lowest index.
            if modes.iter().any(|x: &Mode| {
                x.grid as usize == gi && x.range == g.range && x.dual == g.dual_plane
            }) {
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
        // The footprint's texels as a bitset.
        let mask: [u64; 4] = core::array::from_fn(|w| {
            let lo = 64 * w;
            match texels.saturating_sub(lo) {
                0 => 0,
                n if n >= 64 => u64::MAX,
                n => (1u64 << n) - 1,
            }
        });
        let mut part2: Vec<Partitioning> = Vec::new();
        for seed in 0..1024u32 {
            let gen = PartitionGen::new(seed, 2, small);
            let mut labels = Vec::with_capacity(texels);
            for z in 0..fp.depth() {
                for y in 0..fp.height() {
                    for x in 0..fp.width() {
                        labels.push(gen.label(x, y, z));
                    }
                }
            }
            // Labels are 0 or 1: the partition-1 bitset is the labelling.
            let mut bits = [0u64; 4];
            for (i, &l) in labels.iter().enumerate() {
                if l == 1 {
                    bits[i / 64] |= 1 << (i % 64);
                }
            }
            let ones: u32 = bits.iter().map(|w| w.count_ones()).sum();
            if ones == 0 || ones as usize == texels {
                continue;
            }
            // Deduplicate, including the complement (same split, swapped
            // labels): equal labels are equal bitsets, all labels differing
            // is the bitset's complement within the footprint's texels.
            let comp: [u64; 4] = core::array::from_fn(|w| bits[w] ^ mask[w]);
            let dup = part2.iter().any(|p| p.bits == bits || p.bits == comp);
            if !dup {
                part2.push(Partitioning {
                    seed: seed as u16,
                    labels,
                    bits,
                });
            }
        }
        let (part3, bits3) = if max_partitions >= 3 {
            build_parts(fp, 3)
        } else {
            (Vec::new(), Vec::new())
        };
        let (part4, bits4) = if max_partitions >= 4 {
            build_parts(fp, 4)
        } else {
            (Vec::new(), Vec::new())
        };
        let ng = grids.len();
        Ctx {
            texels,
            grids,
            modes,
            part2,
            part3,
            part4,
            bits3,
            bits4,
            scratch: core::cell::RefCell::new(Scratch {
                dec_err: alloc::vec![-1.0; ng],
                dec1: alloc::vec![[0.0; 64]; ng],
                dec2: alloc::vec![[0.0; 64]; ng],
                refined: alloc::vec![false; ng],
            }),
            fits: core::cell::RefCell::new(Default::default()),
            // 4x4 (16 texels) stays scalar: there the eight-lane twins
            // measured no faster (3/8 paired wins, 0.979x, Medium, Kodak).
            #[cfg(all(feature = "simd", target_arch = "x86_64"))]
            avx2: if texels > 16 {
                super::simd::Avx2::detect()
            } else {
                None
            },
            #[cfg(all(feature = "simd", target_arch = "x86_64"))]
            popcnt: super::simd::Popcnt::detect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::astc::infill::{infill, GRID_PAD};
    use crate::astc::testgen::Rng;

    /// The bitset duplicate tests keep exactly the patterns the label-vector
    /// comparison kept (two partitions: equal or complementary labels; three
    /// and four: equal labels), in the same order.
    #[test]
    fn pattern_dedup_equals_label_compare() {
        use crate::astc::partition::select_partition;
        let fps = [
            AstcFootprint::new_2d(4, 4).unwrap(),
            AstcFootprint::new_2d(6, 5).unwrap(),
            AstcFootprint::new_2d(8, 8).unwrap(),
            AstcFootprint::ALL_3D[0],
        ];
        for fp in fps {
            let texels = fp.texels() as usize;
            let small = texels < 31;
            let labels_of = |seed: u32, count: u32| -> Vec<u8> {
                let mut l = Vec::new();
                for z in 0..fp.depth() {
                    for y in 0..fp.height() {
                        for x in 0..fp.width() {
                            l.push(select_partition(seed, x, y, z, count, small));
                        }
                    }
                }
                l
            };
            let ctx = Ctx::new(fp, 4);
            let mut want2: Vec<Vec<u8>> = Vec::new();
            let mut seeds2 = Vec::new();
            for seed in 0..1024 {
                let l = labels_of(seed, 2);
                let ones = l.iter().filter(|&&v| v == 1).count();
                if ones == 0 || ones == texels {
                    continue;
                }
                if want2
                    .iter()
                    .any(|p| *p == l || p.iter().zip(&l).all(|(a, b)| a != b))
                {
                    continue;
                }
                want2.push(l);
                seeds2.push(seed as u16);
            }
            let got2: Vec<u16> = ctx.part2.iter().map(|p| p.seed).collect();
            assert_eq!(got2, seeds2, "{fp} two partitions");
            for (count, got) in [(3u32, &ctx.part3), (4, &ctx.part4)] {
                let mut want: Vec<Vec<u8>> = Vec::new();
                let mut seeds = Vec::new();
                for seed in 0..1024 {
                    let l = labels_of(seed, count);
                    if (0..count as u8).any(|k| !l.contains(&k)) || want.contains(&l) {
                        continue;
                    }
                    want.push(l);
                    seeds.push(seed as u16);
                }
                let g: Vec<u16> = got.iter().map(|p| p.seed).collect();
                assert_eq!(g, seeds, "{fp} {count} partitions");
            }
        }
    }

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
                        let s: u32 = tp[..n as usize]
                            .iter()
                            .map(|&(i, c)| grid[i as usize] as u32 * c as u32)
                            .sum();
                        assert_eq!(
                            ((s + 8) >> 4) as u8,
                            want[t],
                            "{fp} grid {}x{}x{} texel {t}",
                            g.w,
                            g.h,
                            g.d
                        );
                    }
                }
            }
        }
    }
}
