//! BC7 modes 6, 5 and 4 (mode 1 lives in [`super::m1`]).
//!
//! Mode 6 is the always-tried baseline: one subset, coupled colour+alpha
//! through a single index set. Modes 5 and 4 decouple colour and alpha indices
//! and win on content whose alpha gradient disagrees with its colour gradient
//! (UI, decals) — they are trialed only when the block has varying alpha.

use super::*;

// ---------------------------------------------------------------------------
// BC7 mode 6
// ---------------------------------------------------------------------------

/// BC7 encode: mode 6 always; mode 5 (decoupled color/alpha indices) is
/// trialed on alpha-varying blocks and wins by the same RGBA SSE metric.
/// Mode 6 couples color+alpha through ONE index set, which craters on
/// blocks whose alpha gradient disagrees with the color gradient (UI/decal
/// content); mode 5 carries separate 2-bit index sets per channel group.
pub fn encode_bc7_mode6(pixels: [[u8; 4]; 16], out: &mut [u8]) {
    encode_bc7_mode6_scored(pixels, out);
}

/// [`encode_bc7_mode6`] returning the block's SSE, which it computes anyway.
///
/// The RDO driver needs that error and used to recompute it with
/// `bc7_block_sse` — 52 instructions a block to re-derive a value this function
/// already had in hand.
pub(crate) fn encode_bc7_mode6_scored(pixels: [[u8; 4]; 16], out: &mut [u8]) -> i64 {
    // `a_lo`/`a_hi` come back from the inner encoder, which computes the whole
    // per-channel min/max for its seeds. Walking the block again here for the
    // alpha pair alone was the same sixteen pixels a second time.
    let (bits6, err6, a_lo, a_hi) = encode_bc7_mode6_inner(&pixels);
    // Alpha-flat blocks: mode 6's 4-bit shared index dominates; skip mode 5/4.
    let mut best_bits = bits6;
    let mut best_err = err6;
    if err6 > 0 && a_hi - a_lo > 2 {
        // One seed set for both modes: at rotation 0 they see the same pixels.
        let seeds = ColorSeeds::new(&pixels);
        if let Some((bits5, err5)) = try_bc7_mode5(&pixels, 0, &seeds, best_err) {
            if err5 < best_err {
                best_err = err5;
                best_bits = bits5;
            }
        }
        // Mode 4 (isb 0): 3-bit alpha indices + 6-bit alpha endpoints trade
        // color precision (5-bit) for finer alpha — wins when the alpha
        // gradient needs more steps than mode 5's 2-bit set offers.
        if best_err > 0 {
            if let Some((bits4, err4)) = try_bc7_mode4(&pixels, &seeds, best_err) {
                if err4 < best_err {
                    best_err = err4;
                    best_bits = bits4;
                }
            }
        }
    }
    // Mode 1 (2-subset, opaque): partition-edge blocks where any single
    // subset fits poorly. Gate: fully-opaque alpha AND residual in (4, 1024]
    // — the T-sweep showed every mode-1 win lives there (smooth/UI blocks
    // taken to near-zero; textured blocks with big residuals never win
    // against mode 6's 4-bit indices, so they skip the 64-shape ranking
    // entirely). Inside, the ranking's 2-cluster bound must still PROMISE
    // a >=2x reduction before a full fit runs.
    if best_err > bc7_m1_min_err().max(4) && best_err <= 1024 && a_lo == 255 {
        if let Some((bits1, err1)) = m1::try_bc7_mode1(&pixels, best_err) {
            if err1 < best_err {
                best_err = err1;
                best_bits = bits1;
            }
        }
    }
    // Rotations 1..3 move a COLOR channel into the decoupled-index slot —
    // for blocks where R/G/B is the gradient that disagrees with the rest.
    // Trial a rotation only when that channel's span exceeds the span of
    // the remaining three (otherwise mode 6 / rot 0 already fit).
    if best_err > 0 {
        let (mx, mn) = channel_minmax_rgba(&pixels);
        let spans: [i32; 4] = [
            (mx[0] - mn[0]) as i32,
            (mx[1] - mn[1]) as i32,
            (mx[2] - mn[2]) as i32,
            (mx[3] - mn[3]) as i32,
        ];
        for rot in 1u8..=3 {
            let c = rot as usize - 1; // rot 1↔R, 2↔G, 3↔B
            let rest = spans[3].max(spans[(c + 1) % 3]).max(spans[(c + 2) % 3]);
            if spans[c] > 2 && spans[c] > rest {
                let mut rotated = pixels;
                for p in rotated.iter_mut() {
                    p.swap(c, 3);
                }
                // Rotated pixels are different pixels, so this set is its own.
                let rseeds = ColorSeeds::new(&rotated);
                if let Some((bits5, err5)) = try_bc7_mode5(&rotated, rot, &rseeds, best_err) {
                    if err5 < best_err {
                        best_err = err5;
                        best_bits = bits5;
                    }
                }
            }
        }
    }
    out[..16].copy_from_slice(&best_bits);
    best_err
}

/// 2-bit BC7 interpolation weights (symmetric: W2[3-i] == 64 - W2[i]).
pub(super) const W2: [u32; 4] = [0, 21, 43, 64];
/// 3-bit BC7 interpolation weights (symmetric: W3[7-i] == 64 - W3[i]).
pub(super) const W3: [u32; 8] = [0, 9, 18, 27, 37, 46, 55, 64];

/// 7-bit color endpoint dequant (no p-bit): v' = (v<<1) | (v>>6).
#[inline]
pub(super) fn unquant7(v: u8) -> u8 {
    (v << 1) | (v >> 6)
}

/// The three colour endpoint seeds modes 4 and 5 both search.
///
/// All three are pure functions of `pixels`, and modes 4 and 5 run on the *same*
/// pixels at rotation 0 — so before 0.3.33 every block computed `extrema_opaque`,
/// `channel_minmax_rgb` and `pca_extremes_rgb` **twice**, measured at 2.00 calls
/// each per block. Computing them once and passing them in halves that, and PCA
/// is the expensive one.
///
/// Rotations get their own set, because rotated pixels are different pixels.
pub(super) struct ColorSeeds {
    extrema: ([u8; 3], [u8; 3]),
    cminmax: ([u8; 3], [u8; 3]),
    pca: Option<([u8; 3], [u8; 3])>,
}

impl ColorSeeds {
    pub(super) fn new(pixels: &[[u8; 4]; 16]) -> Self {
        Self {
            extrema: extrema_opaque(pixels),
            cminmax: channel_minmax_rgb(pixels),
            pca: pca_extremes_rgb(pixels),
        }
    }
}

