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

    // Two palette entries a pass: the loop body is 22 instructions of which the
    // increment, compare and branch are pure overhead, so amortising them over
    // two entries removes about two instructions per entry.
    // The index vector just counts 0..15 in order, so it is carried and
    // incremented rather than rebuilt from a scalar `k` each entry — that was a
    // `vmovd` plus a `vpbroadcastd` sixteen times a call.
    let one = _mm256_set1_epi32(1);
    let mut kv = _mm256_setzero_si256();
    for kk in 0..4usize {
      for k in [kk * 4, kk * 4 + 1, kk * 4 + 2, kk * 4 + 3] {
        let pv = _mm256_set1_epi64x(*(pal16.as_ptr().add(k * 4) as *const i64));

        // The permute that puts lanes back in pixel order is NOT done here.
        // Everything downstream of it — the compare, both blends, and the
        // running minima — is lane-wise, so a consistent permutation of all
        // lanes commutes with the whole loop. Two permutes a pass become two at
        // the end, and the error sum does not need one at all.
        let da = _mm256_sub_epi16(q0, pv);
        let db = _mm256_sub_epi16(q1, pv);
        let cur_lo = _mm256_hadd_epi32(_mm256_madd_epi16(da, da), _mm256_madd_epi16(db, db));
        let dc = _mm256_sub_epi16(q2, pv);
        let dd = _mm256_sub_epi16(q3, pv);
        let cur_hi = _mm256_hadd_epi32(_mm256_madd_epi16(dc, dc), _mm256_madd_epi16(dd, dd));

        let m_lo = _mm256_cmpgt_epi32(best_lo, cur_lo);
        let m_hi = _mm256_cmpgt_epi32(best_hi, cur_hi);
        best_lo = _mm256_blendv_epi8(best_lo, cur_lo, m_lo);
        best_hi = _mm256_blendv_epi8(best_hi, cur_hi, m_hi);
        idx_lo = _mm256_blendv_epi8(idx_lo, kv, m_lo);
        idx_hi = _mm256_blendv_epi8(idx_hi, kv, m_hi);
        kv = _mm256_add_epi32(kv, one);
      }
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
    // Pixel order is restored ONCE, here, and only for the indices — the error
    // sum above is order-independent so it never needs it.
    let idx_lo = _mm256_permutevar8x32_epi32(idx_lo, perm);
    let idx_hi = _mm256_permutevar8x32_epi32(idx_hi, perm);
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
/// Widen a BC1 palette to the `[i16; 16]` form the index fit broadcasts from.
///
/// `colors` is `[[u8; 3]; 4]` — twelve bytes — so it cannot be loaded as a
/// sixteen-byte vector directly; the entries are restrided to four bytes with a
/// zero alpha, which is also what keeps the broadcast's fourth lane zero so the
/// masked pixel alpha subtracts to nothing.
///
/// Callers holding a palette across many fits should widen ONCE via
/// [`bc1_widen_palette`] and call [`bc1_fit_4color_pre_avx2`] — the RDO window
/// reuses each palette about fourteen times a block, and re-widening it every
/// time is the same pack/unpack round trip the mode-6 path already shed.
/// A BC1 palette prepared for the dot-product fit.
///
/// The fit does not compute `sum (p - q)^2` directly. Expanding it gives
/// `sum p^2 - 2*dot(p,q) + sum q^2`, and the first term is a property of the
/// PIXEL alone — identical for all four palette entries, so it cannot influence
/// which entry is nearest. Dropping it from the inner loop removes every
/// `vpsubw`, leaving a bare inner product, which is precisely what `vpmaddwd`
/// computes natively. The dropped term is added back once at the end, so the
/// error returned is the true SSE and the choice of index is unchanged: this is
/// an exact rewrite, not an approximation.
///
/// Both fields are pre-scaled so the inner loop is a multiply and a subtract:
/// `p8` holds `8*q`, and `cst` holds `4*sum(q^2) + k`, which folds the index
/// tag, the `*4` the tag needs and the constant side of the expansion into one
/// broadcast. A lane then reads `cst[k] - 8*dot`, which is exactly
/// `4*(sum q^2 - 2*dot) + k`.
///
/// Ranges: `q <= 255` so `8*q <= 2040` fits i16, and `8*dot <= 8*3*255^2` is
/// under 1.6M, so nothing approaches i32.
#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy)]
pub(super) struct Bc1Pal {
    p8: [i16; 16],
    cst: [i32; 4],
}

#[cfg(target_arch = "x86_64")]
impl Bc1Pal {
    pub(super) const ZERO: Self = Self {
        p8: [0; 16],
        cst: [0; 4],
    };
}

/// Widen sixteen palette bytes (`[R,G,B,0]` per entry) into the prepared form.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn prep_palette_bytes(v: std::arch::x86_64::__m128i) -> Bc1Pal {
    use std::arch::x86_64::*;
    let w = _mm256_cvtepu8_epi16(v);
    let mut p8 = [0i16; 16];
    _mm256_storeu_si256(p8.as_mut_ptr() as *mut __m256i, _mm256_slli_epi16(w, 3));

    // `sum q^2` per entry: `madd` folds adjacent words, so lane 2k holds
    // R^2+G^2 and lane 2k+1 holds B^2 (the alpha byte is zero by construction),
    // and one `hadd` folds each pair. `hadd` works within 128-bit halves, so
    // entries 0,1 land in the low half's first two lanes and 2,3 in the high
    // half's — `unpacklo_epi64` brings the two pairs together in order.
    //
    // This stays in registers on purpose: the obvious spelling stores the eight
    // `madd` lanes to an array and adds them with a scalar loop, which is vector
    // code writing memory that scalar code immediately reads back — the
    // store-forwarding shape this file has removed six times.
    let hp = _mm256_hadd_epi32(_mm256_madd_epi16(w, w), _mm256_madd_epi16(w, w));
    let q4 = _mm_unpacklo_epi64(
        _mm256_castsi256_si128(hp),
        _mm256_extracti128_si256(hp, 1),
    );
    let mut cst = [0i32; 4];
    _mm_storeu_si128(
        cst.as_mut_ptr() as *mut __m128i,
        _mm_add_epi32(_mm_slli_epi32(q4, 2), _mm_setr_epi32(0, 1, 2, 3)),
    );
    Bc1Pal { p8, cst }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn widen_palette(colors: &[[u8; 3]; 4]) -> Bc1Pal {
    use std::arch::x86_64::*;
    let mut cb = [0u8; 16];
    for k in 0..4usize {
        cb[k * 4] = colors[k][0];
        cb[k * 4 + 1] = colors[k][1];
        cb[k * 4 + 2] = colors[k][2];
    }
    prep_palette_bytes(_mm_loadu_si128(cb.as_ptr() as *const __m128i))
}

/// Widen a palette once, for reuse across fits.
#[cfg(target_arch = "x86_64")]
pub(super) fn bc1_widen_palette(colors: &[[u8; 3]; 4]) -> Bc1Pal {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above).
    unsafe { widen_palette(colors) }
}

/// [`bc1_fit_4color_avx2`] with the palette already widened.
#[cfg(target_arch = "x86_64")]
pub(super) fn bc1_fit_4color_pre_avx2(
    pixels: &[[u8; 4]; 16],
    pal: &Bc1Pal,
    err_limit: i32,
) -> Option<(u32, i32)> {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above).
    unsafe { bc1_fit_4color_pre_avx2_impl(pixels, pal, err_limit) }
}

