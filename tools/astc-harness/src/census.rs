//! Census (plan section 6.2): which classes of block have decoded bit-exactly
//! in all six output cells, and how many times.
//!
//! Cells are marginal per footprint for the structural axes (kind, partition
//! count, CEM per partition count, CEM selector, planes, weight range, colour
//! range, grid full/decimated, block-mode layout row) and global for the
//! endpoint sub-modes (HDR mode 7/11/15 sub-modes, LDR branch directions),
//! whose reachability depends on colour range rather than footprint.
//! The structural universe per footprint is enumerated exactly from the
//! block-mode table and the colour bit budget; the sub-mode universe is listed.

use std::collections::{BTreeMap, BTreeSet};

use rusty_dds::astc::internals as ai;
use rusty_dds::astc::{AstcFootprint, IllegalReason};

pub fn illegal_name(r: IllegalReason) -> &'static str {
    match r {
        IllegalReason::ReservedBlockMode => "reserved-mode",
        IllegalReason::VoidExtentReservedBits => "void-reserved-bits",
        IllegalReason::VoidExtentBadCoords => "void-bad-coords",
        IllegalReason::TooManyWeights => "too-many-weights",
        IllegalReason::WeightBitsTooMany => "weight-bits-high",
        IllegalReason::WeightBitsTooFew => "weight-bits-low",
        IllegalReason::GridLargerThanFootprint => "grid-too-large",
        IllegalReason::TooManyColorIntegers => "too-many-color-ints",
        IllegalReason::ColorBitsTooFew => "color-bits-low",
        IllegalReason::DualPlaneFourPartitions => "dual-plane-4-parts",
        _ => "other",
    }
}

/// Layout row of a block mode (2D: 0..=9, 3D: 0..=6), numbered as the
/// specification's tables list them.
pub fn layout_row(m: u32, fp: AstcFootprint) -> u8 {
    if m & 3 != 0 {
        if fp.is_3d() {
            return 0;
        }
        return match (m >> 2) & 3 {
            0 => 0,
            1 => 1,
            2 => 2,
            _ => {
                if (m >> 8) & 1 == 0 {
                    3
                } else {
                    4
                }
            }
        };
    }
    let a = (m >> 7) & 3;
    let b = (m >> 5) & 3;
    if fp.is_3d() {
        match a {
            0 => 1,
            1 => 2,
            2 => 3,
            _ => 4 + b.min(2) as u8,
        }
    } else {
        match a {
            0 => 5,
            1 => 6,
            2 => 9,
            _ => 7 + b.min(1) as u8,
        }
    }
}

/// Structural keys of a block (per footprint).
pub fn structural_keys(blk: &[u8; 16], fp: AstcFootprint, out: &mut Vec<String>) {
    let s = ai::parse(blk, fp);
    match s.kind {
        ai::BlockKind::Illegal(r) => out.push(format!("kind:illegal:{}", illegal_name(r))),
        ai::BlockKind::VoidLdr => out.push("kind:void-ldr".into()),
        ai::BlockKind::VoidHdr => out.push("kind:void-hdr".into()),
        ai::BlockKind::Normal => {
            out.push("kind:normal".into());
            let p = s.partitions;
            out.push(format!("parts:{p}"));
            for i in 0..p as usize {
                out.push(format!("cem:{p}:{}", s.cem[i]));
            }
            if p > 1 {
                let sel = if s.cem_matched {
                    "same".to_string()
                } else {
                    format!("class{}", s.cem[..p as usize].iter().map(|c| c >> 2).min().unwrap())
                };
                out.push(format!("cemsel:{sel}"));
            }
            out.push(if s.dual_plane {
                format!("plane:dual{}", s.plane2_component)
            } else {
                "plane:single".into()
            });
            out.push(format!("wrange:{}", s.weight_range));
            out.push(format!("crange:{}", s.color_range));
            let full = s.grid[0] as u32 == fp.width() && s.grid[1] as u32 == fp.height() && s.grid[2] as u32 == fp.depth();
            out.push(format!("grid:{}", if full { "full" } else { "decimated" }));
            out.push(format!("row:{}", layout_row(s.mode as u32, fp)));
            let hdr = s.cem[..p as usize].iter().any(|&c| matches!(c, 2 | 3 | 7 | 11 | 14 | 15));
            let ldr = s.cem[..p as usize].iter().any(|&c| !matches!(c, 2 | 3 | 7 | 11 | 14 | 15));
            if hdr && ldr {
                out.push("cause:hdr-and-ldr-partitions".into());
            }
        }
    }
}