/// `err_limit` is the incumbent error. Both halves of a BC7 mode contribute a
/// **non-negative** squared error to the total, so as soon as *either* half
/// alone reaches the incumbent the mode cannot win, and abandoning it is exactly
/// equivalent to finishing it and losing the `<` comparison at the call site.
///
/// This is worth a great deal because these modes almost always lose. Measured
/// on alpha-structured content, per block: mode 4 loses **96%** of the time and
/// is already provably beaten after its colour search **69%** of the time; mode
/// 5 loses **95%** and is already beaten after its alpha search **89%** of the
/// time. Each mode searches the two halves in the opposite order, so each gets
/// to skip the other half.
pub(super) fn try_bc7_mode5(
    pixels: &[[u8; 4]; 16],
    rotation: u8,
    seeds: &ColorSeeds,
    err_limit: i64,
) -> Option<([u8; 16], i64)> {
    // --- alpha half: 8-bit endpoints, 4-entry palette, own index set ---
    let alpha: [u8; 16] = pixels.map(|p| p[3]);
    let mut a0 = 255u8;
    let mut a1 = 0u8;
    for &a in &alpha {
        a0 = a0.min(a);
        a1 = a1.max(a);
    }
    let (a_ep0, a_ep1, a_idx, a_err) = fit_alpha_mode5(&alpha, a1, a0);
    // Colour error is non-negative, so this mode can no longer win: skip the
    // whole colour search. Fires on 89% of blocks.
    if a_err as i64 >= err_limit {
        return None;
    }

    // --- color half: 7-bit endpoints, RGB-only search (BC1-shaped) ---
    let (mut best_c, mut c_err) = fit_color_mode5(pixels, seeds.extrema.0, seeds.extrema.1);
    {
        // A seed identical to one already fitted cannot change anything: same
        // endpoints give the same palette, the same indices and the same error,
        // and the guard below is a strict `<`. Measured at 1.04 such fits per
        // block, so skipping them is free and exact.
        if seeds.cminmax != seeds.extrema {
            let cand = fit_color_mode5(pixels, seeds.cminmax.0, seeds.cminmax.1);
            if cand.1 < c_err {
                c_err = cand.1;
                best_c = cand.0;
            }
        }
        if let Some((pa, pb)) = seeds.pca {
            if (pa, pb) != seeds.extrema && (pa, pb) != seeds.cminmax {
                let cand = fit_color_mode5(pixels, pa, pb);
                if cand.1 < c_err {
                    c_err = cand.1;
                    best_c = cand.0;
                }
            }
        }
        // One LS refit round from the winner's indices.
        if let Some((e0, e1)) = ls_endpoints_mode5(pixels, &best_c.2) {
            let cand = fit_color_mode5(pixels, e0, e1);
            if cand.1 < c_err {
                c_err = cand.1;
                best_c = cand.0;
            }
        }
    }
    let (c_ep0, c_ep1, c_idx) = best_c;

    let err = c_err as i64 + a_err as i64;
    Some((
        pack_bc7_mode5(rotation, c_ep0, c_ep1, a_ep0, a_ep1, &c_idx, &a_idx),
        err,
    ))
}

/// Mode 4, isb 0: 5-bit color endpoints + 2-bit color indices, 6-bit alpha
/// endpoints + 3-bit alpha indices, rotation 0.
pub(super) fn try_bc7_mode4(
    pixels: &[[u8; 4]; 16],
    seeds: &ColorSeeds,
    err_limit: i64,
) -> Option<([u8; 16], i64)> {
    // Color half (5-bit endpoints, W2): same seed set as mode 5.
    let (mut best_c, mut c_err) = fit_color_mode4(pixels, seeds.extrema.0, seeds.extrema.1);
    {
        // A seed identical to one already fitted cannot change anything: same
        // endpoints give the same palette, the same indices and the same error,
        // and the guard below is a strict `<`. Measured at 1.04 such fits per
        // block, so skipping them is free and exact.
        if seeds.cminmax != seeds.extrema {
            let cand = fit_color_mode4(pixels, seeds.cminmax.0, seeds.cminmax.1);
            if cand.1 < c_err {
                c_err = cand.1;
                best_c = cand.0;
            }
        }
        if let Some((pa, pb)) = seeds.pca {
            if (pa, pb) != seeds.extrema && (pa, pb) != seeds.cminmax {
                let cand = fit_color_mode4(pixels, pa, pb);
                if cand.1 < c_err {
                    c_err = cand.1;
                    best_c = cand.0;
                }
            }
        }
        if let Some((e0, e1)) = ls_endpoints_mode5(pixels, &best_c.2) {
            let cand = fit_color_mode4(pixels, e0, e1);
            if cand.1 < c_err {
                c_err = cand.1;
                best_c = cand.0;
            }
        }
    }
    let (c_ep0, c_ep1, c_idx) = best_c;
    // Alpha error is non-negative, so this mode can no longer win: skip the
    // whole alpha search, seed and neighbourhood both. Fires on 69% of blocks.
    if c_err as i64 >= err_limit {
        return None;
    }

    // Alpha half: 6-bit endpoints, 8-entry W3 palette, ±2 lattice window.
    let alpha: [u8; 16] = pixels.map(|p| p[3]);
    let mut lo = 255u8;
    let mut hi = 0u8;
    for &a in &alpha {
        lo = lo.min(a);
        hi = hi.max(a);
    }
    let (mut a_ep0, mut a_ep1, mut a_idx, mut a_err) = score_alpha_mode4(&alpha, hi >> 2, lo >> 2);
    if a_err > 0 {
        #[cfg(all(feature = "simd", target_arch = "x86_64"))]
        let vectorised = simd::has_avx2();
        #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
        let vectorised = false;
        #[cfg(all(feature = "simd", target_arch = "x86_64"))]
        if vectorised {
            let (q0, q1, e) =
                simd::alpha_nbhd_avx2::<8>(&alpha, hi >> 2, lo >> 2, 63, a_err);
            if e < a_err {
                (a_ep0, a_ep1, a_idx, a_err) = score_alpha_mode4(&alpha, q0, q1);
            }
        }
        if !vectorised {
        for d0 in -2i32..=2 {
            for d1 in -2i32..=2 {
                if d0 == 0 && d1 == 0 {
                    continue;
                }
                let q0 = ((hi >> 2) as i32 + d0).clamp(0, 63) as u8;
                let q1 = ((lo >> 2) as i32 + d1).clamp(0, 63) as u8;
                let cand = score_alpha_mode4(&alpha, q0, q1);
                if cand.3 < a_err {
                    (a_ep0, a_ep1, a_idx, a_err) = cand;
                }
            }
        }
        }
    }

    let err = c_err as i64 + a_err as i64;
    Some((pack_bc7_mode4(c_ep0, c_ep1, a_ep0, a_ep1, &c_idx, &a_idx), err))
}

#[inline]
pub(super) fn unquant5(v: u8) -> u8 {
    (v << 3) | (v >> 2)
}

#[inline]
pub(super) fn unquant6(v: u8) -> u8 {
    (v << 2) | (v >> 4)
}

#[allow(clippy::type_complexity)]
/// Nearest of four RGB palette entries for sixteen pixels.
///
/// This scan is character-for-character [`super::bc1::bc1_fit_4color_scalar`] —
/// same `sqr_rgb`, same strict `<`, same lowest-index tie-break — so modes 4 and
/// 5 reuse that kernel and the 200 000-case oracle that already guards it,
/// rather than growing a third copy. The only difference is the output form:
/// BC1 wants a packed 2-bit table, these want `[u8; 16]`.
///
/// A doubling probe puts the four mode-4/5 fits at **~24% of BC7 encode**.
#[inline]
fn fit_indices_rgb4(pixels: &[[u8; 4]; 16], pal: &[[u8; 3]; 4]) -> ([u8; 16], i32) {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        // `i32::MAX` disables the abort: the largest possible error is
        // 16 * 3 * 255^2 = 3_121_200, so `Some` is guaranteed here.
        if let Some((table, err)) = simd::bc1_fit_4color_avx2(pixels, pal, i32::MAX) {
            return (
                core::array::from_fn(|i| ((table >> (2 * i)) & 3) as u8),
                err,
            );
        }
    }
    fit_indices_rgb4_scalar(pixels, pal)
}

/// The scalar arm of [`fit_indices_rgb4`], kept OUT of line.
///
/// Sixteen pixels against four palette entries unrolls to 600 instructions,
/// not one of them a vector op — and because the dispatch is `#[inline]`, every
/// one of those went into EVERY call site. The mode-4/5 fits are about 24% of
/// BC7 encode, so this was the fallback bloating the hot BC7 bodies on machines
/// that never run it.
#[cold]
#[inline(never)]
fn fit_indices_rgb4_scalar(pixels: &[[u8; 4]; 16], pal: &[[u8; 3]; 4]) -> ([u8; 16], i32) {
    let mut idx = [0u8; 16];
    let mut err = 0i32;
    for (i, p) in pixels.iter().enumerate() {
        let mut bi = 0u8;
        let mut be = i32::MAX;
        for (j, pc) in pal.iter().enumerate() {
            let e = sqr_rgb([p[0], p[1], p[2]], *pc);
            if e < be {
                be = e;
                bi = j as u8;
            }
        }
        idx[i] = bi;
        err += be;
    }
    (idx, err)
}

