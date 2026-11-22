//! Integer sequence encoding (Khronos Data Format Specification, ASTC,
//! "Integer Sequence Encoding").
//!
//! A range is identified by its index `0..=20` in ascending number of levels:
//! 2, 3, 4, 5, 6, 8, 10, 12, 16, 20, 24, 32, 40, 48, 64, 80, 96, 128, 160,
//! 192, 256. Weights use indices `0..=11`; colour endpoints use `4..=20`.

/// Number of ranges.
pub const RANGE_COUNT: usize = 21;

/// Levels per range index.
pub const LEVELS: [u16; RANGE_COUNT] = [
    2, 3, 4, 5, 6, 8, 10, 12, 16, 20, 24, 32, 40, 48, 64, 80, 96, 128, 160, 192, 256,
];

/// `(bits, trits, quints)` per range index: a value is `t * 2^bits + m` or
/// `q * 2^bits + m` or plain `m`.
pub(crate) const FORM: [(u8, u8, u8); RANGE_COUNT] = [
    (1, 0, 0),
    (0, 1, 0),
    (2, 0, 0),
    (0, 0, 1),
    (1, 1, 0),
    (3, 0, 0),
    (1, 0, 1),
    (2, 1, 0),
    (4, 0, 0),
    (2, 0, 1),
    (3, 1, 0),
    (5, 0, 0),
    (3, 0, 1),
    (4, 1, 0),
    (6, 0, 0),
    (4, 0, 1),
    (5, 1, 0),
    (7, 0, 0),
    (5, 0, 1),
    (6, 1, 0),
    (8, 0, 0),
];

/// Bits needed to store `count` values of range `range`.
pub const fn sequence_bits(count: u32, range: usize) -> u32 {
    let (bits, trits, quints) = FORM[range];
    let mut n = count * bits as u32;
    if trits != 0 {
        n += (8 * count + 4) / 5;
    }
    if quints != 0 {
        n += (7 * count + 2) / 3;
    }
    n
}

/// The five trits packed in 8 bits `T`, per the specification's bit equations.
const fn bt(v: u32, i: u32) -> u32 {
    (v >> i) & 1
}

const fn unpack_trits(t: u32) -> [u8; 5] {
    let (c, t4, t3);
    if (t >> 2) & 7 == 7 {
        c = (((t >> 5) & 7) << 2) | (t & 3);
        t4 = 2;
        t3 = 2;
    } else {
        c = t & 0x1F;
        if (t >> 5) & 3 == 3 {
            t4 = 2;
            t3 = bt(t, 7);
        } else {
            t4 = bt(t, 7);
            t3 = (t >> 5) & 3;
        }
    }
    let (t2, t1, t0);
    if c & 3 == 3 {
        t2 = 2;
        t1 = bt(c, 4);
        t0 = (bt(c, 3) << 1) | (bt(c, 2) & !bt(c, 3) & 1);
    } else if (c >> 2) & 3 == 3 {
        t2 = 2;
        t1 = 2;
        t0 = c & 3;
    } else {
        t2 = bt(c, 4);
        t1 = (c >> 2) & 3;
        t0 = (bt(c, 1) << 1) | (bt(c, 0) & !bt(c, 1) & 1);
    }
    [t0 as u8, t1 as u8, t2 as u8, t3 as u8, t4 as u8]
}

/// The three quints packed in 7 bits `Q`, per the specification's bit equations.
const fn unpack_quints(q: u32) -> [u8; 3] {
    let (q0, q1, q2);
    if (q >> 1) & 3 == 3 && (q >> 5) & 3 == 0 {
        q2 = (bt(q, 0) << 2) | ((bt(q, 4) & !bt(q, 0) & 1) << 1) | (bt(q, 3) & !bt(q, 0) & 1);
        q1 = 4;
        q0 = 4;
    } else {
        let c;
        if (q >> 1) & 3 == 3 {
            q2 = 4;
            // C = { Q[4:3], ~Q[6:5], Q[0] }
            c = (((q >> 3) & 3) << 3) | ((!(q >> 5) & 3) << 1) | bt(q, 0);
        } else {
            q2 = (q >> 5) & 3;
            c = q & 0x1F;
        }
        if c & 7 == 5 {
            q1 = 4;
            q0 = (c >> 3) & 3;
        } else {
            q1 = (c >> 3) & 3;
            q0 = c & 7;
        }
    }
    [q0 as u8, q1 as u8, q2 as u8]
}

const fn build_trits() -> [[u8; 5]; 256] {
    let mut t = [[0u8; 5]; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = unpack_trits(i as u32);
        i += 1;
    }
    t
}

const fn build_quints() -> [[u8; 3]; 128] {
    let mut t = [[0u8; 3]; 128];
    let mut i = 0;
    while i < 128 {
        t[i] = unpack_quints(i as u32);
        i += 1;
    }
    t
}

