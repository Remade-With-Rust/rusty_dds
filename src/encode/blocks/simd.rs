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



/// Nearest-palette selection for one BC4/BC5 alpha block: sixteen samples
/// against an eight-entry palette, in registers.
///
/// The scalar path scans the eight entries per sample with a strict `<`, so the
/// lowest index wins a tie; `_mm256_cmpgt_epi16(best, cur)` is exactly
/// `cur < best` and preserves that. `AlphaSelect` exists to make the scalar scan
/// cheap by turning it into a threshold lookup; vectorised, the plain scan is
/// cheaper still and needs no selector built per candidate.
///
/// Returns the sixteen indices and the total squared error. The caller's early
/// abort is applied to the completed total: error only accumulates, so a prefix
/// that would have tripped the limit leaves a total that trips it too, and
/// acceptance is unchanged.
#[cfg(target_arch = "x86_64")]
pub(super) fn alpha_fit_avx2(palette: &[u8; 8], samples: &[u8; 16]) -> ([u8; 16], i32) {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above).
    unsafe { alpha_fit_avx2_impl(palette, samples) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn alpha_fit_avx2_impl(palette: &[u8; 8], samples: &[u8; 16]) -> ([u8; 16], i32) {
    use std::arch::x86_64::*;
    // Sixteen samples as sixteen i16 lanes — one register.
    let sv = _mm256_cvtepu8_epi16(_mm_loadu_si128(samples.as_ptr() as *const __m128i));
    let mut best = _mm256_set1_epi16(i16::MAX);
    let mut idx = _mm256_setzero_si256();

    for (k, &p) in palette.iter().enumerate() {
        let pv = _mm256_set1_epi16(p as i16);
        // |p - s| fits i16 for u8 inputs, and abs keeps the square exact below.
        let d = _mm256_abs_epi16(_mm256_sub_epi16(pv, sv));
        let m = _mm256_cmpgt_epi16(best, d);
        best = _mm256_blendv_epi8(best, d, m);
        idx = _mm256_blendv_epi8(idx, _mm256_set1_epi16(k as i16), m);
    }

    // madd squares and folds adjacent pairs: eight i32 partial sums.
    let sq = _mm256_madd_epi16(best, best);
    let mut parts = [0i32; 8];
    _mm256_storeu_si256(parts.as_mut_ptr() as *mut __m256i, sq);
    let err: i32 = parts.iter().sum();

    let mut iw = [0i16; 16];
    _mm256_storeu_si256(iw.as_mut_ptr() as *mut __m256i, idx);
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = iw[i] as u8;
    }
    (out, err)
}


