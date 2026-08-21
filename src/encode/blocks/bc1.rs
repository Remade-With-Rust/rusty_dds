//! BC1 / BC2 / BC3 colour blocks.
//!
//! BC1: luminance seed, PCA-axis seed, iterated least-squares refine and a
//! 565-lattice contract refine. BC2 and BC3 reuse the BC1 colour path and pair
//! it with the explicit / interpolated alpha blocks in [`super::alpha`].

use super::*;

// ---------------------------------------------------------------------------

pub fn encode_bc1(pixels: [[u8; 4]; 16], out: &mut [u8]) {
    out[..8].copy_from_slice(&encode_bc1_bytes(pixels));
}

/// Sum over the block of (r^2 + g^2 + b^2) — the pixel-only term of the SSE.
///
/// Every candidate fit needs it and none of them can change it, so the encoder
/// computes it once a block and passes it down. See `bc1_fit_core_avx2`: the
/// fit works in `sum q^2 - 2*dot`, which is the true SSE short of exactly this.
pub(super) fn psq_rgb(pixels: &[[u8; 4]; 16]) -> i32 {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        return simd::bc1_psq_rgb_avx2(pixels);
    }
    psq_rgb_scalar(pixels)
}

/// Kept OUT of line: the fallback arm of an AVX2 dispatch, never executed
/// on a machine with AVX2, but inlined at the dispatch it lands in the hot
/// body and interleaves with the code that does run.
#[cold]
#[inline(never)]
fn psq_rgb_scalar(pixels: &[[u8; 4]; 16]) -> i32 {
    let mut t = 0i32;
    for p in pixels {
        for c in 0..3 {
            let v = p[c] as i32;
            t += v * v;
        }
    }
    t
}

pub(super) fn encode_bc1_bytes(pixels: [[u8; 4]; 16]) -> [u8; 8] {
    // A single fused walk producing BOTH reductions was tried here and REFUTED:
    // `extrema_opaque` and `channel_minmax_rgb` do issue the same four loads
    // over the same sixty-four bytes, but that is all they share — the
    // reductions are disjoint, so the fused kernel measured 75 instructions
    // against 51 + 23 separate, and the wider return plus the extra symbol cost
    // +120 across the tree. Both run ONCE A BLOCK, so the whole prize was four
    // L1 loads a block, against a lattice that fits eleven times a block.
    // Fusing two once-per-block walks cannot matter at this scale.
    let (max_c, min_c) = extrema_opaque(&pixels);
    // Once a block, for every fit below — see `psq_rgb`.
    let psq = psq_rgb(&pixels);
    // Fused pack+score: the index fit's per-pixel argmin distance IS the SSE
    // contribution, so the old pack-then-bc1_sse re-walk is pure recompute.
    let (a, a_err) = pack_bc1_scored(&pixels, max_c, min_c, psq, i32::MAX)
        .expect("unbounded pack always packs");
    // `rgb_channel_span_sum` and `channel_minmax_rgb` are character-for-character
    // the same sixteen-pixel walk — the first just sums the second's spans — and
    // both ran on this block. One walk now serves both. (`encode_bc7_mode6_inner`
    // had the same duplication and records the same fix.)
    let (mx, mn) = channel_minmax_rgb(&pixels);
    let span = (mx[0] - mn[0]) as i32 + (mx[1] - mn[1]) as i32 + (mx[2] - mn[2]) as i32;
    if span < 24 {
        return a;
    }
    let mut best = a;
    let mut best_err = a_err;
    if best_err == 0 {
        return best;
    }
    // The second seed is redundant whenever it QUANTIZES to the same 565 pair as
    // the first — not merely when the byte triples match, which is what this
    // used to test. 565 drops three bits from red and blue and two from green,
    // so distinct extrema routinely land on the same pair, and when they do the
    // whole candidate is identical: same nudge, same ordering, same palette,
    // same indices, same error.
    //
    // Skipping is exact rather than a heuristic. `consider_bc1` passes
    // `*best_err` as the limit and the fit returns `None` on `err >= limit`, so
    // a candidate that merely TIES the incumbent is already rejected — which is
    // precisely what an identical candidate does. The old byte test is a strict
    // subset of this one.
    if (to_565(mx), to_565(mn)) != (to_565(max_c), to_565(min_c)) {
        consider_bc1(&pixels, mx, mn, psq, &mut best, &mut best_err);
    }
    // Refine gate: a tiny residual can't repay PCA + LS (gain <= best_err).
    // Smooth-map blocks skip the whole refine; busy blocks keep the quality.
    if quality_is_fast() || best_err <= 16 {
        return best;
    }
    // PCA-axis extremes: luminance extrema mis-seed chroma-dominant blocks.
    // DEFAULT OFF — see `BC1_PCA_SEED`. Ablated on the corpus, this seed costs
    // 21-26% of BC1 encode time and buys 0.00-0.01 dB: it offers a candidate on
    // essentially every block and wins on 1.9-6.2% of them, because the
    // luminance seed and the 565 lattice have already found the same answer.
    if bc1_pca_seed_enabled() {
        if let Some((pa, pb)) = pca_extremes_rgb(&pixels) {
            consider_bc1(&pixels, pa, pb, psq, &mut best, &mut best_err);
        }
    }
    // Least-squares endpoint refine from the winner's indices, iterated while
    // the decode-matched SSE keeps falling (candidates only ever ADD, picked
    // by the same scoring — per-block error is monotonically ≤ the old path).
    for _ in 0..4 {
        if best_err == 0 {
            break;
        }
        let Some((e0, e1)) = ls_endpoints_bc1(&pixels, &best) else {
            break;
        };
        let prev = best_err;
        consider_bc1(&pixels, e0, e1, psq, &mut best, &mut best_err);
        if best_err >= prev {
            break;
        }
    }
    if best_err > bc1_lattice_min_err() {
        lattice_refine_bc1(&pixels, psq, &mut best, &mut best_err);
    }
    best
}


