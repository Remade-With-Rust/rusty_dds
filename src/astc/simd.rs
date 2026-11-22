//! SIMD twins of the ASTC fast decoder's 8-bit output loop (plan P2).
//!
//! # Lanes and ranges
//!
//! Under an LDR profile every endpoint is an 8-bit value `c`, and 8-bit
//! output is computed from `X = c0 (64 - w) + c1 w` (see
//! [`super::fast::u8_from_8bit`]). In 16-bit lanes:
//!
//! | term | range | fits `u16` |
//! |---|---|---|
//! | `c0 (64 - w)`, `c1 w` | `0 ..= 16_320` | yes, so `mullo` is exact |
//! | `X` | `0 ..= 16_320` | yes |
//! | `4 X` | `0 ..= 65_280` | yes (unsigned) |
//! | `4 X + ((X + 32) >> 6)` (linear) | `0 ..= 65_535` | yes |
//! | `4 X + 128` (sRGB) | `128 ..= 65_408` | yes |
//!
//! Additions wrap in signed lanes but the true values never exceed `u16`, and
//! every shift is logical, so the lanes hold the exact unsigned values. One
//! 128-bit register carries two texels (eight channels).
//!
//! SSE2 is baseline on x86_64 (no runtime detection); NEON is baseline on
//! aarch64. The scalar loop stays as the oracle and the fallback, and
//! `*_matches_scalar` tests prove the twins byte-identical.

pub(crate) use super::fast::{u8_texels_scalar, Ends};

/// 8-bit output for `n` texels: the SIMD twin where available.
#[allow(clippy::too_many_arguments)]
#[inline]
pub(crate) fn u8_texels(
    ends: &Ends,
    pmap: &[u8],
    w1: &[u8; 216],
    w2: &[u8; 216],
    ccs: usize,
    srgb: bool,
    n: usize,
    out: &mut [[u16; 4]],
) {
    #[cfg(target_arch = "x86_64")]
    {
        sse2::u8_texels(ends, pmap, w1, w2, ccs, srgb, n, out);
    }
    #[cfg(target_arch = "aarch64")]
    {
        neon::u8_texels(ends, pmap, w1, w2, ccs, srgb, n, out);
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        u8_texels_scalar(ends, pmap, w1, w2, ccs, srgb, n, out);
    }
}

use super::fast::wv;

#[cfg(target_arch = "x86_64")]
mod sse2 {
    use super::{wv, Ends};
    use core::arch::x86_64::{
        __m128i, _mm_add_epi16, _mm_loadu_si128, _mm_mullo_epi16, _mm_set1_epi16, _mm_slli_epi16,
        _mm_srli_epi16, _mm_storeu_si128, _mm_sub_epi16,
    };

    #[inline(always)]
    fn pair(x: [u16; 4], y: [u16; 4]) -> [u16; 8] {
        [x[0], x[1], x[2], x[3], y[0], y[1], y[2], y[3]]
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn u8_texels(
        ends: &Ends,
        pmap: &[u8],
        w1: &[u8; 216],
        w2: &[u8; 216],
        ccs: usize,
        srgb: bool,
        n: usize,
        out: &mut [[u16; 4]],
    ) {
        let out = &mut out[..n];
        let mut t = 0;
        while t + 2 <= n {
            let (a0, b0) = &ends[if pmap.is_empty() { 0 } else { pmap[t] as usize }];
            let (a1, b1) = &ends[if pmap.is_empty() { 0 } else { pmap[t + 1] as usize }];
            let a = pair(*a0, *a1);
            let b = pair(*b0, *b1);
            let w = pair(wv(w1, w2, ccs, t), wv(w1, w2, ccs, t + 1));
            let mut r = [0u16; 8];
            // SAFETY: SSE2 is unconditionally present on x86_64. The loads read
            // and the store writes exactly 16 bytes from/to local [u16; 8]
            // arrays (unaligned forms, no alignment requirement). The lane
            // arithmetic is exact by the range table in the module docs.
            unsafe {
                let va = _mm_loadu_si128(a.as_ptr() as *const __m128i);
                let vb = _mm_loadu_si128(b.as_ptr() as *const __m128i);
                let vw = _mm_loadu_si128(w.as_ptr() as *const __m128i);
                let vw0 = _mm_sub_epi16(_mm_set1_epi16(64), vw);
                let x = _mm_add_epi16(_mm_mullo_epi16(va, vw0), _mm_mullo_epi16(vb, vw));
                let x4 = _mm_slli_epi16(x, 2);
                let c = if srgb {
                    _mm_add_epi16(x4, _mm_set1_epi16(128))
                } else {
                    _mm_add_epi16(x4, _mm_srli_epi16(_mm_add_epi16(x, _mm_set1_epi16(32)), 6))
                };
                let v = _mm_srli_epi16(c, 8);
                _mm_storeu_si128(r.as_mut_ptr() as *mut __m128i, v);
            }
            out[t] = [r[0], r[1], r[2], r[3]];
            out[t + 1] = [r[4], r[5], r[6], r[7]];
            t += 2;
        }
        if t < n {
            super::u8_texels_scalar(ends, &pmap[t.min(pmap.len())..], &shift(w1, t), &shift(w2, t), ccs, srgb, n - t, &mut out[t..]);
        }
    }

    /// Weights from texel `t` on, for the scalar tail.
    fn shift(w: &[u8; 216], t: usize) -> [u8; 216] {
        let mut o = [0u8; 216];
        o[..216 - t].copy_from_slice(&w[t..]);
        o
    }
}

#[cfg(target_arch = "aarch64")]
mod neon {
    use super::{wv, Ends};
    use core::arch::aarch64::{vaddq_u16, vdupq_n_u16, vld1q_u16, vmulq_u16, vshlq_n_u16, vshrq_n_u16, vst1q_u16, vsubq_u16};

