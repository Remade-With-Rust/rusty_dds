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

/// Per-pixel squared distances of all 16 RGBA pixels to one RGBA point.
///
/// # Safety
/// Caller guarantees AVX2 is available.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn sse16_rgba(pixels: &[[u8; 4]; 16], point: [u8; 4], out: &mut [i32; 16]) {
    use std::arch::x86_64::*;
    let base = pixels.as_ptr() as *const u8;
    // Broadcast the point's RGBA as u16 lanes: [r,g,b,a] repeated 4x per ymm.
    let p64 = u64::from_le_bytes([
        point[0], 0, point[1], 0, point[2], 0, point[3], 0,
    ]);
    let pv = _mm256_set1_epi64x(p64 as i64);
    for q in 0..4 {
        // 4 pixels (16 bytes) -> 16 u16 lanes.
        let raw = _mm_loadu_si128(base.add(q * 16) as *const __m128i);
        let px = _mm256_cvtepu8_epi16(raw);
        let d = _mm256_sub_epi16(px, pv);
        // (r*r+g*g, b*b+a*a) per pixel: 8 i32 lanes.
        let sq = _mm256_madd_epi16(d, d);
        // Sum adjacent i32 pairs -> per-pixel SSE.
        let hi = _mm256_srli_epi64(sq, 32);
        let s = _mm256_add_epi32(sq, hi);
        // Per-pixel sums now sit in i32 lanes 0 and 2 of each 128-bit half.
        let mut tmp = [0i32; 8];
        _mm256_storeu_si256(tmp.as_mut_ptr() as *mut __m256i, s);
        out[q * 4] = tmp[0];
        out[q * 4 + 1] = tmp[2];
        out[q * 4 + 2] = tmp[4];
        out[q * 4 + 3] = tmp[6];
    }
}

/// AVX2 twin of the mode-6 exhaustive index fit: evaluates ALL 16 palette
/// entries; identical output to `fit_indices_mode6_exhaustive`.
#[cfg(target_arch = "x86_64")]
pub(super) fn fit_indices_mode6_avx2(
    pixels: &[[u8; 4]; 16],
    pal: &[[u8; 4]; 16],
) -> ([u8; 16], i64) {
    debug_assert!(has_avx2());
    let mut best_e = [i32::MAX; 16];
    let mut best_i = [0u8; 16];
    let mut sse = [0i32; 16];
    for (k, &entry) in pal.iter().enumerate() {
        // SAFETY: dispatch guaranteed AVX2 (debug-asserted above, checked
        // at the call site).
        unsafe { sse16_rgba(pixels, entry, &mut sse) };
        for i in 0..16 {
            if sse[i] < best_e[i] {
                best_e[i] = sse[i];
                best_i[i] = k as u8;
            }
        }
    }
    let mut err = 0i64;
    for &e in &best_e {
        err += e as i64;
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
    let mut best_e = [i32::MAX; 16];
    let mut best_i = [0u8; 16];
    let mut sse = [0i32; 16];
    for (k, c) in colors.iter().enumerate() {
        // Alpha lane must not contribute: give the point each pixel's own
        // alpha? Cheaper: zero contribution by matching — use alpha 0 and
        // mask pixel alphas by copying RGB into a scratch with A=0.
        let point = [c[0], c[1], c[2], 0];
        // SAFETY: AVX2 guaranteed by dispatch.
        unsafe { sse16_rgba_noalpha(pixels, point, &mut sse) };
        for i in 0..16 {
            if sse[i] < best_e[i] {
                best_e[i] = sse[i];
                best_i[i] = k as u8;
            }
        }
    }
    let mut table = 0u32;
    let mut err = 0i32;
    for i in 0..16 {
        table |= (best_i[i] as u32) << (2 * i);
        err += best_e[i];
        if err >= err_limit {
            return None;
        }
    }
    Some((table, err))
}

/// Like `sse16_rgba` but the alpha channel is excluded (RGB distance).
///
/// # Safety
/// Caller guarantees AVX2 is available.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn sse16_rgba_noalpha(pixels: &[[u8; 4]; 16], point: [u8; 4], out: &mut [i32; 16]) {
    use std::arch::x86_64::*;
    let base = pixels.as_ptr() as *const u8;
    let p64 = u64::from_le_bytes([
        point[0], 0, point[1], 0, point[2], 0, 0, 0,
    ]);
    let pv = _mm256_set1_epi64x(p64 as i64);
    // Zero the alpha u16 lane of the pixels: lane mask keeps r,g,b.
    let keep = _mm256_set1_epi64x(u64::from_le_bytes([
        0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0,
    ]) as i64);
    for q in 0..4 {
        let raw = _mm_loadu_si128(base.add(q * 16) as *const __m128i);
        let px = _mm256_and_si256(_mm256_cvtepu8_epi16(raw), keep);
        let d = _mm256_sub_epi16(px, pv);
        let sq = _mm256_madd_epi16(d, d);
        let hi = _mm256_srli_epi64(sq, 32);
        let s = _mm256_add_epi32(sq, hi);
        let mut tmp = [0i32; 8];
        _mm256_storeu_si256(tmp.as_mut_ptr() as *mut __m256i, s);
        out[q * 4] = tmp[0];
        out[q * 4 + 1] = tmp[2];
        out[q * 4 + 2] = tmp[4];
        out[q * 4 + 3] = tmp[6];
    }
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