pub(super) fn consider_bc1(
    pixels: &[[u8; 4]; 16],
    e0: [u8; 3],
    e1: [u8; 3],
    psq: i32,
    best: &mut [u8; 8],
    best_err: &mut i32,
) {
    if let Some((cand, err)) = pack_bc1_scored(pixels, e0, e1, psq, *best_err) {
        *best = cand;
        *best_err = err;
    }
}

/// 4-color index fit + SSE with early abort. Projection fast path (1 dot +
/// 3 threshold compares/pixel vs 12 multiplies): the palette lies on the
/// c0→c1 line at t = 0, 1/3, 2/3, 1, so nearest-along-the-line is a
/// threshold count at t = 1/6, 1/2, 5/6 — then a ±1 SSE check absorbs the
/// per-channel rounding of the interpolated entries. Near-degenerate axes
/// keep the exhaustive scan (same reasoning as the BC7 projection fit).
#[inline]
pub(super) fn bc1_fit_4color(
    pixels: &[[u8; 4]; 16],
    colors: &[[u8; 3]; 4],
    err_limit: i32,
) -> Option<(u32, i32)> {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        return simd::bc1_fit_4color_avx2(pixels, colors, err_limit);
    }
    bc1_fit_4color_scalar(pixels, colors, err_limit)
}

/// Scalar oracle/fallback: prefix early-abort and total-abort agree because
/// squared errors are non-negative (prefix >= limit iff total >= limit for
/// the acceptance decision).
/// Kept OUT of line: this is the fallback arm of an AVX2 dispatch, so on
/// any machine that has AVX2 it is never executed — but inlined at the
/// dispatch it lands in the hot body and interleaves with the code that
/// does run.
#[cold]
#[inline(never)]
pub(super) fn bc1_fit_4color_scalar(
    pixels: &[[u8; 4]; 16],
    colors: &[[u8; 3]; 4],
    err_limit: i32,
) -> Option<(u32, i32)> {
    let mut table = 0u32;
    let mut err = 0i32;
    for (i, p) in pixels.iter().enumerate() {
        // The RGB triple is built once, not rebuilt inside all four
        // comparisons, and entry 0 SEEDS the argmin rather than being compared
        // against a sentinel. Seeding is exact: the scan uses a strict `<`, so
        // entry 0 already won every tie it could reach, and starting from it
        // just retires the comparison against `i32::MAX` that could never fail.
        let px = [p[0], p[1], p[2]];
        let mut best = 0usize;
        let mut best_d = sqr_rgb(px, colors[0]);
        for j in 1..4usize {
            let d = sqr_rgb(px, colors[j]);
            if d < best_d {
                best_d = d;
                best = j;
            }
        }
        table |= (best as u32) << (2 * i);
        err += best_d;
        if err >= err_limit {
            return None;
        }
    }
    Some((table, err))
}

