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
    let (bits6, err6) = encode_bc7_mode6_inner(&pixels);
    let mut a_lo = 255u8;
    let mut a_hi = 0u8;
    for p in &pixels {
        a_lo = a_lo.min(p[3]);
        a_hi = a_hi.max(p[3]);
    }
    // Alpha-flat blocks: mode 6's 4-bit shared index dominates; skip mode 5/4.
    let mut best_bits = bits6;
    let mut best_err = err6;
    if err6 > 0 && a_hi - a_lo > 2 {
        if let Some((bits5, err5)) = try_bc7_mode5(&pixels, 0) {
            if err5 < best_err {
                best_err = err5;
                best_bits = bits5;
            }
        }
        // Mode 4 (isb 0): 3-bit alpha indices + 6-bit alpha endpoints trade
        // color precision (5-bit) for finer alpha — wins when the alpha
        // gradient needs more steps than mode 5's 2-bit set offers.
        if best_err > 0 {
            if let Some((bits4, err4)) = try_bc7_mode4(&pixels) {
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
                if let Some((bits5, err5)) = try_bc7_mode5(&rotated, rot) {
                    if err5 < best_err {
                        best_err = err5;
                        best_bits = bits5;
                    }
                }
            }
        }
    }
    out[..16].copy_from_slice(&best_bits);
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

