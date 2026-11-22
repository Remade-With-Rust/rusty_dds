//! SIMD twins of the LDR encoder's hot kernels.
//!
//! Every twin computes exactly what its scalar oracle computes (integer
//! kernels: the same values; float kernels: the same IEEE operations in the
//! same order, no FMA), so the encoder's output bytes do not depend on which
//! one ran. The scalar versions stay in `block.rs` as the oracle and as the
//! fallback on targets without a twin; `*_matches_scalar` tests compare the
//! two bit for bit.
//!
//! SSE2 is baseline on x86_64, so the SSE2 twins need no runtime detection.
//! The `unsafe` here is confined to `core::arch` intrinsics whose only memory
//! accesses are the loads and stores spelled out in each SAFETY comment; the
//! functions exported to `block.rs` are safe.
#![allow(unsafe_code)]

#[cfg(target_arch = "x86_64")]
pub(super) use self::sse2::part_err_acc;

#[cfg(target_arch = "x86_64")]
mod sse2 {
    use super::super::block::MAX_TEXELS;
    use crate::astc::cem::Endpoints;
    use core::arch::x86_64::{
        __m128i, _mm_add_epi16, _mm_add_epi32, _mm_add_epi64, _mm_and_si128, _mm_cvtsi64_si128,
        _mm_mulhi_epu16, _mm_mullo_epi16, _mm_or_si128, _mm_set1_epi16, _mm_set_epi64x,
        _mm_setzero_si128, _mm_slli_epi16, _mm_srli_epi16, _mm_storeu_si128, _mm_sub_epi16,
        _mm_subs_epu16, _mm_unpackhi_epi16, _mm_unpackhi_epi32, _mm_unpacklo_epi16,
        _mm_unpacklo_epi32, _mm_unpacklo_epi8,
    };

    /// Four 16-bit lanes in one word, lane `c` = `x[c]`.
    #[inline(always)]
    fn lanes(x: [u16; 4]) -> u64 {
        x[0] as u64 | (x[1] as u64) << 16 | (x[2] as u64) << 32 | (x[3] as u64) << 48
    }