/// Score a 4-color candidate whose endpoints are ALREADY 565 values (the
/// 565-lattice refine works in quantized space directly, so no re-rounding).
pub(super) fn pack_bc1_scored_565(
    pixels: &[[u8; 4]; 16],
    a: u16,
    b: u16,
    psq: i32,
    err_limit: i32,
) -> Option<([u8; 8], i32)> {
    debug_assert_ne!(a, b);
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    // The lattice runs this ~6.6 times a block, and it used to build the BYTE
    // palette scalar (77 instructions) only for the fit kernel to widen it
    // straight back to i16 — the same pack-then-unpack round trip removed from
    // `pack_bc1_scored`. Both come from the 565 words directly here.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        let (table, err) = simd::bc1_fit_565_avx2(pixels, hi, lo, psq, err_limit)?;
        let v = (hi as u64) | ((lo as u64) << 16) | ((table as u64) << 32);
        return Some((v.to_le_bytes(), err));
    }
    pack_bc1_scored_565_cold(pixels, hi, lo, err_limit)
}

/// Kept OUT of line: the fallback arm of an AVX2 dispatch, never executed on a
/// machine with AVX2. Inlined at the dispatch it drags the scalar palette build
/// and the whole scalar fit in with it.
#[cold]
#[inline(never)]
fn pack_bc1_scored_565_cold(
    pixels: &[[u8; 4]; 16],
    hi: u16,
    lo: u16,
    err_limit: i32,
) -> Option<([u8; 8], i32)> {
    pack_bc1_scored_with(pixels, hi, lo, &bc1_palette_565(hi, lo), err_limit)
}

/// The four-colour palette of a 565 endpoint pair.
///
/// Split out because the RDO endpoint-reuse path calls the packer ~16 times a
/// block with endpoints drawn from a sixteen-entry sliding window: the palette
/// is fixed while an entry is resident, but was rebuilt — two `from_565` and two
/// `lerp_rgb` — by every block that tried it.
pub(super) fn bc1_palette_565(hi: u16, lo: u16) -> [[u8; 3]; 4] {
    let ca = from_565(hi);
    let cb = from_565(lo);
    [ca, cb, lerp_rgb::<2, 1>(ca, cb), lerp_rgb::<1, 2>(ca, cb)]
}

/// Per-block widened palette for [`pack_bc1_scored_pre`]. Empty without SIMD.
#[cfg(all(feature = "simd", target_arch = "x86_64"))]
pub(super) type Pal16 = simd::Bc1Pal;
#[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
pub(super) type Pal16 = ();

/// Widen a palette once, for reuse across many fits.
#[cfg(all(feature = "simd", target_arch = "x86_64"))]
pub(super) fn widen_pal(colors: &[[u8; 3]; 4]) -> Pal16 {
    if simd::has_avx2() {
        simd::bc1_widen_palette(colors)
    } else {
        simd::Bc1Pal::ZERO
    }
}
#[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
pub(super) fn widen_pal(_colors: &[[u8; 3]; 4]) -> Pal16 {}

/// The byte palette, but only where something will read it.
///
/// Since the widened form comes straight from the 565 words, the byte palette is
/// consumed by the scalar fallback alone — so on a machine that takes the vector
/// path it is 77 instructions a block producing a value nothing reads.
#[cfg(all(feature = "simd", target_arch = "x86_64"))]
pub(super) fn byte_pal_if_needed(hi: u16, lo: u16) -> [[u8; 3]; 4] {
    if simd::has_avx2() {
        [[0u8; 3]; 4]
    } else {
        bc1_palette_565(hi, lo)
    }
}
#[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
pub(super) fn byte_pal_if_needed(hi: u16, lo: u16) -> [[u8; 3]; 4] {
    bc1_palette_565(hi, lo)
}

/// The widened palette straight from the two 565 words, skipping the byte form.
#[cfg(all(feature = "simd", target_arch = "x86_64"))]
pub(super) fn pal16_from_565(hi: u16, lo: u16) -> Pal16 {
    if simd::has_avx2() {
        simd::bc1_palette_565_i16_avx2(hi, lo)
    } else {
        pal16_from_565_scalar(hi, lo)
    }
}

