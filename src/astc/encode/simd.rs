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
pub(super) use self::avx2::{decimate, infill_eff, residual, Avx2};
#[cfg(target_arch = "x86_64")]
pub(super) use self::popcnt::{rank_patterns, Popcnt};
#[cfg(target_arch = "x86_64")]
pub(super) use self::sse2::{part_err_acc, refit_sums};

#[cfg(target_arch = "x86_64")]
mod sse2 {
    use super::super::block::{RefitSums, MAX_TEXELS};
    use crate::astc::cem::Endpoints;
    use core::arch::x86_64::{
        __m128i, _mm_add_epi16, _mm_add_epi32, _mm_add_epi64, _mm_add_ps, _mm_and_ps,
        _mm_and_si128, _mm_andnot_ps, _mm_castsi128_ps, _mm_cvtepi32_ps, _mm_cvtsi32_si128,
        _mm_cvtsi64_si128, _mm_mul_ps, _mm_mulhi_epu16, _mm_mullo_epi16, _mm_or_ps, _mm_or_si128,
        _mm_set1_epi16, _mm_set1_ps, _mm_set_epi32, _mm_set_epi64x, _mm_setzero_ps,
        _mm_setzero_si128, _mm_shuffle_ps, _mm_slli_epi16, _mm_srli_epi16, _mm_storeu_ps,
        _mm_storeu_si128, _mm_sub_epi16, _mm_sub_ps, _mm_subs_epu16, _mm_unpackhi_epi16,
        _mm_unpackhi_epi32, _mm_unpacklo_epi16, _mm_unpacklo_epi32, _mm_unpacklo_epi8,
        _mm_unpacklo_ps,
    };

    /// Twin of `block::refit_sums_scalar`, bit for bit, for every channel
    /// (the scalar's `chans` only limits which channels it bothers with).
    ///
    /// Lanes are channels, so each lane's sums run over the texels in the
    /// scalar's order: `x0 += u x`, `x1 += t x` with the plane's `t`/`u` in
    /// each lane (plane 2 in lane `ccs`). The weight sums are one register per
    /// plane, lanes `(a, b, d)` += `(u, u, t) * (u, t, t)`, the scalar's three
    /// products. `t = w * (1/64)` and `u = 1 - t` are the scalar's
    /// operations; no FMA.
    pub(in crate::astc::encode) fn refit_sums(
        px: &[[u8; 4]],
        idx: &[u8],
        w1: &[u8; MAX_TEXELS],
        w2: &[u8; MAX_TEXELS],
        ccs: usize,
    ) -> RefitSums {
        let dual = ccs < 4;
        let mut m = [0u32; 4];
        if dual {
            m[ccs] = u32::MAX;
        }
        let mut o = [[0f32; 4]; 4];
        // SAFETY: SSE2 is unconditionally present on x86_64. Operands are
        // built from scalars; the only memory accesses are the stores into
        // the local `o` (four 16-byte rows, unaligned form).
        unsafe {
            let sel = _mm_castsi128_ps(_mm_set_epi32(
                m[3] as i32,
                m[2] as i32,
                m[1] as i32,
                m[0] as i32,
            ));
            let one = _mm_set1_ps(1.0);
            let k = _mm_set1_ps(1.0 / 64.0);
            let z = _mm_setzero_si128();
            let (mut p1, mut p2) = (_mm_setzero_ps(), _mm_setzero_ps());
            let (mut x0, mut x1) = (_mm_setzero_ps(), _mm_setzero_ps());
            for &i in idx {
                let i = i as usize;
                let t1 = _mm_mul_ps(_mm_set1_ps(w1[i] as f32), k);
                let u1 = _mm_sub_ps(one, t1);
                // (u, u, t, t) * (u, t, t, t): lanes a, b, d.
                let l1 = _mm_shuffle_ps::<0b00_00_00_00>(u1, t1);
                let r1 = _mm_shuffle_ps::<0b00_00_01_00>(_mm_unpacklo_ps(u1, t1), t1);
                p1 = _mm_add_ps(p1, _mm_mul_ps(l1, r1));
                let (t, u) = if dual {
                    let t2 = _mm_mul_ps(_mm_set1_ps(w2[i] as f32), k);
                    let u2 = _mm_sub_ps(one, t2);
                    let l2 = _mm_shuffle_ps::<0b00_00_00_00>(u2, t2);
                    let r2 = _mm_shuffle_ps::<0b00_00_01_00>(_mm_unpacklo_ps(u2, t2), t2);
                    p2 = _mm_add_ps(p2, _mm_mul_ps(l2, r2));
                    (
                        _mm_or_ps(_mm_and_ps(sel, t2), _mm_andnot_ps(sel, t1)),
                        _mm_or_ps(_mm_and_ps(sel, u2), _mm_andnot_ps(sel, u1)),
                    )
                } else {
                    (t1, u1)
                };
                let pxi = _mm_cvtsi32_si128(i32::from_le_bytes(px[i]));
                let x = _mm_cvtepi32_ps(_mm_unpacklo_epi16(_mm_unpacklo_epi8(pxi, z), z));
                x0 = _mm_add_ps(x0, _mm_mul_ps(u, x));
                x1 = _mm_add_ps(x1, _mm_mul_ps(t, x));
            }
            _mm_storeu_ps(o[0].as_mut_ptr(), p1);
            _mm_storeu_ps(o[1].as_mut_ptr(), p2);
            _mm_storeu_ps(o[2].as_mut_ptr(), x0);
            _mm_storeu_ps(o[3].as_mut_ptr(), x1);
        }
        RefitSums {
            a: [o[0][0], o[1][0]],
            b: [o[0][1], o[1][1]],
            d: [o[0][2], o[1][2]],
            x0: o[2],
            x1: o[3],
        }
    }

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

#[cfg(target_arch = "x86_64")]
mod popcnt {
    use super::super::block::rank_patterns_body;
    use super::super::context::PartBits;
    use alloc::vec::Vec;

