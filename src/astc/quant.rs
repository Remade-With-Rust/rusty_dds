//! Colour endpoint and weight unquantisation ("Endpoint Unquantization" and
//! "Weight Unquantization"), as compile-time tables indexed by the raw ISE
//! value (`t * 2^bits + m`).

use super::ise::{FORM, RANGE_COUNT};

const fn bt(v: u32, i: u32) -> u32 {
    (v >> i) & 1
}

/// Unquantise one colour value of range `range` (`4..=20`) to 0..=255.
const fn unquant_color(range: usize, v: u32) -> u8 {
    let (bits, trits, quints) = FORM[range];
    let bits = bits as u32;
    if trits == 0 && quints == 0 {
        // Bit replication from the most significant bit to 8 bits.
        let mut out = 0u32;
        let mut have = 0u32;
        while have < 8 {
            if 8 - have >= bits {
                out = (out << bits) | v;
                have += bits;
            } else {
                let take = 8 - have;
                out = (out << take) | (v >> (bits - take));
                have = 8;
            }
        }
        return out as u8;
    }
    let d = v >> bits;
    let m = v & ((1 << bits) - 1);
    let a = if m & 1 != 0 { 0x1FF } else { 0 };
    let (b, c) = if trits != 0 {
        match bits {
            1 => (0, 204),
            2 => (bt(m, 1) * 0x116, 93),
            3 => (bt(m, 2) * 0x10A + bt(m, 1) * 0x085, 44),
            4 => (bt(m, 3) * 0x104 + bt(m, 2) * 0x082 + bt(m, 1) * 0x041, 22),
            5 => (bt(m, 4) * 0x102 + bt(m, 3) * 0x081 + bt(m, 2) * 0x040 + bt(m, 1) * 0x020, 11),
            _ => (
                bt(m, 5) * 0x101 + bt(m, 4) * 0x080 + bt(m, 3) * 0x040 + bt(m, 2) * 0x020 + bt(m, 1) * 0x010,
                5,
            ),
        }
    } else {
        match bits {
            1 => (0, 113),
            2 => (bt(m, 1) * 0x10C, 54),
            3 => (bt(m, 2) * 0x105 + bt(m, 1) * 0x082, 26),
            4 => (bt(m, 3) * 0x102 + bt(m, 2) * 0x081 + bt(m, 1) * 0x040, 13),
            _ => (bt(m, 4) * 0x101 + bt(m, 3) * 0x080 + bt(m, 2) * 0x040 + bt(m, 1) * 0x020, 6),
        }
    };
    let mut unq = d * c + b;
    unq ^= a;
    ((a & 0x80) | (unq >> 2)) as u8
}

/// Unquantise one weight value of range `range` (`0..=11`) to 0..=64.
const fn unquant_weight(range: usize, v: u32) -> u8 {
    let (bits, trits, quints) = FORM[range];
    let bits = bits as u32;
    let unq = if trits == 0 && quints == 0 {
        // Bit replication from the most significant bit to 6 bits.
        let mut out = 0u32;
        let mut have = 0u32;
        while have < 6 {
            if 6 - have >= bits {
                out = (out << bits) | v;
                have += bits;
            } else {
                let take = 6 - have;
                out = (out << take) | (v >> (bits - take));
                have = 6;
            }
        }
        out
    } else if bits == 0 {
        if trits != 0 {
            [0, 32, 63][v as usize]
        } else {
            [0, 16, 32, 47, 63][v as usize]
        }
    } else {
        let d = v >> bits;
        let m = v & ((1 << bits) - 1);
        let a = if m & 1 != 0 { 0x7F } else { 0 };
        let (b, c) = if trits != 0 {
            match bits {
                1 => (0, 50),
                2 => (bt(m, 1) * 0x45, 23),
                _ => (bt(m, 2) * 0x42 + bt(m, 1) * 0x21, 11),
            }
        } else {
            match bits {
                1 => (0, 28),
                _ => (bt(m, 1) * 0x42, 13),
            }
        };
        let mut u = d * c + b;
        u ^= a;
        (a & 0x20) | (u >> 2)
    };
    (if unq > 32 { unq + 1 } else { unq }) as u8
}

const fn build_color() -> [[u8; 256]; RANGE_COUNT] {
    let mut t = [[0u8; 256]; RANGE_COUNT];
    let mut r = 4;
    while r < RANGE_COUNT {
        let levels = super::ise::LEVELS[r] as usize;
        let mut v = 0;
        while v < levels {
            t[r][v] = unquant_color(r, v as u32);
            v += 1;
        }
        r += 1;
    }
    t
}

const fn build_weight() -> [[u8; 32]; 12] {
    let mut t = [[0u8; 32]; 12];
    let mut r = 0;
    while r < 12 {
        let levels = super::ise::LEVELS[r] as usize;
        let mut v = 0;
        while v < levels {
            t[r][v] = unquant_weight(r, v as u32);
            v += 1;
        }
        r += 1;
    }
    t
}

/// `COLOR_UNQUANT[range][value]` for ranges `4..=20` (rows 0..=3 are unused).
pub static COLOR_UNQUANT: [[u8; 256]; RANGE_COUNT] = build_color();
/// `WEIGHT_UNQUANT[range][value]` for ranges `0..=11`.
pub static WEIGHT_UNQUANT: [[u8; 32]; 12] = build_weight();