/// Nearest of eight single-channel palette entries for sixteen samples.
///
/// The same scan BC4 and BC5 alpha run, so it reuses [`super::simd::alpha_fit_avx2`]
/// and its oracle. That kernel compares `|p - s|` where this compares
/// `(p - s)^2`; squaring is monotonic on non-negative values, so the argmin and
/// the lowest-index tie-break are identical.
#[inline]
fn fit_indices_alpha8(alpha: &[u8; 16], pal: &[u8; 8]) -> ([u8; 16], i32) {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        return simd::alpha_fit_avx2(pal, alpha);
    }
    fit_indices_alpha8_scalar(alpha, pal)
}

/// Kept OUT of line: the fallback arm of an AVX2 dispatch, never executed
/// on a machine with AVX2, but inlined at the dispatch it lands in the hot
/// body and interleaves with the code that does run.
#[cold]
#[inline(never)]
fn fit_indices_alpha8_scalar(alpha: &[u8; 16], pal: &[u8; 8]) -> ([u8; 16], i32) {
    let mut idx = [0u8; 16];
    let mut err = 0i32;
    for (i, &a) in alpha.iter().enumerate() {
        let mut bi = 0u8;
        let mut be = i32::MAX;
        for (j, &p) in pal.iter().enumerate() {
            let d = (p as i32 - a as i32).pow(2);
            if d < be {
                be = d;
                bi = j as u8;
            }
        }
        idx[i] = bi;
        err += be;
    }
    (idx, err)
}

/// Nearest of *four* single-channel entries, via the eight-entry kernel.
///
/// Entries 4..8 are filled with entry 0. Under a strict `<` tie-break a later
/// duplicate can never win, so those four candidates can never be selected and
/// can never change the result — scanning eight is exactly scanning four. That
/// buys mode 5's alpha the same vector kernel without a second one.
#[inline]
fn fit_indices_alpha4(alpha: &[u8; 16], pal: &[u8; 4]) -> ([u8; 16], i32) {
    let pal8 = [pal[0], pal[1], pal[2], pal[3], pal[0], pal[0], pal[0], pal[0]];
    fit_indices_alpha8(alpha, &pal8)
}

pub(super) fn fit_color_mode4(
    pixels: &[[u8; 4]; 16],
    e0: [u8; 3],
    e1: [u8; 3],
) -> (([u8; 3], [u8; 3], [u8; 16]), i32) {
    let mut q0 = [0u8; 3];
    let mut q1 = [0u8; 3];
    for c in 0..3 {
        q0[c] = e0[c] >> 3;
        q1[c] = e1[c] >> 3;
    }
    let c0 = [unquant5(q0[0]), unquant5(q0[1]), unquant5(q0[2])];
    let c1 = [unquant5(q1[0]), unquant5(q1[1]), unquant5(q1[2])];
    let mut pal = [[0u8; 3]; 4];
    for (k, &w) in W2.iter().enumerate() {
        for c in 0..3 {
            pal[k][c] = (((64 - w) * c0[c] as u32 + w * c1[c] as u32 + 32) / 64) as u8;
        }
    }
    let (mut idx, err) = fit_indices_rgb4(pixels, &pal);
    if idx[0] >= 2 {
        std::mem::swap(&mut q0, &mut q1);
        for v in idx.iter_mut() {
            *v = 3 - *v;
        }
    }
    ((q0, q1, idx), err)
}

/// Score one 6-bit alpha endpoint pair with the 8-entry W3 palette; anchor
/// constraint applied (W3 symmetry keeps recon identical under swap+invert).
pub(super) fn score_alpha_mode4(alpha: &[u8; 16], q0: u8, q1: u8) -> (u8, u8, [u8; 16], i32) {
    let c0 = unquant6(q0);
    let c1 = unquant6(q1);
    let mut pal = [0u8; 8];
    for (k, &w) in W3.iter().enumerate() {
        pal[k] = (((64 - w) * c0 as u32 + w * c1 as u32 + 32) / 64) as u8;
    }
    let (mut idx, err) = fit_indices_alpha8(alpha, &pal);
    let (mut r0, mut r1) = (q0, q1);
    if idx[0] >= 4 {
        std::mem::swap(&mut r0, &mut r1);
        for v in idx.iter_mut() {
            *v = 7 - *v;
        }
    }
    (r0, r1, idx, err)
}

pub(super) fn pack_bc7_mode4(
    c0: [u8; 3],
    c1: [u8; 3],
    a0: u8,
    a1: u8,
    c_idx: &[u8; 16],
    a_idx: &[u8; 16],
) -> [u8; 16] {
    let mut bw = BitWriter::default();
    // Mode 4: four 0 bits then a 1.
    for _ in 0..4 {
        bw.write_bits(0, 1);
    }
    bw.write_bits(1, 1);
    bw.write_bits(0, 2); // rotation 0
    bw.write_bits(0, 1); // isb 0: 2-bit color, 3-bit alpha
    for c in 0..3 {
        bw.write_bits(c0[c] as u32, 5);
        bw.write_bits(c1[c] as u32, 5);
    }
    bw.write_bits(a0 as u32, 6);
    bw.write_bits(a1 as u32, 6);
    bw.write_bits(c_idx[0] as u32, 1);
    for &v in &c_idx[1..] {
        bw.write_bits(v as u32, 2);
    }
    bw.write_bits(a_idx[0] as u32, 2);
    for &v in &a_idx[1..] {
        bw.write_bits(v as u32, 3);
    }
    bw.into_array()
}

/// Fit the mode-5 color half for one endpoint seed; returns
/// ((q0, q1, indices), rgb_sse) with the anchor constraint applied.
#[allow(clippy::type_complexity)]
pub(super) fn fit_color_mode5(
    pixels: &[[u8; 4]; 16],
    e0: [u8; 3],
    e1: [u8; 3],
) -> (([u8; 3], [u8; 3], [u8; 16]), i32) {
    let mut q0 = [0u8; 3];
    let mut q1 = [0u8; 3];
    for c in 0..3 {
        q0[c] = e0[c] >> 1;
        q1[c] = e1[c] >> 1;
    }
    let pal = palette_mode5_color(q0, q1);
    let (mut idx, err) = fit_indices_rgb4(pixels, &pal);
    // Anchor: idx[0] MSB must be 0 (W2 symmetry keeps recon identical).
    if idx[0] >= 2 {
        std::mem::swap(&mut q0, &mut q1);
        for v in idx.iter_mut() {
            *v = 3 - *v;
        }
    }
    ((q0, q1, idx), err)
}

pub(super) fn palette_mode5_color(q0: [u8; 3], q1: [u8; 3]) -> [[u8; 3]; 4] {
    let c0 = [unquant7(q0[0]), unquant7(q0[1]), unquant7(q0[2])];
    let c1 = [unquant7(q1[0]), unquant7(q1[1]), unquant7(q1[2])];
    let mut pal = [[0u8; 3]; 4];
    for (k, &w) in W2.iter().enumerate() {
        for c in 0..3 {
            pal[k][c] = (((64 - w) * c0[c] as u32 + w * c1[c] as u32 + 32) / 64) as u8;
        }
    }
    pal
}

/// LS endpoints for the 2-bit color indices (same normal equations as the
/// mode-6 refine, W2 weights, RGB only).
pub(super) fn ls_endpoints_mode5(pixels: &[[u8; 4]; 16], indices: &[u8; 16]) -> Option<([u8; 3], [u8; 3])> {
    const WF: [f32; 4] = [0.0, 21.0 / 64.0, 43.0 / 64.0, 1.0];
    let mut a00 = 0f32;
    let mut a01 = 0f32;
    let mut a11 = 0f32;
    let mut b0 = [0f32; 3];
    let mut b1 = [0f32; 3];
    for (i, p) in pixels.iter().enumerate() {
        let w = WF[indices[i] as usize];
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
        e0[c] = ((a11 * b0[c] - a01 * b1[c]) / det).round().clamp(0.0, 255.0) as u8;
        e1[c] = ((a00 * b1[c] - a01 * b0[c]) / det).round().clamp(0.0, 255.0) as u8;
    }
    Some((e0, e1))
}