    /// Proof that the CPU has POPCNT: only [`Popcnt::detect`] makes one.
    #[derive(Clone, Copy)]
    pub(in crate::astc::encode) struct Popcnt(());

    impl Popcnt {
        /// `Some` when the CPU reports POPCNT (with the development-only
        /// `tuning` feature, `RUSTY_DDS_ASTC_NO_AVX2` also turns this off).
        pub(in crate::astc::encode) fn detect() -> Option<Self> {
            #[cfg(feature = "tuning")]
            if std::env::var_os("RUSTY_DDS_ASTC_NO_AVX2").is_some() {
                return None;
            }
            if std::is_x86_feature_detected!("popcnt") {
                Some(Popcnt(()))
            } else {
                None
            }
        }
    }

    /// `block::rank_patterns` compiled with POPCNT: the same body (integer
    /// popcounts, so the same ranking), `count_ones` as one instruction
    /// instead of the baseline bit-twiddling sequence.
    pub(in crate::astc::encode) fn rank_patterns(
        _: Popcnt,
        bits: &[PartBits],
        labels: &[u8],
        k: usize,
        m: usize,
        top: &mut Vec<(u32, usize)>,
    ) {
        // SAFETY: a `Popcnt` exists only if the CPU supports POPCNT; the
        // body is safe code with no other requirement.
        unsafe { rank_k(bits, labels, k, m, top) }
    }

    /// # Safety
    ///
    /// The CPU supports POPCNT.
    #[target_feature(enable = "popcnt")]
    unsafe fn rank_k(
        bits: &[PartBits],
        labels: &[u8],
        k: usize,
        m: usize,
        top: &mut Vec<(u32, usize)>,
    ) {
        rank_patterns_body(bits, labels, k, m, top)
    }
}

#[cfg(target_arch = "x86_64")]
mod avx2 {
    use super::super::block::MAX_TEXELS;
    use crate::astc::tables::Grid;
    use core::arch::x86_64::{
        __m128i, __m256i, _mm256_add_epi32, _mm256_add_ps, _mm256_and_si256, _mm256_blendv_ps,
        _mm256_cmp_ps, _mm256_cmpgt_epi32, _mm256_cvtepu8_epi32, _mm256_cvttps_epi32,
        _mm256_div_ps, _mm256_i32gather_epi32, _mm256_i32gather_ps, _mm256_loadu_ps,
        _mm256_loadu_si256, _mm256_maskstore_ps, _mm256_max_ps, _mm256_min_epu32, _mm256_min_ps,
        _mm256_mul_ps, _mm256_mullo_epi32, _mm256_set1_epi32, _mm256_set1_ps, _mm256_setr_epi32,
        _mm256_setzero_ps, _mm256_setzero_si256, _mm256_srli_epi32, _mm256_storeu_ps,
        _mm256_storeu_si256, _mm256_sub_ps, _mm_loadl_epi64, _CMP_GT_OQ,
    };

