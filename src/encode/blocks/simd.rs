//! AVX2 twins of the hot index-fit kernels, feature-gated (`simd`, default
//! on) with runtime detection and the scalar paths as permanent oracles.
//!
//! Layout trick shared by all kernels: vectorize ACROSS the block's 16
//! pixels for one palette entry at a time — `_mm256_madd_epi16(diff, diff)`
//! yields per-pixel channel-pair squared sums, and entries are processed in
//! ascending index order with strict `<` blends, so the argmin (including
//! lowest-index tie behaviour) is EXACTLY the scalar exhaustive scan's.
//! Every kernel is proven against its scalar twin by an exhaustive-random
//! oracle test; the dispatchers below fall back to scalar off-x86 or when
//! AVX2 is absent, so output is identical on every CPU.

#![allow(unsafe_code)]

#[cfg(target_arch = "x86_64")]
#[inline]
pub(super) fn has_avx2() -> bool {
    use std::sync::OnceLock;
    static F: OnceLock<bool> = OnceLock::new();
    *F.get_or_init(|| std::is_x86_feature_detected!("avx2"))
}

#[cfg(not(target_arch = "x86_64"))]
#[inline]
pub(super) fn has_avx2() -> bool {
    false
}


/// AVX2 twin of the mode-6 exhaustive index fit: evaluates ALL 16 palette
/// entries; identical output to `fit_indices_mode6_exhaustive`.
#[cfg(target_arch = "x86_64")]
pub(super) fn fit_indices_mode6_avx2(
    pixels: &[[u8; 4]; 16],
    pal: &[[u8; 4]; 16],
) -> ([u8; 16], i64) {
    debug_assert!(has_avx2());
    // SAFETY: dispatch guaranteed AVX2 (debug-asserted above, checked at the
    // call site).
    unsafe { fit_indices_mode6_avx2_impl(pixels, pal) }
}

/// Squared distance from eight consecutive pixels to one palette point, as
/// eight packed `i32` in pixel order.
///
/// `_mm256_hadd_epi32` folds within 128-bit lanes, so the pair sums come out
/// interleaved as `[p0,p1,p4,p5,p2,p3,p6,p7]`; `perm` puts them back in order.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn sse8_rgba(
    base: *const u8,
    off: usize,
    pv: std::arch::x86_64::__m256i,
    perm: std::arch::x86_64::__m256i,
) -> std::arch::x86_64::__m256i {
    use std::arch::x86_64::*;
    let a = _mm256_cvtepu8_epi16(_mm_loadu_si128(base.add(off) as *const __m128i));
    let b = _mm256_cvtepu8_epi16(_mm_loadu_si128(base.add(off + 16) as *const __m128i));
    let da = _mm256_sub_epi16(a, pv);
    let db = _mm256_sub_epi16(b, pv);
    // madd gives (r*r + g*g, b*b + a*a) per pixel; hadd folds those two.
    let h = _mm256_hadd_epi32(_mm256_madd_epi16(da, da), _mm256_madd_epi16(db, db));
    _mm256_permutevar8x32_epi32(h, perm)
}