/// Mode-5 alpha half: exact 8-bit endpoints, 4-entry palette, ±2 endpoint
/// window (2-bit indices quantize hard; the window recovers the lattice).
pub(super) fn fit_alpha_mode5(alpha: &[u8; 16], hi: u8, lo: u8) -> (u8, u8, [u8; 16], i32) {
    let (mut e0, mut e1, mut idx, mut err) = score_alpha_mode5(alpha, hi, lo);
    if err > 0 && hi != lo {
        // The whole 24-offset search in one crossing instead of 24 — see
        // `simd::alpha_nbhd_avx2`. It reports only a strictly better candidate,
        // in the same order, so the winner is the one the loop below would pick;
        // re-scoring it scalar-ly recovers the indices and the anchor exactly.
        #[cfg(all(feature = "simd", target_arch = "x86_64"))]
        if simd::has_avx2() {
            let (c0, c1, e) = simd::alpha_nbhd_avx2::<4>(alpha, hi, lo, 255, err);
            if e < err {
                (e0, e1, idx, err) = score_alpha_mode5(alpha, c0, c1);
            }
            return (e0, e1, idx, err);
        }
        for d0 in -2i32..=2 {
            for d1 in -2i32..=2 {
                if d0 == 0 && d1 == 0 {
                    continue;
                }
                let c0 = (hi as i32 + d0).clamp(0, 255) as u8;
                let c1 = (lo as i32 + d1).clamp(0, 255) as u8;
                let cand = score_alpha_mode5(alpha, c0, c1);
                if cand.3 < err {
                    (e0, e1, idx, err) = cand;
                }
            }
        }
    }
    (e0, e1, idx, err)
}

pub(super) fn score_alpha_mode5(alpha: &[u8; 16], c0: u8, c1: u8) -> (u8, u8, [u8; 16], i32) {
    let mut pal = [0u8; 4];
    for (k, &w) in W2.iter().enumerate() {
        pal[k] = (((64 - w) * c0 as u32 + w * c1 as u32 + 32) / 64) as u8;
    }
    let (mut idx, err) = fit_indices_alpha4(alpha, &pal);
    let (mut r0, mut r1) = (c0, c1);
    if idx[0] >= 2 {
        std::mem::swap(&mut r0, &mut r1);
        for v in idx.iter_mut() {
            *v = 3 - *v;
        }
    }
    (r0, r1, idx, err)
}

pub(super) fn pack_bc7_mode5(
    rotation: u8,
    c0: [u8; 3],
    c1: [u8; 3],
    a0: u8,
    a1: u8,
    c_idx: &[u8; 16],
    a_idx: &[u8; 16],
) -> [u8; 16] {
    let mut bw = BitWriter::default();
    // Mode 5: five 0 bits then a 1.
    for _ in 0..5 {
        bw.write_bits(0, 1);
    }
    bw.write_bits(1, 1);
    bw.write_bits(rotation as u32, 2);
    for c in 0..3 {
        bw.write_bits(c0[c] as u32, 7);
        bw.write_bits(c1[c] as u32, 7);
    }
    bw.write_bits(a0 as u32, 8);
    bw.write_bits(a1 as u32, 8);
    bw.write_bits(c_idx[0] as u32, 1);
    for &v in &c_idx[1..] {
        bw.write_bits(v as u32, 2);
    }
    bw.write_bits(a_idx[0] as u32, 1);
    for &v in &a_idx[1..] {
        bw.write_bits(v as u32, 2);
    }
    bw.into_array()
}

/// BC7 mode 6: single subset, RGBA 7-bit endpoints + P-bits, 4-bit indices.

pub(super) fn encode_bc7_mode6_inner(pixels: &[[u8; 4]; 16]) -> ([u8; 16], i64, u8, u8) {
    let mut best_bits = [0u8; 16];
    let mut best_err = i64::MAX;
    // Walk the block ONCE per statistic. `extrema_rgba` was computed here and
    // again inside the seed builder; `channel_minmax_rgba` was computed for
    // seed 1 and again inside `rgba_span_sum`, which is just a sum over the same
    // min/max. Counted: 2.245 extrema, 3.245 channel-minmax and 1.245 span calls
    // per block, for statistics the block needs once each.
    let ex = extrema_rgba(pixels);
    let cm = channel_minmax_rgba(pixels);
    let span: i32 = (0..4).map(|c| (cm.0[c] - cm.1[c]) as i32).sum();

    let mut best_seed = ex;
    let mut have = false;

    // Keep the winning candidate itself, not just its endpoints: the refine
    // below starts from this rather than re-deriving it.
    let mut best_fit: Option<Mode6Fit> = None;
    let (mut seeds, mut n_seeds) = bc7_mode6_seeds_base(ex, cm);
    let mut tried = 0usize;
    loop {
        for &(ep0, ep1) in &seeds[tried..n_seeds] {
            let f = mode6_base(pixels, ep0, ep1);
            if f.err < best_err {
                best_err = f.err;
                let (bits, _) = f.pack();
                best_bits = bits;
                best_seed = (ep0, ep1);
                best_fit = Some(f);
                have = true;
            }
        }
        tried = n_seeds;
        // The extras cost ~11% of encode and win 6.4% of blocks. Spend them
        // only where the cheap seeds left error worth chasing.
        if best_err <= SEED_EXTRA_ERR_GATE {
            break;
        }
        bc7_mode6_seeds_extra(pixels, ex, span, &mut seeds, &mut n_seeds);
        if n_seeds == tried {
            break;
        }
    }
    // Skip LS on near-solid blocks — seed endpoints already win.
    let do_ls = span > 8;
    if do_ls {
        if let Some(base) = best_fit {
            let (bits, err) = mode6_refine(pixels, base).pack();
            if err < best_err {
            }
            if err <= best_err {
                best_bits = bits;
                best_err = err;
                have = true;
            }
        }
    }
    if !have {
        if let Some((bits, err)) = try_bc7_mode6(pixels, best_seed.0, best_seed.1, true) {
            best_bits = bits;
            best_err = err;
        }
    }
    (best_bits, best_err, cm.1[3], cm.0[3])
}


pub(super) type Seed = ([u8; 4], [u8; 4]);

/// Push with dedup: a duplicate trial can never win under strict `<`, so
/// skipping it is byte-identical and saves a whole index-fit pass.
#[inline]
pub(super) fn push_seed(seeds: &mut [Seed; 5], n: &mut usize, s: Seed) {
    for seed in seeds[..*n].iter() {
        if *seed == s {
            return;
        }
    }
    seeds[*n] = s;
    *n += 1;
}

/// The two cheap seeds, always worth trying: they win 93.6% of blocks between
/// them (74.3% + 19.3%, counted over 21 847 blocks).
// Two further gates of this shape were built and REFUTED, and the numbers are
// here so they are not rebuilt:
//
//   * Gating the least-squares refine on residual error. Quality-free only at
//     SSE <= 4, where it fires on **0.2%** of blocks. At 16 it makes 7 corpus
//     cases worse, at 64 thirteen, at 256 twenty-one (worst -0.78 dB). The
//     refine earns its fit almost everywhere — unlike the seed extras, it is
//     not waste.
//   * Gating seed 1 on seed 0's error. Quality-free only at SSE <= 16, firing
//     on **2.2%** of blocks; at 32 it costs 6 corpus cases.
//
// Together they moved 3.168 -> 3.147 fits per block, 0.7%, for two more tuned
// constants. Not kept.
//
// That puts the search at 3.147 against a structural floor of 3.0 — two base
// seeds plus one refine. **Index fits are finished at the quality-free level**;
// the remaining 5% is protected by the corpus gate, not by inattention.