    /// Per-channel sums of squared errors of one partition's texels: the twin
    /// of `block::part_err_acc_scalar`, with identical results.
    ///
    /// # Lanes and ranges
    ///
    /// Under an LDR profile every decoded endpoint is `257 c` (linear) or
    /// `256 c + 128` (sRGB) for an 8-bit `c`, and with `X = c0 (64 - w) + c1 w`
    /// the decoder's `lerp` is `(e0 (64 - w) + e1 w + 32) >> 6` =
    /// `4 X + ((X + 32) >> 6)` (linear) or `4 X + 128` (sRGB), exactly: the
    /// `256 X` part of the numerator is a multiple of 64. In 16-bit lanes:
    ///
    /// | term | range | fits `u16` |
    /// |---|---|---|
    /// | `c0 (64 - w)`, `c1 w`, `X` | `0 ..= 16_320` | yes (`mullo` exact) |
    /// | interpolated `v` | `0 ..= 65_535` | yes |
    /// | `s 257` (target) | `0 ..= 65_535` | yes |
    /// | `|v - s 257|` (saturating subtracts, OR) | `0 ..= 65_535` | yes |
    /// | `|(v >> 8) - s|` (`decode_unorm8`) | `0 ..= 255` | yes |
    ///
    /// A square of a 16-bit `|d|` is assembled from `mullo` / `mulhi_epu16`
    /// into a 32-bit lane (at most `65_535^2 < 2^32`) and summed into 64-bit
    /// lanes, as the scalar sums into `u64`; under `decode_unorm8` the square
    /// is at most `65_025` and 216 of them fit a 32-bit lane. Integer sums,
    /// so the order of accumulation cannot change the result.
    ///
    /// Returns `None` (the caller falls back to the scalar) if an endpoint is
    /// not in the LDR form above, which `cem::expand` never produces under an
    /// LDR profile.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::astc::encode) fn part_err_acc(
        px: &[[u8; 4]],
        idx: &[u8],
        ep: &Endpoints,
        w1: &[u8; MAX_TEXELS],
        w2: &[u8; MAX_TEXELS],
        ccs: usize,
        chans: usize,
        unorm8: bool,
        srgb: bool,
    ) -> Option<[u64; 4]> {
        let mut c0 = [0u16; 4];
        let mut c1 = [0u16; 4];
        for c in 0..4 {
            let (a, b) = (ep.e0[c] >> 8, ep.e1[c] >> 8);
            let form = |x: u16| if srgb { (x << 8) | 0x80 } else { x * 257 };
            if form(a) != ep.e0[c] || form(b) != ep.e1[c] {
                return None;
            }
            c0[c] = a;
            c1[c] = b;
        }
        // Lane mask of the measured channels (alpha off for opaque blocks).
        let keep: u64 = if chans == 3 {
            0x0000_FFFF_FFFF_FFFF
        } else {
            u64::MAX
        };
        // Weight lanes of texel i: plane 1 everywhere, plane 2 in lane ccs.
        let (pmask, plane2) = if ccs < 4 {
            (!(0xFFFFu64 << (16 * ccs)), 16 * ccs as u32)
        } else {
            (u64::MAX, 0)
        };
        let dual = ccs < 4;
        let wl = |i: usize| -> u64 {
            let w = (w1[i] as u64) * 0x0001_0001_0001_0001;
            if dual {
                (w & pmask) | (w2[i] as u64) << plane2
            } else {
                w
            }
        };
        let (e0, e1) = (lanes(c0) as i64, lanes(c1) as i64);
        let keep = keep as i64;
        let mut acc = [0u64; 4];
        let mut out = [0u64; 4];
        let pairs = idx.len() / 2;
        // SAFETY: SSE2 is unconditionally present on x86_64. Every operand
        // is built from scalars (`_mm_set*`, `_mm_cvtsi64_si128`); the only
        // memory accesses are the two `_mm_storeu_si128` at the end, each
        // writing exactly 16 bytes into `out` (a local `[u64; 4]`, 32
        // bytes: offsets 0 and 16, unaligned form). The lane arithmetic is
        // exact by the range table above.
        unsafe {
            let z = _mm_setzero_si128();
            let va = _mm_set_epi64x(e0, e0);
            let vb = _mm_set_epi64x(e1, e1);
            let vkeep = _mm_set_epi64x(keep, keep);
            let k64 = _mm_set1_epi16(64);
            let k32 = _mm_set1_epi16(32);
            let k128 = _mm_set1_epi16(128);
            // Linear: two u64 lanes per register, channels (0, 1) and (2, 3).
            let mut lo = _mm_setzero_si128();
            let mut hi = _mm_setzero_si128();
            // decode_unorm8: four u32 lanes, channels 0..=3.
            let mut a32 = _mm_setzero_si128();
            for p in 0..pairs {
                let (i0, i1) = (idx[2 * p] as usize, idx[2 * p + 1] as usize);
                let vw = _mm_set_epi64x(wl(i1) as i64, wl(i0) as i64);
                let x = _mm_add_epi16(
                    _mm_mullo_epi16(va, _mm_sub_epi16(k64, vw)),
                    _mm_mullo_epi16(vb, vw),
                );
                let x4 = _mm_slli_epi16(x, 2);
                let v = if srgb {
                    _mm_add_epi16(x4, k128)
                } else {
                    _mm_add_epi16(x4, _mm_srli_epi16(_mm_add_epi16(x, k32), 6))
                };
                let sp =
                    u32::from_le_bytes(px[i0]) as u64 | (u32::from_le_bytes(px[i1]) as u64) << 32;
                let s = _mm_unpacklo_epi8(_mm_cvtsi64_si128(sp as i64), z);
                if unorm8 {
                    let v8 = _mm_srli_epi16(v, 8);
                    let d = _mm_or_si128(_mm_subs_epu16(v8, s), _mm_subs_epu16(s, v8));
                    let d = _mm_and_si128(d, vkeep);
                    let sq = _mm_mullo_epi16(d, d);
                    a32 = _mm_add_epi32(a32, _mm_unpacklo_epi16(sq, z));
                    a32 = _mm_add_epi32(a32, _mm_unpackhi_epi16(sq, z));
                } else {
                    let t = _mm_or_si128(_mm_slli_epi16(s, 8), s);
                    let d = _mm_or_si128(_mm_subs_epu16(v, t), _mm_subs_epu16(t, v));
                    let d = _mm_and_si128(d, vkeep);
                    let (ql, qh) = (_mm_mullo_epi16(d, d), _mm_mulhi_epu16(d, d));
                    // 32-bit squares: texel 0 channels 0..=3, texel 1 likewise.
                    let q0 = _mm_unpacklo_epi16(ql, qh);
                    let q1 = _mm_unpackhi_epi16(ql, qh);
                    lo = _mm_add_epi64(lo, _mm_unpacklo_epi32(q0, z));
                    hi = _mm_add_epi64(hi, _mm_unpackhi_epi32(q0, z));
                    lo = _mm_add_epi64(lo, _mm_unpacklo_epi32(q1, z));
                    hi = _mm_add_epi64(hi, _mm_unpackhi_epi32(q1, z));
                }
            }
            if unorm8 {
                lo = _mm_unpacklo_epi32(a32, z);
                hi = _mm_unpackhi_epi32(a32, z);
            }
            _mm_storeu_si128(out.as_mut_ptr() as *mut __m128i, lo);
            _mm_storeu_si128(out.as_mut_ptr().add(2) as *mut __m128i, hi);
        }
        acc.copy_from_slice(&out);
        // The odd texel, if any, as the scalar does it.
        if idx.len() % 2 == 1 {
            let i = idx[idx.len() - 1] as usize;
            for c in 0..chans {
                let w = if c == ccs { w2[i] } else { w1[i] };
                let v = crate::astc::texel::lerp(ep.e0[c], ep.e1[c], w) as i64;
                let s = px[i][c] as i64;
                let d = if unorm8 { (v >> 8) - s } else { v - s * 257 };
                acc[c] += (d * d) as u64;
            }
        }
        Some(acc)
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::super::block::{part_err_acc_scalar, MAX_TEXELS};
    use crate::astc::cem::Endpoints;
    use crate::astc::testgen::Rng;

    fn ldr(c: u8, srgb: bool) -> u16 {
        if srgb {
            (c as u16) << 8 | 0x80
        } else {
            c as u16 * 257
        }
    }

    /// The SSE2 part error equals the scalar for random texels, partitions
    /// (odd and even sizes), endpoints, weights (including 0 and 64), every
    /// dual-plane channel, both channel counts and both error rules.
    #[test]
    fn part_err_acc_matches_scalar() {
        let mut rng = Rng::new(0xE77_0A55);
        for trial in 0..20_000 {
            let n = 1 + rng.below(MAX_TEXELS as u64) as usize;
            let px: Vec<[u8; 4]> = (0..n)
                .map(|_| {
                    let v = rng.next_u64();
                    // Extremes often: they reach the 16-bit range limits.
                    if trial % 7 == 0 {
                        [0, 255, (v & 1) as u8 * 255, 255]
                    } else {
                        [v as u8, (v >> 8) as u8, (v >> 16) as u8, (v >> 24) as u8]
                    }
                })
                .collect();
            let m = 1 + rng.below(n as u64) as usize;
            let idx: Vec<u8> = (0..m).map(|_| rng.below(n as u64) as u8).collect();
            let mut w1 = [0u8; MAX_TEXELS];
            let mut w2 = [0u8; MAX_TEXELS];
            for i in 0..MAX_TEXELS {
                w1[i] = [0, 64, rng.below(65) as u8][rng.below(3) as usize];
                w2[i] = rng.below(65) as u8;
            }
            let srgb = trial % 3 == 0;
            let unorm8 = srgb || trial % 2 == 0;
            let chans = if trial % 5 == 0 { 3 } else { 4 };
            let ccs = rng.below(5) as usize;
            let ccs = if chans == 3 && ccs == 3 { 4 } else { ccs };
            let e = |rng: &mut Rng| -> [u16; 4] {
                core::array::from_fn(|_| {
                    ldr([0, 255, rng.below(256) as u8][rng.below(3) as usize], srgb)
                })
            };
            let ep = Endpoints {
                e0: e(&mut rng),
                e1: e(&mut rng),
                lns: [false; 4],
                hdr_in_ldr: false,
            };
            let a = super::part_err_acc(&px, &idx, &ep, &w1, &w2, ccs, chans, unorm8, srgb);
            let b = part_err_acc_scalar(&px, &idx, &ep, &w1, &w2, ccs, chans, unorm8);
            assert_eq!(
                a,
                Some(b),
                "trial {trial} n {n} m {m} ccs {ccs} chans {chans} unorm8 {unorm8} srgb {srgb}"
            );
        }
        // A non-LDR endpoint declines (the caller falls back to the scalar).
        let ep = Endpoints {
            e0: [1, 0, 0, 0],
            e1: [0; 4],
            lns: [false; 4],
            hdr_in_ldr: false,
        };
        let (w, px) = ([0u8; MAX_TEXELS], [[0u8; 4]; 2]);
        assert_eq!(
            super::part_err_acc(&px, &[0, 1], &ep, &w, &w, 4, 4, false, false),
            None
        );
    }
}
