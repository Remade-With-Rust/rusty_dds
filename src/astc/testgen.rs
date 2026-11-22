//! Deterministic block generator shared by the golden-vector test and the
//! harness that produces the goldens from the astcenc oracle, so both see the
//! same blocks. Test and dev builds only.

use super::block_mode::{self, ModeClass};
use super::footprint::AstcFootprint;

/// splitmix64.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
    pub fn block(&mut self) -> [u8; 16] {
        let a = self.next_u64().to_le_bytes();
        let b = self.next_u64().to_le_bytes();
        let mut o = [0u8; 16];
        o[..8].copy_from_slice(&a);
        o[8..].copy_from_slice(&b);
        o
    }
}

fn set_bits(b: &mut u128, pos: u32, n: u32, v: u128) {
    let m = ((1u128 << n) - 1) << pos;
    *b = (*b & !m) | ((v << pos) & m);
}

/// Legal (mode, dual-plane) pairs for a footprint.
pub fn legal_modes(fp: AstcFootprint) -> Vec<(u16, bool)> {
    (0..2048u32)
        .filter_map(|m| match block_mode::classify(m, fp) {
            ModeClass::Grid(g) if block_mode::check(&g, fp).is_ok() => Some((m as u16, g.dual_plane)),
            _ => None,
        })
        .collect()
}

/// A random block biased toward legal layouts: a legal block mode, a
/// partition count it allows, random remaining bits; 1 in 16 is a void-extent
/// block (LDR or HDR, sometimes with a valid extent, sometimes illegal).
pub fn mixed(rng: &mut Rng, fp: AstcFootprint, legal: &[(u16, bool)]) -> [u8; 16] {
    let mut b = u128::from_le_bytes(rng.block());
    if rng.below(16) == 0 {
        let hdr = rng.below(2) as u128;
        set_bits(&mut b, 0, 9, 0x1FC);
        set_bits(&mut b, 9, 1, hdr);
        if !fp.is_3d() {
            let rsv = if rng.below(8) == 0 { rng.below(4) as u128 } else { 3 };
            set_bits(&mut b, 10, 2, rsv);
            match rng.below(3) {
                0 => set_bits(&mut b, 12, 52, (1u128 << 52) - 1),
                1 => {
                    let (s0, t0) = (rng.below(4096) as u128, rng.below(4096) as u128);
                    set_bits(&mut b, 12, 13, s0);
                    set_bits(&mut b, 25, 13, s0 + 1 + rng.below(4000) as u128);
                    set_bits(&mut b, 38, 13, t0);
                    set_bits(&mut b, 51, 13, t0 + 1 + rng.below(4000) as u128);
                }
                _ => {}
            }
        } else if rng.below(2) == 0 {
            set_bits(&mut b, 10, 54, (1u128 << 54) - 1);
        }
        if hdr == 1 && rng.below(2) == 0 {
            for c in 0..4 {
                let h = rng.below(0x7C00) | (rng.below(2) << 15);
                set_bits(&mut b, 64 + 16 * c, 16, h as u128);
            }
        }
        return b.to_le_bytes();
    }
    let (mode, dual) = legal[rng.below(legal.len() as u64) as usize];
    set_bits(&mut b, 0, 11, mode as u128);
    let parts = if dual { rng.below(3) } else { rng.below(4) };
    set_bits(&mut b, 11, 2, parts as u128);
    b.to_le_bytes()
}

/// Blocks per footprint in the golden set: legal-biased, then uniform.
pub const GOLDEN_MIXED: usize = 4096;
pub const GOLDEN_RANDOM: usize = 2048;
/// Blocks per golden hash.
pub const GOLDEN_CHUNK: usize = 16;

/// The golden block sequence for a footprint.
pub fn golden_blocks(fp: AstcFootprint) -> Vec<[u8; 16]> {
    let seed = 0x601D_0000 ^ ((fp.width() as u64) << 16) ^ ((fp.height() as u64) << 8) ^ fp.depth() as u64;
    let mut rng = Rng::new(seed);
    let legal = legal_modes(fp);
    let mut v = Vec::with_capacity(GOLDEN_MIXED + GOLDEN_RANDOM);
    for _ in 0..GOLDEN_MIXED {
        v.push(mixed(&mut rng, fp, &legal));
    }
    for _ in 0..GOLDEN_RANDOM {
        v.push(rng.block());
    }
    v
}

/// FNV-1a 64.
pub fn fnv1a(h: u64, bytes: &[u8]) -> u64 {
    let mut h = h;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

pub const FNV_OFFSET: u64 = 0xCBF2_9CE4_8422_2325;