/// Error below which the expensive mode-6 seeds are skipped.
///
/// SSE over 16 pixels x 4 channels. Calibrated on the BC7 corpus, and the
/// choice is measured rather than picked: the error the two cheap seeds leave
/// behind distributes so that this gate skips the extras on **83.5%** of blocks
/// while the corpus reports **zero** cases worse (mean -0.0001 dB, worst
/// -0.003). A tighter gate of 64 fires on only 29.5% of blocks and measured
/// neutral; 1024 would fire on 96.4% but was not quality-tested.
const SEED_EXTRA_ERR_GATE: i64 = 256;

pub(super) fn bc7_mode6_seeds_base(ex: Seed, cm: Seed) -> ([Seed; 5], usize) {
    let mut seeds = [([0u8; 4], [0u8; 4]); 5];
    let mut n = 0usize;
    push_seed(&mut seeds, &mut n, ex);
    push_seed(&mut seeds, &mut n, cm);
    (seeds, n)
}

/// The expensive extras — mean-split pair, and a farthest-pair scan that is
/// O(16^2) — appended to an existing seed set.
///
/// These win only **6.4%** of blocks, and dropping them outright measured
/// -11.2% encode time for -0.0028 dB mean (worst case -0.049 dB) across the BC7
/// corpus. That is a trade, and this encoder's mandate is faster *and* better,
/// so instead they are gated on the error the cheap seeds left behind: a block
/// the first two already fit well cannot be rescued by a third seed, and a
/// block they fit badly is exactly where the extras earn their cost.
pub(super) fn bc7_mode6_seeds_extra(
    pixels: &[[u8; 4]; 16],
    ex: Seed,
    span: i32,
    seeds: &mut [Seed; 5],
    n: &mut usize,
) {
    if span <= 16 {
        return;
    }
    let (mx, mn) = ex;
    let mut mean = [0u32; 4];
    for p in pixels {
        for c in 0..4 {
            mean[c] += p[c] as u32;
        }
    }
    let mean = mean.map(|v| (v / 16) as u8);
    push_seed(seeds, n, (mx, mean));
    push_seed(seeds, n, (mean, mn));

    // Farthest-pair only on busy blocks (O(16^2)).
    if span > 48 {
        let mut best_d = -1i32;
        let mut pa = pixels[0];
        let mut pb = pixels[0];
        for i in 0..16 {
            for j in (i + 1)..16 {
                let mut d = 0i32;
                for c in 0..4 {
                    let t = pixels[i][c] as i32 - pixels[j][c] as i32;
                    d += t * t;
                }
                if d > best_d {
                    best_d = d;
                    pa = pixels[i];
                    pb = pixels[j];
                }
            }
        }
        push_seed(seeds, n, (pa, pb));
    }
}

pub(super) fn channel_minmax_rgba(pixels: &[[u8; 4]; 16]) -> ([u8; 4], [u8; 4]) {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        return simd::channel_minmax_avx2(pixels);
    }
    channel_minmax_rgba_scalar(pixels)
}

/// The scalar arm of [`channel_minmax_rgba`], kept OUT of line.
///
/// Sixteen pixels by four channels by min-and-max, inlined at the dispatch:
/// 582 instructions, not one of them a vector op, on a path whose vector arm
/// is a call to a 23-instruction kernel.
#[cold]
#[inline(never)]
fn channel_minmax_rgba_scalar(pixels: &[[u8; 4]; 16]) -> ([u8; 4], [u8; 4]) {
    let mut mn = [255u8; 4];
    let mut mx = [0u8; 4];
    for p in pixels {
        for c in 0..4 {
            mn[c] = mn[c].min(p[c]);
            mx[c] = mx[c].max(p[c]);
        }
    }
    (mx, mn)
}

/// Mode-6 interpolation weights. Symmetric: `W[15-i] == 64 - W[i]`, so an
/// endpoint swap + index inversion reconstructs identical pixels.
pub(super) const W6M: [u32; 16] = [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64];

/// Reconstructed 16-entry palette for one (c0, c1) pair — computed ONCE per
/// trial instead of re-lerping per pixel per candidate index.
#[inline]
/// `c0 * 64 + 32` per channel — the half of the palette that depends only on the
/// FIRST endpoint.
///
/// The RDO head-reuse path builds two palettes per donor that share `c0`, so it
/// computes this once and calls [`palette_mode6_from_base`] twice.
pub(super) fn palette_mode6_base(c0: [u8; 4]) -> [i32; 4] {
    [
        c0[0] as i32 * 64 + 32,
        c0[1] as i32 * 64 + 32,
        c0[2] as i32 * 64 + 32,
        c0[3] as i32 * 64 + 32,
    ]
}

/// [`palette_mode6`] with the base already in hand.
pub(super) fn palette_mode6_from_base(base: [i32; 4], c0: [u8; 4], c1: [u8; 4]) -> [[u8; 4]; 16] {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        return simd::palette_mode6_avx2(base, c0, c1);
    }
    let delta = [
        c1[0] as i32 - c0[0] as i32,
        c1[1] as i32 - c0[1] as i32,
        c1[2] as i32 - c0[2] as i32,
        c1[3] as i32 - c0[3] as i32,
    ];
    let mut pal = [[0u8; 4]; 16];
    for (k, &w) in W6M.iter().enumerate() {
        let w = w as i32;
        for c in 0..4 {
            pal[k][c] = ((base[c] + w * delta[c]) >> 6) as u8;
        }
    }
    pal
}

#[cfg(test)]
pub(super) fn palette_mode6(c0: [u8; 4], c1: [u8; 4]) -> [[u8; 4]; 16] {
    palette_mode6_from_base(palette_mode6_base(c0), c0, c1)
}

/// Nearest palette entry (strict `<`: lowest index wins ties) + its SSE.
#[inline]
pub(super) fn best_index_pal(px: &[u8; 4], pal: &[[u8; 4]; 16]) -> (u8, i32) {
    let mut best_i = 0u8;
    let mut best_e = i32::MAX;
    for (k, p) in pal.iter().enumerate() {
        let mut e = 0i32;
        for c in 0..4 {
            let d = p[c] as i32 - px[c] as i32;
            e += d * d;
        }
        if e < best_e {
            best_e = e;
            best_i = k as u8;
        }
    }
    (best_i, best_e)
}

// NOTE: a projection-window index fit (a t64→index LUT, ±2 candidates around
// the pixel's projection onto the endpoint axis) lived here and was retired
// when the AVX2 kernel landed: the SIMD twin is both EXACT and faster, so the
// approximation had nothing left to buy. See `simd::fit_indices_mode6_avx2`.

/// Index-fit a whole block against one palette; returns (indices, total SSE).
///
/// Dispatcher: exact AVX2 kernel when available (`simd` feature, runtime
/// detected), scalar exhaustive otherwise — SAME selection semantics on
/// every CPU, so payloads are machine-independent. (An earlier projection
/// heuristic is superseded: the SIMD kernel is exact AND faster.)
#[inline]
/// Build the mode-6 palette for `(c0, c1)` and fit the block's indices to it.
///
/// Every caller that built a palette used it for exactly one index fit and
/// nothing else, so the two are one operation. Fusing them keeps the palette in
/// its i16 form instead of packing it to bytes and immediately widening it back
/// — see [`simd::palette_fit_mode6_avx2`].
pub(super) fn palette_and_fit_mode6(
    pixels: &[[u8; 4]; 16],
    base: [i32; 4],
    c0: [u8; 4],
    c1: [u8; 4],
) -> ([u8; 16], i64) {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        return simd::palette_fit_mode6_avx2(pixels, base, c0, c1);
    }
    palette_and_fit_mode6_scalar(pixels, base, c0, c1)
}

/// Kept OUT of line: the fallback arm of an AVX2 dispatch, never executed on a
/// machine with AVX2, but inlined at the dispatch it drags both
/// `palette_mode6_from_base` and `fit_indices_mode6` into the hot body with it.
#[cold]
#[inline(never)]
fn palette_and_fit_mode6_scalar(
    pixels: &[[u8; 4]; 16],
    base: [i32; 4],
    c0: [u8; 4],
    c1: [u8; 4],
) -> ([u8; 16], i64) {
    let pal = palette_mode6_from_base(base, c0, c1);
    fit_indices_mode6(pixels, &pal)
}

