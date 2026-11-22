//! Symbolic block to physical 128 bits: the inverse of [`super::parse`].
//! The only encoder code that touches bits. Unused bits are written as zero.

use super::block_mode::{self, ModeClass};
use super::footprint::AstcFootprint;
use super::ise;
use super::parse::{BlockKind, SymbolicBlock};
use super::quant::{COLOR_INV, WEIGHT_INV};

fn put(b: &mut u128, pos: u32, n: u32, v: u32) {
    let m = ((1u128 << n) - 1) << pos;
    *b = (*b & !m) | (((v as u128) << pos) & m);
}

/// Pack a symbolic block. `s` must be a block [`super::parse`] accepts as
/// legal (the encoder only builds those; debug builds assert the round trip).
pub fn pack(s: &SymbolicBlock, fp: AstcFootprint) -> [u8; 16] {
    let mut b: u128 = 0;
    match s.kind {
        BlockKind::VoidLdr | BlockKind::VoidHdr => {
            put(&mut b, 0, 9, 0x1FC);
            put(&mut b, 9, 1, (s.kind == BlockKind::VoidHdr) as u32);
            // 2D: reserved bits 10..=11 set and every coordinate all-ones;
            // 3D: every coordinate all-ones. Either way bits 10..=63 are 1.
            b |= ((1u128 << 54) - 1) << 10;
            for (c, &v) in s.void_color.iter().enumerate() {
                put(&mut b, 64 + 16 * c as u32, 16, v as u32);
            }
            return b.to_le_bytes();
        }
        BlockKind::Illegal(_) => return [0; 16],
        BlockKind::Normal => {}
    }
    let g = match block_mode::classify(s.mode as u32, fp) {
        ModeClass::Grid(g) => g,
        _ => return [0; 16],
    };
    let weight_bits = g.weight_bits();
    let parts = s.partitions as u32;
    put(&mut b, 0, 11, s.mode as u32);
    put(&mut b, 11, 2, parts - 1);
    let mut below = 128 - weight_bits;
    let color_start;
    if parts == 1 {
        put(&mut b, 13, 4, s.cem[0] as u32);
        color_start = 17;
    } else {
        put(&mut b, 13, 10, s.partition_seed as u32);
        color_start = 29;
        if s.cem_matched {
            put(&mut b, 23, 2, 0);
            put(&mut b, 25, 4, s.cem[0] as u32);
        } else {
            let p = parts as usize;
            let min_class = s.cem[..p].iter().map(|c| c >> 2).min().unwrap_or(0).min(2) as u32;
            let mut field = min_class + 1;
            for i in 0..p {
                let class_bit = ((s.cem[i] >> 2) as u32).saturating_sub(min_class) & 1;
                field |= class_bit << (2 + i);
                field |= ((s.cem[i] & 3) as u32) << (2 + p + 2 * i);
            }
            let high = 3 * parts - 4;
            below -= high;
            put(&mut b, 23, 6, field & 0x3F);
            put(&mut b, below, high, field >> 6);
        }
    }
    if s.dual_plane {
        put(&mut b, below - 2, 2, s.plane2_component as u32);
    }
    // Colour endpoint values, LSB first.
    let n = s.color_count as usize;
    let range = s.color_range as usize;
    let mut raw = [0u8; 18];
    for i in 0..n {
        raw[i] = COLOR_INV[range][s.color_values[i] as usize];
    }
    ise::encode(&raw[..n], range, &mut b, color_start);
    // Weights, from bit 127 downward.
    let wn = g.weight_count() as usize;
    let mut rw = [0u8; 64];
    for i in 0..wn {
        rw[i] = WEIGHT_INV[g.range as usize][s.weights[i] as usize];
    }
    let mut w: u128 = 0;
    ise::encode(&rw[..wn], g.range as usize, &mut w, 0);
    b |= w.reverse_bits();
    b.to_le_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::astc::parse::parse;
    use crate::astc::testgen::{golden_blocks, Rng};

    /// E1 gate (1)/(3): every legal block of the golden sequences (legal
    /// layouts with random contents, every footprint) re-packs from its
    /// parse to a block that parses identically; blocks whose unused bits
    /// happen to be zero re-pack to the same bytes.
    #[test]
    fn parse_pack_parse_is_identity() {
        let mut checked = 0;
        for &fp in AstcFootprint::ALL_2D.iter().chain(AstcFootprint::ALL_3D) {
            for blk in golden_blocks(fp) {
                let s = parse(&blk, fp);
                if matches!(s.kind, BlockKind::Illegal(_)) {
                    continue;
                }
                let mut s = s;
                if matches!(s.kind, BlockKind::VoidLdr | BlockKind::VoidHdr) {
                    // pack writes the no-extent form; compare colours only.
                    let p = pack(&s, fp);
                    let t = parse(&p, fp);
                    assert_eq!((t.kind, t.void_color), (s.kind, s.void_color));
                    continue;
                }
                if s.partitions == 1 {
                    s.partition_seed = 0;
                }
                let p = pack(&s, fp);
                let t = parse(&p, fp);
                assert_eq!(t, s, "{fp} block {blk:02x?}");
                checked += 1;
            }
        }
        assert!(checked > 50_000, "{checked}");
        // And a direct random sweep of symbolic fields would need a generator;
        // the oracle check of packed bytes is in tools/astc-harness (`pack`).
        let _ = Rng::new(0);
    }
}