    /// Proof that the CPU supports AVX2: only [`Avx2::detect`] makes one.
    #[derive(Clone, Copy)]
    pub(in crate::astc::encode) struct Avx2(());

    impl Avx2 {
        /// `Some` when the CPU reports AVX2. With the development-only
        /// `tuning` feature, `RUSTY_DDS_ASTC_NO_AVX2` set forces `None` (an
        /// A/B switch for the twins; a shipped build never reads it).
        pub(in crate::astc::encode) fn detect() -> Option<Self> {
            #[cfg(feature = "tuning")]
            if std::env::var_os("RUSTY_DDS_ASTC_NO_AVX2").is_some() {
                return None;
            }
            if std::is_x86_feature_detected!("avx2") {
                Some(Avx2(()))
            } else {
                None
            }
        }
    }

    /// The table shapes the kernels read through raw pointers (every load
    /// and store offset in them is bounded by these).
    fn shapes_ok(g: &Grid) -> bool {
        g.tp % 8 == 0
            && g.tp <= MAX_TEXELS
            && g.tap_i.len() <= g.tp
            && g.tap_iv.len() >= 4 * g.tp
            && g.tap_cv.len() >= 4 * g.tp
            && g.jp % 8 == 0
            && g.jp <= 64
            && g.count <= g.jp
            && g.feed_tv.len() >= g.fmax * g.jp
            && g.feed_cv.len() >= g.fmax * g.jp
            && g.feed_c16v.len() >= g.fmax * g.jp
            && g.feed_csv.len() >= g.jp
            && g.feed_denv.len() >= g.jp
    }

    /// Twin of `block::decimate` for a grid that is not the identity; the
    /// results equal the scalar's bit for bit. Returns false (nothing
    /// written) if the grid's tables do not have the padded shapes.
    ///
    /// Eight stored weights (or eight texels) per register, one per lane:
    /// each lane performs exactly the scalar's operations on its weight
    /// (texel), in the scalar's order, without FMA. The padded tables end
    /// each lane's tap or feed list with zero contributions; such a term adds
    /// a signed zero to a sum that is never `-0.0` (it starts at `+0.0`, and
    /// under round-to-nearest a sum is `-0.0` only if both terms are), which
    /// leaves the sum unchanged. `clamp(0, 64)` is `max(0, min(64, x))` with
    /// the operands in the order that returns `x` itself on ties and NaN, as
    /// `clamp` does. Lanes past the grid's weight count are never stored.
    pub(in crate::astc::encode) fn decimate(
        _: Avx2,
        g: &Grid,
        ideal: &[f32; MAX_TEXELS],
        iters: u8,
        out: &mut [f32; 64],
    ) -> bool {
        if !shapes_ok(g) {
            return false;
        }
        // SAFETY: an `Avx2` exists only if the CPU supports AVX2, and the
        // shapes the kernel relies on were just checked.
        unsafe { decimate_k(g, ideal, iters, out) };
        true
    }

    /// Lanes `j0 .. j0 + 8` below `count`, as a store mask.
    #[inline(always)]
    unsafe fn below(j0: usize, count: usize) -> __m256i {
        let lane = _mm256_setr_epi32(0, 1, 2, 3, 4, 5, 6, 7);
        _mm256_cmpgt_epi32(_mm256_set1_epi32(count as i32 - j0 as i32), lane)
    }