pub(super) fn fit_indices_mode6(pixels: &[[u8; 4]; 16], pal: &[[u8; 4]; 16]) -> ([u8; 16], i64) {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        return simd::fit_indices_mode6_avx2(pixels, pal);
    }
    fit_indices_mode6_exhaustive(pixels, pal)
}

/// Exhaustive twin (oracle + fallback for near-degenerate palettes).
#[inline]
pub(super) fn fit_indices_mode6_exhaustive(pixels: &[[u8; 4]; 16], pal: &[[u8; 4]; 16]) -> ([u8; 16], i64) {
    let mut indices = [0u8; 16];
    let mut err = 0i64;
    for (i, px) in pixels.iter().enumerate() {
        let (idx, e) = best_index_pal(px, pal);
        indices[i] = idx;
        err += e as i64;
    }
    (indices, err)
}

/// One evaluated mode-6 candidate: quantized endpoints, fitted indices, SSE.
///
/// Carrying this out of the seed loop is what lets the refine start from the
/// winner instead of recomputing it — see [`encode_bc7_mode6_inner`].
#[derive(Clone, Copy)]
pub(super) struct Mode6Fit {
    q0: [u8; 4],
    p0: u8,
    q1: [u8; 4],
    p1: u8,
    indices: [u8; 16],
    err: i64,
}

impl Mode6Fit {
    #[inline]
    fn pack(&self) -> ([u8; 16], i64) {
        (
            pack_bc7_mode6(self.q0, self.p0, self.q1, self.p1, self.indices),
            self.err,
        )
    }
}

/// Quantize a seed, build its palette, fit indices, canonicalise the anchor.
pub(super) fn mode6_base(pixels: &[[u8; 4]; 16], ep0: [u8; 4], ep1: [u8; 4]) -> Mode6Fit {
    let (mut q0, mut p0) = quantize_7p(ep0);
    let (mut q1, mut p1) = quantize_7p(ep1);
    // SSE is accumulated during the index fit — the recon after an endpoint
    // swap + index inversion is identical (W6M symmetry), so no re-walk.
    let (u0, u1) = (unquantize_7p(q0, p0), unquantize_7p(q1, p1));
    let (mut indices, err) = palette_and_fit_mode6(pixels, palette_mode6_base(u0), u0, u1);
    if indices[0] > 7 {
        std::mem::swap(&mut q0, &mut q1);
        std::mem::swap(&mut p0, &mut p1);
        for idx in indices.iter_mut() {
            *idx = 15 - *idx;
        }
    }
    Mode6Fit {
        q0,
        p0,
        q1,
        p1,
        indices,
        err,
    }
}

/// Least-squares refine an ALREADY-FITTED candidate.
///
/// This used to be the tail of a call that re-derived `base` from the seed
/// endpoints first — quantize twice, rebuild the palette, and run
/// `fit_indices_mode6` again — for a seed the search loop had just evaluated.
/// That redundant fit was **one of every 5.14** the encoder performed, counted
/// directly. Starting from the winner is byte-identical and simply skips it.
pub(super) fn mode6_refine(pixels: &[[u8; 4]; 16], base: Mode6Fit) -> Mode6Fit {
    let Some((r0, r1)) = ls_endpoints_mode6(pixels, &base.indices) else {
        return base;
    };
    let (nq0, np0) = quantize_7p(r0);
    let (nq1, np1) = quantize_7p(r1);
    let (n0, n1) = (unquantize_7p(nq0, np0), unquantize_7p(nq1, np1));
    let (mut nidx, nerr) = palette_and_fit_mode6(pixels, palette_mode6_base(n0), n0, n1);
    let (q0, p0, q1, p1) = if nidx[0] > 7 {
        for idx in nidx.iter_mut() {
            *idx = 15 - *idx;
        }
        (nq1, np1, nq0, np0)
    } else {
        (nq0, np0, nq1, np1)
    };
    Mode6Fit {
        q0,
        p0,
        q1,
        p1,
        indices: nidx,
        err: nerr,
    }
}

pub(super) fn try_bc7_mode6(
    pixels: &[[u8; 4]; 16],
    ep0: [u8; 4],
    ep1: [u8; 4],
    refine: bool,
) -> Option<([u8; 16], i64)> {
    let f = mode6_base(pixels, ep0, ep1);
    let f = if refine { mode6_refine(pixels, f) } else { f };
    Some(f.pack())
}

/// # A refuted optimisation, recorded so it is not retried
///
/// Splitting the index-only half (`a00`, `a01`, `a11`, `det`) out and caching it
/// per window entry — the trick that won **+13.4%** for BC1's
/// `refit_endpoints_for_table` — **does not transfer here**, measured twice:
///
/// | form | instructions per block |
/// |---|---|
/// | as-is | 9.23 calls x 272 = **2,511** |
/// | split, caching `(1-w, w)` too | 9.23 x 244 + 451 = 2,703 |
/// | split, four scalars only | 9.23 x 255 + 362 = 2,716 |
///
/// The reason is the shape of this loop rather than the idea: the index-only
/// terms are three mult-adds of eleven, and **both** halves still pay the table
/// lookup and the `1 - w`. Removing three of eleven saved 17 instructions a
/// call while the cached half cost 362 once. BC1's version wins because its
/// pixel loop is three channels rather than four, so the fixed part is a much
/// larger share.
///
/// Timing agreed it was not there: +2.0%, z = +1.41, 8 ties of 16.
/// # A second refuted optimisation, for a different reason than the first
///
/// Vectorising the `b0`/`b1` accumulation with the same kernel that won **-51.7%**
/// on BC1's `refit_with_ls` measured **272 -> 605 instructions here**.
///
/// The difference is not the loop, it is what the caller already has. BC1's
/// version receives a `TableLs` that **already carries** `(1-w, w)` per pixel, so
/// handing it to the kernel is free. This function computes `w` and `u` inline
/// and uses each for *both* the `a`-accumulation and the `b`-accumulation, so
/// vectorising only the second half forces a 128-byte `uw` array into existence
/// that nothing needed before — and that costs more than the vectorisation saves.
///
/// Caching the index-only half was refuted separately above. Both routes are
/// now closed with numbers.
/// Least-squares endpoints for a fixed mode-6 index assignment.
///
/// Called 9.14 times a block by the RDO donor loop on identical pixels, so the
/// hot entry point is [`ls_endpoints_mode6_pxv`], which takes them
/// pre-converted. This wrapper is for the one-per-block baseline call.
pub(super) fn ls_endpoints_mode6(
    pixels: &[[u8; 4]; 16],
    indices: &[u8; 16],
) -> Option<([u8; 4], [u8; 4])> {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        return ls_endpoints_mode6_pxv(&simd::ls_pixels_mode6(pixels), indices);
    }
    ls_endpoints_mode6_scalar(pixels, indices)
}

/// [`ls_endpoints_mode6`] with the pixels already in `[rgba, rgba]` float form.
#[cfg(all(feature = "simd", target_arch = "x86_64"))]
pub(super) fn ls_endpoints_mode6_pxv(
    pxv: &[[f32; 8]; 16],
    indices: &[u8; 16],
) -> Option<([u8; 4], [u8; 4])> {
    if !simd::has_avx2() {
        unreachable!("ls_endpoints_mode6_pxv requires AVX2");
    }
    let (a, b0, b1) = simd::ls_accum_mode6(pxv, indices);
    let (a00, a01, a11) = (a[0], a[1], a[2]);
    let det = a00 * a11 - a01 * a01;
    if det.abs() < 1e-4 {
        return None;
    }
    // The same solve BC1 uses: six divisions become two `divps`, bit-identical
    // because IEEE defines division lane-wise. Rounding stays scalar — Rust's
    // `round` is half-away-from-zero and no SSE mode matches it.
    // The solve returns the endpoints already clamped and rounded — see
    // `bc1_ls_solve`, which folds `round_clamp_u8` in lane-wise.
    Some(simd::bc1_ls_solve(b0, b1, a00, a01, a11, det))
}