    #[inline(always)]
    fn pair(x: [u16; 4], y: [u16; 4]) -> [u16; 8] {
        [x[0], x[1], x[2], x[3], y[0], y[1], y[2], y[3]]
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn u8_texels(
        ends: &Ends,
        pmap: &[u8],
        w1: &[u8; 216],
        w2: &[u8; 216],
        ccs: usize,
        srgb: bool,
        n: usize,
        out: &mut [[u16; 4]],
    ) {
        let out = &mut out[..n];
        let mut t = 0;
        while t + 2 <= n {
            let (a0, b0) = &ends[if pmap.is_empty() { 0 } else { pmap[t] as usize }];
            let (a1, b1) = &ends[if pmap.is_empty() { 0 } else { pmap[t + 1] as usize }];
            let a = pair(*a0, *a1);
            let b = pair(*b0, *b1);
            let w = pair(wv(w1, w2, ccs, t), wv(w1, w2, ccs, t + 1));
            let mut r = [0u16; 8];
            // SAFETY: NEON is baseline on aarch64. The loads read and the store
            // writes exactly eight u16 from/to local arrays. The lane
            // arithmetic is exact by the range table in the module docs.
            unsafe {
                let va = vld1q_u16(a.as_ptr());
                let vb = vld1q_u16(b.as_ptr());
                let vw = vld1q_u16(w.as_ptr());
                let vw0 = vsubq_u16(vdupq_n_u16(64), vw);
                let x = vaddq_u16(vmulq_u16(va, vw0), vmulq_u16(vb, vw));
                let x4 = vshlq_n_u16::<2>(x);
                let c = if srgb {
                    vaddq_u16(x4, vdupq_n_u16(128))
                } else {
                    vaddq_u16(x4, vshrq_n_u16::<6>(vaddq_u16(x, vdupq_n_u16(32))))
                };
                vst1q_u16(r.as_mut_ptr(), vshrq_n_u16::<8>(c));
            }
            out[t] = [r[0], r[1], r[2], r[3]];
            out[t + 1] = [r[4], r[5], r[6], r[7]];
            t += 2;
        }
        if t < n {
            let mut a1 = [0u8; 216];
            let mut a2 = [0u8; 216];
            a1[..216 - t].copy_from_slice(&w1[t..]);
            a2[..216 - t].copy_from_slice(&w2[t..]);
            super::u8_texels_scalar(ends, &pmap[t.min(pmap.len())..], &a1, &a2, ccs, srgb, n - t, &mut out[t..]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::astc::testgen::Rng;

    /// The SIMD twin equals the scalar oracle for random endpoints, weights,
    /// partition maps, planes and both expansions, at every texel count.
    #[test]
    fn u8_texels_matches_scalar() {
        let mut rng = Rng::new(0x51D);
        for round in 0..20_000 {
            let n = 1 + (round % 216);
            let mut ends: Ends = [([0; 4], [0; 4]); 4];
            for e in ends.iter_mut() {
                for c in 0..4 {
                    e.0[c] = rng.below(256) as u16;
                    e.1[c] = rng.below(256) as u16;
                }
            }
            if round % 7 == 0 {
                ends[0] = ([255; 4], [255; 4]);
            }
            let mut w1 = [0u8; 216];
            let mut w2 = [0u8; 216];
            for i in 0..216 {
                w1[i] = rng.below(65) as u8;
                w2[i] = rng.below(65) as u8;
            }
            let parts = 1 + rng.below(4) as usize;
            let pmap: Vec<u8> = if parts == 1 { Vec::new() } else { (0..n).map(|_| rng.below(parts as u64) as u8).collect() };
            let ccs = rng.below(5) as usize;
            let srgb = round % 2 == 0;
            let mut a = [[0u16; 4]; 216];
            let mut b = [[0u16; 4]; 216];
            u8_texels(&ends, &pmap, &w1, &w2, ccs, srgb, n, &mut a);
            u8_texels_scalar(&ends, &pmap, &w1, &w2, ccs, srgb, n, &mut b);
            assert_eq!(&a[..n], &b[..n], "round {round}");
        }
    }
}