/// Search a BC7 alpha endpoint neighbourhood — all 24 offsets — inside **one**
/// `#[target_feature]` call.
///
/// # Why this exists
///
/// Call counters on alpha-structured content: BC7 encode crosses into an AVX2
/// kernel **58 times per block**, and **50 of those are alpha** — modes 4 and 5
/// each run a 5x5-minus-centre endpoint search, 25 scans apiece, and each scan
/// was its own boundary crossing plus its own `OnceLock` check. A
/// `#[target_feature]` function cannot be inlined into a caller that lacks the
/// feature, so that is 50 real calls per block. 0.3.28 measured that same
/// boundary at 26.7% of BC1 decode — enough there to invert the sign of the
/// whole result.
///
/// Hoisting the loop inside drops mode 5 and mode 4 to three crossings each,
/// and lets the sixteen samples be loaded and widened **once** rather than
/// twenty-five times.
///
/// # Why it can score without tracking indices
///
/// The scalar twin adds `(pal[best] - a)^2` where `best` is the nearest entry,
/// and that equals `(min_j |pal[j] - a|)^2` — the error depends only on the
/// minimum distance, never on which entry achieved it. So the search needs only
/// `_mm256_min_epi16` and no index blending at all; the winner's indices come
/// from one ordinary scan afterwards, which is also what keeps the lowest-index
/// tie-break exactly the scalar one.
///
/// `N` is 4 for mode 5 (`W2`, 8-bit endpoints) or 8 for mode 4 (`W3`, 6-bit
/// endpoints, unquantized here). Returns the best `(c0, c1, err)`, seeded with
/// `seed_err` so it only ever reports a **strictly** better candidate — same
/// order and same strict `<` as the loop it replaces.
#[cfg(target_arch = "x86_64")]
pub(super) fn alpha_nbhd_avx2<const N: usize>(
    alpha: &[u8; 16],
    s0: u8,
    s1: u8,
    clamp_hi: i32,
    seed_err: i32,
) -> (u8, u8, i32) {
    debug_assert!(has_avx2());
    debug_assert!(N == 4 || N == 8);
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above).
    unsafe { alpha_nbhd_avx2_impl::<N>(alpha, s0, s1, clamp_hi, seed_err) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn alpha_nbhd_avx2_impl<const N: usize>(
    alpha: &[u8; 16],
    s0: u8,
    s1: u8,
    clamp_hi: i32,
    seed_err: i32,
) -> (u8, u8, i32) {
    use std::arch::x86_64::*;
    const W2: [u32; 4] = [0, 21, 43, 64];
    const W3: [u32; 8] = [0, 9, 18, 27, 37, 46, 55, 64];

    // Loaded and widened ONCE, not once per candidate.
    let sv = _mm256_cvtepu8_epi16(_mm_loadu_si128(alpha.as_ptr() as *const __m128i));

    let mut best = (s0, s1, seed_err);
    for d0 in -2i32..=2 {
        for d1 in -2i32..=2 {
            if d0 == 0 && d1 == 0 {
                continue;
            }
            let c0 = (s0 as i32 + d0).clamp(0, clamp_hi) as u8;
            let c1 = (s1 as i32 + d1).clamp(0, clamp_hi) as u8;
            // Mode 4 stores 6-bit endpoints and dequantizes; mode 5 stores 8-bit.
            let (u0, u1) = if N == 8 {
                ((c0 << 2) | (c0 >> 4), (c1 << 2) | (c1 >> 4))
            } else {
                (c0, c1)
            };

            let mut mn = _mm256_set1_epi16(i16::MAX);
            for k in 0..N {
                let w = if N == 8 { W3[k] } else { W2[k] };
                let pe = (((64 - w) * u0 as u32 + w * u1 as u32 + 32) / 64) as i16;
                let d = _mm256_abs_epi16(_mm256_sub_epi16(_mm256_set1_epi16(pe), sv));
                mn = _mm256_min_epi16(mn, d);
            }

            // Distances are at most 255, so each square fits well inside i32 and
            // the sixteen of them cannot overflow it.
            let sq = _mm256_madd_epi16(mn, mn);
            let h = _mm256_hadd_epi32(sq, sq);
            let h = _mm256_hadd_epi32(h, h);
            let err = _mm_cvtsi128_si32(_mm_add_epi32(
                _mm256_castsi256_si128(h),
                _mm256_extracti128_si256(h, 1),
            ));

            if err < best.2 {
                best = (c0, c1, err);
            }
        }
    }
    best
}


/// SSE of a BC1 block against source pixels, with the index table FIXED.
///
/// The RDO path scores candidate blocks against the source constantly — 17.7
/// calls per block across `bc1_block_sse` and its limited twin, measured at
/// **~22% of BC1 RDO encode**. The scalar form walks sixteen pixels, indexing a
/// four-entry palette per pixel and summing three channels.
///
/// A BC1 palette is four RGBA entries, so the decoder's `pshufb` selector table
/// expands four pixels of it per shuffle — the same trick, reused here to
/// materialise the reconstructed block in registers and difference it against
/// the source four pixels at a time.
///
/// Alpha is masked on both sides: BC1 SSE is RGB-only, and the palette's alpha
/// byte is whatever the caller packed.
#[cfg(target_arch = "x86_64")]
pub(super) fn bc1_fixed_sse_avx2(pixels: &[[u8; 4]; 16], pal: &[u32; 4], table: u32) -> i32 {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above); SSSE3 is
    // implied by it, and every load is a fixed offset inside a fixed-size array.
    unsafe { bc1_fixed_sse_avx2_impl(pixels, pal, table) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn bc1_fixed_sse_avx2_impl(pixels: &[[u8; 4]; 16], pal: &[u32; 4], table: u32) -> i32 {
    use std::arch::x86_64::*;
    let p = _mm_loadu_si128(pal.as_ptr() as *const __m128i);
    let src = pixels.as_ptr() as *const u8;
    // Keep R, G, B; drop A. Four 16-bit lanes per pixel.
    let keep = _mm256_set1_epi64x(
        u64::from_le_bytes([0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0]) as i64,
    );
    let mut acc = _mm256_setzero_si256();
    for g in 0..4usize {
        let sel = _mm_loadu_si128(
            crate::decode::simd::BC1_SEL[((table >> (8 * g)) & 0xff) as usize].as_ptr()
                as *const __m128i,
        );
        // Four reconstructed pixels, and the four source pixels beside them.
        let rec = _mm256_and_si256(_mm256_cvtepu8_epi16(_mm_shuffle_epi8(p, sel)), keep);
        let want = _mm256_and_si256(
            _mm256_cvtepu8_epi16(_mm_loadu_si128(src.add(g * 16) as *const __m128i)),
            keep,
        );
        let d = _mm256_sub_epi16(rec, want);
        acc = _mm256_add_epi32(acc, _mm256_madd_epi16(d, d));
    }
    let h = _mm256_hadd_epi32(acc, acc);
    let h = _mm256_hadd_epi32(h, h);
    _mm_cvtsi128_si32(_mm_add_epi32(
        _mm256_castsi256_si128(h),
        _mm256_extracti128_si256(h, 1),
    ))
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

    /// The vector neighbourhood must pick exactly the candidate the scalar loop
    /// picks, and report exactly its error — same traversal order, same strict
    /// `<`, same clamping — for both palette widths.
    /// The vector block-SSE must equal the scalar walk exactly, across random
    /// palettes, random pixels and every index pattern shape.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn bc1_fixed_sse_matches_scalar() {
        use super::*;
        if !has_avx2() {
            return;
        }
        let mut state = 0xb17e_55e0_4444_9999u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..60_000u32 {
            let mut px = [[0u8; 4]; 16];
            for q in px.iter_mut() {
                let r = next();
                *q = [r as u8, (r >> 8) as u8, (r >> 16) as u8, (r >> 24) as u8];
            }
            let mut rgb = [[0u8; 3]; 4];
            for c in rgb.iter_mut() {
                let r = next();
                *c = [r as u8, (r >> 8) as u8, (r >> 16) as u8];
            }
            let table = match case {
                0 => 0,
                1 => u32::MAX,
                _ => next() as u32,
            };
            let pal: [u32; 4] = core::array::from_fn(|i| {
                u32::from_le_bytes([rgb[i][0], rgb[i][1], rgb[i][2], 0])
            });
            let got = bc1_fixed_sse_avx2(&px, &pal, table);
            let mut want = 0i32;
            for (i, q) in px.iter().enumerate() {
                let c = rgb[((table >> (2 * i)) & 3) as usize];
                for k in 0..3 {
                    let d = c[k] as i32 - q[k] as i32;
                    want += d * d;
                }
            }
            assert_eq!(got, want, "case {case}");
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn alpha_nbhd_avx2_matches_scalar() {
        use super::*;
        if !has_avx2() {
            return;
        }
        const W2: [u32; 4] = [0, 21, 43, 64];
        const W3: [u32; 8] = [0, 9, 18, 27, 37, 46, 55, 64];

        fn scalar<const N: usize>(
            alpha: &[u8; 16],
            s0: u8,
            s1: u8,
            clamp_hi: i32,
            seed_err: i32,
        ) -> (u8, u8, i32) {
            let mut best = (s0, s1, seed_err);
            for d0 in -2i32..=2 {
                for d1 in -2i32..=2 {
                    if d0 == 0 && d1 == 0 {
                        continue;
                    }
                    let c0 = (s0 as i32 + d0).clamp(0, clamp_hi) as u8;
                    let c1 = (s1 as i32 + d1).clamp(0, clamp_hi) as u8;
                    let (u0, u1) = if N == 8 {
                        ((c0 << 2) | (c0 >> 4), (c1 << 2) | (c1 >> 4))
                    } else {
                        (c0, c1)
                    };
                    let mut pal = [0u8; N];
                    for k in 0..N {
                        let w = if N == 8 { W3[k] } else { W2[k] };
                        pal[k] = (((64 - w) * u0 as u32 + w * u1 as u32 + 32) / 64) as u8;
                    }
                    let mut err = 0i32;
                    for &a in alpha.iter() {
                        let mut be = i32::MAX;
                        for &pe in pal.iter() {
                            let d = (pe as i32 - a as i32).pow(2);
                            if d < be {
                                be = d;
                            }
                        }
                        err += be;
                    }
                    if err < best.2 {
                        best = (c0, c1, err);
                    }
                }
            }
            best
        }

        let mut state = 0xb7a1_0ba0_5eed_1234u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..60_000u32 {
            let mut alpha = [0u8; 16];
            match case {
                0 => {}
                1 => alpha = [255; 16],
                // Flat: every candidate ties, so the seed must survive.
                2 => alpha = [77; 16],
                _ => {
                    alpha[..8].copy_from_slice(&next().to_le_bytes());
                    alpha[8..].copy_from_slice(&next().to_le_bytes());
                }
            }
            let r = next();
            // Seeds at and beyond the clamp edges, where the +/-2 offsets saturate.
            let (s0, s1) = ((r >> 3) as u8, (r >> 19) as u8);
            let seed = (next() % 40_000) as i32;
            assert_eq!(
                alpha_nbhd_avx2::<4>(&alpha, s0, s1, 255, seed),
                scalar::<4>(&alpha, s0, s1, 255, seed),
                "N=4 case {case}"
            );
            let (q0, q1) = (s0 & 63, s1 & 63);
            assert_eq!(
                alpha_nbhd_avx2::<8>(&alpha, q0, q1, 63, seed),
                scalar::<8>(&alpha, q0, q1, 63, seed),
                "N=8 case {case}"
            );
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn alpha_avx2_matches_scalar() {
        use super::*;
        if !has_avx2() {
            return;
        }
        fn scalar(palette: &[u8; 8], samples: &[u8; 16]) -> ([u8; 16], i32) {
            let mut idx = [0u8; 16];
            let mut err = 0i32;
            for (i, &s) in samples.iter().enumerate() {
                let mut best = 0u8;
                let mut best_d = i32::MAX;
                for (j, &p) in palette.iter().enumerate() {
                    let d = (p as i32 - s as i32).abs();
                    if d < best_d {
                        best_d = d;
                        best = j as u8;
                    }
                }
                idx[i] = best;
                let diff = palette[best as usize] as i32 - s as i32;
                err += diff * diff;
            }
            (idx, err)
        }
        let mut state = 0xfeed_1234_9876_abcdu64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..200_000u32 {
            let mut pal = [0u8; 8];
            let mut sm = [0u8; 16];
            match case {
                0 => {}
                1 => {
                    pal = [255; 8];
                    sm = [255; 16];
                }
                // Duplicate palette values: the tie-break must keep the lowest index.
                2 => {
                    pal = [7, 7, 7, 7, 200, 200, 200, 200];
                    sm = [7, 200, 100, 0, 255, 7, 200, 3, 9, 199, 201, 6, 8, 128, 64, 32];
                }
                _ => {
                    let a = next().to_le_bytes();
                    pal.copy_from_slice(&a);
                    let b = next().to_le_bytes();
                    let c = next().to_le_bytes();
                    sm[..8].copy_from_slice(&b);
                    sm[8..].copy_from_slice(&c);
                }
            }
            assert_eq!(alpha_fit_avx2(&pal, &sm), scalar(&pal, &sm), "case {case}");
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