pub(super) fn bc1_fit_4color_avx2(
    pixels: &[[u8; 4]; 16],
    colors: &[[u8; 3]; 4],
    err_limit: i32,
) -> Option<(u32, i32)> {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above).
    unsafe { bc1_fit_4color_avx2_impl(pixels, colors, err_limit) }
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
    let pal = widen_palette(colors);
    bc1_fit_4color_pre_avx2_impl(pixels, &pal, err_limit)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn bc1_fit_4color_pre_avx2_impl(
    pixels: &[[u8; 4]; 16],
    pal: &Bc1Pal,
    err_limit: i32,
) -> Option<(u32, i32)> {
    use std::arch::x86_64::*;
    let base = pixels.as_ptr() as *const u8;
    let perm = _mm256_setr_epi32(0, 1, 4, 5, 2, 3, 6, 7);
    let keep = _mm256_set1_epi64x(
        u64::from_le_bytes([0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0]) as i64,
    );
    // Sixteen pixels widened to i16 with alpha masked off, four at a time.
    let ld = |off: usize| {
        _mm256_and_si256(
            _mm256_cvtepu8_epi16(_mm_loadu_si128(base.add(off) as *const __m128i)),
            keep,
        )
    };
    let (p0, p1, p2, p3) = (ld(0), ld(16), ld(32), ld(48));

    // The `sum p^2` term dropped from the inner loop, summed back once here.
    // It is the same for every palette entry, which is exactly why it can leave
    // the loop; it is NOT the same across blocks, so it cannot leave the call.
    let psq = _mm256_add_epi32(
        _mm256_add_epi32(_mm256_madd_epi16(p0, p0), _mm256_madd_epi16(p1, p1)),
        _mm256_add_epi32(_mm256_madd_epi16(p2, p2), _mm256_madd_epi16(p3, p3)),
    );

    // Nearest palette entry, with the index riding in the low two bits.
    //
    // Each lane holds `cst[k] - 8*dot`, which is `4*(sum q^2 - 2*dot) + k`, and
    // that differs from `4*SSE + k` only by the constant `4*sum p^2`. A constant
    // shift cannot reorder anything, so `vpminsd` picks the same entry the old
    // `cmpgt`-and-blend pair did — including ties, which both resolve toward the
    // smaller k. Two accumulators and four `vpblendvb` an iteration disappear
    // with it; `blendv` is two uops on Intel where `min` and `sub` are one.
    let mut best_lo = _mm256_set1_epi32(i32::MAX);
    let mut best_hi = _mm256_set1_epi32(i32::MAX);
    for k in 0..4usize {
        let pv = _mm256_set1_epi64x(*(pal.p8.as_ptr().add(k * 4) as *const i64));
        let cv = _mm256_set1_epi32(pal.cst[k]);
        // `hadd` leaves lanes in the permuted order the tail restores; every
        // step from here to the index extraction is lane-wise, so it commutes.
        let lo = _mm256_sub_epi32(
            cv,
            _mm256_hadd_epi32(_mm256_madd_epi16(p0, pv), _mm256_madd_epi16(p1, pv)),
        );
        let hi = _mm256_sub_epi32(
            cv,
            _mm256_hadd_epi32(_mm256_madd_epi16(p2, pv), _mm256_madd_epi16(p3, pv)),
        );
        best_lo = _mm256_min_epi32(best_lo, lo);
        best_hi = _mm256_min_epi32(best_hi, hi);
    }

    // Total error: shift the two tag bits off (arithmetic — the relative term is
    // signed, and `4*v + k` floors back to `v` for negative `v` too), add the
    // per-pixel term back, then the usual hadd chain. The true SSE is at most
    // 16*3*255^2 = 3.1M, nowhere near i32.
    let s = _mm256_add_epi32(
        psq,
        _mm256_add_epi32(_mm256_srai_epi32(best_lo, 2), _mm256_srai_epi32(best_hi, 2)),
    );
    let h = _mm256_hadd_epi32(s, s);
    let h = _mm256_hadd_epi32(h, h);
    let err = _mm_cvtsi128_si32(_mm_add_epi32(
        _mm256_castsi256_si128(h),
        _mm256_extracti128_si256(h, 1),
    ));
    // Every term is a squared distance and so non-negative, which makes the
    // running sum monotone — "some prefix reaches the limit" and "the total
    // reaches the limit" are the same statement. One comparison decides it, and
    // deciding it HERE means a losing candidate never pays for the index pack
    // below, which is most of them on the lattice's contract-only moves.
    if err >= err_limit {
        return None;
    }

    // Pixel order is restored once, here, and only for the indices — the error
    // total above is a sum, so it is order-independent.
    let tag = _mm256_set1_epi32(3);
    let idx_lo = _mm256_permutevar8x32_epi32(_mm256_and_si256(best_lo, tag), perm);
    let idx_hi = _mm256_permutevar8x32_epi32(_mm256_and_si256(best_hi, tag), perm);

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
    // Sixteen 2-bit indices into one `u32`, by weighted accumulation rather than
    // by lifting bit-planes and interleaving them.
    //
    // `maddubs` folds adjacent BYTES with weights 1 and 4, giving eight 4-bit
    // groups (at most 3 + 12 = 15). `madd` then folds adjacent WORDS with
    // weights 1 and 16, giving four 8-bit groups (at most 255). Two saturating
    // packs bring those four bytes together in the right order, and the low
    // dword is the finished table. Every intermediate is inside its lane's
    // range, so neither pack actually saturates.
    let g8 = _mm_maddubs_epi16(
        iv,
        _mm_setr_epi8(1, 4, 1, 4, 1, 4, 1, 4, 1, 4, 1, 4, 1, 4, 1, 4),
    );
    let g16 = _mm_madd_epi16(g8, _mm_setr_epi16(1, 16, 1, 16, 1, 16, 1, 16));
    let packed = _mm_packus_epi16(
        _mm_packus_epi32(g16, _mm_setzero_si128()),
        _mm_setzero_si128(),
    );
    let table = _mm_cvtsi128_si32(packed) as u32;
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

/// Build a BC1 palette from the two packed 565 words, entirely in registers.
///
/// # Why this can be vector code at all
///
/// The 5- and 6-bit expansions are pure bit replication — `EXP5[v]` is
/// `(v << 3) | (v >> 2)` and `EXP6[v]` is `(v << 2) | (v >> 4)` — so there is no
/// table lookup to vectorise around, only shifts.
///
/// # Why `mulhi`-style division by 3 is exact here
///
/// Both dividends are at most `2 * 255 + 255 = 765`. `(x * 21846) >> 16` errs
/// from `x / 3` by `2x / 196_608`, at most `0.0078` over that range, against a
/// largest possible fractional part of `2/3`. Since `0.667 + 0.008 < 1` the
/// floor never moves. In 32-bit lanes `x * 21846 <= 16.7M`, well inside `i32`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn bc1_palette_565_avx2(c0: u16, c1: u16) -> std::arch::x86_64::__m128i {
    use std::arch::x86_64::*;
    let sh = _mm_setr_epi32(11, 5, 0, 0);
    let msk = _mm_setr_epi32(31, 63, 31, 0);
    let up = _mm_setr_epi32(3, 2, 3, 0);
    let dn = _mm_setr_epi32(2, 4, 2, 0);
    let expand = |c: u16| {
        let t = _mm_and_si128(_mm_srlv_epi32(_mm_set1_epi32(c as i32), sh), msk);
        _mm_or_si128(_mm_sllv_epi32(t, up), _mm_srlv_epi32(t, dn))
    };
    let e = expand(c0);
    let f = expand(c1);
    let sum = _mm_add_epi32(e, f);
    let third = _mm_set1_epi32(21846);
    let (p2, p3) = if c0 > c1 {
        (
            _mm_srli_epi32(_mm_mullo_epi32(_mm_add_epi32(sum, e), third), 16),
            _mm_srli_epi32(_mm_mullo_epi32(_mm_add_epi32(sum, f), third), 16),
        )
    } else {
        // 3-colour + punch-through: the third entry is the midpoint (integer
        // division, so a plain shift) and the fourth is black.
        (_mm_srli_epi32(sum, 1), _mm_setzero_si128())
    };
    // Each of the four is [R, G, B, 0] in 32-bit lanes, all values in 0..=255,
    // so two narrowing packs put them in palette order directly: `packus_epi32`
    // pairs them into `[R,G,B,0, R,G,B,0]` words and `packus_epi16` brings the
    // two halves down to the sixteen bytes wanted. Three instructions where
    // four `pshufb` and three `or` were used, and nothing saturates because
    // every value already fits a byte.
    _mm_packus_epi16(
        _mm_packus_epi32(e, f),
        _mm_packus_epi32(p2, p3),
    )
}

