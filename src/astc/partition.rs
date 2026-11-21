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
    let s12v = ((rnum >> 30) | (rnum << 2)) & 0xF;
    let s12 = s12v * s12v;

    let (sh1, sh2) = if seed & 1 != 0 {
        (if seed & 2 != 0 { 4 } else { 5 }, if count == 3 { 6 } else { 5 })
    } else {
        (if count == 3 { 6 } else { 5 }, if seed & 2 != 0 { 4 } else { 5 })
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