/// Endpoint sub-mode keys (global).
pub fn submode_keys(blk: &[u8; 16], fp: AstcFootprint, out: &mut Vec<String>) {
    let s = ai::parse(blk, fp);
    if s.kind != ai::BlockKind::Normal {
        return;
    }
    let mut off = 0;
    for i in 0..s.partitions as usize {
        let cem = s.cem[i];
        let n = ai::value_count(cem);
        let v: Vec<i32> = s.color_values[off..off + n].iter().map(|&x| x as i32).collect();
        off += n;
        match cem {
            7 => {
                let mv = ((v[0] & 0xC0) >> 6) | ((v[1] & 0x80) >> 5) | ((v[2] & 0x80) >> 4);
                out.push(format!("hdr7:modeval{mv}"));
            }
            11 | 14 | 15 => {
                let maj = ((v[4] & 0x80) >> 7) | ((v[5] & 0x80) >> 6);
                if maj == 3 {
                    out.push("hdr11:direct".into());
                } else {
                    let mode = ((v[1] & 0x80) >> 7) | ((v[2] & 0x80) >> 6) | ((v[3] & 0x80) >> 5);
                    out.push(format!("hdr11:maj{maj}:mode{mode}"));
                }
                if cem == 15 {
                    let am = ((v[6] >> 7) & 1) | ((v[7] >> 6) & 2);
                    out.push(format!("hdr15:alpha{am}"));
                }
            }
            2 => out.push(format!("hdr2:{}", if v[1] >= v[0] { "ordered" } else { "swapped" })),
            3 => out.push(format!("hdr3:m{}", (v[0] >> 7) & 1)),
            8 | 12 => {
                let s0 = v[0] + v[2] + v[4];
                let s1 = v[1] + v[3] + v[5];
                out.push(format!("ldr{cem}:{}", if s1 >= s0 { "direct" } else { "blue-contract" }));
            }
            9 | 13 => {
                let t = |a: i32| {
                    let a = (a >> 1) & 0x3F;
                    if a & 0x20 != 0 {
                        a - 0x40
                    } else {
                        a
                    }
                };
                let sum = t(v[1]) + t(v[3]) + t(v[5]);
                out.push(format!("ldr{cem}:{}", if sum >= 0 { "direct" } else { "blue-contract" }));
            }
            1 => {
                let l0 = (v[0] >> 2) | (v[1] & 0xC0);
                out.push(format!("ldr1:{}", if l0 + (v[1] & 0x3F) > 255 { "clamped" } else { "free" }));
            }
            _ => {}
        }
    }
}

pub fn submode_universe() -> Vec<String> {
    let mut u = Vec::new();
    for mv in 0..16 {
        u.push(format!("hdr7:modeval{mv}"));
    }
    u.push("hdr11:direct".into());
    for maj in 0..3 {
        for mode in 0..8 {
            u.push(format!("hdr11:maj{maj}:mode{mode}"));
        }
    }
    for am in 0..4 {
        u.push(format!("hdr15:alpha{am}"));
    }
    u.push("hdr2:ordered".into());
    u.push("hdr2:swapped".into());
    u.push("hdr3:m0".into());
    u.push("hdr3:m1".into());
    for c in [8, 12, 9, 13] {
        u.push(format!("ldr{c}:direct"));
        u.push(format!("ldr{c}:blue-contract"));
    }
    u.push("ldr1:clamped".into());
    u.push("ldr1:free".into());
    u
}