/// Kept OUT of line: the fallback arm of an AVX2 dispatch, never executed
/// on a machine with AVX2, but inlined at the dispatch it lands in the hot
/// body and interleaves with the code that does run.
#[cold]
#[inline(never)]
fn pal16_from_565_scalar(hi: u16, lo: u16) -> Pal16 {
    widen_pal(&bc1_palette_565(hi, lo))
}
#[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
pub(super) fn pal16_from_565(_hi: u16, _lo: u16) -> Pal16 {}

/// [`pack_bc1_scored_with`] using a palette widened once by the caller.
///
/// The RDO window reuses each cached palette about fourteen times a block, and
/// the fit kernel used to re-widen it on every one of those calls.
pub(super) fn pack_bc1_scored_pre(
    pixels: &[[u8; 4]; 16],
    hi: u16,
    lo: u16,
    colors: &[[u8; 3]; 4],
    pal16: &Pal16,
    psq: i32,
    err_limit: i32,
) -> Option<([u8; 8], i32)> {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        let (table, err) = simd::bc1_fit_4color_pre_avx2(pixels, pal16, psq, err_limit)?;
        let v = (hi as u64) | ((lo as u64) << 16) | ((table as u64) << 32);
        return Some((v.to_le_bytes(), err));
    }
    let _ = pal16;
    pack_bc1_scored_with(pixels, hi, lo, colors, err_limit)
}

pub(super) fn pack_bc1_scored_with(
    pixels: &[[u8; 4]; 16],
    hi: u16,
    lo: u16,
    colors: &[[u8; 3]; 4],
    err_limit: i32,
) -> Option<([u8; 8], i32)> {
    let (table, err) = bc1_fit_4color(pixels, colors, err_limit)?;
    // A BC1 block is two 565 words then the 32-bit index table, all
    // little-endian and contiguous — one `u64`, not three `copy_from_slice`
    // calls into a stack array.
    let v = (hi as u64) | ((lo as u64) << 16) | ((table as u64) << 32);
    Some((v.to_le_bytes(), err))
}

/// 565-lattice hill climb around the winner: LS optimizes continuous RGB and
/// rounds through 565, so adjacent LATTICE points can beat the rounded
/// answer (the same discrete-lattice effect the signed window exploits).
/// ±1 per component per endpoint (12 candidates/round), up to 2 rounds,
/// strict `<` acceptance — quality-monotone.
/// # A refuted prune, recorded so it is not retried
///
/// A per-candidate lower bound was tried here, the same one
/// `signed_window_sweep` uses: a 4-colour palette's reconstructions lie between
/// its endpoints, so a sample above the ceiling contributes at least
/// `(smax - ceil)^2` and one below the floor at least `(floor - smin)^2`.
/// Byte-identical, provably safe — and **measurably slower**: serial medians
/// went 0.967 -> 1.102 (Bricks), 0.932 -> 1.053 (Metal), 1.019 -> 1.133 (Rock),
/// 0.884 -> 1.038 (Wood).
///
/// The bound needs `from_565` on both endpoints — six table lookups, about 35
/// instructions — against a fit of roughly 160, so it must fire on more than a
/// fifth of candidates to break even. It does not: the lattice makes
/// CONTRACT-ONLY moves from an already-good incumbent, so a candidate's palette
/// range rarely excludes enough of the block to reach `best_err`.
pub(super) fn lattice_refine_bc1(
    pixels: &[[u8; 4]; 16],
    psq: i32,
    best: &mut [u8; 8],
    best_err: &mut i32,
) {
    // Contract-only, harvest-chosen (1.3M wins over the bc1 corpus): moves
    // that SHRINK the endpoint interval (hi component down / lo component
    // up) carry ~82% of the full ±1 neighborhood's gain at half the packs —
    // 4-color quantization wants the interpolants pulled toward the data
    // mass, and the seeds/LS systematically overshoot outward. Hill-climb
    // up to 3 rounds while improving.
    for _round in 0..bc1_lattice_rounds() {
        let c0 = u16::from_le_bytes([best[0], best[1]]);
        let c1 = u16::from_le_bytes([best[2], best[3]]);
        if c0 <= c1 {
            return; // 3-color/punch block: lattice targets 4-color mode only.
        }
        let prev = *best_err;
        // (endpoint base, other endpoint, contract direction)
        for (base, other, d) in [(c0, c1, -1i32), (c1, c0, 1i32)] {
            for (shift, maxv) in [(11u16, 31u16), (5, 63), (0, 31)] {
                let cur = (base >> shift) & maxv;
                let nv = cur as i32 + d;
                if nv < 0 || nv > maxv as i32 {
                    continue;
                }
                let cand = (base & !(maxv << shift)) | ((nv as u16) << shift);
                if cand == other {
                    continue;
                }
                if let Some((blk, e)) = pack_bc1_scored_565(pixels, cand, other, psq, *best_err) {
                    *best = blk;
                    *best_err = e;
                    if e == 0 {
                        return;
                    }
                }
            }
        }
        if *best_err >= prev {
            break;
        }
    }
}