/// Number of the 21 ranges usable for colour endpoints (`4..=20`, 6 levels and up).
pub(crate) const MIN_COLOR_RANGE: usize = 4;

/// The colour range for `values` endpoint integers in `bits` available bits:
/// the largest range whose sequence fits, or `None` if not even the 6-level
/// range fits (an illegal block).
pub(crate) const fn color_range(values: u32, bits: u32) -> Option<usize> {
    let mut r = RANGE_COUNT;
    while r > MIN_COLOR_RANGE {
        r -= 1;
        if super::ise::sequence_bits(values, r) <= bits {
            return Some(r);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::astc::ise::LEVELS;

    #[test]
    fn color_unquant_endpoints_and_monotone_set() {
        for r in MIN_COLOR_RANGE..RANGE_COUNT {
            let n = LEVELS[r] as usize;
            let mut vals: Vec<u8> = COLOR_UNQUANT[r][..n].to_vec();
            vals.sort_unstable();
            vals.dedup();
            assert_eq!(vals.len(), n, "range {r} has duplicate levels");
            assert_eq!(vals[0], 0);
            assert_eq!(*vals.last().unwrap(), 255);
        }
        // Bit replication: 3-bit 101 -> 10110110.
        assert_eq!(COLOR_UNQUANT[5][5], 0b1011_0110);
    }

    #[test]
    fn weight_unquant_reaches_64() {
        for r in 0..12 {
            let n = LEVELS[r] as usize;
            let mut vals: Vec<u8> = WEIGHT_UNQUANT[r][..n].to_vec();
            vals.sort_unstable();
            vals.dedup();
            assert_eq!(vals.len(), n);
            assert_eq!(vals[0], 0);
            assert_eq!(*vals.last().unwrap(), 64);
        }
    }
}

// ------------------------------------------------------------------ encode --

const fn build_color_inverse() -> [[u8; 256]; RANGE_COUNT] {
    // COLOR_INV[r][u] = the raw ISE value whose unquantised value is u, for
    // each u the range can represent (the maps are bijective, tested above).
    let mut t = [[0u8; 256]; RANGE_COUNT];
    let mut r = MIN_COLOR_RANGE;
    while r < RANGE_COUNT {
        let levels = super::ise::LEVELS[r] as usize;
        let mut v = 0;
        while v < levels {
            t[r][COLOR_UNQUANT_C[r][v] as usize] = v as u8;
            v += 1;
        }
        r += 1;
    }
    t
}

const COLOR_UNQUANT_C: [[u8; 256]; RANGE_COUNT] = build_color();
pub(crate) const WEIGHT_UNQUANT_C: [[u8; 32]; 12] = build_weight();

const fn build_color_nearest() -> [[u8; 256]; RANGE_COUNT] {
    // COLOR_NEAREST[r][x] = the representable unquantised value closest to x
    // (ties to the lower value).
    let mut t = [[0u8; 256]; RANGE_COUNT];
    let mut r = MIN_COLOR_RANGE;
    while r < RANGE_COUNT {
        let levels = super::ise::LEVELS[r] as usize;
        let mut x = 0;
        while x < 256 {
            let mut best = 0u8;
            let mut bd = 1000i32;
            let mut v = 0;
            while v < levels {
                let u = COLOR_UNQUANT_C[r][v] as i32;
                let d = if u > x as i32 { u - x as i32 } else { x as i32 - u };
                if d < bd || (d == bd && (u as u8) < best) {
                    bd = d;
                    best = u as u8;
                }
                v += 1;
            }
            t[r][x] = best;
            x += 1;
        }
        r += 1;
    }
    t
}

const fn build_weight_inverse() -> [[u8; 65]; 12] {
    let mut t = [[0u8; 65]; 12];
    let mut r = 0;
    while r < 12 {
        let levels = super::ise::LEVELS[r] as usize;
        let mut v = 0;
        while v < levels {
            t[r][WEIGHT_UNQUANT_C[r][v] as usize] = v as u8;
            v += 1;
        }
        r += 1;
    }
    t
}

/// Raw ISE value for a representable unquantised colour value.
pub static COLOR_INV: [[u8; 256]; RANGE_COUNT] = build_color_inverse();
/// Nearest representable unquantised colour value.
pub static COLOR_NEAREST: [[u8; 256]; RANGE_COUNT] = build_color_nearest();
/// Raw ISE value for a representable unquantised weight (0..=64).
pub static WEIGHT_INV: [[u8; 65]; 12] = build_weight_inverse();


#[cfg(test)]
mod inverse_tests {
    use super::*;
    use crate::astc::ise::LEVELS;

    #[test]
    fn inverses_invert() {
        for r in MIN_COLOR_RANGE..RANGE_COUNT {
            for v in 0..LEVELS[r] as usize {
                let u = COLOR_UNQUANT[r][v];
                assert_eq!(COLOR_INV[r][u as usize] as usize, v);
                assert_eq!(COLOR_NEAREST[r][u as usize], u);
            }
            for x in 0..256usize {
                let n = COLOR_NEAREST[r][x];
                assert_eq!(COLOR_UNQUANT[r][COLOR_INV[r][n as usize] as usize], n);
            }
        }
        for r in 0..12 {
            for v in 0..LEVELS[r] as usize {
                assert_eq!(WEIGHT_INV[r][WEIGHT_UNQUANT[r][v] as usize] as usize, v);
            }
        }
    }
}