/// `TRITS[T]`: the five trits of an 8-bit trit block.
pub(crate) static TRITS: [[u8; 5]; 256] = build_trits();
/// `QUINTS[Q]`: the three quints of a 7-bit quint block.
pub(crate) static QUINTS: [[u8; 3]; 128] = build_quints();

/// Read `n` (0..=8) bits of `src` starting at `pos`, LSB first. Bits at or
/// above position 128 read as zero.
#[inline]
pub(crate) fn read_bits(src: u128, pos: u32, n: u32) -> u32 {
    if pos >= 128 {
        return 0;
    }
    ((src >> pos) as u32) & ((1u32 << n) - 1)
}

/// Decode `out.len()` (at most 64) values of range `range` from `src`,
/// starting at bit `start`, LSB first.
///
/// Each value takes its own `bits` low bits and its share of the trit/quint
/// block bits; trit/quint bits belonging to values past the end of the
/// sequence are zero, as the specification requires for a final partial block.
pub fn decode(src: u128, start: u32, range: usize, out: &mut [u8]) {
    let (bits, trits, quints) = FORM[range];
    let bits = bits as u32;
    let count = out.len();
    let mut pos = start;
    if trits != 0 {
        // Per value within a block of 5: (trit bits, shift within T).
        const TB: [(u32, u32); 5] = [(2, 0), (2, 2), (1, 4), (2, 5), (1, 7)];
        let mut base = 0;
        while base < count {
            let n = (count - base).min(5);
            let mut m = [0u32; 5];
            let mut t = 0u32;
            for (j, mj) in m.iter_mut().enumerate().take(n) {
                *mj = read_bits(src, pos, bits);
                pos += bits;
                let (tb, sh) = TB[j];
                t |= read_bits(src, pos, tb) << sh;
                pos += tb;
            }
            let tv = &TRITS[t as usize];
            for j in 0..n {
                out[base + j] = ((tv[j] as u32) << bits | m[j]) as u8;
            }
            base += 5;
        }
    } else if quints != 0 {
        const QB: [(u32, u32); 3] = [(3, 0), (2, 3), (2, 5)];
        let mut base = 0;
        while base < count {
            let n = (count - base).min(3);
            let mut m = [0u32; 3];
            let mut q = 0u32;
            for (j, mj) in m.iter_mut().enumerate().take(n) {
                *mj = read_bits(src, pos, bits);
                pos += bits;
                let (qb, sh) = QB[j];
                q |= read_bits(src, pos, qb) << sh;
                pos += qb;
            }
            let qv = &QUINTS[q as usize];
            for j in 0..n {
                out[base + j] = ((qv[j] as u32) << bits | m[j]) as u8;
            }
            base += 3;
        }
    } else {
        for o in out.iter_mut() {
            *o = read_bits(src, pos, bits) as u8;
            pos += bits;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trit_and_quint_tables_cover_every_value() {
        // Every 5-trit tuple (243) and 3-quint tuple (125) is produced by at
        // least one packed block; every produced digit is in range.
        let mut seen = [false; 243];
        for t in TRITS.iter() {
            let mut v = 0usize;
            for &d in t.iter().rev() {
                assert!(d < 3);
                v = v * 3 + d as usize;
            }
            seen[v] = true;
        }
        assert!(seen.iter().all(|&s| s));
        let mut seen = [false; 125];
        for q in QUINTS.iter() {
            let mut v = 0usize;
            for &d in q.iter().rev() {
                assert!(d < 5);
                v = v * 5 + d as usize;
            }
            seen[v] = true;
        }
        assert!(seen.iter().all(|&s| s));
    }

    #[test]
    fn levels_match_form() {
        for r in 0..RANGE_COUNT {
            let (b, t, q) = FORM[r];
            let mul = if t != 0 { 3 } else if q != 0 { 5 } else { 1 };
            assert_eq!(LEVELS[r] as u32, mul << b);
        }
    }

    #[test]
    fn bit_counts() {
        assert_eq!(sequence_bits(1, 1), 2); // one trit: ceil(8/5)
        assert_eq!(sequence_bits(5, 1), 8);
        assert_eq!(sequence_bits(3, 3), 7);
        assert_eq!(sequence_bits(64, 0), 64);
        assert_eq!(sequence_bits(18, 20), 144);
    }
}

// ------------------------------------------------------------------ encode --

const fn build_trit_inverse() -> [u8; 243] {
    // For each 5-trit tuple (t0 + 3 t1 + 9 t2 + ...), the smallest packed T
    // that decodes to it. The smallest choice keeps the bits of a final
    // partial block that are never written at zero (tested below).
    let mut inv = [0u8; 243];
    let mut seen = [false; 243];
    let mut t = 0;
    while t < 256 {
        let d = unpack_trits(t as u32);
        let k = d[0] as usize + 3 * d[1] as usize + 9 * d[2] as usize + 27 * d[3] as usize + 81 * d[4] as usize;
        if !seen[k] {
            seen[k] = true;
            inv[k] = t as u8;
        }
        t += 1;
    }
    inv
}

const fn build_quint_inverse() -> [u8; 125] {
    let mut inv = [0u8; 125];
    let mut seen = [false; 125];
    let mut q = 0;
    while q < 128 {
        let d = unpack_quints(q as u32);
        let k = d[0] as usize + 5 * d[1] as usize + 25 * d[2] as usize;
        if !seen[k] {
            seen[k] = true;
            inv[k] = q as u8;
        }
        q += 1;
    }
    inv
}

static TRIT_INV: [u8; 243] = build_trit_inverse();
static QUINT_INV: [u8; 125] = build_quint_inverse();

#[inline]
fn put_bits(dst: &mut u128, pos: u32, n: u32, v: u32) {
    if n == 0 || pos >= 128 {
        return;
    }
    let m = ((1u128 << n) - 1) << pos;
    *dst = (*dst & !m) | (((v as u128) << pos) & m);
}

/// Encode `vals` (raw ISE values of range `range`) into `dst` from bit
/// `start`, LSB first: the exact inverse of [`decode`].
pub fn encode(vals: &[u8], range: usize, dst: &mut u128, start: u32) {
    let (bits, trits, quints) = FORM[range];
    let bits = bits as u32;
    let mask = (1u32 << bits) - 1;
    let mut pos = start;
    if trits != 0 {
        const TB: [(u32, u32); 5] = [(2, 0), (2, 2), (1, 4), (2, 5), (1, 7)];
        for chunk in vals.chunks(5) {
            let mut k = 0usize;
            let mut mul = 1usize;
            for &v in chunk {
                k += (v as usize >> bits) * mul;
                mul *= 3;
            }
            let t = TRIT_INV[k] as u32;
            for (j, &v) in chunk.iter().enumerate() {
                put_bits(dst, pos, bits, v as u32 & mask);
                pos += bits;
                let (tb, sh) = TB[j];
                put_bits(dst, pos, tb, (t >> sh) & ((1 << tb) - 1));
                pos += tb;
            }
        }
    } else if quints != 0 {
        const QB: [(u32, u32); 3] = [(3, 0), (2, 3), (2, 5)];
        for chunk in vals.chunks(3) {
            let mut k = 0usize;
            let mut mul = 1usize;
            for &v in chunk {
                k += (v as usize >> bits) * mul;
                mul *= 5;
            }
            let q = QUINT_INV[k] as u32;
            for (j, &v) in chunk.iter().enumerate() {
                put_bits(dst, pos, bits, v as u32 & mask);
                pos += bits;
                let (qb, sh) = QB[j];
                put_bits(dst, pos, qb, (q >> sh) & ((1 << qb) - 1));
                pos += qb;
            }
        }
    } else {
        for &v in vals {
            put_bits(dst, pos, bits, v as u32);
            pos += bits;
        }
    }
}

#[cfg(test)]
mod encode_tests {
    use super::*;

    #[test]
    fn partial_blocks_leave_unwritten_bits_zero() {
        // A final block of k < 5 trits writes 2, 4, 5, 7 T bits; the chosen T
        // for a tuple whose missing digits are zero must fit in those bits.
        for (k, covered) in [(1usize, 2u32), (2, 4), (3, 5), (4, 7)] {
            for code in 0..3usize.pow(k as u32) {
                assert!((TRIT_INV[code] as u32) < (1 << covered), "trits k={k} code={code}");
            }
        }
        for (k, covered) in [(1usize, 3u32), (2, 5)] {
            for code in 0..5usize.pow(k as u32) {
                assert!((QUINT_INV[code] as u32) < (1 << covered), "quints k={k} code={code}");
            }
        }
    }

    #[test]
    fn encode_decode_round_trip_every_range_and_length() {
        let mut x: u32 = 1;
        for r in 0..RANGE_COUNT {
            for n in 1..=64usize {
                if sequence_bits(n as u32, r) > 128 {
                    continue;
                }
                let vals: Vec<u8> = (0..n)
                    .map(|_| {
                        x ^= x << 13;
                        x ^= x >> 17;
                        x ^= x << 5;
                        (x % LEVELS[r] as u32) as u8
                    })
                    .collect();
                let mut b = 0u128;
                encode(&vals, r, &mut b, 0);
                let used = sequence_bits(n as u32, r);
                if used < 128 {
                    assert_eq!(b >> used, 0, "bits past the sequence, range {r} n {n}");
                }
                let mut out = vec![0u8; n];
                decode(b, 0, r, &mut out);
                assert_eq!(out, vals, "range {r} n {n}");
            }
        }
    }
}
