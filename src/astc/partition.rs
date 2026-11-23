//! Partition pattern generation ("Partition Pattern Generation"): `hash52`
//! and `select_partition`, with the specification's integer widths.

/// The specification's 32-bit seed hash.
pub(crate) const fn hash52(p: u32) -> u32 {
    let mut p = p;
    p ^= p >> 15;
    p = p.wrapping_sub(p << 17);
    p = p.wrapping_add(p << 7);
    p = p.wrapping_add(p << 4);
    p ^= p >> 5;
    p = p.wrapping_add(p << 16);
    p ^= p >> 7;
    p ^= p >> 3;
    p ^= p << 6;
    p ^= p >> 17;
    p
}

/// The partition (0..=3) of texel `(x, y, z)` for a `count`-partition block
/// with 10-bit `seed`. `small_block` is "fewer than 31 texels in the block".
pub const fn select_partition(
    seed: u32,
    x: u32,
    y: u32,
    z: u32,
    count: u32,
    small_block: bool,
) -> u8 {
    let (x, y, z) = if small_block {
        (x << 1, y << 1, z << 1)
    } else {
        (x, y, z)
    };
    let seed = seed + (count - 1) * 1024;
    let rnum = hash52(seed);
    // u8 seeds squared: the largest is 15 * 15 = 225, which fits a u8.
    let s1 = (rnum & 0xF) * (rnum & 0xF);
    let s2 = ((rnum >> 4) & 0xF) * ((rnum >> 4) & 0xF);
    let s3 = ((rnum >> 8) & 0xF) * ((rnum >> 8) & 0xF);
    let s4 = ((rnum >> 12) & 0xF) * ((rnum >> 12) & 0xF);
    let s5 = ((rnum >> 16) & 0xF) * ((rnum >> 16) & 0xF);
    let s6 = ((rnum >> 20) & 0xF) * ((rnum >> 20) & 0xF);
    let s7 = ((rnum >> 24) & 0xF) * ((rnum >> 24) & 0xF);
    let s8 = ((rnum >> 28) & 0xF) * ((rnum >> 28) & 0xF);
    let s9 = ((rnum >> 18) & 0xF) * ((rnum >> 18) & 0xF);
    let s10 = ((rnum >> 22) & 0xF) * ((rnum >> 22) & 0xF);
    let s11 = ((rnum >> 26) & 0xF) * ((rnum >> 26) & 0xF);
    let s12v = rnum.rotate_left(2) & 0xF;
    let s12 = s12v * s12v;

    let (sh1, sh2) = if seed & 1 != 0 {
        (
            if seed & 2 != 0 { 4 } else { 5 },
            if count == 3 { 6 } else { 5 },
        )
    } else {
        (
            if count == 3 { 6 } else { 5 },
            if seed & 2 != 0 { 4 } else { 5 },
        )
    };
    let sh3 = if seed & 0x10 != 0 { sh1 } else { sh2 };

    let a = ((s1 >> sh1) * x + (s2 >> sh2) * y + (s11 >> sh3) * z + (rnum >> 14)) & 0x3F;
    let b = ((s3 >> sh1) * x + (s4 >> sh2) * y + (s12 >> sh3) * z + (rnum >> 10)) & 0x3F;
    let c = ((s5 >> sh1) * x + (s6 >> sh2) * y + (s9 >> sh3) * z + (rnum >> 6)) & 0x3F;
    let d = ((s7 >> sh1) * x + (s8 >> sh2) * y + (s10 >> sh3) * z + (rnum >> 2)) & 0x3F;
    let d = if count < 4 { 0 } else { d };
    let c = if count < 3 { 0 } else { c };

    if a >= b && a >= c && a >= d {
        0
    } else if b >= c && b >= d {
        1
    } else if c >= d {
        2
    } else {
        3
    }
}

/// [`select_partition`] for one `(seed, count, small_block)`, with the work
/// that depends only on those (the seed hash, the squared nibbles and the
/// shifts) done once: the encoder labels every texel of a footprint for
/// each of 1024 seeds, and per texel only the four weighted sums and the
/// comparison remain. Same integer arithmetic, so the same labels
/// (`generator_equals_select_partition`).
#[cfg_attr(not(feature = "encode"), allow(dead_code))]
pub(crate) struct PartitionGen {
    /// Per lane (a, b, c, d): the x, y and z multipliers and the addend.
    k: [[u32; 4]; 4],
    count: u32,
    small: bool,
}

#[cfg_attr(not(feature = "encode"), allow(dead_code))]
impl PartitionGen {
    pub(crate) fn new(seed: u32, count: u32, small_block: bool) -> Self {
        let seed = seed + (count - 1) * 1024;
        let rnum = hash52(seed);
        let sq = |v: u32| v * v;
        let s1 = sq(rnum & 0xF);
        let s2 = sq((rnum >> 4) & 0xF);
        let s3 = sq((rnum >> 8) & 0xF);
        let s4 = sq((rnum >> 12) & 0xF);
        let s5 = sq((rnum >> 16) & 0xF);
        let s6 = sq((rnum >> 20) & 0xF);
        let s7 = sq((rnum >> 24) & 0xF);
        let s8 = sq((rnum >> 28) & 0xF);
        let s9 = sq((rnum >> 18) & 0xF);
        let s10 = sq((rnum >> 22) & 0xF);
        let s11 = sq((rnum >> 26) & 0xF);
        let s12 = sq(rnum.rotate_left(2) & 0xF);
        let (sh1, sh2) = if seed & 1 != 0 {
            (
                if seed & 2 != 0 { 4 } else { 5 },
                if count == 3 { 6 } else { 5 },
            )
        } else {
            (
                if count == 3 { 6 } else { 5 },
                if seed & 2 != 0 { 4 } else { 5 },
            )
        };
        let sh3 = if seed & 0x10 != 0 { sh1 } else { sh2 };
        PartitionGen {
            k: [
                [s1 >> sh1, s2 >> sh2, s11 >> sh3, rnum >> 14],
                [s3 >> sh1, s4 >> sh2, s12 >> sh3, rnum >> 10],
                [s5 >> sh1, s6 >> sh2, s9 >> sh3, rnum >> 6],
                [s7 >> sh1, s8 >> sh2, s10 >> sh3, rnum >> 2],
            ],
            count,
            small: small_block,
        }
    }

    /// The partition of texel `(x, y, z)`.
    #[inline]
    pub(crate) fn label(&self, x: u32, y: u32, z: u32) -> u8 {
        let (x, y, z) = if self.small {
            (x << 1, y << 1, z << 1)
        } else {
            (x, y, z)
        };
        let f = |k: [u32; 4]| (k[0] * x + k[1] * y + k[2] * z + k[3]) & 0x3F;
        let a = f(self.k[0]);
        let b = f(self.k[1]);
        let c = if self.count < 3 { 0 } else { f(self.k[2]) };
        let d = if self.count < 4 { 0 } else { f(self.k[3]) };
        if a >= b && a >= c && a >= d {
            0
        } else if b >= c && b >= d {
            1
        } else if c >= d {
            2
        } else {
            3
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The generator labels every texel of every seed, count and block
    /// size class exactly as `select_partition` does.
    #[test]
    fn generator_equals_select_partition() {
        for count in 1..=4 {
            for small in [false, true] {
                for seed in 0..1024 {
                    let g = PartitionGen::new(seed, count, small);
                    for z in 0..6 {
                        for y in 0..12 {
                            for x in 0..12 {
                                assert_eq!(
                                    g.label(x, y, z),
                                    select_partition(seed, x, y, z, count, small),
                                    "seed {seed} count {count} small {small} ({x},{y},{z})"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