/// Pack a BC1 block AND its decode-matched SSE in one walk, aborting early
/// once the partial SSE reaches `err_limit` (an aborted candidate could
/// never win under strict `<`, so selection is identical to pack+bc1_sse).
pub(super) fn pack_bc1_scored(
    pixels: &[[u8; 4]; 16],
    max_c: [u8; 3],
    min_c: [u8; 3],
    psq: i32,
    err_limit: i32,
) -> Option<([u8; 8], i32)> {
    let mut max565 = to_565(max_c);
    let min565 = to_565(min_c);
    if max565 == min565 {
        max565 = max565.saturating_add(1);
        // Still equal after the nudge means both words are 0xFFFF — the
        // all-white-565 block, and the only input that reaches true 3-color
        // punch-through mode.
        if max565 == min565 {
            return pack_bc1_scored_cold(pixels, max565, min565, true, err_limit);
        }
    }
    // Everything past the quantization IS `pack_bc1_scored_565`: order the pair
    // larger-first, fit against the decode-true 4-color palette, pack. This
    // function carried its own copy of all three.
    //
    // The two agree on the scalar arm as well, which is what makes the merge
    // exact rather than merely equivalent-looking: the copy here built
    // `[ca, cb, lerp<2,1>, lerp<1,2>]` and called `bc1_fit_4color`, and that
    // array is `bc1_palette_565(c0, c1)` — the very argument the other one
    // passes.
    pack_bc1_scored_565(pixels, max565, min565, psq, err_limit)
}

#[cold]
#[inline(never)]
fn pack_bc1_scored_cold(
    pixels: &[[u8; 4]; 16],
    c0: u16,
    c1: u16,
    punch: bool,
    err_limit: i32,
) -> Option<([u8; 8], i32)> {
    // Reached only by the punch branch and the scalar fallback.
    let colors = {
        let ca = from_565(c0);
        let cb = from_565(c1);
        if punch {
            [ca, cb, lerp_rgb::<1, 1>(ca, cb), [0, 0, 0]]
        } else {
            [ca, cb, lerp_rgb::<2, 1>(ca, cb), lerp_rgb::<1, 2>(ca, cb)]
        }
    };
    let (table, err) = if punch {
        let mut table = 0u32;
        let mut err = 0i32;
        for (i, p) in pixels.iter().enumerate() {
            let (idx, e) = if p[3] < 128 {
                (3usize, sqr_rgb([p[0], p[1], p[2]], colors[3]))
            } else {
                let mut best = 0usize;
                let mut best_d = i32::MAX;
                for (j, c) in colors.iter().enumerate() {
                    let d = sqr_rgb([p[0], p[1], p[2]], *c);
                    if d < best_d {
                        best_d = d;
                        best = j;
                    }
                }
                (best, best_d)
            };
            table |= (idx as u32) << (2 * i);
            err += e;
            if err >= err_limit {
                return None;
            }
        }
        (table, err)
    } else {
        bc1_fit_4color(pixels, &colors, err_limit)?
    };
    let mut out = [0u8; 8];
    out[0..2].copy_from_slice(&c0.to_le_bytes());
    out[2..4].copy_from_slice(&c1.to_le_bytes());
    out[4..8].copy_from_slice(&table.to_le_bytes());
    Some((out, err))
}

