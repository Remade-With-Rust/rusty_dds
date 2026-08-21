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

    // The sixteen pixels are invariant across all sixteen palette entries, but
    // LLVM will not hoist the loads: `pixels` and `pal` are both raw-pointer
    // casts of `&[[u8; 4]; 16]`, so it must assume the palette read may alias
    // the pixel data and reloads them every iteration. The emitted body carried
    // four `vpmovzxbw` — 64 loads and converts a call — for a block constant.
    let q0 = _mm256_cvtepu8_epi16(_mm_loadu_si128(base as *const __m128i));
    let q1 = _mm256_cvtepu8_epi16(_mm_loadu_si128(base.add(16) as *const __m128i));
    let q2 = _mm256_cvtepu8_epi16(_mm_loadu_si128(base.add(32) as *const __m128i));
    let q3 = _mm256_cvtepu8_epi16(_mm_loadu_si128(base.add(48) as *const __m128i));

    // Same story for the palette. `u64::from_le_bytes([e0,0,e1,0,e2,0,e3,0])` is
    // exactly a byte-to-i16 widen, but written as bytes it emitted ten scalar
    // instructions per entry — four `movzbl`, three `orq`, three shifts — 160 a
    // call to rebuild a value `vpmovzxbw` produces in one. Widening all sixteen
    // entries up front turns the loop's copy into a single broadcast-from-memory.
    let pb = pal.as_ptr() as *const u8;
    let mut pal16 = [0i16; 64];
    for h in 0..4usize {
        _mm256_storeu_si256(
            pal16.as_mut_ptr().add(h * 16) as *mut __m256i,
            _mm256_cvtepu8_epi16(_mm_loadu_si128(pb.add(h * 16) as *const __m128i)),
        );
    }

    let mut best_lo = _mm256_set1_epi32(i32::MAX);
    let mut best_hi = _mm256_set1_epi32(i32::MAX);
    let mut idx_lo = _mm256_setzero_si256();
    let mut idx_hi = _mm256_setzero_si256();

    for k in 0..16usize {
        let pv = _mm256_set1_epi64x(*(pal16.as_ptr().add(k * 4) as *const i64));
        let kv = _mm256_set1_epi32(k as i32);

        let da = _mm256_sub_epi16(q0, pv);
        let db = _mm256_sub_epi16(q1, pv);
        let cur_lo = _mm256_permutevar8x32_epi32(
            _mm256_hadd_epi32(_mm256_madd_epi16(da, da), _mm256_madd_epi16(db, db)),
            perm,
        );
        let dc = _mm256_sub_epi16(q2, pv);
        let dd = _mm256_sub_epi16(q3, pv);
        let cur_hi = _mm256_permutevar8x32_epi32(
            _mm256_hadd_epi32(_mm256_madd_epi16(dc, dc), _mm256_madd_epi16(dd, dd)),
            perm,
        );

        let m_lo = _mm256_cmpgt_epi32(best_lo, cur_lo);
        let m_hi = _mm256_cmpgt_epi32(best_hi, cur_hi);
        best_lo = _mm256_blendv_epi8(best_lo, cur_lo, m_lo);
        best_hi = _mm256_blendv_epi8(best_hi, cur_hi, m_hi);
        idx_lo = _mm256_blendv_epi8(idx_lo, kv, m_lo);
        idx_hi = _mm256_blendv_epi8(idx_hi, kv, m_hi);
    }

    // The tail stored 128 bytes and read them back scalar; LLVM rendered that as
    // twelve `vpextrd` lane extractions plus a scalar add chain. Both halves
    // fold in registers instead.
    //
    // Each lane is at most 4*255^2 = 260_100 and there are sixteen, so the total
    // is under 4.2M — inside i32, and the i64 return is just the caller's type.
    let s = _mm256_add_epi32(best_lo, best_hi);
    let h = _mm256_hadd_epi32(s, s);
    let h = _mm256_hadd_epi32(h, h);
    let err = _mm_cvtsi128_si32(_mm_add_epi32(
        _mm256_castsi256_si128(h),
        _mm256_extracti128_si256(h, 1),
    )) as i64;

    // Indices are 0..=15, so neither `packs` saturates.
    let i0 = _mm_packs_epi32(
        _mm256_castsi256_si128(idx_lo),
        _mm256_extracti128_si256(idx_lo, 1),
    );
    let i1 = _mm_packs_epi32(
        _mm256_castsi256_si128(idx_hi),
        _mm256_extracti128_si256(idx_hi, 1),
    );
    let mut best_i = [0u8; 16];
    _mm_storeu_si128(best_i.as_mut_ptr() as *mut __m128i, _mm_packs_epi16(i0, i1));
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

    // The tail used to store 64 bytes of errors and 64 of indices and read them
    // straight back in a scalar sixteen-iteration loop — vector code writing an
    // array that scalar code immediately reloads, the store-forwarding shape
    // this codebase has removed six times. Both halves fold in registers.

    // Total error: eight lane-pairs, then the usual hadd chain. Each lane is at
    // most 3*255^2 = 195_075, so the pairwise sum is under 390_150 and the total
    // under 3.2M — nowhere near i32.
    let s = _mm256_add_epi32(best_lo, best_hi);
    let h = _mm256_hadd_epi32(s, s);
    let h = _mm256_hadd_epi32(h, h);
    let err = _mm_cvtsi128_si32(_mm_add_epi32(
        _mm256_castsi256_si128(h),
        _mm256_extracti128_si256(h, 1),
    ));
    // The old loop bailed as soon as the RUNNING sum reached the limit. Every
    // term is a sum of squares and so non-negative, which makes the running sum
    // monotone — "some prefix reaches the limit" and "the total reaches the
    // limit" are the same statement. One comparison decides it.
    if err >= err_limit {
        return None;
    }

    // Indices: 32-bit lanes in pixel order down to sixteen bytes of 0..=3.
    // `packs` saturates, which cannot bite on values that small.
    let i0 = _mm_packs_epi32(
        _mm256_castsi256_si128(idx_lo),
        _mm256_extracti128_si256(idx_lo, 1),
    );
    let i1 = _mm_packs_epi32(
        _mm256_castsi256_si128(idx_hi),
        _mm256_extracti128_si256(idx_hi, 1),
    );
    let iv = _mm_packs_epi16(i0, i1);
    // Each index is two bits, so two movemasks lift them out as two 16-bit
    // planes, and the 2-bit table is their bit-interleave.
    let m0 = _mm_movemask_epi8(_mm_cmpeq_epi8(
        _mm_and_si128(iv, _mm_set1_epi8(1)),
        _mm_set1_epi8(1),
    )) as u32;
    let m1 = _mm_movemask_epi8(_mm_cmpeq_epi8(
        _mm_and_si128(iv, _mm_set1_epi8(2)),
        _mm_set1_epi8(2),
    )) as u32;
    let table = spread2(m0) | (spread2(m1) << 1);
    Some((table, err))
}