    /// # Safety
    ///
    /// AVX2 is available and `shapes_ok(g)`.
    #[target_feature(enable = "avx2")]
    unsafe fn decimate_k(g: &Grid, ideal: &[f32; MAX_TEXELS], iters: u8, out: &mut [f32; 64]) {
        let (jp, fmax, tp) = (g.jp, g.fmax, g.tp);
        let zero = _mm256_setzero_ps();
        let k64 = _mm256_set1_ps(64.0);
        // Gather indices are clamped into the buffers they address, so the
        // gathers stay in bounds whatever the tables hold: texels < 216,
        // stored weights < 64 (`build_grid` already guarantees both).
        let tmax = _mm256_set1_epi32(MAX_TEXELS as i32 - 1);
        let wmask = _mm256_set1_epi32(63);
        // SAFETY (all pointer arithmetic below): table loads read 8 lanes at
        // `f * jp + j0` (< fmax * jp, j0 + 8 <= jp) or `k * tp + t0` (k < 4,
        // t0 + 8 <= tp) and `feed_csv/denv[j0..j0 + 8]`, inside the tables by
        // `shapes_ok`; `ideal`/`resid` loads and stores cover `t0..t0 + 8`
        // with t0 + 8 <= tp <= 216; `out` loads cover `j0..j0 + 8` with
        // j0 + 8 <= jp <= 64, and its masked stores write only lanes
        // j0 + l < count <= 64. Unaligned forms throughout.
        let mut j0 = 0;
        while j0 < jp {
            let mut s = zero;
            for f in 0..fmax {
                let o = f * jp + j0;
                let t = _mm256_min_epu32(
                    _mm256_loadu_si256(g.feed_tv.as_ptr().add(o) as *const __m256i),
                    tmax,
                );
                let c = _mm256_loadu_ps(g.feed_cv.as_ptr().add(o));
                s = _mm256_add_ps(
                    s,
                    _mm256_mul_ps(_mm256_i32gather_ps::<4>(ideal.as_ptr(), t), c),
                );
            }
            let cs = _mm256_loadu_ps(g.feed_csv.as_ptr().add(j0));
            let q = _mm256_div_ps(_mm256_mul_ps(k64, s), cs);
            let v = _mm256_blendv_ps(
                _mm256_set1_ps(32.0),
                q,
                _mm256_cmp_ps::<_CMP_GT_OQ>(cs, zero),
            );
            _mm256_maskstore_ps(out.as_mut_ptr().add(j0), below(j0, g.count), v);
            j0 += 8;
        }
        if iters == 0 {
            return;
        }
        let mut resid = [0f32; MAX_TEXELS];
        let k16 = _mm256_set1_ps(1.0 / 16.0);
        for _ in 0..iters {
            let mut t0 = 0;
            while t0 < tp {
                let mut v = zero;
                for k in 0..4 {
                    let o = k * tp + t0;
                    let i = _mm256_and_si256(
                        _mm256_loadu_si256(g.tap_iv.as_ptr().add(o) as *const __m256i),
                        wmask,
                    );
                    let c = _mm256_loadu_ps(g.tap_cv.as_ptr().add(o));
                    v = _mm256_add_ps(
                        v,
                        _mm256_mul_ps(_mm256_i32gather_ps::<4>(out.as_ptr(), i), c),
                    );
                }
                let r = _mm256_sub_ps(
                    _mm256_mul_ps(v, k16),
                    _mm256_mul_ps(k64, _mm256_loadu_ps(ideal.as_ptr().add(t0))),
                );
                _mm256_storeu_ps(resid.as_mut_ptr().add(t0), r);
                t0 += 8;
            }
            let mut j0 = 0;
            while j0 < jp {
                let mut num = zero;
                for f in 0..fmax {
                    let o = f * jp + j0;
                    let t = _mm256_min_epu32(
                        _mm256_loadu_si256(g.feed_tv.as_ptr().add(o) as *const __m256i),
                        tmax,
                    );
                    let c = _mm256_loadu_ps(g.feed_c16v.as_ptr().add(o));
                    num = _mm256_add_ps(
                        num,
                        _mm256_mul_ps(c, _mm256_i32gather_ps::<4>(resid.as_ptr(), t)),
                    );
                }
                let den = _mm256_loadu_ps(g.feed_denv.as_ptr().add(j0));
                let o = _mm256_loadu_ps(out.as_ptr().add(j0));
                let x = _mm256_sub_ps(o, _mm256_div_ps(num, den));
                let x = _mm256_max_ps(zero, _mm256_min_ps(k64, x));
                let v = _mm256_blendv_ps(o, x, _mm256_cmp_ps::<_CMP_GT_OQ>(den, zero));
                _mm256_maskstore_ps(out.as_mut_ptr().add(j0), below(j0, g.count), v);
                j0 += 8;
            }
        }
    }