/// Principal-axis extremes: project RGB onto the covariance principal axis
/// (3 power iterations) and return the two extreme PIXELS along it.
pub(super) fn pca_extremes_rgb(pixels: &[[u8; 4]; 16]) -> Option<([u8; 3], [u8; 3])> {
    let mut mean = [0f32; 3];
    for p in pixels {
        for c in 0..3 {
            mean[c] += p[c] as f32;
        }
    }
    for m in mean.iter_mut() {
        *m /= 16.0;
    }
    // Covariance (upper triangle).
    let mut cov = [0f32; 6]; // rr rg rb gg gb bb
    for p in pixels {
        let d = [
            p[0] as f32 - mean[0],
            p[1] as f32 - mean[1],
            p[2] as f32 - mean[2],
        ];
        cov[0] += d[0] * d[0];
        cov[1] += d[0] * d[1];
        cov[2] += d[0] * d[2];
        cov[3] += d[1] * d[1];
        cov[4] += d[1] * d[2];
        cov[5] += d[2] * d[2];
    }
    let mut axis = [
        cov[0] + cov[1] + cov[2],
        cov[1] + cov[3] + cov[4],
        cov[2] + cov[4] + cov[5],
    ];
    for _ in 0..3 {
        let n = [
            cov[0] * axis[0] + cov[1] * axis[1] + cov[2] * axis[2],
            cov[1] * axis[0] + cov[3] * axis[1] + cov[4] * axis[2],
            cov[2] * axis[0] + cov[4] * axis[1] + cov[5] * axis[2],
        ];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if len < 1e-6 {
            return None;
        }
        axis = [n[0] / len, n[1] / len, n[2] / len];
    }
    let mut lo_t = f32::MAX;
    let mut hi_t = f32::MIN;
    let mut lo_p = [0u8; 3];
    let mut hi_p = [0u8; 3];
    for p in pixels {
        let t = (p[0] as f32 - mean[0]) * axis[0]
            + (p[1] as f32 - mean[1]) * axis[1]
            + (p[2] as f32 - mean[2]) * axis[2];
        if t < lo_t {
            lo_t = t;
            lo_p = [p[0], p[1], p[2]];
        }
        if t > hi_t {
            hi_t = t;
            hi_p = [p[0], p[1], p[2]];
        }
    }
    if lo_p == hi_p {
        return None;
    }
    Some((hi_p, lo_p))
}

/// LS endpoints from a packed BC1 block's indices (4-color mode only).
/// Weights toward c1: idx0=0, idx1=1, idx2=1/3, idx3=2/3.
pub(super) fn ls_endpoints_bc1(pixels: &[[u8; 4]; 16], block: &[u8; 8]) -> Option<([u8; 3], [u8; 3])> {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    if c0 <= c1 {
        return None; // 3-color + punch-through mode: skip LS.
    }
    let table = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    // The RDO path has solved these same normal equations with a vector
    // accumulator since section 71; the plain encoder was never routed through
    // it. Scalar this measured 551 instructions dynamic, 2.0 times a block.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        let (e0, e1) = simd::bc1_ls_endpoints_avx2(pixels, table)?;
        return Some(([e0[0], e0[1], e0[2]], [e1[0], e1[1], e1[2]]));
    }
    const W: [f32; 4] = [0.0, 1.0, 1.0 / 3.0, 2.0 / 3.0];
    let mut a00 = 0f32;
    let mut a01 = 0f32;
    let mut a11 = 0f32;
    let mut b0 = [0f32; 3];
    let mut b1 = [0f32; 3];
    for (i, p) in pixels.iter().enumerate() {
        let w = W[((table >> (2 * i)) & 3) as usize];
        let u = 1.0 - w;
        a00 += u * u;
        a01 += u * w;
        a11 += w * w;
        for c in 0..3 {
            let x = p[c] as f32;
            b0[c] += u * x;
            b1[c] += w * x;
        }
    }
    let det = a00 * a11 - a01 * a01;
    if det.abs() < 1e-4 {
        return None;
    }
    let mut e0 = [0u8; 3];
    let mut e1 = [0u8; 3];
    for c in 0..3 {
        let x0 = (a11 * b0[c] - a01 * b1[c]) / det;
        let x1 = (a00 * b1[c] - a01 * b0[c]) / det;
        e0[c] = super::round_clamp_u8(x0);
        e1[c] = super::round_clamp_u8(x1);
    }
    Some((e0, e1))
}


pub(super) fn channel_minmax_rgb(pixels: &[[u8; 4]; 16]) -> ([u8; 3], [u8; 3]) {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        let (mx, mn) = simd::channel_minmax_avx2(pixels);
        return ([mx[0], mx[1], mx[2]], [mn[0], mn[1], mn[2]]);
    }
    channel_minmax_rgb_scalar(pixels)
}