/// The structural cells reachable for a footprint, by enumeration.
pub fn structural_universe(fp: AstcFootprint) -> BTreeSet<String> {
    let mut u = BTreeSet::new();
    u.insert("kind:normal".to_string());
    u.insert("kind:void-ldr".into());
    u.insert("kind:void-hdr".into());
    u.insert("kind:illegal:reserved-mode".into());
    u.insert("kind:illegal:void-bad-coords".into());
    if !fp.is_3d() {
        u.insert("kind:illegal:void-reserved-bits".into());
    }
    let classes = |p: u32| -> Vec<(Vec<u8>, bool)> {
        // (cems, matched) configurations for p partitions.
        let mut v = Vec::new();
        if p == 1 {
            for c in 0..16u8 {
                v.push((vec![c], false));
            }
            return v;
        }
        for c in 0..16u8 {
            v.push((vec![c; p as usize], true));
        }
        for base in 0..3u8 {
            let combos = 1u32 << p;
            for cls in 0..combos {
                for low in 0..(1u32 << (2 * p)) {
                    let cems: Vec<u8> = (0..p)
                        .map(|i| (((base + ((cls >> i) & 1) as u8) << 2) | ((low >> (2 * i)) & 3) as u8))
                        .collect();
                    v.push((cems, false));
                }
            }
        }
        v
    };
    let cfgs: Vec<Vec<(Vec<u8>, bool)>> = (1..=4).map(classes).collect();
    for m in 0..2048u32 {
        match ai::classify(m, fp) {
            ai::ModeClass::Grid(g) => {
                if let Err(e) = ai::check(&g, fp) {
                    let name = match e {
                        ai::GridIllegal::TooManyWeights => "too-many-weights",
                        ai::GridIllegal::WeightBitsTooMany => "weight-bits-high",
                        ai::GridIllegal::WeightBitsTooFew => "weight-bits-low",
                        ai::GridIllegal::GridLargerThanFootprint => "grid-too-large",
                    };
                    u.insert(format!("kind:illegal:{name}"));
                    continue;
                }
                let wb = g.weight_bits();
                for p in 1..=4u32 {
                    if g.dual_plane && p == 4 {
                        u.insert("kind:illegal:dual-plane-4-parts".into());
                        continue;
                    }
                    for (cems, matched) in &cfgs[p as usize - 1] {
                        let count: u32 = cems.iter().map(|&c| ai::value_count(c) as u32).sum();
                        if count > 18 {
                            u.insert("kind:illegal:too-many-color-ints".into());
                            continue;
                        }
                        let config = if p == 1 { 17 } else if *matched { 29 } else { 25 + 3 * p };
                        let bits = 128u32.saturating_sub(config + wb + if g.dual_plane { 2 } else { 0 });
                        let mut range = None;
                        for r in (4..21).rev() {
                            if ai::sequence_bits(count, r) <= bits {
                                range = Some(r);
                                break;
                            }
                        }
                        let Some(r) = range else {
                            u.insert("kind:illegal:color-bits-low".into());
                            continue;
                        };
                        u.insert(format!("parts:{p}"));
                        for &c in cems {
                            u.insert(format!("cem:{p}:{c}"));
                        }
                        if p > 1 {
                            u.insert(if *matched {
                                "cemsel:same".into()
                            } else {
                                format!("cemsel:class{}", cems.iter().map(|c| c >> 2).min().unwrap())
                            });
                        }
                        u.insert(format!("crange:{r}"));
                        let hdr = cems.iter().any(|&c| matches!(c, 2 | 3 | 7 | 11 | 14 | 15));
                        let ldr = cems.iter().any(|&c| !matches!(c, 2 | 3 | 7 | 11 | 14 | 15));
                        if hdr && ldr {
                            u.insert("cause:hdr-and-ldr-partitions".into());
                        }
                    }
                }
                u.insert(if g.dual_plane {
                    "plane:dual0".into()
                } else {
                    "plane:single".into()
                });
                if g.dual_plane {
                    for c in 1..4 {
                        u.insert(format!("plane:dual{c}"));
                    }
                }
                u.insert(format!("wrange:{}", g.range));
                let full = g.w as u32 == fp.width() && g.h as u32 == fp.height() && g.d as u32 == fp.depth();
                u.insert(format!("grid:{}", if full { "full" } else { "decimated" }));
                u.insert(format!("row:{}", layout_row(m, fp)));
            }
            _ => {}
        }
    }
    u
}

#[derive(Default)]
pub struct Census {
    pub per_fp: BTreeMap<AstcFootprint, BTreeMap<String, u64>>,
    pub global: BTreeMap<String, u64>,
}

impl Census {
    pub fn add(&mut self, blk: &[u8; 16], fp: AstcFootprint) {
        let mut keys = Vec::new();
        structural_keys(blk, fp, &mut keys);
        let m = self.per_fp.entry(fp).or_default();
        for k in keys.drain(..) {
            *m.entry(k).or_default() += 1;
        }
        submode_keys(blk, fp, &mut keys);
        for k in keys {
            *self.global.entry(k).or_default() += 1;
        }
    }

    /// Markdown report; returns (cells, cells with >= min blocks).
    pub fn report(&self, min: u64, md: &mut String) -> (usize, usize) {
        let mut cells = 0;
        let mut full = 0;
        md.push_str(&format!(
            "| Footprint | Reachable cells | Cells with >= {min} exact blocks | Under-filled |\n|---|---|---|---|\n"
        ));
        let empty = BTreeMap::new();
        for (fp, _) in self.per_fp.iter() {
            let uni = structural_universe(*fp);
            let got = self.per_fp.get(fp).unwrap_or(&empty);
            let mut under = Vec::new();
            for k in &uni {
                let n = *got.get(k).unwrap_or(&0);
                cells += 1;
                if n >= min {
                    full += 1;
                } else {
                    under.push(format!("{k}={n}"));
                }
            }
            let extra: Vec<&String> = got.keys().filter(|k| !uni.contains(*k)).collect();
            md.push_str(&format!(
                "| {fp} | {} | {} | {}{} |\n",
                uni.len(),
                uni.len() - under.len(),
                under.join(", "),
                if extra.is_empty() { String::new() } else { format!(" (outside universe: {extra:?})") }
            ));
        }
        md.push_str(&format!("\n| Endpoint sub-mode (all footprints) | Exact blocks |\n|---|---|\n"));
        for k in submode_universe() {
            let n = *self.global.get(&k).unwrap_or(&0);
            cells += 1;
            if n >= min {
                full += 1;
            }
            md.push_str(&format!("| {k} | {n} |\n"));
        }
        (cells, full)
    }
}