/// Spread the low sixteen bits of `x` into even bit positions — the standard
/// Morton interleave, used here to rebuild a 2-bit-per-pixel index table from
/// two 1-bit planes without needing BMI2.
#[cfg(target_arch = "x86_64")]
#[inline]
fn spread2(x: u32) -> u32 {
    let mut x = x & 0x0000_FFFF;
    x = (x | (x << 8)) & 0x00FF_00FF;
    x = (x | (x << 4)) & 0x0F0F_0F0F;
    x = (x | (x << 2)) & 0x3333_3333;
    (x | (x << 1)) & 0x5555_5555
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


/// Mode-6 SSE for one channel of a block: sixteen pixels, one AVX2 register.
///
/// # Why this is the whole ballgame
///
/// The RDO endpoint polish calls this **259 times per block**, and as a scalar
/// sixteen-iteration loop it measured **305 instructions a call — 79,094 per
/// block, 88.8% of BC7 RDO's entire instruction cost**. It is by a wide margin
/// the most expensive thing in the RDO path.
///
/// # Why sixteen-bit lanes are exact
///
/// `base` is `v0 * 64 + 32` for an unquantized endpoint in `0..=255`, so it
/// spans `32 ..= 16_352`; `w * delta` spans `+/-16_320`. Their sum *is* the
/// original `(64 - w) * v0 + w * v1 + 32`, which cannot leave `32 ..= 16_352`.
/// So `>> 6` lands in `0..=255`, the scalar's `as u8` truncation is the
/// identity, and sixteen `i16` lanes hold the whole block in one register.
///
/// # Why the caller passes planar pixels and pre-looked-up weights
///
/// Both are **invariant across all 259 calls**: the indices are fixed by this
/// function's contract, so `W6M[indices[i]]` is too, and the pixels never
/// change. Hoisting them turns a strided gather and sixteen table lookups per
/// call into one contiguous load.
#[cfg(target_arch = "x86_64")]
pub(super) fn mode6_chan_sse_avx2(px: &[u8; 16], w: &[i16; 16], v0: u8, v1: u8) -> i64 {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above); both arrays are
    // fixed-size and read with unaligned loads.
    unsafe { mode6_chan_sse_avx2_impl(px, w, v0, v1) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn mode6_chan_sse_avx2_impl(px: &[u8; 16], w: &[i16; 16], v0: u8, v1: u8) -> i64 {
    use std::arch::x86_64::*;
    let base = _mm256_set1_epi16(v0 as i16 * 64 + 32);
    let delta = _mm256_set1_epi16(v1 as i16 - v0 as i16);
    let wv = _mm256_loadu_si256(w.as_ptr() as *const __m256i);
    let v = _mm256_srai_epi16(
        _mm256_add_epi16(base, _mm256_mullo_epi16(delta, wv)),
        6,
    );
    let pv = _mm256_cvtepu8_epi16(_mm_loadu_si128(px.as_ptr() as *const __m128i));
    let d = _mm256_sub_epi16(v, pv);
    // Each square is at most 255^2 and `madd` folds pairs, so the eight lanes
    // sum to at most 1_040_400 — comfortably inside i32.
    let sq = _mm256_madd_epi16(d, d);
    let h = _mm256_hadd_epi32(sq, sq);
    let h = _mm256_hadd_epi32(h, h);
    _mm_cvtsi128_si32(_mm_add_epi32(
        _mm256_castsi256_si128(h),
        _mm256_extracti128_si256(h, 1),
    )) as i64
}


/// The pixel-dependent half of BC1's LS refit: sixteen pixels accumulated into
/// two four-lane float sums.
///
/// **59% of BC1 RDO's instruction cost** by the deterministic model — 476
/// instructions a call, 32.19 calls a block — and never vectorised.
///
/// # Why one pixel per iteration, and why no FMA
///
/// Float addition is not associative, so the accumulation order has to survive
/// exactly. One pixel per iteration keeps each lane's order at `i = 0..15`,
/// identical to the scalar loop. And `mul` then `add` is kept separate rather
/// than fused: `fmadd` rounds once where the scalar rounds twice, which would
/// change the result. Both choices cost throughput and buy bit-identity, which
/// is the trade this crate makes everywhere.
///
/// The fourth lane accumulates alpha, which the caller ignores. It is free —
/// the lane exists either way — and keeps the load a single `u32`.
#[cfg(target_arch = "x86_64")]
/// Least-squares accumulator for a BC1 index table: `b0 = sum (1-w)*px`,
/// `b1 = sum w*px`.
///
/// # Two changes over the obvious kernel, both structural
///
/// **The pixels arrive pre-converted.** `refit_with_ls` runs 25.4 times a block
/// and the pixels are identical on every one of them, yet the old kernel
/// converted all sixteen from bytes to floats each time — three instructions a
/// pixel, 48 a call, **1,219 instructions a block** re-deriving a constant. The
/// caller now converts once per block and passes `pxv`.
///
/// **Both accumulator chains live in one register.** `b0` and `b1` are
/// independent 3-lane chains; as two `__m128`s that is half of each register
/// idle and two separate broadcast/mul/add triples per pixel. Packed as
/// `[b0.rgba, b1.rgba]` in one `__m256` they cost one broadcast, one permute,
/// one mul and one add.
///
/// # Why this is still bit-identical
///
/// Each lane performs exactly `acc += weight * x`, one pixel per iteration, in
/// pixel order, with the multiply and the add separate — the same sequence the
/// scalar loop performs for that lane. Widening the register changes which
/// lanes run in parallel, never the order or the rounding within a lane. The
/// byte-to-float conversion is exact for `u8`, so hoisting it out of the loop
/// cannot move a result either.
#[cfg(target_arch = "x86_64")]
pub(super) fn ls_accum_sse(pxv: &[[f32; 8]; 16], uw: &[(f32, f32); 16]) -> ([f32; 4], [f32; 4]) {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above); both arrays
    // are fixed-size and read with unaligned loads.
    unsafe { ls_accum_sse_impl(pxv, uw) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn ls_accum_sse_impl(
    pxv: &[[f32; 8]; 16],
    uw: &[(f32, f32); 16],
) -> ([f32; 4], [f32; 4]) {
    use std::arch::x86_64::*;
    // [u,u,u,u, w,w,w,w] out of the broadcast [u,w,u,w,u,w,u,w].
    let idx = _mm256_setr_epi32(0, 0, 0, 0, 1, 1, 1, 1);
    let mut acc = _mm256_setzero_ps();
    for i in 0..16usize {
        let pair = _mm256_castpd_ps(_mm256_broadcast_sd(&*(uw.as_ptr().add(i) as *const f64)));
        let wv = _mm256_permutevar8x32_ps(pair, idx);
        let px = _mm256_loadu_ps(pxv.as_ptr().add(i) as *const f32);
        acc = _mm256_add_ps(acc, _mm256_mul_ps(wv, px));
    }
    let mut o0 = [0f32; 4];
    let mut o1 = [0f32; 4];
    _mm_storeu_ps(o0.as_mut_ptr(), _mm256_castps256_ps128(acc));
    _mm_storeu_ps(o1.as_mut_ptr(), _mm256_extractf128_ps(acc, 1));
    (o0, o1)
}

/// Convert a block's sixteen pixels to the `[rgba, rgba]` float pairs
/// [`ls_accum_sse`] wants. Block-invariant, so this runs once where the
/// accumulator runs 25.4 times.
#[cfg(target_arch = "x86_64")]
pub(super) fn ls_pixels(pixels: &[[u8; 4]; 16]) -> [[f32; 8]; 16] {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above).
    unsafe { ls_pixels_impl(pixels) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn ls_pixels_impl(pixels: &[[u8; 4]; 16]) -> [[f32; 8]; 16] {
    use std::arch::x86_64::*;
    let mut out = [[0f32; 8]; 16];
    for i in 0..16usize {
        let px = _mm_cvtepi32_ps(_mm_cvtepu8_epi32(_mm_cvtsi32_si128(
            u32::from_le_bytes(pixels[i]) as i32,
        )));
        _mm_storeu_ps(out[i].as_mut_ptr(), px);
        _mm_storeu_ps(out[i].as_mut_ptr().add(4), px);
    }
    out
}



/// Four index bytes per table byte, for expanding BC1's 2-bit indices.
///
/// `SEL1[b]` is the four indices packed in `b`, one per byte — a `pshufb`
/// selector for four pixels of a single channel. 1 KiB, built at compile time.
const fn build_sel1() -> [[u8; 4]; 256] {
    let mut t = [[0u8; 4]; 256];
    let mut b = 0usize;
    while b < 256 {
        let mut k = 0usize;
        while k < 4 {
            t[b][k] = ((b >> (2 * k)) & 3) as u8;
            k += 1;
        }
        b += 1;
    }
    t
}

static SEL1: [[u8; 4]; 256] = build_sel1();

/// SSE of ONE channel of a 4-colour BC1 block, indices fixed.
///
/// The BC1 twin of [`mode6_chan_sse_avx2`], and the same shape: sixteen values
/// against a four-entry palette. 182 instructions as a scalar loop, 5.94 calls a
/// block. The palette is four bytes, so one `pshufb` expands all sixteen pixels
/// from it; the source channel arrives planar because the caller holds the
/// pixels fixed across the whole sweep.
#[cfg(target_arch = "x86_64")]
pub(super) fn bc1_chan_sse_avx2(px: &[u8; 16], cols: [u8; 4], table: u32) -> i32 {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above).
    unsafe { bc1_chan_sse_avx2_impl(px, cols, table) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn bc1_chan_sse_avx2_impl(px: &[u8; 16], cols: [u8; 4], table: u32) -> i32 {
    use std::arch::x86_64::*;
    let b = table.to_le_bytes();
    let sel = _mm_setr_epi32(
        i32::from_le_bytes(SEL1[b[0] as usize]),
        i32::from_le_bytes(SEL1[b[1] as usize]),
        i32::from_le_bytes(SEL1[b[2] as usize]),
        i32::from_le_bytes(SEL1[b[3] as usize]),
    );
    let pal = _mm_cvtsi32_si128(i32::from_le_bytes(cols));
    let rec = _mm256_cvtepu8_epi16(_mm_shuffle_epi8(pal, sel));
    let want = _mm256_cvtepu8_epi16(_mm_loadu_si128(px.as_ptr() as *const __m128i));
    let d = _mm256_sub_epi16(rec, want);
    let sq = _mm256_madd_epi16(d, d);
    let h = _mm256_hadd_epi32(sq, sq);
    let h = _mm256_hadd_epi32(h, h);
    _mm_cvtsi128_si32(_mm_add_epi32(
        _mm256_castsi256_si128(h),
        _mm256_extracti128_si256(h, 1),
    ))
}


/// The BC1 LS solve: six `(A*b0 - B*b1) / det` in four vector ops.
///
/// `refit_with_ls` is BC1 RDO's largest function, and its solve performs **six
/// float divisions** per call — twelve multiplies, six subtracts and six
/// divides across three channels and two endpoints.
///
/// Vectorised it is two `mul`, two `mul`, two `sub` and **two `div`**, and it is
/// bit-identical for free: IEEE 754 defines these lane-wise, so `divps` gives
/// each lane exactly what a scalar `div` would. Rounding and clamping stay
/// scalar, because Rust's `f32::round` is half-away-from-zero and no SSE
/// rounding mode matches it.
#[cfg(target_arch = "x86_64")]
pub(super) fn bc1_ls_solve(
    b0: [f32; 4],
    b1: [f32; 4],
    a00: f32,
    a01: f32,
    a11: f32,
    det: f32,
) -> ([f32; 4], [f32; 4]) {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above).
    unsafe { bc1_ls_solve_impl(b0, b1, a00, a01, a11, det) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn bc1_ls_solve_impl(
    b0: [f32; 4],
    b1: [f32; 4],
    a00: f32,
    a01: f32,
    a11: f32,
    det: f32,
) -> ([f32; 4], [f32; 4]) {
    use std::arch::x86_64::*;
    let v0 = _mm_loadu_ps(b0.as_ptr());
    let v1 = _mm_loadu_ps(b1.as_ptr());
    let dv = _mm_set1_ps(det);
    let e0 = _mm_div_ps(
        _mm_sub_ps(_mm_mul_ps(_mm_set1_ps(a11), v0), _mm_mul_ps(_mm_set1_ps(a01), v1)),
        dv,
    );
    let e1 = _mm_div_ps(
        _mm_sub_ps(_mm_mul_ps(_mm_set1_ps(a00), v1), _mm_mul_ps(_mm_set1_ps(a01), v0)),
        dv,
    );
    let mut o0 = [0f32; 4];
    let mut o1 = [0f32; 4];
    _mm_storeu_ps(o0.as_mut_ptr(), e0);
    _mm_storeu_ps(o1.as_mut_ptr(), e1);
    (o0, o1)
}


/// `(1 - w, w)` per mode-6 index. Every value is `k/64` for `k` in the mode-6
/// weight table, so all sixteen are exact binary fractions and these literals
/// are the f32 the runtime `W[i]` / `1.0 - w` computation produces, bit for bit.
#[cfg(target_arch = "x86_64")]
static UW6: [[f32; 2]; 16] = [
    [1.0, 0.0],
    [0.9375, 0.0625],
    [0.859375, 0.140625],
    [0.796875, 0.203125],
    [0.734375, 0.265625],
    [0.671875, 0.328125],
    [0.59375, 0.40625],
    [0.53125, 0.46875],
    [0.46875, 0.53125],
    [0.40625, 0.59375],
    [0.328125, 0.671875],
    [0.265625, 0.734375],
    [0.203125, 0.796875],
    [0.140625, 0.859375],
    [0.0625, 0.9375],
    [0.0, 1.0]
];

/// `(u*u, u*w, w*w)` per mode-6 index — the three normal-equation terms, which
/// depend only on the INDEX, never on the pixels.
///
/// Every entry is an exact multiple of `1/4096` and bounded by 1, so each
/// product is exact in f32 and so is every partial sum (they are multiples of
/// `1/4096` bounded by 16, well inside a 24-bit mantissa). Reading them from a
/// table is therefore bit-identical to recomputing `u*u` from `1.0 - w` each
/// time, not merely close.
#[cfg(target_arch = "x86_64")]
static AW6: [[f32; 4]; 16] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.87890625, 0.05859375, 0.00390625, 0.0],
    [0.738525390625, 0.120849609375, 0.019775390625, 0.0],
    [0.635009765625, 0.161865234375, 0.041259765625, 0.0],
    [0.539306640625, 0.195068359375, 0.070556640625, 0.0],
    [0.451416015625, 0.220458984375, 0.107666015625, 0.0],
    [0.3525390625, 0.2412109375, 0.1650390625, 0.0],
    [0.2822265625, 0.2490234375, 0.2197265625, 0.0],
    [0.2197265625, 0.2490234375, 0.2822265625, 0.0],
    [0.1650390625, 0.2412109375, 0.3525390625, 0.0],
    [0.107666015625, 0.220458984375, 0.451416015625, 0.0],
    [0.070556640625, 0.195068359375, 0.539306640625, 0.0],
    [0.041259765625, 0.161865234375, 0.635009765625, 0.0],
    [0.019775390625, 0.120849609375, 0.738525390625, 0.0],
    [0.00390625, 0.05859375, 0.87890625, 0.0],
    [0.0, 0.0, 1.0, 0.0]
];

/// Mode-6 least-squares accumulation: the three normal-equation terms and both
/// right-hand sides, for all four channels, in one pass.
///
/// # Why one AVX2 register covers it
///
/// BC7 has four channels, so `b0` and `b1` are eight accumulators — exactly one
/// `__m256`. The scalar loop ran them as two `[f32; 4]` arrays with an inner
/// `for c in 0..4`, which is 8 multiplies and 8 adds a pixel plus the array
/// traffic. Packed as `[b0.rgba, b1.rgba]` it is one broadcast, one permute,
/// one multiply and one add.
///
/// The three `a` terms ride along in an `__m128` fed straight from [`AW6`],
/// replacing a subtract and three multiply-adds a pixel with a load and an add.
///
/// # Bit-identity
///
/// Every lane performs the same `acc += weight * x` in pixel order with the
/// multiply and add separate, so nothing about a lane's arithmetic changes.
/// `pxv` is exact (`u8` -> `f32`), and [`AW6`]'s exactness is argued above.
#[cfg(target_arch = "x86_64")]
pub(super) fn ls_accum_mode6(
    pxv: &[[f32; 8]; 16],
    indices: &[u8; 16],
) -> ([f32; 4], [f32; 4], [f32; 4]) {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above). Indices are
    // 4-bit mode-6 values, so every table read below is in bounds — asserted.
    unsafe { ls_accum_mode6_impl(pxv, indices) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn ls_accum_mode6_impl(
    pxv: &[[f32; 8]; 16],
    indices: &[u8; 16],
) -> ([f32; 4], [f32; 4], [f32; 4]) {
    use std::arch::x86_64::*;
    let sel = _mm256_setr_epi32(0, 0, 0, 0, 1, 1, 1, 1);
    let mut acc = _mm256_setzero_ps();
    let mut aacc = _mm_setzero_ps();
    for i in 0..16usize {
        let k = indices[i] as usize;
        debug_assert!(k < 16);
        let pair = _mm256_castpd_ps(_mm256_broadcast_sd(&*(UW6.as_ptr().add(k) as *const f64)));
        let wv = _mm256_permutevar8x32_ps(pair, sel);
        let px = _mm256_loadu_ps(pxv.as_ptr().add(i) as *const f32);
        acc = _mm256_add_ps(acc, _mm256_mul_ps(wv, px));
        aacc = _mm_add_ps(aacc, _mm_loadu_ps(AW6.as_ptr().add(k) as *const f32));
    }
    let mut a = [0f32; 4];
    let mut o0 = [0f32; 4];
    let mut o1 = [0f32; 4];
    _mm_storeu_ps(a.as_mut_ptr(), aacc);
    _mm_storeu_ps(o0.as_mut_ptr(), _mm256_castps256_ps128(acc));
    _mm_storeu_ps(o1.as_mut_ptr(), _mm256_extractf128_ps(acc, 1));
    (a, o0, o1)
}


/// Two mode-6 candidates for ONE channel in a single call, sharing the pixel
/// and weight loads.
///
/// The endpoint polish always evaluates a `-1` and a `+1` move of the same
/// channel, and both read the identical sixteen pixels and sixteen weights.
/// Split across two calls those loads happen twice and cross a
/// `#[target_feature]` boundary twice; fused they happen once.
///
/// Range analysis, lane exactness and the `madd` bound are exactly as in
/// [`mode6_chan_sse_avx2`] — this is that kernel twice over shared inputs, with
/// no arithmetic changed.
#[cfg(target_arch = "x86_64")]
pub(super) fn mode6_chan_sse_pair_avx2(
    px: &[u8; 16],
    w: &[i16; 16],
    a0: u8,
    b0: u8,
    a1: u8,
    b1: u8,
) -> (i64, i64) {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above); both arrays
    // are fixed-size and read with unaligned loads.
    unsafe { mode6_chan_sse_pair_avx2_impl(px, w, a0, b0, a1, b1) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn mode6_chan_sse_pair_avx2_impl(
    px: &[u8; 16],
    w: &[i16; 16],
    a0: u8,
    b0: u8,
    a1: u8,
    b1: u8,
) -> (i64, i64) {
    use std::arch::x86_64::*;
    let wv = _mm256_loadu_si256(w.as_ptr() as *const __m256i);
    let pv = _mm256_cvtepu8_epi16(_mm_loadu_si128(px.as_ptr() as *const __m128i));
    let one = |a: u8, b: u8| -> i64 {
        let base = _mm256_set1_epi16(a as i16 * 64 + 32);
        let delta = _mm256_set1_epi16(b as i16 - a as i16);
        let v = _mm256_srai_epi16(_mm256_add_epi16(base, _mm256_mullo_epi16(delta, wv)), 6);
        let d = _mm256_sub_epi16(v, pv);
        let sq = _mm256_madd_epi16(d, d);
        let h = _mm256_hadd_epi32(sq, sq);
        let h = _mm256_hadd_epi32(h, h);
        _mm_cvtsi128_si32(_mm_add_epi32(
            _mm256_castsi256_si128(h),
            _mm256_extracti128_si256(h, 1),
        )) as i64
    };
    (one(a0, b0), one(a1, b1))
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
    /// The vector channel-SSE must equal the scalar formula exactly, across the
    /// full endpoint range and every weight the mode-6 table can produce.
    /// The vector LS accumulation must be **bit-identical** to the scalar loop,
    /// which means the same order and no fused multiply-add.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn bc1_chan_sse_matches_scalar() {
        use super::*;
        if !has_avx2() {
            return;
        }
        let mut state = 0xc1a5_5e50_7788_1122u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..60_000u32 {
            let mut px = [0u8; 16];
            for q in px.iter_mut() {
                *q = (next() >> 11) as u8;
            }
            let r = next();
            let cols = [r as u8, (r >> 8) as u8, (r >> 16) as u8, (r >> 24) as u8];
            let table = match case {
                0 => 0,
                1 => u32::MAX,
                _ => next() as u32,
            };
            let got = bc1_chan_sse_avx2(&px, cols, table);
            let mut want = 0i32;
            for (i, &x) in px.iter().enumerate() {
                let d = cols[((table >> (2 * i)) & 3) as usize] as i32 - x as i32;
                want += d * d;
            }
            assert_eq!(got, want, "case {case}");
        }
    }

    /// Bit-identical to the scalar solve, lane by lane.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn bc1_ls_solve_matches_scalar_bitwise() {
        use super::*;
        if !has_avx2() {
            return;
        }
        let mut state = 0x501e_1234_abcd_5678u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut f = move || (next() as u32 as f32) / 1.0e5 - 20_000.0;
        for case in 0..60_000u32 {
            let b0 = [f(), f(), f(), f()];
            let b1 = [f(), f(), f(), f()];
            let (a00, a01, a11) = (f(), f(), f());
            let det = if case == 0 { 1.0 } else { f() };
            if det == 0.0 || !det.is_finite() {
                continue;
            }
            let (g0, g1) = bc1_ls_solve(b0, b1, a00, a01, a11, det);
            for c in 0..4 {
                let w0 = (a11 * b0[c] - a01 * b1[c]) / det;
                let w1 = (a00 * b1[c] - a01 * b0[c]) / det;
                assert_eq!(g0[c].to_bits(), w0.to_bits(), "case {case} e0[{c}]");
                assert_eq!(g1[c].to_bits(), w1.to_bits(), "case {case} e1[{c}]");
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn ls_accum_matches_scalar_bitwise() {
        use super::*;
        if !has_avx2() {
            return;
        }
        let mut state = 0x15ac_c072_9090_3131u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        const W: [f32; 4] = [0.0, 1.0, 1.0 / 3.0, 2.0 / 3.0];
        for case in 0..60_000u32 {
            let mut px = [[0u8; 4]; 16];
            for q in px.iter_mut() {
                let r = next();
                *q = [r as u8, (r >> 8) as u8, (r >> 16) as u8, (r >> 24) as u8];
            }
            let table = if case == 0 { 0 } else { next() as u32 };
            let mut uw = [(0f32, 0f32); 16];
            for (i, slot) in uw.iter_mut().enumerate() {
                let w = W[((table >> (2 * i)) & 3) as usize];
                *slot = (1.0 - w, w);
            }
            // The oracle now covers BOTH halves of the change: `ls_pixels`
            // must reproduce the bytes exactly as floats, and the accumulator
            // must reproduce the scalar loop bitwise from them.
            let pxv = ls_pixels(&px);
            for (i, q) in px.iter().enumerate() {
                for c in 0..4 {
                    assert_eq!(pxv[i][c], q[c] as f32, "ls_pixels lo [{i}][{c}]");
                    assert_eq!(pxv[i][c + 4], q[c] as f32, "ls_pixels hi [{i}][{c}]");
                }
            }
            let (g0, g1) = ls_accum_sse(&pxv, &uw);
            let mut b0 = [0f32; 3];
            let mut b1 = [0f32; 3];
            for (i, p) in px.iter().enumerate() {
                let (u, wgt) = uw[i];
                for c in 0..3 {
                    let x = p[c] as f32;
                    b0[c] += u * x;
                    b1[c] += wgt * x;
                }
            }
            for c in 0..3 {
                assert_eq!(g0[c].to_bits(), b0[c].to_bits(), "case {case} b0[{c}]");
                assert_eq!(g1[c].to_bits(), b1[c].to_bits(), "case {case} b1[{c}]");
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn mode6_chan_sse_matches_scalar() {
        use super::*;
        if !has_avx2() {
            return;
        }
        const W6M: [u32; 16] = [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64];
        let mut state = 0x6d0d_6c1a_5151_2727u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..80_000u32 {
            let mut px = [0u8; 16];
            let mut w = [0i16; 16];
            let (v0, v1) = match case {
                0 => (0u8, 0u8),
                1 => (255, 255),
                // Widest separation in both directions: where an i16 lane would
                // overflow if the range analysis were wrong.
                2 => (0, 255),
                3 => (255, 0),
                _ => (next() as u8, (next() >> 8) as u8),
            };
            for k in 0..16usize {
                px[k] = (next() >> 16) as u8;
                w[k] = W6M[(next() >> 24) as usize % 16] as i16;
            }
            let got = mode6_chan_sse_avx2(&px, &w, v0, v1);
            let base = v0 as i32 * 64 + 32;
            let delta = v1 as i32 - v0 as i32;
            let mut want = 0i64;
            for k in 0..16usize {
                let v = ((base + w[k] as i32 * delta) >> 6) as u8;
                let d = v as i64 - px[k] as i64;
                want += d * d;
            }
            assert_eq!(got, want, "case {case}");
        }
    }

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