/// Kept OUT of line: this is the fallback arm of an AVX2 dispatch, so on
/// any machine that has AVX2 it is never executed — but inlined at the
/// dispatch it lands in the hot body and interleaves with the code that
/// does run.
#[cold]
#[inline(never)]
pub(super) fn ls_endpoints_mode6_scalar(
    pixels: &[[u8; 4]; 16],
    indices: &[u8; 16],
) -> Option<([u8; 4], [u8; 4])> {
    const W: [f32; 16] = [
        0.0, 4.0 / 64.0, 9.0 / 64.0, 13.0 / 64.0, 17.0 / 64.0, 21.0 / 64.0, 26.0 / 64.0,
        30.0 / 64.0, 34.0 / 64.0, 38.0 / 64.0, 43.0 / 64.0, 47.0 / 64.0, 51.0 / 64.0, 55.0 / 64.0,
        60.0 / 64.0, 1.0,
    ];
    // pixel ~= (1-w)*e0 + w*e1
    let mut a00 = 0.0f32;
    let mut a01 = 0.0f32;
    let mut a11 = 0.0f32;
    let mut b0 = [0.0f32; 4];
    let mut b1 = [0.0f32; 4];
    for i in 0..16 {
        let w = W[indices[i] as usize];
        let u = 1.0 - w;
        a00 += u * u;
        a01 += u * w;
        a11 += w * w;
        for c in 0..4 {
            let x = pixels[i][c] as f32;
            b0[c] += u * x;
            b1[c] += w * x;
        }
    }
    let det = a00 * a11 - a01 * a01;
    if det.abs() < 1e-4 {
        return None;
    }
    let mut e0 = [0u8; 4];
    let mut e1 = [0u8; 4];
    for c in 0..4 {
        let x0 = (a11 * b0[c] - a01 * b1[c]) / det;
        let x1 = (a00 * b1[c] - a01 * b0[c]) / det;
        e0[c] = super::round_clamp_u8(x0);
        e1[c] = super::round_clamp_u8(x1);
    }
    Some((e0, e1))
}

pub(super) fn pack_bc7_mode6(q0: [u8; 4], p0: u8, q1: [u8; 4], p1: u8, indices: [u8; 16]) -> [u8; 16] {
    // `BitWriter` is a little-endian 128-bit accumulator — `low |= v << pos`,
    // `high` above 64, and `into_array` emits low then high as LE bytes. That is
    // exactly `u128::to_le_bytes`, and mode 6's layout is FIXED, so every shift
    // here is a compile-time constant instead of 33 calls through a running bit
    // cursor. Measured at 118 instructions a call, 19 calls a block.
    //
    // Layout: 7 mode bits (unary, so bit 6 set), then eight 7-bit endpoints
    // interleaved q0/q1 per channel, two p-bits, a 3-bit anchor index and
    // fifteen 4-bit indices. 7 + 56 + 2 + 3 + 60 = 128.
    let mut v: u128 = 1 << 6;
    let mut pos = 7u32;
    for c in 0..4 {
        v |= (q0[c] as u128) << pos;
        v |= (q1[c] as u128) << (pos + 7);
        pos += 14;
    }
    v |= (p0 as u128) << pos;
    v |= (p1 as u128) << (pos + 1);
    pos += 2;
    // Sixteen 4-bit indices packed by SWAR rather than fifteen shift-or pairs.
    //
    // `nib8` folds eight nibble-valued bytes into 32 bits: `x | (x >> 4)` pairs
    // adjacent bytes, then two more fold-and-mask steps halve the span twice.
    // With all sixteen packed as `sum idx[i] << 4i`, the block's layout is two
    // shifts of that one value: the anchor's four bits land at 65 and the rest
    // at 68, which is exactly what the loop produced — including the overlap at
    // bit 68, which both forms OR together.
    fn nib8(x: u64) -> u64 {
        let x = (x | (x >> 4)) & 0x00FF_00FF_00FF_00FF;
        let x = (x | (x >> 8)) & 0x0000_FFFF_0000_FFFF;
        (x | (x >> 16)) & 0x0000_0000_FFFF_FFFF
    }
    let lo = u64::from_le_bytes([
        indices[0], indices[1], indices[2], indices[3],
        indices[4], indices[5], indices[6], indices[7],
    ]);
    let hi = u64::from_le_bytes([
        indices[8], indices[9], indices[10], indices[11],
        indices[12], indices[13], indices[14], indices[15],
    ]);
    let packed = nib8(lo) | (nib8(hi) << 32);
    let _ = pos;
    v |= ((packed & 0xF) as u128) << 65;
    v |= ((packed >> 4) as u128) << 68;
    v.to_le_bytes()
}

/// Best 7-bit quantization of one channel for a given p-bit, and its squared
/// error — precomputed for all 512 inputs.
///
/// `unquantize_7p_chan(q, p)` is `(q << 1) | p`, so the inner search is a pure
/// function of `(channel_value, p_bit)` over 256 x 2 inputs. The direct form
/// re-derived one of those 512 answers **24 times per call** (2 p-bits x 4
/// channels x a 3-wide candidate window), and `quantize_7p` runs roughly six
/// times per block.
///
/// The table is built by a `const fn` running the *identical* search, so the
/// result is byte-identical by construction rather than by argument. 768 bytes
/// total, which is L1-resident forever.
const fn qtab_entry(c: u8, p: u8) -> (u8, u16) {
    let q0 = c >> 1; // c is u8, so this is already <= 127
    let lo = if q0 == 0 { 0 } else { q0 - 1 };
    let hi = if q0 >= 127 { 127 } else { q0 + 1 };
    let mut best_qi = q0;
    let mut best_e = i32::MAX;
    let mut cand = lo;
    while cand <= hi {
        let recon = ((cand as u32) << 1) | (p as u32);
        let d = recon as i32 - c as i32;
        let e = d * d;
        if e < best_e {
            best_e = e;
            best_qi = cand;
        }
        cand += 1;
    }
    (best_qi, best_e as u16)
}

const fn build_qtab() -> ([[u8; 256]; 2], [[u16; 256]; 2]) {
    let mut q = [[0u8; 256]; 2];
    let mut e = [[0u16; 256]; 2];
    let mut p = 0usize;
    while p < 2 {
        let mut c = 0usize;
        while c < 256 {
            let (qi, ei) = qtab_entry(c as u8, p as u8);
            q[p][c] = qi;
            e[p][c] = ei;
            c += 1;
        }
        p += 1;
    }
    (q, e)
}

static QTAB: ([[u8; 256]; 2], [[u16; 256]; 2]) = build_qtab();

pub(super) fn quantize_7p(c: [u8; 4]) -> ([u8; 4], u8) {
    // Decide the p-bit from the errors FIRST, then read the four quantized
    // values once. The previous form built `best_q` inside the loop, so a p=1
    // win meant reading all four values twice — sixteen table reads where twelve
    // suffice. `quantize_7p_best` in the RDO path already worked this way.
    //
    // Same tie-break: p=0 was evaluated first and p=1 needed a strict `<`.
    let (qt, et) = (&QTAB.0, &QTAB.1);
    let (e0, e1) = (&et[0], &et[1]);
    let s0 = e0[c[0] as usize] as i32
        + e0[c[1] as usize] as i32
        + e0[c[2] as usize] as i32
        + e0[c[3] as usize] as i32;
    let s1 = e1[c[0] as usize] as i32
        + e1[c[1] as usize] as i32
        + e1[c[2] as usize] as i32
        + e1[c[3] as usize] as i32;
    let p = usize::from(s1 < s0);
    let q = &qt[p];
    (
        [
            q[c[0] as usize],
            q[c[1] as usize],
            q[c[2] as usize],
            q[c[3] as usize],
        ],
        p as u8,
    )
}

#[cfg(test)]
mod qtab_tests {