pub(super) fn try_bc7_mode5(pixels: &[[u8; 4]; 16], rotation: u8) -> Option<([u8; 16], i64)> {
    // --- alpha half: 8-bit endpoints, 4-entry palette, own index set ---
    let alpha: [u8; 16] = pixels.map(|p| p[3]);
    let mut a0 = 255u8;
    let mut a1 = 0u8;
    for &a in &alpha {
        a0 = a0.min(a);
        a1 = a1.max(a);
    }
    let (a_ep0, a_ep1, a_idx, a_err) = fit_alpha_mode5(&alpha, a1, a0);

    // --- color half: 7-bit endpoints, RGB-only search (BC1-shaped) ---
    let (mut best_c, mut c_err) = {
        let (mx, mn) = extrema_opaque(pixels);
        fit_color_mode5(pixels, mx, mn)
    };
    {
        let (mx, mn) = channel_minmax_rgb(pixels);
        let cand = fit_color_mode5(pixels, mx, mn);
        if cand.1 < c_err {
            c_err = cand.1;
            best_c = cand.0;
        }
        if let Some((pa, pb)) = pca_extremes_rgb(pixels) {
            let cand = fit_color_mode5(pixels, pa, pb);
            if cand.1 < c_err {
                c_err = cand.1;
                best_c = cand.0;
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
pub(super) fn try_bc7_mode4(pixels: &[[u8; 4]; 16]) -> Option<([u8; 16], i64)> {
    // Color half (5-bit endpoints, W2): same seed set as mode 5.
    let (mut best_c, mut c_err) = {
        let (mx, mn) = extrema_opaque(pixels);
        fit_color_mode4(pixels, mx, mn)
    };
    {
        let (mx, mn) = channel_minmax_rgb(pixels);
        let cand = fit_color_mode4(pixels, mx, mn);
        if cand.1 < c_err {
            c_err = cand.1;
            best_c = cand.0;
        }
        if let Some((pa, pb)) = pca_extremes_rgb(pixels) {
            let cand = fit_color_mode4(pixels, pa, pb);
            if cand.1 < c_err {
                c_err = cand.1;
                best_c = cand.0;
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

pub(super) fn encode_bc7_mode6_inner(pixels: &[[u8; 4]; 16]) -> ([u8; 16], i64) {
    let mut best_bits = [0u8; 16];
    let mut best_err = i64::MAX;
    let mut best_seed = extrema_rgba(pixels);
    let mut have = false;

    // Keep the winning candidate itself, not just its endpoints: the refine
    // below starts from this rather than re-deriving it.
    let mut best_fit: Option<Mode6Fit> = None;
    let (mut seeds, mut n_seeds) = bc7_mode6_seeds_base(pixels);
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
        bc7_mode6_seeds_extra(pixels, &mut seeds, &mut n_seeds);
        if n_seeds == tried {
            break;
        }
    }
    // Skip LS on near-solid blocks — seed endpoints already win.
    let do_ls = rgba_span_sum(pixels) > 8;
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
    (best_bits, best_err)
}

pub(super) fn rgba_span_sum(pixels: &[[u8; 4]; 16]) -> i32 {
    let (mx, mn) = channel_minmax_rgba(pixels);
    (0..4).map(|c| (mx[c] - mn[c]) as i32).sum()
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
/// Error below which the expensive mode-6 seeds are skipped.
///
/// SSE over 16 pixels x 4 channels. Calibrated on the BC7 corpus, and the
/// choice is measured rather than picked: the error the two cheap seeds leave
/// behind distributes so that this gate skips the extras on **83.5%** of blocks
/// while the corpus reports **zero** cases worse (mean -0.0001 dB, worst
/// -0.003). A tighter gate of 64 fires on only 29.5% of blocks and measured
/// neutral; 1024 would fire on 96.4% but was not quality-tested.
const SEED_EXTRA_ERR_GATE: i64 = 256;

pub(super) fn bc7_mode6_seeds_base(pixels: &[[u8; 4]; 16]) -> ([Seed; 5], usize) {
    let mut seeds = [([0u8; 4], [0u8; 4]); 5];
    let mut n = 0usize;
    push_seed(&mut seeds, &mut n, extrema_rgba(pixels));
    push_seed(&mut seeds, &mut n, channel_minmax_rgba(pixels));
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
    seeds: &mut [Seed; 5],
    n: &mut usize,
) {
    let span = rgba_span_sum(pixels);
    if span <= 16 {
        return;
    }
    let (mx, mn) = extrema_rgba(pixels);
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
pub(super) fn palette_mode6(c0: [u8; 4], c1: [u8; 4]) -> [[u8; 4]; 16] {
    // `(64 - w) * c0 + w * c1 + 32` is exactly `c0 * 64 + 32 + w * (c1 - c0)`,
    // and only the right operand varies with the weight. Base and delta are
    // constant for the whole palette, so this is one multiply per component
    // instead of two — 64 rather than 128 per call, and this is called once per
    // seed candidate plus each refine, up to seven times a block.
    //
    // The same identity the decoder uses in `bc7_bd3`; the value is always
    // >= 32 so the shift is exact either way. Byte-identical by construction,
    // and the encode determinism tests gate it.
    let base = [
        c0[0] as i32 * 64 + 32,
        c0[1] as i32 * 64 + 32,
        c0[2] as i32 * 64 + 32,
        c0[3] as i32 * 64 + 32,
    ];
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
    let pal = palette_mode6(unquantize_7p(q0, p0), unquantize_7p(q1, p1));
    // SSE is accumulated during the index fit — the recon after an endpoint
    // swap + index inversion is identical (W6M symmetry), so no re-walk.
    let (mut indices, err) = fit_indices_mode6(pixels, &pal);
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
    let npal = palette_mode6(unquantize_7p(nq0, np0), unquantize_7p(nq1, np1));
    let (mut nidx, nerr) = fit_indices_mode6(pixels, &npal);
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

pub(super) fn ls_endpoints_mode6(pixels: &[[u8; 4]; 16], indices: &[u8; 16]) -> Option<([u8; 4], [u8; 4])> {
    const W: [f32; 16] = [
        0.0, 4.0 / 64.0, 9.0 / 64.0, 13.0 / 64.0, 17.0 / 64.0, 21.0 / 64.0, 26.0 / 64.0,
        30.0 / 64.0, 34.0 / 64.0, 38.0 / 64.0, 43.0 / 64.0, 47.0 / 64.0, 51.0 / 64.0, 55.0 / 64.0,
        60.0 / 64.0, 1.0,
    ];
    // pixel ≈ (1-w)*e0 + w*e1
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
        e0[c] = x0.round().clamp(0.0, 255.0) as u8;
        e1[c] = x1.round().clamp(0.0, 255.0) as u8;
    }
    Some((e0, e1))
}

pub(super) fn pack_bc7_mode6(q0: [u8; 4], p0: u8, q1: [u8; 4], p1: u8, indices: [u8; 16]) -> [u8; 16] {
    let mut bw = BitWriter::default();
    for _ in 0..6 {
        bw.write_bits(0, 1);
    }
    bw.write_bits(1, 1);
    for c in 0..4 {
        bw.write_bits(q0[c] as u32, 7);
        bw.write_bits(q1[c] as u32, 7);
    }
    bw.write_bits(p0 as u32, 1);
    bw.write_bits(p1 as u32, 1);
    bw.write_bits(indices[0] as u32, 3);
    for i in 1..16 {
        bw.write_bits(indices[i] as u32, 4);
    }
    bw.into_array()
}

pub(super) fn quantize_7p(c: [u8; 4]) -> ([u8; 4], u8) {
    let mut best_p = 0u8;
    let mut best_q = [0u8; 4];
    let mut best_err = i32::MAX;
    for p in 0..2u8 {
        let mut q = [0u8; 4];
        let mut err = 0i32;
        for i in 0..4 {
            q[i] = (c[i] >> 1).min(127);
            let mut best_qi = q[i];
            let mut best_e = i32::MAX;
            for cand in q[i].saturating_sub(1)..=q[i].saturating_add(1).min(127) {
                let recon = unquantize_7p_chan(cand, p);
                let e = (recon as i32 - c[i] as i32).pow(2);
                if e < best_e {
                    best_e = e;
                    best_qi = cand;
                }
            }
            q[i] = best_qi;
            err += best_e;
        }
        if err < best_err {
            best_err = err;
            best_p = p;
            best_q = q;
        }
    }
    (best_q, best_p)
}

pub(super) fn unquantize_7p(q: [u8; 4], p: u8) -> [u8; 4] {
    [
        unquantize_7p_chan(q[0], p),
        unquantize_7p_chan(q[1], p),
        unquantize_7p_chan(q[2], p),
        unquantize_7p_chan(q[3], p),
    ]
}

pub(super) fn unquantize_7p_chan(q: u8, p: u8) -> u8 {
    let v = ((q as u32) << 1) | (p as u32);
    v as u8
}

pub(super) fn extrema_opaque(pixels: &[[u8; 4]; 16]) -> ([u8; 3], [u8; 3]) {
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