    /// Twin of `block::infill_eff_scalar`: the specification's infill
    /// `(sum(grid[k] * c) + 8) >> 4` for eight texels at a time, in 32-bit
    /// integer lanes (exact: at most 64 * 16 before the shift), from the
    /// padded tap tables. Writes only texels `0..taps`. False (nothing
    /// written) if the grid's tables do not have the padded shapes.
    pub(in crate::astc::encode) fn infill_eff(
        _: Avx2,
        g: &Grid,
        gw: &[u8; 64],
        out: &mut [u8; MAX_TEXELS],
    ) -> bool {
        if !shapes_ok(g) {
            return false;
        }
        // SAFETY: as in `decimate`.
        unsafe { infill_k(g, gw, out) };
        true
    }

    /// # Safety
    ///
    /// AVX2 is available and `shapes_ok(g)`.
    #[target_feature(enable = "avx2")]
    unsafe fn infill_k(g: &Grid, gw: &[u8; 64], out: &mut [u8; MAX_TEXELS]) {
        let (tp, n) = (g.tp, g.tap_i.len());
        let mut wi = [0i32; 64];
        // SAFETY: the widening loads read `gw[8 q..8 q + 8]` (q < 8, inside
        // the 64 bytes) and store 8 lanes at `wi[8 q..]` (inside 64); table
        // loads at `k * tp + t0` (k < 4, t0 + 8 <= tp) are inside
        // `tap_iv`/`tap_cv`; gathers from `wi` use indices masked to 0..64;
        // the result store writes the local `r`.
        for q in 0..8 {
            let b = _mm_loadl_epi64(gw.as_ptr().add(8 * q) as *const __m128i);
            _mm256_storeu_si256(
                wi.as_mut_ptr().add(8 * q) as *mut __m256i,
                _mm256_cvtepu8_epi32(b),
            );
        }
        let wmask = _mm256_set1_epi32(63);
        let k8 = _mm256_set1_epi32(8);
        let mut r = [0i32; 8];
        let mut t0 = 0;
        while t0 < tp {
            let mut s = _mm256_setzero_si256();
            for k in 0..4 {
                let o = k * tp + t0;
                let i = _mm256_and_si256(
                    _mm256_loadu_si256(g.tap_iv.as_ptr().add(o) as *const __m256i),
                    wmask,
                );
                let c = _mm256_cvttps_epi32(_mm256_loadu_ps(g.tap_cv.as_ptr().add(o)));
                s = _mm256_add_epi32(
                    s,
                    _mm256_mullo_epi32(_mm256_i32gather_epi32::<4>(wi.as_ptr(), i), c),
                );
            }
            let v = _mm256_srli_epi32(_mm256_add_epi32(s, k8), 4);
            _mm256_storeu_si256(r.as_mut_ptr() as *mut __m256i, v);
            for (l, &x) in r.iter().enumerate().take(n.saturating_sub(t0).min(8)) {
                out[t0 + l] = x as u8;
            }
            t0 += 8;
        }
    }

    /// Twin of `block::residual`, bit for bit: the per-texel terms
    /// `d * d * tw` eight texels at a time (each lane the scalar's
    /// operations in its order, no FMA), then summed one at a time in texel
    /// order, as the scalar sums them. `None` if the grid's tables do not
    /// have the padded shapes.
    pub(in crate::astc::encode) fn residual(
        _: Avx2,
        g: &Grid,
        dec: &[f32; 64],
        ideal: &[f32; MAX_TEXELS],
        tw: &[f32; MAX_TEXELS],
    ) -> Option<f32> {
        if !shapes_ok(g) {
            return None;
        }
        // SAFETY: as in `decimate`.
        Some(unsafe { residual_k(g, dec, ideal, tw) })
    }