    /// The vectorised palette must equal the scalar build for **every** entry.
    ///
    /// The i16 range argument says the sum stays in 32..=16_352 and every result
    /// in 0..=255, so the saturating pack never clamps. This checks it at the
    /// extremes that would break it — endpoints at 0 and 255 in both orders,
    /// which drive `delta` to both bounds.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    #[test]
    fn palette_mode6_vector_matches_scalar() {
        if !super::simd::has_avx2() {
            return;
        }
        let mut state = 0x2b7e_1516_28ae_d2a6u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..60_000u32 {
            let (c0, c1) = match case {
                0 => ([0u8; 4], [0u8; 4]),
                1 => ([255u8; 4], [255u8; 4]),
                2 => ([0u8; 4], [255u8; 4]),
                3 => ([255u8; 4], [0u8; 4]),
                _ => {
                    let (a, b) = (next(), next());
                    (
                        [a as u8, (a >> 8) as u8, (a >> 16) as u8, (a >> 24) as u8],
                        [b as u8, (b >> 8) as u8, (b >> 16) as u8, (b >> 24) as u8],
                    )
                }
            };
            let base = super::palette_mode6_base(c0);
            let got = super::simd::palette_mode6_avx2(base, c0, c1);
            let mut want = [[0u8; 4]; 16];
            for (k, &w) in super::W6M.iter().enumerate() {
                for c in 0..4 {
                    want[k][c] = ((base[c] + w as i32 * (c1[c] as i32 - c0[c] as i32)) >> 6) as u8;
                }
            }
            assert_eq!(got, want, "case {case} c0={c0:?} c1={c1:?}");
        }
    }


    /// The vectorised mode-6 least-squares must reproduce the scalar loop
    /// **bitwise**, including the table-driven normal-equation terms.
    ///
    /// The exactness argument says every `AW6` entry is a multiple of `1/4096`
    /// bounded by 1, so both the products and every partial sum are exact in
    /// f32. This checks it rather than trusting it, over the full index range
    /// and including the degenerate all-one-index tables that drive `det` to
    /// zero.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    #[test]
    fn ls_endpoints_mode6_vector_matches_scalar_bitwise() {
        if !super::simd::has_avx2() {
            return;
        }
        let mut state = 0x51ac_1d0e_7717_3355u64;
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
            let mut idx = [0u8; 16];
            match case {
                // Degenerate: every pixel on one endpoint, so `det` collapses.
                0 => {}
                1 => idx = [15u8; 16],
                // Widest spread.
                2 => {
                    for (i, s) in idx.iter_mut().enumerate() {
                        *s = if i % 2 == 0 { 0 } else { 15 };
                    }
                }
                _ => {
                    for s in idx.iter_mut() {
                        *s = (next() >> 20) as u8 & 15;
                    }
                }
            }
            // Must be the channel-interleaved layout — this is the one
            // `ls_endpoints_mode6_pxv` consumes.
            let pxv = super::simd::ls_pixels_mode6(&px);
            let got = super::ls_endpoints_mode6_pxv(&pxv, &idx);
            let want = super::ls_endpoints_mode6_scalar(&px, &idx);
            assert_eq!(got, want, "case {case}");
        }
    }

    use super::{quantize_7p, unquantize_7p_chan};

    /// The table must reproduce the direct search for **every** input, not the
    /// values encoders happen to produce. 2^32 colours is too many, so this
    /// checks the per-channel primitive exhaustively (512 cases) and the
    /// four-channel selection over a wide sweep.
    #[test]
    fn qtab_matches_the_direct_search() {
        fn direct_chan(c: u8, p: u8) -> (u8, i32) {
            let q0 = (c >> 1).min(127);
            let mut best_qi = q0;
            let mut best_e = i32::MAX;
            for cand in q0.saturating_sub(1)..=q0.saturating_add(1).min(127) {
                let recon = unquantize_7p_chan(cand, p);
                let e = (recon as i32 - c as i32).pow(2);
                if e < best_e {
                    best_e = e;
                    best_qi = cand;
                }
            }
            (best_qi, best_e)
        }
        fn direct(c: [u8; 4]) -> ([u8; 4], u8) {
            let mut best_p = 0u8;
            let mut best_q = [0u8; 4];
            let mut best_err = i32::MAX;
            for p in 0..2u8 {
                let mut q = [0u8; 4];
                let mut err = 0i32;
                for i in 0..4 {
                    let (qi, e) = direct_chan(c[i], p);
                    q[i] = qi;
                    err += e;
                }
                if err < best_err {
                    best_err = err;
                    best_p = p;
                    best_q = q;
                }
            }
            (best_q, best_p)
        }

        // Per-channel primitive: all 512 inputs.
        for p in 0..2u8 {
            for c in 0..=255u8 {
                let want = direct_chan(c, p);
                let got = (super::QTAB.0[p as usize][c as usize],
                           super::QTAB.1[p as usize][c as usize] as i32);
                assert_eq!(got, want, "channel c={c} p={p}");
            }
        }
        // Whole-colour selection, including the p-bit tie-break.
        let mut state = 0x1234_5678_9abc_def0u64;
        for case in 0..200_000 {
            let c = if case < 256 {
                [case as u8; 4]
            } else {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let b = state.to_le_bytes();
                [b[0], b[1], b[2], b[3]]
            };
            assert_eq!(quantize_7p(c), direct(c), "colour {c:?}");
        }
    }
}

pub(super) fn unquantize_7p(q: [u8; 4], p: u8) -> [u8; 4] {
    // All four channels in one word. `(q << 1) | p` per byte becomes a shift, a
    // mask and an or: `& 0xFEFE_FEFE` clears each byte's bit 0, which is exactly
    // where the previous byte's bit 7 spills, and `p * 0x0101_0101` sets it.
    //
    // Identical to the per-channel form for every input, including `q > 127`:
    // there the scalar's `as u8` drops the same high bit the mask does.
    // Measured at 24 instructions a call, 72 calls a block.
    let v = u32::from_le_bytes(q);
    (((v << 1) & 0xFEFE_FEFE) | (p as u32 * 0x0101_0101)).to_le_bytes()
}

pub(super) fn unquantize_7p_chan(q: u8, p: u8) -> u8 {
    let v = ((q as u32) << 1) | (p as u32);
    v as u8
}

pub(super) fn extrema_opaque(pixels: &[[u8; 4]; 16]) -> ([u8; 3], [u8; 3]) {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        return simd::extrema_opaque_avx2(pixels);
    }
    extrema_opaque_scalar(pixels)
}

/// The scalar arm of [`extrema_opaque`], kept OUT of line.
///
/// The vector arm above is a call to a 51-instruction kernel, so every one of
/// this function's other 339 instructions — not one of them a vector op — was
/// the fallback, inlined at the dispatch and interleaved with the hot path on
/// machines that never execute it.
#[cold]
#[inline(never)]
fn extrema_opaque_scalar(pixels: &[[u8; 4]; 16]) -> ([u8; 3], [u8; 3]) {
    let mut min_l = i32::MAX;
    let mut max_l = i32::MIN;
    let mut min_rgb = [0u8; 3];
    let mut max_rgb = [0u8; 3];
    for p in pixels {
        let l = p[0] as i32 * 2 + p[1] as i32 * 3 + p[2] as i32;
        if l < min_l {
            min_l = l;
            min_rgb = [p[0], p[1], p[2]];
        }
        if l > max_l {
            max_l = l;
            max_rgb = [p[0], p[1], p[2]];
        }
    }
    (max_rgb, min_rgb)
}

pub(super) fn extrema_rgba(pixels: &[[u8; 4]; 16]) -> ([u8; 4], [u8; 4]) {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        return simd::extrema_rgba_avx2(pixels);
    }
    let mut min_l = i32::MAX;
    let mut max_l = i32::MIN;
    let mut min_p = [0u8; 4];
    let mut max_p = [255u8; 4];
    for p in pixels {
        let l = p[0] as i32 + p[1] as i32 + p[2] as i32 + p[3] as i32;
        if l < min_l {
            min_l = l;
            min_p = *p;
        }
        if l > max_l {
            max_l = l;
            max_p = *p;
        }
    }
    (max_p, min_p)
}