/// The BC1 palette for two 565 words, in the `[i16; 16]` form the index fit
/// broadcasts from.
///
/// The driver used to build the byte palette scalar (77 instructions) and then
/// widen it. Both come straight from the 565 words here — it is
/// [`bc1_palette_565_avx2`] without the final narrowing to bytes, since the
/// intermediate `packus_epi32` result already IS the i16 layout wanted.
#[cfg(target_arch = "x86_64")]
pub(super) fn bc1_palette_565_i16_avx2(c0: u16, c1: u16) -> Bc1Pal {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above).
    unsafe { bc1_palette_565_i16_avx2_impl(c0, c1) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn bc1_palette_565_i16_avx2_impl(c0: u16, c1: u16) -> Bc1Pal {
    prep_palette_bytes(bc1_palette_565_avx2(c0, c1))
}

/// [`bc1_fixed_sse_avx2`] that builds its own palette from the 565 words.
///
/// The caller used to run `bc1_colors_packed` — 91 scalar instructions — and
/// hand the result in. Building it here costs about 25 vector instructions and
/// crosses no new `#[target_feature]` boundary, because this kernel already is
/// one.
#[cfg(target_arch = "x86_64")]
pub(super) fn bc1_fixed_sse_565_avx2(
    pixels: &[[u8; 4]; 16],
    c0: u16,
    c1: u16,
    table: u32,
) -> i32 {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above).
    unsafe { bc1_fixed_sse_565_avx2_impl(pixels, c0, c1, table) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn bc1_fixed_sse_565_avx2_impl(
    pixels: &[[u8; 4]; 16],
    c0: u16,
    c1: u16,
    table: u32,
) -> i32 {
    bc1_sse_from_pal(pixels, bc1_palette_565_avx2(c0, c1), table)
}



/// The fixed-table scoring loop, shared by both entry points.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn bc1_sse_from_pal(
    pixels: &[[u8; 4]; 16],
    p: std::arch::x86_64::__m128i,
    table: u32,
) -> i32 {
    use std::arch::x86_64::*;
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
        // `rec` needs no alpha mask: the palette's fourth byte is already zero
        // by construction, so the AND that used to be here was a no-op.
        let rec = _mm256_cvtepu8_epi16(_mm_shuffle_epi8(p, sel));
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
#[cfg(test)]
pub(super) fn ls_accum_sse(pxv: &[[f32; 8]; 16], uw: &[[f32; 8]; 16]) -> ([f32; 4], [f32; 4]) {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above); both arrays
    // are fixed-size and read with unaligned loads.
    unsafe { ls_accum_sse_impl(pxv, uw) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[cfg(test)]
unsafe fn ls_accum_sse_impl(
    pxv: &[[f32; 8]; 16],
    uw: &[[f32; 8]; 16],
) -> ([f32; 4], [f32; 4]) {
    use std::arch::x86_64::*;
    let mut acc = _mm256_setzero_ps();
    for i in 0..16usize {
        // `uw` arrives pre-spread to [u,u,u,u,w,w,w,w]; this was a
        // `vbroadcastsd` plus a `vpermps` per pixel. `i` is the unrolled loop
        // counter, so the offset is a compile-time constant.
        let wv = _mm256_loadu_ps(uw.as_ptr().add(i) as *const f32);
        let px = _mm256_loadu_ps(pxv.as_ptr().add(i) as *const f32);
        acc = _mm256_add_ps(acc, _mm256_mul_ps(wv, px));
    }
    let mut o0 = [0f32; 4];
    let mut o1 = [0f32; 4];
    _mm_storeu_ps(o0.as_mut_ptr(), _mm256_castps256_ps128(acc));
    _mm_storeu_ps(o1.as_mut_ptr(), _mm256_extractf128_ps(acc, 1));
    (o0, o1)
}

/// Convert a block's sixteen pixels to `[r,r,g,g,b,b,a,a]` float octets — the
/// layout [`ls_accum_mode6`] wants.
///
/// Interleaving the channels here is what lets that kernel multiply by the raw
/// `[u,w,u,w,...]` broadcast, with no per-pixel permute: lane `2c` accumulates
/// `b0[c]` and lane `2c+1` accumulates `b1[c]`. One permute at the end
/// deinterleaves the result.
#[cfg(target_arch = "x86_64")]
pub(super) fn ls_pixels_mode6(pixels: &[[u8; 4]; 16]) -> [[f32; 8]; 16] {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above).
    unsafe { ls_pixels_mode6_impl(pixels) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn ls_pixels_mode6_impl(pixels: &[[u8; 4]; 16]) -> [[f32; 8]; 16] {
    use std::arch::x86_64::*;
    // TWO pixels per pass: eight bytes widen to eight i32 in one
    // `vpmovzxbd`, so one load and one convert serve both, and each pixel then
    // costs a permute and a store. One pixel at a time left this rolled at
    // seven instructions a pass -- 132 for the block.
    let dup0 = _mm256_setr_epi32(0, 0, 1, 1, 2, 2, 3, 3);
    let dup1 = _mm256_setr_epi32(4, 4, 5, 5, 6, 6, 7, 7);
    let src = pixels.as_ptr() as *const u8;
    let mut out = [[0f32; 8]; 16];
    for i in (0..16usize).step_by(2) {
        let two = _mm256_cvtepi32_ps(_mm256_cvtepu8_epi32(_mm_loadl_epi64(
            src.add(i * 4) as *const __m128i,
        )));
        _mm256_storeu_ps(out[i].as_mut_ptr(), _mm256_permutevar8x32_ps(two, dup0));
        _mm256_storeu_ps(out[i + 1].as_mut_ptr(), _mm256_permutevar8x32_ps(two, dup1));
    }
    out
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
    // Two pixels per pass, as in `ls_pixels_mode6` — one load and one convert
    // serve both, then a permute and a store each.
    let dup0 = _mm256_setr_epi32(0, 1, 2, 3, 0, 1, 2, 3);
    let dup1 = _mm256_setr_epi32(4, 5, 6, 7, 4, 5, 6, 7);
    let src = pixels.as_ptr() as *const u8;
    let mut out = [[0f32; 8]; 16];
    for i in (0..16usize).step_by(2) {
        let two = _mm256_cvtepi32_ps(_mm256_cvtepu8_epi32(_mm_loadl_epi64(
            src.add(i * 4) as *const __m128i,
        )));
        _mm256_storeu_ps(out[i].as_mut_ptr(), _mm256_permutevar8x32_ps(two, dup0));
        _mm256_storeu_ps(out[i + 1].as_mut_ptr(), _mm256_permutevar8x32_ps(two, dup1));
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
) -> ([u8; 4], [u8; 4]) {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above).
    unsafe { bc1_ls_solve_impl(b0, b1, a00, a01, a11, det) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
/// The divide itself, shared by both entry points.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn solve_pair(
    b0: [f32; 4],
    b1: [f32; 4],
    a00: f32,
    a01: f32,
    a11: f32,
    det: f32,
) -> (std::arch::x86_64::__m128, std::arch::x86_64::__m128) {
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
    (e0, e1)
}

unsafe fn bc1_ls_solve_impl(
    b0: [f32; 4],
    b1: [f32; 4],
    a00: f32,
    a01: f32,
    a11: f32,
    det: f32,
) -> ([u8; 4], [u8; 4]) {
    let (e0, e1) = solve_pair(b0, b1, a00, a01, a11, det);
    (round_pack(e0), round_pack(e1))
}


/// `round_clamp_u8` for four lanes at once: clamp to `[0, 255]` in f32, widen to
/// f64, add a half, truncate.
///
/// This is the same argument `round_clamp_u8` documents, lane-wise. The widen to
/// f64 is the load-bearing step — in f32 the `+ 0.5` can tie and round up
/// (`0.49999997f32 + 0.5 == 1.0`), which is exactly why no SSE rounding mode can
/// be used here. `cvttpd` truncates toward zero, which is `floor` for the
/// non-negative values a clamp to `[0, 255]` guarantees.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn round_pack(v: std::arch::x86_64::__m128) -> [u8; 4] {
    use std::arch::x86_64::*;
    let i = round_lanes(v);
    let mut out = [0i32; 4];
    _mm_storeu_si128(out.as_mut_ptr() as *mut __m128i, i);
    [out[0] as u8, out[1] as u8, out[2] as u8, out[3] as u8]
}

/// The rounding itself, left in 32-bit lanes so callers can pack it either way.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn round_lanes(v: std::arch::x86_64::__m128) -> std::arch::x86_64::__m128i {
    use std::arch::x86_64::*;
    let c = _mm_min_ps(_mm_max_ps(v, _mm_setzero_ps()), _mm_set1_ps(255.0));
    let d = _mm256_add_pd(_mm256_cvtps_pd(c), _mm256_set1_pd(0.5));
    _mm256_cvttpd_epi32(d)
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
/// # Two refuted layout changes, recorded so they are not retried
///
/// Both were motivated by the same theory — that the row stride forces extra
/// address arithmetic, since x86 SIB scales are limited to 1, 2, 4 and 8 — and
/// both measured WORSE:
///
/// - Pre-spreading [`UW6`] to `[u,u,u,u,w,w,w,w]` rows to replace the per-pixel
///   `vbroadcastsd` + `vpermps` with one load: **160 -> 177**. Scale 32 is not
///   legal, and the address arithmetic cost more than the two ops it removed.
/// - Dropping this table to `(u*w, w*w)` at stride 8 — a legal scale — and
///   recovering `a00` as `16 - 2*a01 - a11` (exact, since `u + w == 1` makes
///   `sum (u+w)^2 == 16` and every term an exact multiple of `1/4096`):
///   **160 -> 183**.
///
/// The theory was simply wrong: LLVM was not paying the extra shift-and-add the
/// stride seemed to imply, so there was nothing to win.
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
    let mut acc = _mm256_setzero_ps();
    let mut aacc = _mm_setzero_ps();
    for i in 0..16usize {
        let k = indices[i] as usize;
        debug_assert!(k < 16);
        // The raw `[u,w,u,w,u,w,u,w]` broadcast is used as-is — the per-pixel
        // `vpermps` that used to reshape it into `[u,u,u,u,w,w,w,w]` is gone,
        // because `pxv` arrives channel-interleaved instead (see
        // `ls_pixels_mode6`). Lane `2c` accumulates `b0[c]`, lane `2c+1`
        // accumulates `b1[c]`; each lane still performs the same
        // `acc += weight * x` in pixel order, so nothing about the arithmetic
        // moves.
        let wv = _mm256_castpd_ps(_mm256_broadcast_sd(&*(UW6.as_ptr().add(k) as *const f64)));
        let px = _mm256_loadu_ps(pxv.as_ptr().add(i) as *const f32);
        acc = _mm256_add_ps(acc, _mm256_mul_ps(wv, px));
        aacc = _mm_add_ps(aacc, _mm_loadu_ps(AW6.as_ptr().add(k) as *const f32));
    }
    // Deinterleave once: [b0.r,b1.r,b0.g,b1.g,...] -> [b0.rgba, b1.rgba].
    let acc = _mm256_permutevar8x32_ps(acc, _mm256_setr_epi32(0, 2, 4, 6, 1, 3, 5, 7));
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
    a: u8,
    b: u8,
    dbase: i16,
    ddelta: i16,
) -> (i64, i64) {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above); both arrays
    // are fixed-size and read with unaligned loads.
    unsafe { mode6_chan_sse_pair_avx2_impl(px, w, a, b, dbase, ddelta) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn mode6_chan_sse_pair_avx2_impl(
    px: &[u8; 16],
    w: &[i16; 16],
    a: u8,
    b: u8,
    dbase: i16,
    ddelta: i16,
) -> (i64, i64) {
    use std::arch::x86_64::*;
    let wv = _mm256_loadu_si256(w.as_ptr() as *const __m256i);
    let pv = _mm256_cvtepu8_epi16(_mm_loadu_si128(px.as_ptr() as *const __m128i));
    // The second candidate is derived from the first with two adds rather than
    // rebuilt from scratch. The polish only ever moves ONE quantized endpoint by
    // +/-1, and `unquantize` is `(q << 1) | p`, so the two candidates'
    // unquantized values differ by exactly 4 — which makes `base` differ by
    // `4 * 64 = 256` and `delta` by 4, both known constants at the call site.
    // Building the second pair the long way cost two scalar operations and two
    // broadcasts.
    let base0 = _mm256_set1_epi16(a as i16 * 64 + 32);
    let delta0 = _mm256_set1_epi16(b as i16 - a as i16);
    let base1 = _mm256_add_epi16(base0, _mm256_set1_epi16(dbase));
    let delta1 = _mm256_add_epi16(delta0, _mm256_set1_epi16(ddelta));
    let sq = |base: __m256i, delta: __m256i| {
        let v = _mm256_srai_epi16(_mm256_add_epi16(base, _mm256_mullo_epi16(delta, wv)), 6);
        let d = _mm256_sub_epi16(v, pv);
        _mm256_madd_epi16(d, d)
    };
    // ONE reduction for both candidates. `hadd(x, y)` folds two registers at
    // once, so the second `hadd` leaves lane 0 holding candidate 0's per-half
    // sum and lane 1 candidate 1's; adding the two 128-bit halves finishes both.
    // Two separate six-instruction chains become one of seven.
    let h = _mm256_hadd_epi32(
        _mm256_hadd_epi32(sq(base0, delta0), sq(base1, delta1)),
        _mm256_setzero_si256(),
    );
    let t = _mm_add_epi32(
        _mm256_castsi256_si128(h),
        _mm256_extracti128_si256(h, 1),
    );
    (_mm_cvtsi128_si32(t) as i64, _mm_extract_epi32(t, 1) as i64)
}


/// The mode-6 weight table, each entry repeated four times (once per channel),
/// in groups of four palette entries — the shape [`palette_mode6_avx2`] wants.
#[cfg(target_arch = "x86_64")]
static W6M_REP: [[i16; 16]; 4] = [
    [0, 0, 0, 0, 4, 4, 4, 4, 9, 9, 9, 9, 13, 13, 13, 13],
    [17, 17, 17, 17, 21, 21, 21, 21, 26, 26, 26, 26, 30, 30, 30, 30],
    [34, 34, 34, 34, 38, 38, 38, 38, 43, 43, 43, 43, 47, 47, 47, 47],
    [51, 51, 51, 51, 55, 55, 55, 55, 60, 60, 60, 60, 64, 64, 64, 64]
];

/// Build a full mode-6 palette: sixteen entries, four channels.
///
/// # Why this was worth vectorising
///
/// The scalar form is a sixteen-iteration loop with a four-channel inner body —
/// **24 instructions a pass, about 423 a call**, and the RDO donor loop calls it
/// 16.4 times a block. That is the largest single item in BC7 RDO.
///
/// # Layout
///
/// One `__m256i` holds sixteen `i16` = **four palette entries by four
/// channels**, which is exactly the output's byte order, so no transpose is
/// needed — four groups cover all sixteen entries.
///
/// # Why i16 is exact, and why `packus` never clamps
///
/// `base[c] = c0[c] * 64 + 32` spans `32 ..= 16_352` and `w * delta` spans
/// `+/-16_320`, and their sum is the original `(64 - w) * v0 + w * v1 + 32`,
/// which cannot leave `32 ..= 16_352`. So the sum fits `i16` with room, the
/// product `delta * w` fits too (`+/-16_320`), and `mullo`'s low half is the
/// exact product. After `>> 6` every value is in `0 ..= 255`, so the saturating
/// pack never actually saturates and matches the scalar's `as u8` exactly. The
/// sum being non-negative also makes the arithmetic shift agree with the
/// scalar's `i32` shift.
#[cfg(target_arch = "x86_64")]
pub(super) fn palette_mode6_avx2(base: [i32; 4], c0: [u8; 4], c1: [u8; 4]) -> [[u8; 4]; 16] {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above); all arrays are
    // fixed-size and accessed with unaligned loads and stores.
    unsafe { palette_mode6_avx2_impl(base, c0, c1) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn palette_mode6_avx2_impl(base: [i32; 4], c0: [u8; 4], c1: [u8; 4]) -> [[u8; 4]; 16] {
    use std::arch::x86_64::*;
    // Both of these used to be assembled byte by byte through
    // `u64::from_le_bytes`, which is about thirty scalar instructions to rebuild
    // values the vector unit produces in nine. Same pattern removed twice in
    // round 6; it had crept back into this kernel.
    //
    // `base` spans 32..=16_352, so packing i32 -> i16 cannot saturate.
    let b = _mm256_broadcastq_epi64(_mm_packs_epi32(
        _mm_loadu_si128(base.as_ptr() as *const __m128i),
        _mm_setzero_si128(),
    ));
    let d = _mm256_broadcastq_epi64(_mm_sub_epi16(
        _mm_cvtepu8_epi16(_mm_cvtsi32_si128(u32::from_le_bytes(c1) as i32)),
        _mm_cvtepu8_epi16(_mm_cvtsi32_si128(u32::from_le_bytes(c0) as i32)),
    ));
    let zero = _mm256_setzero_si256();
    let mut out = [[0u8; 4]; 16];
    let op = out.as_mut_ptr() as *mut u8;
    for g in 0..4usize {
        let wv = _mm256_loadu_si256(W6M_REP.as_ptr().add(g) as *const __m256i);
        let v = _mm256_srai_epi16(_mm256_add_epi16(b, _mm256_mullo_epi16(d, wv)), 6);
        // `packus` works per 128-bit lane, so the useful bytes land in the low
        // half of each lane; one `permute4x64` gathers them into sixteen.
        let p = _mm256_permute4x64_epi64(_mm256_packus_epi16(v, zero), 0b00_00_10_00);
        _mm_storeu_si128(op.add(g * 16) as *mut __m128i, _mm256_castsi256_si128(p));
    }
    out
}


/// All four channel errors for one endpoint pair, in a single call.
///
/// Unlike the four-candidate polish fusion — which was refuted because it had to
/// score candidates the range guards would discard — every one of these four is
/// always needed, so there is no speculation and nothing is wasted. The weight
/// vector is loaded once instead of four times, and three call boundaries
/// disappear.
#[cfg(target_arch = "x86_64")]
pub(super) fn mode6_chan_errs_avx2(
    planar: &[[u8; 16]; 4],
    w: &[i16; 16],
    v: &[(u8, u8); 4],
) -> [i64; 4] {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above); all arrays are
    // fixed-size and read with unaligned loads.
    unsafe { mode6_chan_errs_avx2_impl(planar, w, v) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn mode6_chan_errs_avx2_impl(
    planar: &[[u8; 16]; 4],
    w: &[i16; 16],
    v: &[(u8, u8); 4],
) -> [i64; 4] {
    use std::arch::x86_64::*;
    let wv = _mm256_loadu_si256(w.as_ptr() as *const __m256i);
    let sq = |c: usize| {
        let (a, b) = v[c];
        let pv = _mm256_cvtepu8_epi16(_mm_loadu_si128(planar[c].as_ptr() as *const __m128i));
        let base = _mm256_set1_epi16(a as i16 * 64 + 32);
        let delta = _mm256_set1_epi16(b as i16 - a as i16);
        let val = _mm256_srai_epi16(_mm256_add_epi16(base, _mm256_mullo_epi16(delta, wv)), 6);
        let d = _mm256_sub_epi16(val, pv);
        _mm256_madd_epi16(d, d)
    };
    // ONE reduction for all four channels. `hadd` folds two registers at a time,
    // so two of them collapse the four squared-difference vectors into a single
    // register holding one lane per channel, per 128-bit half; adding the halves
    // finishes all four. Four six-instruction chains become one of six.
    let h = _mm256_hadd_epi32(
        _mm256_hadd_epi32(sq(0), sq(1)),
        _mm256_hadd_epi32(sq(2), sq(3)),
    );
    let t = _mm_add_epi32(
        _mm256_castsi256_si128(h),
        _mm256_extracti128_si256(h, 1),
    );
    let mut o = [0i32; 4];
    _mm_storeu_si128(o.as_mut_ptr() as *mut __m128i, t);
    [o[0] as i64, o[1] as i64, o[2] as i64, o[3] as i64]
}


/// Build a mode-6 palette AND fit the block's indices to it, in one call.
///
/// # The round trip this removes
///
/// The palette builder produced sixteen 4-byte entries — four `packus` and four
/// `permute4x64` to narrow i16 lanes down to bytes — and stored them; the index
/// fit then loaded those bytes straight back and widened them to i16 again, four
/// `vpmovzxbw` and four stores. Sixty-four bytes were packed and unpacked for
/// nothing, across a `#[target_feature]` boundary. Fused, the palette simply
/// stays in its i16 form, which is what the fit wanted all along.
///
/// Correctness of the i16 range and of the fit itself are unchanged — see
/// [`palette_mode6_avx2`] and [`fit_indices_mode6_avx2`], whose bodies this is.
///
/// # Do not factor this into helpers
///
/// A two-palette variant was tried — the donor loop needs both p-bits and they
/// share the block, the base and the first endpoint — by splitting the fit into
/// a reusable `fit_one`. It measured WORSE: **750 against 720**.
///
/// The reason is a hard language limit. `#[inline(always)]` is rejected on a
/// `#[target_feature]` function, and a plain `#[inline]` hint is ignored, so
/// `fit_one` stayed out of line and the "fused" entry point paid two internal
/// call boundaries to save one outer one. The split also regressed the
/// single-palette path from 350 to 375.
///
/// **A `#[target_feature]` kernel cannot be refactored into helpers without
/// measuring** — every helper is a real call, and there is no way to force
/// otherwise on stable. Share code between kernels with a macro, or not at all.
#[cfg(target_arch = "x86_64")]
pub(super) fn palette_fit_mode6_avx2(
    pixels: &[[u8; 4]; 16],
    base: [i32; 4],
    c0: [u8; 4],
    c1: [u8; 4],
) -> ([u8; 16], i64) {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above); all arrays are
    // fixed-size and accessed with unaligned loads and stores.
    unsafe { palette_fit_mode6_avx2_impl(pixels, base, c0, c1) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn palette_fit_mode6_avx2_impl(
    pixels: &[[u8; 4]; 16],
    base: [i32; 4],
    c0: [u8; 4],
    c1: [u8; 4],
) -> ([u8; 16], i64) {
    use std::arch::x86_64::*;

    // --- palette, left in i16 ---
    let b = _mm256_broadcastq_epi64(_mm_packs_epi32(
        _mm_loadu_si128(base.as_ptr() as *const __m128i),
        _mm_setzero_si128(),
    ));
    let d = _mm256_broadcastq_epi64(_mm_sub_epi16(
        _mm_cvtepu8_epi16(_mm_cvtsi32_si128(u32::from_le_bytes(c1) as i32)),
        _mm_cvtepu8_epi16(_mm_cvtsi32_si128(u32::from_le_bytes(c0) as i32)),
    ));
    // Entry `4g+j`, channel `c` lands at lane `4j+c` of group `g` — exactly the
    // order the fit indexes, so no shuffling is needed between the two halves.
    let mut pal16 = [0i16; 64];
    for g in 0..4usize {
        let wv = _mm256_loadu_si256(W6M_REP.as_ptr().add(g) as *const __m256i);
        let v = _mm256_srai_epi16(_mm256_add_epi16(b, _mm256_mullo_epi16(d, wv)), 6);
        _mm256_storeu_si256(pal16.as_mut_ptr().add(g * 16) as *mut __m256i, v);
    }

    // --- index fit ---
    let src = pixels.as_ptr() as *const u8;
    let perm = _mm256_setr_epi32(0, 1, 4, 5, 2, 3, 6, 7);
    let q0 = _mm256_cvtepu8_epi16(_mm_loadu_si128(src as *const __m128i));
    let q1 = _mm256_cvtepu8_epi16(_mm_loadu_si128(src.add(16) as *const __m128i));
    let q2 = _mm256_cvtepu8_epi16(_mm_loadu_si128(src.add(32) as *const __m128i));
    let q3 = _mm256_cvtepu8_epi16(_mm_loadu_si128(src.add(48) as *const __m128i));

    let mut best_lo = _mm256_set1_epi32(i32::MAX);
    let mut best_hi = _mm256_set1_epi32(i32::MAX);
    let mut idx_lo = _mm256_setzero_si256();
    let mut idx_hi = _mm256_setzero_si256();
    let one = _mm256_set1_epi32(1);
    let mut kv = _mm256_setzero_si256();

    for kk in 0..4usize {
      for k in [kk * 4, kk * 4 + 1, kk * 4 + 2, kk * 4 + 3] {
        let pv = _mm256_set1_epi64x(*(pal16.as_ptr().add(k * 4) as *const i64));
        let da = _mm256_sub_epi16(q0, pv);
        let db = _mm256_sub_epi16(q1, pv);
        let cur_lo = _mm256_hadd_epi32(_mm256_madd_epi16(da, da), _mm256_madd_epi16(db, db));
        let dc = _mm256_sub_epi16(q2, pv);
        let dd = _mm256_sub_epi16(q3, pv);
        let cur_hi = _mm256_hadd_epi32(_mm256_madd_epi16(dc, dc), _mm256_madd_epi16(dd, dd));
        let m_lo = _mm256_cmpgt_epi32(best_lo, cur_lo);
        let m_hi = _mm256_cmpgt_epi32(best_hi, cur_hi);
        best_lo = _mm256_blendv_epi8(best_lo, cur_lo, m_lo);
        best_hi = _mm256_blendv_epi8(best_hi, cur_hi, m_hi);
        idx_lo = _mm256_blendv_epi8(idx_lo, kv, m_lo);
        idx_hi = _mm256_blendv_epi8(idx_hi, kv, m_hi);
        kv = _mm256_add_epi32(kv, one);
      }
    }

    let sum = _mm256_add_epi32(best_lo, best_hi);
    let h = _mm256_hadd_epi32(sum, sum);
    let h = _mm256_hadd_epi32(h, h);
    let err = _mm_cvtsi128_si32(_mm_add_epi32(
        _mm256_castsi256_si128(h),
        _mm256_extracti128_si256(h, 1),
    )) as i64;

    let idx_lo = _mm256_permutevar8x32_epi32(idx_lo, perm);
    let idx_hi = _mm256_permutevar8x32_epi32(idx_hi, perm);
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


/// Accumulate the BC1 normal equations AND solve them, in one call.
///
/// `refit_with_ls` ran these back to back — the accumulator's two `[f32; 4]`
/// results were stored, returned across a `#[target_feature]` boundary, and
/// immediately loaded back as the solve's arguments. Fused, they stay in
/// registers and one boundary disappears.
///
/// Written as a single body rather than two helpers on purpose: a
/// `#[target_feature]` function cannot be force-inlined on stable, so factoring
/// would reintroduce exactly the call this removes.
///
/// # Do not also fold the SCORING in
///
/// Fusing one step further — refit, then build the palette and score against
/// the block, so `bc1_block_sse_limited` disappears — was tried and REFUTED.
/// The composed kernel needs nine parameters (`pxv`, `uw`, four normal-equation
/// terms, the pixels, the table and the limit), and the argument marshalling
/// plus the `Option<(u16, u16, i32)>` return cost more than the one boundary it
/// removes: the glue alone measured **~102 instructions** against the 83 of the
/// two separate wrappers it replaced.
///
/// **Fusion stops paying when the fused signature gets wide.** The wins in this
/// campaign all shared few arguments; this one did not.
#[cfg(target_arch = "x86_64")]
pub(super) fn ls_accum_solve_565(
    pxv: &[[f32; 8]; 16],
    uw: &[[f32; 8]; 16],
    a00: f32,
    a01: f32,
    a11: f32,
    det: f32,
) -> (u16, u16) {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above); both arrays
    // are fixed-size and read with unaligned loads.
    unsafe { ls_accum_solve_565_impl(pxv, uw, a00, a01, a11, det) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn ls_accum_solve_565_impl(
    pxv: &[[f32; 8]; 16],
    uw: &[[f32; 8]; 16],
    a00: f32,
    a01: f32,
    a11: f32,
    det: f32,
) -> (u16, u16) {
    use std::arch::x86_64::*;
    // --- accumulate (see `ls_accum_sse` for why this is bit-identical) ---
    let mut acc = _mm256_setzero_ps();
    for i in 0..16usize {
        let wv = _mm256_loadu_ps(uw.as_ptr().add(i) as *const f32);
        let px = _mm256_loadu_ps(pxv.as_ptr().add(i) as *const f32);
        acc = _mm256_add_ps(acc, _mm256_mul_ps(wv, px));
    }
    let v0 = _mm256_castps256_ps128(acc);
    let v1 = _mm256_extractf128_ps(acc, 1);

    // --- solve (see `bc1_ls_solve`) ---
    let dv = _mm_set1_ps(det);
    let e0 = _mm_div_ps(
        _mm_sub_ps(_mm_mul_ps(_mm_set1_ps(a11), v0), _mm_mul_ps(_mm_set1_ps(a01), v1)),
        dv,
    );
    let e1 = _mm_div_ps(
        _mm_sub_ps(_mm_mul_ps(_mm_set1_ps(a00), v1), _mm_mul_ps(_mm_set1_ps(a01), v0)),
        dv,
    );

    // --- round and pack to 565 (see `round_pack` / `pack565`) ---
    let half = _mm256_set1_pd(0.5);
    let lo = _mm_setzero_ps();
    let hi = _mm_set1_ps(255.0);
    let r0 = _mm256_cvttpd_epi32(_mm256_add_pd(
        _mm256_cvtps_pd(_mm_min_ps(_mm_max_ps(e0, lo), hi)),
        half,
    ));
    let r1 = _mm256_cvttpd_epi32(_mm256_add_pd(
        _mm256_cvtps_pd(_mm_min_ps(_mm_max_ps(e1, lo), hi)),
        half,
    ));
    let sh = _mm_setr_epi32(3, 2, 3, 0);
    let wt = _mm_setr_epi32(2048, 32, 1, 0);
    let p565 = |v: __m128i| {
        let w = _mm_mullo_epi32(_mm_srlv_epi32(v, sh), wt);
        let h = _mm_hadd_epi32(w, w);
        _mm_cvtsi128_si32(_mm_hadd_epi32(h, h)) as u16
    };
    (p565(r0), p565(r1))
}


/// As [`extrema_opaque_avx2`], but over `r + g + b + a` and returning whole
/// RGBA pixels — BC7's form of the same search.
#[cfg(target_arch = "x86_64")]
pub(super) fn extrema_rgba_avx2(pixels: &[[u8; 4]; 16]) -> ([u8; 4], [u8; 4]) {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above).
    unsafe { extrema_rgba_avx2_impl(pixels) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn extrema_rgba_avx2_impl(pixels: &[[u8; 4]; 16]) -> ([u8; 4], [u8; 4]) {
    use std::arch::x86_64::*;
    let src = pixels.as_ptr() as *const u8;
    let wv = _mm256_set1_epi16(1);
    let perm = _mm256_setr_epi32(0, 1, 4, 5, 2, 3, 6, 7);
    let lum = |off: usize| {
        let a = _mm256_cvtepu8_epi16(_mm_loadu_si128(src.add(off) as *const __m128i));
        let b = _mm256_cvtepu8_epi16(_mm_loadu_si128(src.add(off + 16) as *const __m128i));
        _mm256_permutevar8x32_epi32(
            _mm256_hadd_epi32(_mm256_madd_epi16(a, wv), _mm256_madd_epi16(b, wv)),
            perm,
        )
    };
    let l0 = lum(0);
    let l1 = lum(32);
    let s0 = _mm256_slli_epi32(l0, 4);
    let s1 = _mm256_slli_epi32(l1, 4);
    let kmin = _mm256_min_epi32(
        _mm256_or_si256(s0, _mm256_setr_epi32(0, 1, 2, 3, 4, 5, 6, 7)),
        _mm256_or_si256(s1, _mm256_setr_epi32(8, 9, 10, 11, 12, 13, 14, 15)),
    );
    let kmax = _mm256_max_epi32(
        _mm256_or_si256(s0, _mm256_setr_epi32(15, 14, 13, 12, 11, 10, 9, 8)),
        _mm256_or_si256(s1, _mm256_setr_epi32(7, 6, 5, 4, 3, 2, 1, 0)),
    );
    let fold = |v: __m256i, is_min: bool| -> i32 {
        let a = _mm256_castsi256_si128(v);
        let b = _mm256_extracti128_si256(v, 1);
        let r = if is_min { _mm_min_epi32(a, b) } else { _mm_max_epi32(a, b) };
        let r2 = _mm_shuffle_epi32(r, 0b01_00_11_10);
        let r = if is_min { _mm_min_epi32(r, r2) } else { _mm_max_epi32(r, r2) };
        let r3 = _mm_shuffle_epi32(r, 0b10_11_00_01);
        let r = if is_min { _mm_min_epi32(r, r3) } else { _mm_max_epi32(r, r3) };
        _mm_cvtsi128_si32(r)
    };
    let imin = (fold(kmin, true) & 15) as usize;
    let imax = (15 - (fold(kmax, false) & 15)) as usize;
    (pixels[imax], pixels[imin])
}

/// The block's luminance-extreme pixels: `(max, min)` by `2r + 3g + b`.
///
/// The scalar form is a sixteen-iteration argmin/argmax with two unpredictable
/// branches a pixel — 310 instructions for the block.
///
/// # Packing the index into the key
///
/// Luminance is at most `2*255 + 3*255 + 255 = 1530`, so `l * 16 + i` fits an
/// i32 with room and a single `min_epi32` finds the argmin. The tie-break has to
/// be watched: the scalar keeps the FIRST extreme (strict `<` and `>`), which
/// `min` reproduces directly for the minimum, but for the maximum the key must
/// carry `15 - i` so that `max` also prefers the smaller index.
#[cfg(target_arch = "x86_64")]
pub(super) fn extrema_opaque_avx2(pixels: &[[u8; 4]; 16]) -> ([u8; 3], [u8; 3]) {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above); the array is
    // fixed-size and read with unaligned loads.
    unsafe { extrema_opaque_avx2_impl(pixels) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn extrema_opaque_avx2_impl(pixels: &[[u8; 4]; 16]) -> ([u8; 3], [u8; 3]) {
    use std::arch::x86_64::*;
    let src = pixels.as_ptr() as *const u8;
    // `madd` folds (r,g) to 2r+3g and (b,a) to b; `hadd` folds those two into
    // the luminance, and the permute undoes `hadd`'s lane interleave.
    let wv = _mm256_setr_epi16(2, 3, 1, 0, 2, 3, 1, 0, 2, 3, 1, 0, 2, 3, 1, 0);
    let perm = _mm256_setr_epi32(0, 1, 4, 5, 2, 3, 6, 7);
    let lum = |off: usize| {
        let a = _mm256_cvtepu8_epi16(_mm_loadu_si128(src.add(off) as *const __m128i));
        let b = _mm256_cvtepu8_epi16(_mm_loadu_si128(src.add(off + 16) as *const __m128i));
        _mm256_permutevar8x32_epi32(
            _mm256_hadd_epi32(_mm256_madd_epi16(a, wv), _mm256_madd_epi16(b, wv)),
            perm,
        )
    };
    let l0 = lum(0);
    let l1 = lum(32);
    let s0 = _mm256_slli_epi32(l0, 4);
    let s1 = _mm256_slli_epi32(l1, 4);
    let kmin = _mm256_min_epi32(
        _mm256_or_si256(s0, _mm256_setr_epi32(0, 1, 2, 3, 4, 5, 6, 7)),
        _mm256_or_si256(s1, _mm256_setr_epi32(8, 9, 10, 11, 12, 13, 14, 15)),
    );
    let kmax = _mm256_max_epi32(
        _mm256_or_si256(s0, _mm256_setr_epi32(15, 14, 13, 12, 11, 10, 9, 8)),
        _mm256_or_si256(s1, _mm256_setr_epi32(7, 6, 5, 4, 3, 2, 1, 0)),
    );
    let fold = |v: __m256i, is_min: bool| -> i32 {
        let a = _mm256_castsi256_si128(v);
        let b = _mm256_extracti128_si256(v, 1);
        let r = if is_min { _mm_min_epi32(a, b) } else { _mm_max_epi32(a, b) };
        let r2 = _mm_shuffle_epi32(r, 0b01_00_11_10);
        let r = if is_min { _mm_min_epi32(r, r2) } else { _mm_max_epi32(r, r2) };
        let r3 = _mm_shuffle_epi32(r, 0b10_11_00_01);
        let r = if is_min { _mm_min_epi32(r, r3) } else { _mm_max_epi32(r, r3) };
        _mm_cvtsi128_si32(r)
    };
    let imin = (fold(kmin, true) & 15) as usize;
    let imax = (15 - (fold(kmax, false) & 15)) as usize;
    let (mx, mn) = (pixels[imax], pixels[imin]);
    ([mx[0], mx[1], mx[2]], [mn[0], mn[1], mn[2]])
}


/// Per-channel `(max, min)` over the block's sixteen pixels.
///
/// The scalar form is a sixteen-iteration loop with four `min` and four `max`
/// per pixel, and it measured **261 instructions for RGB and 550 for RGBA**,
/// once each per block.
///
/// A block is 64 contiguous bytes — four 16-byte vectors — and byte lane `j` of
/// each holds channel `j % 4` of some pixel. So three `min_epu8` reduce the four
/// vectors to one, and two byte-shifts fold that down to the four channels. The
/// same three-then-two for `max`.
#[cfg(target_arch = "x86_64")]
pub(super) fn channel_minmax_avx2(pixels: &[[u8; 4]; 16]) -> ([u8; 4], [u8; 4]) {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 implies SSE2; the array is 64 contiguous bytes read with
    // four unaligned 16-byte loads.
    unsafe { channel_minmax_avx2_impl(pixels) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn channel_minmax_avx2_impl(pixels: &[[u8; 4]; 16]) -> ([u8; 4], [u8; 4]) {
    use std::arch::x86_64::*;
    let src = pixels.as_ptr() as *const u8;
    let v0 = _mm_loadu_si128(src as *const __m128i);
    let v1 = _mm_loadu_si128(src.add(16) as *const __m128i);
    let v2 = _mm_loadu_si128(src.add(32) as *const __m128i);
    let v3 = _mm_loadu_si128(src.add(48) as *const __m128i);
    let mn = _mm_min_epu8(_mm_min_epu8(v0, v1), _mm_min_epu8(v2, v3));
    let mx = _mm_max_epu8(_mm_max_epu8(v0, v1), _mm_max_epu8(v2, v3));
    let mn = _mm_min_epu8(mn, _mm_srli_si128(mn, 8));
    let mn = _mm_min_epu8(mn, _mm_srli_si128(mn, 4));
    let mx = _mm_max_epu8(mx, _mm_srli_si128(mx, 8));
    let mx = _mm_max_epu8(mx, _mm_srli_si128(mx, 4));
    (
        (_mm_cvtsi128_si32(mx) as u32).to_le_bytes(),
        (_mm_cvtsi128_si32(mn) as u32).to_le_bytes(),
    )
}


/// Transpose a block's sixteen RGBA pixels into four 16-byte channel planes.
///
/// Scalar this is 64 individual byte moves. As vectors it is the standard 4x16
/// byte transpose: one `pshufb` per 16-byte group gathers that group's four
/// channels into four contiguous dwords, and four `unpack`s interleave the
/// groups into the finished planes.
#[cfg(target_arch = "x86_64")]
pub(super) fn planar_avx2(pixels: &[[u8; 4]; 16]) -> [[u8; 16]; 4] {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 implies SSSE3; the array is 64 contiguous bytes read with
    // four unaligned 16-byte loads.
    unsafe { planar_avx2_impl(pixels) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn planar_avx2_impl(pixels: &[[u8; 4]; 16]) -> [[u8; 16]; 4] {
    use std::arch::x86_64::*;
    let src = pixels.as_ptr() as *const u8;
    // Each group of four pixels becomes [RRRR GGGG BBBB AAAA].
    let sh = _mm_setr_epi8(0, 4, 8, 12, 1, 5, 9, 13, 2, 6, 10, 14, 3, 7, 11, 15);
    let g = |o: usize| _mm_shuffle_epi8(_mm_loadu_si128(src.add(o) as *const __m128i), sh);
    let (s0, s1, s2, s3) = (g(0), g(16), g(32), g(48));
    // Dword j of `sk` is channel j of pixels 4k..4k+3, so interleaving dwords
    // then quadwords lands each channel's sixteen bytes contiguously.
    let lo01 = _mm_unpacklo_epi32(s0, s1);
    let lo23 = _mm_unpacklo_epi32(s2, s3);
    let hi01 = _mm_unpackhi_epi32(s0, s1);
    let hi23 = _mm_unpackhi_epi32(s2, s3);
    let mut out = [[0u8; 16]; 4];
    _mm_storeu_si128(out[0].as_mut_ptr() as *mut __m128i, _mm_unpacklo_epi64(lo01, lo23));
    _mm_storeu_si128(out[1].as_mut_ptr() as *mut __m128i, _mm_unpackhi_epi64(lo01, lo23));
    _mm_storeu_si128(out[2].as_mut_ptr() as *mut __m128i, _mm_unpacklo_epi64(hi01, hi23));
    _mm_storeu_si128(out[3].as_mut_ptr() as *mut __m128i, _mm_unpackhi_epi64(hi01, hi23));
    out
}


/// Nearest-palette index for sixteen alpha samples against an 8-entry palette,
/// plus the resulting SSE.
///
/// # The shape
///
/// The scalar form is 16 samples x 8 entries of `|pu - s|` with a running
/// argmin — and the `AlphaSelect` fast path is not better, just differently
/// arranged: seven threshold compares per sample, 112 for the block. Either way
/// it measured **738 instructions** inside `pack_alpha_indices_s`.
///
/// # Why the tie-break survives
///
/// The scalar keeps the FIRST minimum (`if d < best_d`, strict). Packing the
/// key as `d * 8 + j` makes a single `min_epi16` reproduce that exactly: equal
/// distances are separated by the index, and the smaller index wins. `d` is at
/// most 255 so the key is at most 2047 — no i16 concern.
#[cfg(target_arch = "x86_64")]
pub(super) fn alpha_select_avx2(pal_u: &[u8; 8], samples: &[u8; 16]) -> ([u8; 16], i32) {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above); both arrays
    // are fixed-size and read with unaligned loads.
    unsafe { alpha_select_avx2_impl(pal_u, samples) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn alpha_select_avx2_impl(pal_u: &[u8; 8], samples: &[u8; 16]) -> ([u8; 16], i32) {
    use std::arch::x86_64::*;
    let sv = _mm256_cvtepu8_epi16(_mm_loadu_si128(samples.as_ptr() as *const __m128i));
    let mut best = _mm256_set1_epi16(0x7FFF);
    for j in 0..8i16 {
        let pv = _mm256_set1_epi16(pal_u[j as usize] as i16);
        let d = _mm256_abs_epi16(_mm256_sub_epi16(pv, sv));
        // key = d * 8 + j
        let key = _mm256_add_epi16(_mm256_slli_epi16(d, 3), _mm256_set1_epi16(j));
        best = _mm256_min_epi16(best, key);
    }
    let idx16 = _mm256_and_si256(best, _mm256_set1_epi16(7));
    let d16 = _mm256_srli_epi16(best, 3);
    // Each squared distance is at most 255^2, and `madd` folds pairs, so the
    // eight lanes sum well inside i32.
    let sq = _mm256_madd_epi16(d16, d16);
    let h = _mm256_hadd_epi32(sq, sq);
    let h = _mm256_hadd_epi32(h, h);
    let err = _mm_cvtsi128_si32(_mm_add_epi32(
        _mm256_castsi256_si128(h),
        _mm256_extracti128_si256(h, 1),
    ));
    // Indices are 0..=7, so `packus` cannot saturate; one permute undoes its
    // 128-bit lane interleave.
    let packed = _mm256_permute4x64_epi64(_mm256_packus_epi16(idx16, idx16), 0b00_00_10_00);
    let mut out = [0u8; 16];
    _mm_storeu_si128(out.as_mut_ptr() as *mut __m128i, _mm256_castsi256_si128(packed));
    (out, err)
}


/// BC1 least-squares endpoints for a fixed index table — the plain encoder's
/// equivalent of what `refit_with_ls` does on the RDO path.
///
/// # Why this exists
///
/// `ls_endpoints_bc1` solved the same normal equations SCALAR: a sixteen-pixel
/// loop accumulating three `a` terms and six `b` terms, measured at **551
/// instructions dynamic** and run 2.0 times a block. The RDO path has had a
/// vectorised accumulator for this since section 71; the plain encoder was
/// never routed through it.
///
/// # Bit-identity
///
/// Every lane performs `acc += w * x` in pixel order with the multiply and add
/// separate, exactly as the scalar loop does for that lane — the same argument
/// `ls_accum_sse` documents. The three `a` terms accumulate in their own lanes
/// in the same order. `u8 -> f32` is exact, and the rounding stays in the
/// existing `bc1_ls_solve`, which is already bit-identical to
/// `round_clamp_u8`.
#[cfg(target_arch = "x86_64")]
pub(super) fn bc1_ls_endpoints_avx2(
    pixels: &[[u8; 4]; 16],
    table: u32,
) -> Option<([u8; 4], [u8; 4])> {
    debug_assert!(has_avx2());
    // SAFETY: AVX2 guaranteed by dispatch (debug-asserted above); the pixel
    // array is fixed-size and read with unaligned loads.
    unsafe { bc1_ls_endpoints_avx2_impl(pixels, table) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn bc1_ls_endpoints_avx2_impl(
    pixels: &[[u8; 4]; 16],
    table: u32,
) -> Option<([u8; 4], [u8; 4])> {
    use std::arch::x86_64::*;
    // 4-colour weights toward c1 by index: 0 -> 0, 1 -> 1, 2 -> 1/3, 3 -> 2/3.
    const W: [f32; 4] = [0.0, 1.0, 1.0 / 3.0, 2.0 / 3.0];
    let src = pixels.as_ptr() as *const u8;
    // [b0.rgba, b1.rgba] in one register, as `ls_accum_sse`.
    let mut acc = _mm256_setzero_ps();
    let (mut a00, mut a01, mut a11) = (0f32, 0f32, 0f32);
    for i in 0..16usize {
        let wgt = W[((table >> (2 * i)) & 3) as usize];
        let u = 1.0 - wgt;
        a00 += u * u;
        a01 += u * wgt;
        a11 += wgt * wgt;
        // Built in registers. Writing `[u, wgt]` to the stack and broadcasting
        // it back is a store-to-load round trip PER PIXEL — the exact
        // store-forwarding shape this codebase has removed six times, and it
        // measured 8-26% SLOWER end to end than the scalar loop it replaced.
        let wv = _mm256_blend_ps(
            _mm256_set1_ps(u),
            _mm256_set1_ps(wgt),
            0b1111_0000,
        );
        let px = _mm_cvtepi32_ps(_mm_cvtepu8_epi32(_mm_cvtsi32_si128(
            *(src.add(i * 4) as *const i32),
        )));
        let pv = _mm256_permutevar8x32_ps(
            _mm256_castps128_ps256(px),
            _mm256_setr_epi32(0, 1, 2, 3, 0, 1, 2, 3),
        );
        acc = _mm256_add_ps(acc, _mm256_mul_ps(wv, pv));
    }
    let det = a00 * a11 - a01 * a01;
    if det.abs() < 1e-4 {
        return None;
    }
    let mut v0 = [0f32; 4];
    let mut v1 = [0f32; 4];
    _mm_storeu_ps(v0.as_mut_ptr(), _mm256_castps256_ps128(acc));
    _mm_storeu_ps(v1.as_mut_ptr(), _mm256_extractf128_ps(acc, 1));
    Some(bc1_ls_solve(v0, v1, a00, a01, a11, det))
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
        // Two generators: a wide one that mostly clamps, and a narrow one that
        // lands inside 0..=255 where the rounding actually decides the answer.
        let mut f = move || (next() as u32 as f32) / 1.0e5 - 20_000.0;
        for case in 0..60_000u32 {
            let narrow = case % 2 == 1;
            let mut g = || {
                if narrow {
                    (f() % 300.0).abs()
                } else {
                    f()
                }
            };
            let b0 = [g(), g(), g(), g()];
            let b1 = [g(), g(), g(), g()];
            let (a00, a01, a11) = (g(), g(), g());
            let det = if case == 0 { 1.0 } else { g() };
            if det == 0.0 || !det.is_finite() {
                continue;
            }
            let (g0, g1) = bc1_ls_solve(b0, b1, a00, a01, a11, det);
            for c in 0..4 {
                // The kernel now rounds internally, so the oracle covers BOTH
                // halves: the divide must be bit-identical and the lane-wise
                // round must equal the scalar `round_clamp_u8`.
                let w0 = (a11 * b0[c] - a01 * b1[c]) / det;
                let w1 = (a00 * b1[c] - a01 * b0[c]) / det;
                if !w0.is_finite() || !w1.is_finite() {
                    continue; // det is bounded away from zero in real callers
                }
                let want0 = crate::encode::blocks::round_clamp_u8(w0);
                let want1 = crate::encode::blocks::round_clamp_u8(w1);
                assert_eq!(g0[c], want0, "case {case} e0[{c}] from {w0:?}");
                assert_eq!(g1[c], want1, "case {case} e1[{c}] from {w1:?}");
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
            let mut uw = [[0f32; 8]; 16];
            for (i, slot) in uw.iter_mut().enumerate() {
                let w = W[((table >> (2 * i)) & 3) as usize];
                *slot = [1.0 - w, 1.0 - w, 1.0 - w, 1.0 - w, w, w, w, w];
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
                let (u, wgt) = (uw[i][0], uw[i][4]);
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
            // Endpoints are 565 words now, because the kernel builds its own
            // palette — so this oracle covers the palette build as well as the
            // scoring loop. Both mode branches are exercised: `c0 > c1` is the
            // 4-colour palette, `c0 <= c1` the 3-colour punch-through one.
            let (c0, c1) = match case {
                0 => (0u16, 0u16),
                1 => (u16::MAX, 0),
                2 => (0, u16::MAX),
                3 => (u16::MAX, u16::MAX),
                _ => (next() as u16, (next() >> 16) as u16),
            };
            let table = match case {
                0 => 0,
                1 => u32::MAX,
                _ => next() as u32,
            };
            // Scalar reference palette, exactly as `bc1_colors_packed` builds it.
            let ex5 = |v: u32| ((v << 3) | (v >> 2)) as u8;
            let ex6 = |v: u32| ((v << 2) | (v >> 4)) as u8;
            let unp = |c: u16| {
                let c = c as u32;
                [ex5((c >> 11) & 31), ex6((c >> 5) & 63), ex5(c & 31)]
            };
            let (a, b) = (unp(c0), unp(c1));
            let mut rgb = [[0u8; 3]; 4];
            rgb[0] = a;
            rgb[1] = b;
            for k in 0..3 {
                if c0 > c1 {
                    rgb[2][k] = ((2 * a[k] as u32 + b[k] as u32) / 3) as u8;
                    rgb[3][k] = ((a[k] as u32 + 2 * b[k] as u32) / 3) as u8;
                } else {
                    rgb[2][k] = ((a[k] as u32 + b[k] as u32) / 2) as u8;
                    rgb[3][k] = 0;
                }
            }
            let got = bc1_fixed_sse_565_avx2(&px, c0, c1, table);
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