    /// # Safety
    ///
    /// AVX2 is available and `shapes_ok(g)`.
    #[target_feature(enable = "avx2")]
    unsafe fn residual_k(
        g: &Grid,
        dec: &[f32; 64],
        ideal: &[f32; MAX_TEXELS],
        tw: &[f32; MAX_TEXELS],
    ) -> f32 {
        let (tp, n) = (g.tp, g.tap_i.len());
        let k = _mm256_set1_ps(1.0 / (16.0 * 64.0));
        let wmask = _mm256_set1_epi32(63);
        let mut e = 0f32;
        let mut p = [0f32; 8];
        // SAFETY: table loads at `kk * tp + t0` (kk < 4, t0 + 8 <= tp) are
        // inside `tap_iv`/`tap_cv`; `ideal`/`tw` loads cover `t0..t0 + 8`
        // with t0 + 8 <= tp <= 216; gathers from `dec` use indices masked
        // to 0..64; the store writes the local `p` (8 lanes).
        let mut t0 = 0;
        while t0 < tp {
            let mut v = _mm256_setzero_ps();
            for kk in 0..4 {
                let o = kk * tp + t0;
                let i = _mm256_and_si256(
                    _mm256_loadu_si256(g.tap_iv.as_ptr().add(o) as *const __m256i),
                    wmask,
                );
                let c = _mm256_loadu_ps(g.tap_cv.as_ptr().add(o));
                v = _mm256_add_ps(
                    v,
                    _mm256_mul_ps(_mm256_i32gather_ps::<4>(dec.as_ptr(), i), c),
                );
            }
            let d = _mm256_sub_ps(_mm256_mul_ps(v, k), _mm256_loadu_ps(ideal.as_ptr().add(t0)));
            let q = _mm256_mul_ps(_mm256_mul_ps(d, d), _mm256_loadu_ps(tw.as_ptr().add(t0)));
            _mm256_storeu_ps(p.as_mut_ptr(), q);
            for &x in &p[..(n - t0).min(8)] {
                e += x;
            }
            t0 += 8;
        }
        e
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

    /// The AVX2 decimation and residual equal the scalar bit for bit
    /// (`to_bits`), for every grid of every 2D footprint, iteration counts
    /// 0..=4, ideal weights including 0, 1 and -0.0; stored weights past the
    /// grid's count are left untouched. Skipped (with a note) on a CPU
    /// without AVX2.
    #[test]
    fn decimate_and_residual_match_scalar() {
        use super::super::block::{decimate_scalar, residual_scalar};
        use super::super::context::Ctx;
        use crate::astc::footprint::AstcFootprint;
        let Some(tok) = super::Avx2::detect() else {
            eprintln!("no AVX2: decimate/residual twins not exercised");
            return;
        };
        let mut rng = Rng::new(0xDEC1_3A7E);
        let f = |rng: &mut Rng| -> f32 {
            match rng.below(8) {
                0 => 0.0,
                1 => 1.0,
                2 => -0.0,
                _ => (rng.next_u64() >> 40) as f32 / (1u64 << 24) as f32,
            }
        };
        let mut grids = 0;
        for &fp in AstcFootprint::ALL_2D {
            let ctx = Ctx::new(fp, 1);
            for g in ctx.grids.iter().filter(|g| !g.full) {
                grids += 1;
                for trial in 0..6 {
                    let mut ideal = [0f32; MAX_TEXELS];
                    let mut tw = [0f32; MAX_TEXELS];
                    for i in 0..MAX_TEXELS {
                        ideal[i] = f(&mut rng);
                        tw[i] = f(&mut rng).abs() * 1000.0;
                    }
                    let iters = (trial % 5) as u8;
                    let mut a = [f32::NAN; 64];
                    let mut b = [f32::NAN; 64];
                    assert!(super::decimate(tok, g, &ideal, iters, &mut a));
                    decimate_scalar(g, &ideal, iters, &mut b);
                    for j in 0..64 {
                        assert_eq!(
                            a[j].to_bits(),
                            b[j].to_bits(),
                            "{fp} grid {}x{} iters {iters} j {j}",
                            g.w,
                            g.h
                        );
                    }
                    let mut dec = [0f32; 64];
                    for v in dec.iter_mut() {
                        *v = f(&mut rng) * 64.0;
                    }
                    let ea = super::residual(tok, g, &dec, &ideal, &tw).unwrap();
                    let eb = residual_scalar(g, &dec, &ideal, &tw);
                    assert_eq!(
                        ea.to_bits(),
                        eb.to_bits(),
                        "{fp} grid {}x{} residual",
                        g.w,
                        g.h
                    );
                    // Also on the decimated weights themselves (the encoder's use).
                    let ea = super::residual(tok, g, &b, &ideal, &tw).unwrap();
                    let eb = residual_scalar(g, &b, &ideal, &tw);
                    assert_eq!(ea.to_bits(), eb.to_bits());
                }
            }
        }
        assert!(grids > 500, "{grids}");
    }

    /// The SSE2 refit sums equal the scalar's bit for bit (`to_bits`) on
    /// every channel the scalar computes, for random partitions, weights
    /// (incl. 0 and 64), every dual-plane channel and both channel counts.
    #[test]
    fn refit_sums_match_scalar() {
        use super::super::block::refit_sums_scalar;
        let mut rng = Rng::new(0x4EF1_7500);
        for trial in 0..20_000 {
            let n = 1 + rng.below(MAX_TEXELS as u64) as usize;
            let px: Vec<[u8; 4]> = (0..n)
                .map(|_| rng.next_u64().to_le_bytes()[..4].try_into().unwrap())
                .collect();
            let m = 1 + rng.below(n as u64) as usize;
            let idx: Vec<u8> = (0..m).map(|_| rng.below(n as u64) as u8).collect();
            let mut w1 = [0u8; MAX_TEXELS];
            let mut w2 = [0u8; MAX_TEXELS];
            for i in 0..MAX_TEXELS {
                w1[i] = [0, 64, rng.below(65) as u8][rng.below(3) as usize];
                w2[i] = rng.below(65) as u8;
            }
            let chans = if trial % 3 == 0 { 3 } else { 4 };
            let ccs = rng.below(5) as usize;
            let ccs = if chans == 3 && ccs == 3 { 4 } else { ccs };
            let a = super::refit_sums(&px, &idx, &w1, &w2, ccs);
            let b = refit_sums_scalar(&px, &idx, &w1, &w2, ccs, chans);
            let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
            assert_eq!(bits(&a.a), bits(&b.a), "trial {trial}");
            assert_eq!(bits(&a.b), bits(&b.b), "trial {trial}");
            assert_eq!(bits(&a.d), bits(&b.d), "trial {trial}");
            assert_eq!(bits(&a.x0[..chans]), bits(&b.x0[..chans]), "trial {trial}");
            assert_eq!(bits(&a.x1[..chans]), bits(&b.x1[..chans]), "trial {trial}");
        }
    }

    /// The AVX2 infill equals the scalar for every grid of every 2D
    /// footprint (identity grids included) and random stored weights in
    /// 0..=64; bytes past the footprint's texels are left untouched.
    #[test]
    fn infill_eff_matches_scalar() {
        use super::super::block::infill_eff_scalar;
        use super::super::context::Ctx;
        use crate::astc::footprint::AstcFootprint;
        let Some(tok) = super::Avx2::detect() else {
            eprintln!("no AVX2: infill twin not exercised");
            return;
        };
        let mut rng = Rng::new(0x1AF1_1100);
        for &fp in AstcFootprint::ALL_2D {
            let ctx = Ctx::new(fp, 1);
            for g in &ctx.grids {
                for _ in 0..8 {
                    let mut gw = [0u8; 64];
                    for v in gw.iter_mut() {
                        *v = [0, 64, rng.below(65) as u8][rng.below(3) as usize];
                    }
                    let mut a = [0xA5u8; MAX_TEXELS];
                    let mut b = [0xA5u8; MAX_TEXELS];
                    assert!(super::infill_eff(tok, g, &gw, &mut a));
                    infill_eff_scalar(g, &gw, &mut b);
                    assert_eq!(a, b, "{fp} grid {}x{}", g.w, g.h);
                }
            }
        }
    }
}