/// The scalar arm of [`channel_minmax_rgb`], kept OUT of line.
///
/// Sixteen pixels by three channels by min-and-max unrolls to about ninety
/// `cmov` plus the loads to feed them, and inlined at the dispatch it landed
/// whole inside `encode_bc1_bytes` — which measured 95 `cmpb`, 85 `movzbl` and
/// 91 `cmov` in its own body, none of which any AVX2 machine executes. Marking
/// it cold moves it away from the hot path instead of interleaving it there.
#[cold]
#[inline(never)]
fn channel_minmax_rgb_scalar(pixels: &[[u8; 4]; 16]) -> ([u8; 3], [u8; 3]) {
    let mut mn = [255u8; 3];
    let mut mx = [0u8; 3];
    for p in pixels {
        for c in 0..3 {
            mn[c] = mn[c].min(p[c]);
            mx[c] = mx[c].max(p[c]);
        }
    }
    (mx, mn)
}

#[cfg(test)]
pub(super) fn bc1_sse(pixels: &[[u8; 4]; 16], block: &[u8]) -> i32 {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let table = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    let colors = if c0 > c1 {
        [
            from_565(c0),
            from_565(c1),
            lerp_rgb::<2, 1>(from_565(c0), from_565(c1)),
            lerp_rgb::<1, 2>(from_565(c0), from_565(c1)),
        ]
    } else {
        [
            from_565(c0),
            from_565(c1),
            lerp_rgb::<1, 1>(from_565(c0), from_565(c1)),
            [0, 0, 0],
        ]
    };
    let mut err = 0i32;
    for (i, p) in pixels.iter().enumerate() {
        let idx = ((table >> (2 * i)) & 3) as usize;
        err += sqr_rgb([p[0], p[1], p[2]], colors[idx]);
    }
    err
}

pub fn encode_bc2(pixels: [[u8; 4]; 16], out: &mut [u8]) {
    out[..16].fill(0);
    for i in 0..16 {
        let a = pixels[i][3] >> 4;
        let byte = i / 2;
        if i % 2 == 0 {
            out[byte] = a;
        } else {
            out[byte] |= a << 4;
        }
    }
    out[8..16].copy_from_slice(&encode_bc1_bytes(pixels));
}

pub fn encode_bc3(pixels: [[u8; 4]; 16], out: &mut [u8]) {
    // Full BC4-grade alpha search (uniques/LS/neighborhood) instead of the
    // min/max-only fast path: quality-monotone (same dual seed, candidates
    // only added under strict `<`), and CryTIF-style UI content is
    // alpha-gradient-heavy.
    out[..8].copy_from_slice(&encode_alpha_block_unsigned(super::alpha::alpha_channel(&pixels)));
    out[8..16].copy_from_slice(&encode_bc1_bytes(pixels));
}

#[cfg(test)]
pub(super) fn pack_bc1(pixels: [[u8; 4]; 16], max_c: [u8; 3], min_c: [u8; 3]) -> [u8; 8] {
    let mut max565 = to_565(max_c);
    let min565 = to_565(min_c);
    if max565 == min565 {
        max565 = max565.saturating_add(1);
    }
    let (c0, c1, table) = if max565 > min565 {
        let colors = [
            from_565(max565),
            from_565(min565),
            lerp_rgb::<2, 1>(from_565(max565), from_565(min565)),
            lerp_rgb::<1, 2>(from_565(max565), from_565(min565)),
        ];
        (max565, min565, pack_indices_2bit(&pixels, &colors, false))
    } else if max565 < min565 {
        // Stored c0 > c1 decodes as 4-color; fit against the decode palette.
        let colors = [
            from_565(min565),
            from_565(max565),
            lerp_rgb::<2, 1>(from_565(min565), from_565(max565)),
            lerp_rgb::<1, 2>(from_565(min565), from_565(max565)),
        ];
        (min565, max565, pack_indices_2bit(&pixels, &colors, false))
    } else {
        let colors = [
            from_565(min565),
            from_565(max565),
            lerp_rgb::<1, 1>(from_565(min565), from_565(max565)),
            [0, 0, 0],
        ];
        (min565, max565, pack_indices_2bit(&pixels, &colors, true))
    };
    let mut out = [0u8; 8];
    out[0..2].copy_from_slice(&c0.to_le_bytes());
    out[2..4].copy_from_slice(&c1.to_le_bytes());
    out[4..8].copy_from_slice(&table.to_le_bytes());
    out
}