/// Exhaustive mode-6 index fit, entirely in registers.
///
/// The previous shape computed the sixteen per-pixel distances into a `[i32; 16]`
/// and then ran a **scalar** sixteen-iteration min-tracking loop over it, once
/// per palette entry — 256 scalar compare-branches per fit, on top of two
/// store-forwarding stalls per entry (the vector code stored to a stack array
/// that scalar code immediately read back). At 3.168 fits per block that is the
/// dominant shape in the encoder's hot path.
///
/// Now the distances stay in two `__m256i` and the running minimum is tracked
/// with compare-and-blend, so nothing round-trips through memory until the two
/// results are extracted once at the end.
///
/// Selection is unchanged: `_mm256_cmpgt_epi32(best, cur)` is exactly
/// `cur < best`, which keeps the lowest index on ties as the scalar twin does.
#[target_feature(enable = "avx2")]
unsafe fn fit_indices_mode6_avx2_impl(
    pixels: &[[u8; 4]; 16],
    pal: &[[u8; 4]; 16],
) -> ([u8; 16], i64) {
    use std::arch::x86_64::*;
    let base = pixels.as_ptr() as *const u8;
    let perm = _mm256_setr_epi32(0, 1, 4, 5, 2, 3, 6, 7);

    let mut best_lo = _mm256_set1_epi32(i32::MAX);
    let mut best_hi = _mm256_set1_epi32(i32::MAX);
    let mut idx_lo = _mm256_setzero_si256();
    let mut idx_hi = _mm256_setzero_si256();

    for (k, &entry) in pal.iter().enumerate() {
        let pv = _mm256_set1_epi64x(u64::from_le_bytes([
            entry[0], 0, entry[1], 0, entry[2], 0, entry[3], 0,
        ]) as i64);
        let kv = _mm256_set1_epi32(k as i32);

        let cur_lo = sse8_rgba(base, 0, pv, perm);
        let cur_hi = sse8_rgba(base, 32, pv, perm);

        let m_lo = _mm256_cmpgt_epi32(best_lo, cur_lo);
        let m_hi = _mm256_cmpgt_epi32(best_hi, cur_hi);
        best_lo = _mm256_blendv_epi8(best_lo, cur_lo, m_lo);
        best_hi = _mm256_blendv_epi8(best_hi, cur_hi, m_hi);
        idx_lo = _mm256_blendv_epi8(idx_lo, kv, m_lo);
        idx_hi = _mm256_blendv_epi8(idx_hi, kv, m_hi);
    }

    let mut e = [0i32; 16];
    let mut ix = [0i32; 16];
    _mm256_storeu_si256(e.as_mut_ptr() as *mut __m256i, best_lo);
    _mm256_storeu_si256(e.as_mut_ptr().add(8) as *mut __m256i, best_hi);
    _mm256_storeu_si256(ix.as_mut_ptr() as *mut __m256i, idx_lo);
    _mm256_storeu_si256(ix.as_mut_ptr().add(8) as *mut __m256i, idx_hi);

    let mut best_i = [0u8; 16];
    let mut err = 0i64;
    for i in 0..16 {
        best_i[i] = ix[i] as u8;
        err += e[i] as i64;
    }
    (best_i, err)
}

/// AVX2 4-color BC1 fit (opaque path): identical selection + SSE to the
/// scalar `bc1_fit_4color` exhaustive branch, full-block evaluation with the
/// early-abort check applied on the completed total (an aborted candidate
/// could never win, so acceptance is unchanged).
#[cfg(target_arch = "x86_64")]
pub(super) fn bc1_fit_4color_avx2(
    pixels: &[[u8; 4]; 16],
    colors: &[[u8; 3]; 4],
    err_limit: i32,
) -> Option<(u32, i32)> {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above).
    unsafe { bc1_fit_4color_avx2_impl(pixels, colors, err_limit) }
}

/// RGB squared distance from eight consecutive pixels to one point, packed in
/// pixel order. The alpha lane is masked to zero on both sides so it cannot
/// contribute — see [`sse8_rgba`] for the `hadd` lane ordering.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn sse8_rgb(
    base: *const u8,
    off: usize,
    pv: std::arch::x86_64::__m256i,
    keep: std::arch::x86_64::__m256i,
    perm: std::arch::x86_64::__m256i,
) -> std::arch::x86_64::__m256i {
    use std::arch::x86_64::*;
    let a = _mm256_and_si256(
        _mm256_cvtepu8_epi16(_mm_loadu_si128(base.add(off) as *const __m128i)),
        keep,
    );
    let b = _mm256_and_si256(
        _mm256_cvtepu8_epi16(_mm_loadu_si128(base.add(off + 16) as *const __m128i)),
        keep,
    );
    let da = _mm256_sub_epi16(a, pv);
    let db = _mm256_sub_epi16(b, pv);
    let h = _mm256_hadd_epi32(_mm256_madd_epi16(da, da), _mm256_madd_epi16(db, db));
    _mm256_permutevar8x32_epi32(h, perm)
}

/// BC1 four-colour fit, entirely in registers.
///
/// Same defect as the mode-6 kernel had before 0.3.24, one file over: the
/// distances were computed in vector registers, **stored to a `[i32; 16]`**, and
/// a **scalar sixteen-iteration loop** read them back to track the minimum —
/// once per colour, four colours per call. Two store-forwarding stalls and 64
/// scalar compare-branches per fit.
///
/// The extraction loop below stays scalar deliberately: it carries the
/// early-abort on the running total, which is order-dependent and runs once per
/// call rather than once per colour.
#[target_feature(enable = "avx2")]
unsafe fn bc1_fit_4color_avx2_impl(
    pixels: &[[u8; 4]; 16],
    colors: &[[u8; 3]; 4],
    err_limit: i32,
) -> Option<(u32, i32)> {
    use std::arch::x86_64::*;
    let base = pixels.as_ptr() as *const u8;
    let perm = _mm256_setr_epi32(0, 1, 4, 5, 2, 3, 6, 7);
    let keep = _mm256_set1_epi64x(
        u64::from_le_bytes([0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0]) as i64,
    );

    let mut best_lo = _mm256_set1_epi32(i32::MAX);
    let mut best_hi = _mm256_set1_epi32(i32::MAX);
    let mut idx_lo = _mm256_setzero_si256();
    let mut idx_hi = _mm256_setzero_si256();

    for (k, c) in colors.iter().enumerate() {
        let pv = _mm256_set1_epi64x(
            u64::from_le_bytes([c[0], 0, c[1], 0, c[2], 0, 0, 0]) as i64,
        );
        let kv = _mm256_set1_epi32(k as i32);
        let cur_lo = sse8_rgb(base, 0, pv, keep, perm);
        let cur_hi = sse8_rgb(base, 32, pv, keep, perm);
        let m_lo = _mm256_cmpgt_epi32(best_lo, cur_lo);
        let m_hi = _mm256_cmpgt_epi32(best_hi, cur_hi);
        best_lo = _mm256_blendv_epi8(best_lo, cur_lo, m_lo);
        best_hi = _mm256_blendv_epi8(best_hi, cur_hi, m_hi);
        idx_lo = _mm256_blendv_epi8(idx_lo, kv, m_lo);
        idx_hi = _mm256_blendv_epi8(idx_hi, kv, m_hi);
    }

    let mut e = [0i32; 16];
    let mut ix = [0i32; 16];
    _mm256_storeu_si256(e.as_mut_ptr() as *mut __m256i, best_lo);
    _mm256_storeu_si256(e.as_mut_ptr().add(8) as *mut __m256i, best_hi);
    _mm256_storeu_si256(ix.as_mut_ptr() as *mut __m256i, idx_lo);
    _mm256_storeu_si256(ix.as_mut_ptr().add(8) as *mut __m256i, idx_hi);

    let mut table = 0u32;
    let mut err = 0i32;
    for i in 0..16 {
        table |= (ix[i] as u32) << (2 * i);
        err += e[i];
        if err >= err_limit {
            return None;
        }
    }
    Some((table, err))
}


#[cfg(test)]
mod oracle {
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn mode6_avx2_matches_scalar_exhaustive() {
        if !super::has_avx2() {
            eprintln!("AVX2 not available; skipping");
            return;
        }
        let mut state = 0xA5A5F00DDEADBEEFu64;
        let mut rng = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..200_000u32 {
            let mut px = [[0u8; 4]; 16];
            let mut pal = [[0u8; 4]; 16];
            for p in px.iter_mut() {
                let r = rng();
                *p = [(r) as u8, (r >> 8) as u8, (r >> 16) as u8, (r >> 24) as u8];
            }
            for p in pal.iter_mut() {
                let r = rng();
                *p = [(r) as u8, (r >> 8) as u8, (r >> 16) as u8, (r >> 24) as u8];
            }
            let fast = super::fit_indices_mode6_avx2(&px, &pal);
            let slow = super::super::fit_indices_mode6_exhaustive(&px, &pal);
            assert_eq!(fast, slow, "case {case}");
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn bc1_avx2_matches_scalar() {
        if !super::has_avx2() {
            eprintln!("AVX2 not available; skipping");
            return;
        }
        let mut state = 0x0123456789ABCDEFu64;
        let mut rng = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..200_000u32 {
            let mut px = [[0u8; 4]; 16];
            for p in px.iter_mut() {
                let r = rng();
                *p = [(r) as u8, (r >> 8) as u8, (r >> 16) as u8, (r >> 24) as u8];
            }
            let mut colors = [[0u8; 3]; 4];
            for c in colors.iter_mut() {
                let r = rng();
                *c = [(r) as u8, (r >> 8) as u8, (r >> 16) as u8];
            }
            let fast = super::bc1_fit_4color_avx2(&px, &colors, i32::MAX);
            let slow = super::super::bc1_fit_4color_scalar(&px, &colors, i32::MAX);
            assert_eq!(fast, slow, "case {case}");
            // Abort parity: prefix-abort and total-abort agree because
            // squared errors are non-negative.
            if let Some((_, e)) = slow {
                let lim = (e / 2).max(1);
                assert_eq!(
                    super::bc1_fit_4color_avx2(&px, &colors, lim).is_none(),
                    super::super::bc1_fit_4color_scalar(&px, &colors, lim).is_none(),
                    "abort parity (case {case})"
                );
            }
        }
    }
}
