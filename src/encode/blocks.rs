//! Per-block BCn encoders (pure Rust).
//!
//! Quality / speed profile (vs DirectXTex):
//! 1. BC4/5 — decoder-matched palettes; unique/axis dispatch; LS + neighborhood search-skip;
//!    signed path scores UNORM recon (scoreboard domain), not SNORM SSE
//! 2. BC1–3 — luminance seed; chroma second seed only when colorful
//! 3. BC7 mode 6 — variance-gated seed menu; LS refine only the winner
//! 4. Strip-parallel encode when block count ≥ 4096 (same threshold as BC7 decode)

use std::cell::Cell;

use crate::error::Error;

/// Encode effort vs speed. Default [`EncodeQuality::Quality`] is the corpus bake-off path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EncodeQuality {
    /// Full adaptive search (unique-pairs, LS, neighborhood with search-skip).
    #[default]
    Quality,
    /// Dual min/max + LS only — no unique-pairs / neighborhood (cook-fast).
    Fast,
}

thread_local! {
    static QUALITY: Cell<EncodeQuality> = const { Cell::new(EncodeQuality::Quality) };
}

pub(crate) fn with_quality<R>(q: EncodeQuality, f: impl FnOnce() -> R) -> R {
    QUALITY.with(|c| {
        let prev = c.replace(q);
        let out = f();
        c.set(prev);
        out
    })
}

#[inline]
fn quality_is_fast() -> bool {
    QUALITY.with(|c| c.get() == EncodeQuality::Fast)
}

/// Match BC7 decode: spawn strips only when work is large enough.
const ENCODE_PARALLEL_MIN_BLOCKS: usize = 4096;

pub fn encode_image(
    rgba: &[u8],
    width: u32,
    height: u32,
    block_bytes: usize,
    encode_block: impl Fn([[u8; 4]; 16], &mut [u8]) + Sync,
    out: &mut [u8],
) -> Result<(), Error> {
    let blocks_x = (width as usize + 3) / 4;
    let blocks_y = (height as usize + 3) / 4;
    let expected = blocks_x
        .checked_mul(blocks_y)
        .and_then(|n| n.checked_mul(block_bytes))
        .ok_or(Error::OutOfBounds)?;
    if out.len() < expected {
        return Err(Error::TruncatedData);
    }
    let w = width as usize;
    let h = height as usize;
    if rgba.len() < w * h * 4 {
        return Err(Error::TruncatedData);
    }
    debug_assert!(block_bytes <= 16);

    let nblocks = blocks_x.saturating_mul(blocks_y);
    if blocks_y >= 2 && nblocks >= ENCODE_PARALLEL_MIN_BLOCKS {
        encode_image_parallel(rgba, w, h, blocks_x, blocks_y, block_bytes, encode_block, out);
    } else {
        encode_image_serial(rgba, w, h, blocks_x, blocks_y, block_bytes, encode_block, out);
    }
    Ok(())
}

fn encode_image_serial(
    rgba: &[u8],
    w: usize,
    h: usize,
    blocks_x: usize,
    blocks_y: usize,
    block_bytes: usize,
    encode_block: impl Fn([[u8; 4]; 16], &mut [u8]),
    out: &mut [u8],
) {
    let mut scratch = [0u8; 16];
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let pixels = gather_block(rgba, w, h, bx, by);
            let slot = &mut scratch[..block_bytes];
            encode_block(pixels, slot);
            let oi = (by * blocks_x + bx) * block_bytes;
            out[oi..oi + block_bytes].copy_from_slice(slot);
        }
    }
}

fn encode_image_parallel(
    rgba: &[u8],
    w: usize,
    h: usize,
    blocks_x: usize,
    blocks_y: usize,
    block_bytes: usize,
    encode_block: impl Fn([[u8; 4]; 16], &mut [u8]) + Sync,
    out: &mut [u8],
) {
    let row_bytes = blocks_x * block_bytes;
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, blocks_y);

    let mut ranges: Vec<(usize, usize)> = Vec::with_capacity(workers);
    let base = blocks_y / workers;
    let extra = blocks_y % workers;
    let mut start = 0;
    for wi in 0..workers {
        let len = base + usize::from(wi < extra);
        ranges.push((start, start + len));
        start += len;
    }

    // Propagate encode quality into worker threads (thread-local is per-thread).
    let q = QUALITY.with(|c| c.get());

    std::thread::scope(|scope| {
        let mut rest = out;
        for &(by0, by1) in &ranges {
            let band_len = (by1 - by0) * row_bytes;
            let (band, tail) = rest.split_at_mut(band_len);
            rest = tail;
            let encode_block = &encode_block;
            scope.spawn(move || {
                with_quality(q, || {
                    let mut scratch = [0u8; 16];
                    for by in by0..by1 {
                        let local = by - by0;
                        for bx in 0..blocks_x {
                            let pixels = gather_block(rgba, w, h, bx, by);
                            let slot = &mut scratch[..block_bytes];
                            encode_block(pixels, slot);
                            let oi = local * row_bytes + bx * block_bytes;
                            band[oi..oi + block_bytes].copy_from_slice(slot);
                        }
                    }
                });
            });
        }
        debug_assert!(rest.is_empty());
    });
}

#[inline]
fn gather_block(rgba: &[u8], w: usize, h: usize, bx: usize, by: usize) -> [[u8; 4]; 16] {
    let x0 = bx * 4;
    let y0 = by * 4;
    if x0 + 4 <= w && y0 + 4 <= h {
        // Interior block: four contiguous 16-byte row copies, no per-pixel clamps.
        let mut pixels = [[0u8; 4]; 16];
        for row in 0..4 {
            let src = ((y0 + row) * w + x0) * 4;
            for col in 0..4 {
                let i = src + col * 4;
                pixels[row * 4 + col] = [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]];
            }
        }
        return pixels;
    }
    let mut pixels = [[0u8, 0, 0, 255]; 16];
    for row in 0..4 {
        for col in 0..4 {
            let x = x0 + col;
            let y = y0 + row;
            let sx = x.min(w.saturating_sub(1));
            let sy = y.min(h.saturating_sub(1));
            let i = (sy * w + sx) * 4;
            pixels[row * 4 + col] = [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]];
        }
    }
    pixels
}

/// Global span of one RGBA channel (for surface-level BC4/5 fast path).
pub fn channel_span(rgba: &[u8], width: u32, height: u32, channel: usize) -> u8 {
    let w = width as usize;
    let h = height as usize;
    let mut lo = 255u8;
    let mut hi = 0u8;
    for y in 0..h {
        let row = y * w * 4;
        for x in 0..w {
            let v = rgba[row + x * 4 + channel];
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    hi.saturating_sub(lo)
}

// ---------------------------------------------------------------------------
// BC1 / BC2 / BC3
// ---------------------------------------------------------------------------

pub fn encode_bc1(pixels: [[u8; 4]; 16], out: &mut [u8]) {
    out[..8].copy_from_slice(&encode_bc1_bytes(pixels));
}

fn encode_bc1_bytes(pixels: [[u8; 4]; 16]) -> [u8; 8] {
    let (max_c, min_c) = extrema_opaque(&pixels);
    let a = pack_bc1(pixels, max_c, min_c);
    if rgb_channel_span_sum(&pixels) < 24 {
        return a;
    }
    let mut best = a;
    let mut best_err = bc1_sse(&pixels, &a);
    if best_err == 0 {
        return best;
    }
    let (mx, mn) = channel_minmax_rgb(&pixels);
    if !(mx == max_c && mn == min_c) {
        consider_bc1(&pixels, mx, mn, &mut best, &mut best_err);
    }
    if quality_is_fast() || best_err == 0 {
        return best;
    }
    // PCA-axis extremes: luminance extrema mis-seed chroma-dominant blocks.
    if let Some((pa, pb)) = pca_extremes_rgb(&pixels) {
        consider_bc1(&pixels, pa, pb, &mut best, &mut best_err);
    }
    // Least-squares endpoint refine from the winner's indices, iterated while
    // the decode-matched SSE keeps falling (candidates only ever ADD, picked
    // by the same bc1_sse — per-block error is monotonically ≤ the old path).
    for _ in 0..4 {
        if best_err == 0 {
            break;
        }
        let Some((e0, e1)) = ls_endpoints_bc1(&pixels, &best) else {
            break;
        };
        let prev = best_err;
        consider_bc1(&pixels, e0, e1, &mut best, &mut best_err);
        if best_err >= prev {
            break;
        }
    }
    best
}

fn consider_bc1(
    pixels: &[[u8; 4]; 16],
    e0: [u8; 3],
    e1: [u8; 3],
    best: &mut [u8; 8],
    best_err: &mut i32,
) {
    let cand = pack_bc1(*pixels, e0, e1);
    let err = bc1_sse(pixels, &cand);
    if err < *best_err {
        *best = cand;
        *best_err = err;
    }
}

/// Principal-axis extremes: project RGB onto the covariance principal axis
/// (3 power iterations) and return the two extreme PIXELS along it.
fn pca_extremes_rgb(pixels: &[[u8; 4]; 16]) -> Option<([u8; 3], [u8; 3])> {
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
fn ls_endpoints_bc1(pixels: &[[u8; 4]; 16], block: &[u8; 8]) -> Option<([u8; 3], [u8; 3])> {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    if c0 <= c1 {
        return None; // 3-color + punch-through mode: skip LS.
    }
    let table = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
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
        e0[c] = x0.round().clamp(0.0, 255.0) as u8;
        e1[c] = x1.round().clamp(0.0, 255.0) as u8;
    }
    Some((e0, e1))
}

fn rgb_channel_span_sum(pixels: &[[u8; 4]; 16]) -> i32 {
    let mut mn = [255u8; 3];
    let mut mx = [0u8; 3];
    for p in pixels {
        for c in 0..3 {
            mn[c] = mn[c].min(p[c]);
            mx[c] = mx[c].max(p[c]);
        }
    }
    (mx[0] - mn[0]) as i32 + (mx[1] - mn[1]) as i32 + (mx[2] - mn[2]) as i32
}

fn channel_minmax_rgb(pixels: &[[u8; 4]; 16]) -> ([u8; 3], [u8; 3]) {
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

fn bc1_sse(pixels: &[[u8; 4]; 16], block: &[u8]) -> i32 {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let table = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    let colors = if c0 > c1 {
        [
            from_565(c0),
            from_565(c1),
            lerp_rgb(from_565(c0), from_565(c1), 2, 1),
            lerp_rgb(from_565(c0), from_565(c1), 1, 2),
        ]
    } else {
        [
            from_565(c0),
            from_565(c1),
            lerp_rgb(from_565(c0), from_565(c1), 1, 1),
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
    out[..8].copy_from_slice(&encode_alpha_block_fast_u(pixels.map(|p| p[3])));
    out[8..16].copy_from_slice(&encode_bc1_bytes(pixels));
}

fn pack_bc1(pixels: [[u8; 4]; 16], max_c: [u8; 3], min_c: [u8; 3]) -> [u8; 8] {
    let mut max565 = to_565(max_c);
    let min565 = to_565(min_c);
    if max565 == min565 {
        max565 = max565.saturating_add(1);
    }
    let (c0, c1, table) = if max565 > min565 {
        let colors = [
            from_565(max565),
            from_565(min565),
            lerp_rgb(from_565(max565), from_565(min565), 2, 1),
            lerp_rgb(from_565(max565), from_565(min565), 1, 2),
        ];
        (max565, min565, pack_indices_2bit(&pixels, &colors, false))
    } else {
        let colors = [
            from_565(min565),
            from_565(max565),
            lerp_rgb(from_565(min565), from_565(max565), 1, 1),
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

/// Min/max endpoints only (BC3 alpha / surface-flat BC4).
fn encode_alpha_block_fast_u(samples: [u8; 16]) -> [u8; 8] {
    let mut lo = 255u8;
    let mut hi = 0u8;
    for &s in &samples {
        lo = lo.min(s);
        hi = hi.max(s);
    }
    if lo == hi {
        return pack_alpha_indices(hi, lo, &alpha_palette4_u(hi, lo), &samples);
    }
    pack_alpha_indices(hi, lo, &alpha_palette6_u(hi, lo), &samples)
}

// ---------------------------------------------------------------------------
// BC4 / BC5
// ---------------------------------------------------------------------------

pub fn encode_bc4(pixels: [[u8; 4]; 16], signed: bool, out: &mut [u8]) {
    out[..8].copy_from_slice(&encode_alpha_block(pixels.map(|p| p[0]), signed));
}

pub fn encode_bc5(pixels: [[u8; 4]; 16], signed: bool, out: &mut [u8]) {
    out[..8].copy_from_slice(&encode_alpha_block(pixels.map(|p| p[0]), signed));
    out[8..16].copy_from_slice(&encode_alpha_block(pixels.map(|p| p[1]), signed));
}

/// Surface-flat BC4/5: dual min/max only.
pub fn encode_bc4_flat(pixels: [[u8; 4]; 16], signed: bool, out: &mut [u8]) {
    out[..8].copy_from_slice(&encode_alpha_block_flat(pixels.map(|p| p[0]), signed));
}

pub fn encode_bc5_flat(pixels: [[u8; 4]; 16], signed: bool, out: &mut [u8]) {
    out[..8].copy_from_slice(&encode_alpha_block_flat(pixels.map(|p| p[0]), signed));
    out[8..16].copy_from_slice(&encode_alpha_block_flat(pixels.map(|p| p[1]), signed));
}

fn encode_alpha_block(samples: [u8; 16], signed: bool) -> [u8; 8] {
    if signed {
        encode_alpha_block_signed(samples)
    } else {
        encode_alpha_block_unsigned(samples)
    }
}

fn encode_alpha_block_flat(samples: [u8; 16], signed: bool) -> [u8; 8] {
    if signed {
        let mut vals = [0i32; 16];
        let mut lo = 127i32;
        let mut hi = -127i32;
        for (i, &s) in samples.iter().enumerate() {
            let v = unorm_u8_to_snorm_i32(s);
            vals[i] = v;
            lo = lo.min(v);
            hi = hi.max(v);
        }
        if lo == hi {
            return pack_alpha_indices_s(hi, lo, &alpha_palette4_s(hi, lo), &samples);
        }
        let mut best = pack_alpha_indices_s(hi, lo, &alpha_palette6_s(hi, lo), &samples);
        let mut best_err = alpha_sse_s(&samples, &best);
        consider_alpha_s(lo, hi, &samples, &mut best, &mut best_err);
        best
    } else {
        encode_alpha_block_fast_u(samples)
    }
}

fn encode_alpha_block_unsigned(samples: [u8; 16]) -> [u8; 8] {
    let mut lo = 255u8;
    let mut hi = 0u8;
    for &s in &samples {
        lo = lo.min(s);
        hi = hi.max(s);
    }
    if lo == hi {
        return pack_alpha_indices(hi, lo, &alpha_palette4_u(hi, lo), &samples);
    }

    let mut best = pack_alpha_indices(hi, lo, &alpha_palette6_u(hi, lo), &samples);
    let mut best_err = alpha_sse_u(&samples, &best);
    consider_alpha_u(lo, hi, &samples, &mut best, &mut best_err);
    let span = hi as i32 - lo as i32;

    let fast = quality_is_fast();
    let (n_unique, uniq) = unique_values_u_capped(&samples, 5);
    // Unique-pairs: skip in Fast; skip when dual already near-perfect; full exhaust only ≤4 uniques
    // (5-uniques only when residual still high after dual).
    if !fast && n_unique <= 5 {
        if best_err > 4 && (n_unique <= 4 || best_err > 16) {
            consider_unique_pairs_u(&samples, &uniq[..n_unique], &mut best, &mut best_err);
        }
    } else if !fast && n_unique > 5 && span > 4 {
        let a_hi = (hi as i32 - 1) as u8;
        let a_lo = (lo as i32 + 1) as u8;
        consider_alpha_u(a_hi, a_lo, &samples, &mut best, &mut best_err);
        consider_alpha_u(a_lo, a_hi, &samples, &mut best, &mut best_err);
    }
    refine_alpha_u(&samples, n_unique, span, &mut best, &mut best_err);
    best
}

/// Collect up to `cap` uniques; returns `cap+1` if busier (no full sort).
fn unique_values_u_capped(samples: &[u8; 16], cap: usize) -> (usize, [u8; 16]) {
    let mut uniq = [0u8; 16];
    let mut n = 0usize;
    'outer: for &s in samples {
        for i in 0..n {
            if uniq[i] == s {
                continue 'outer;
            }
        }
        if n >= cap {
            return (cap + 1, uniq);
        }
        uniq[n] = s;
        n += 1;
    }
    (n, uniq)
}

fn consider_unique_pairs_u(
    samples: &[u8; 16],
    uniq: &[u8],
    best: &mut [u8; 8],
    best_err: &mut i32,
) {
    for i in 0..uniq.len() {
        for j in 0..uniq.len() {
            if i == j {
                continue;
            }
            consider_alpha_u(uniq[i], uniq[j], samples, best, best_err);
            if *best_err == 0 {
                return;
            }
        }
    }
}

/// LS, then content-adaptive local refine.
/// When `RUSTY_DDS_BC45_REFINE_HARVEST` is set, always runs neighborhood and logs
/// null vs post-search SSE (observe-only; decisions match the non-gated path).
fn refine_alpha_u(
    samples: &[u8; 16],
    n_unique: usize,
    span: i32,
    best: &mut [u8; 8],
    best_err: &mut i32,
) {
    if *best_err == 0 {
        return;
    }
    // LS skip: after good seeds / unique-pairs, float LS rarely moves endpoints.
    let full = super::harvest::full_refine();
    let fast = quality_is_fast();
    let do_ls = full || (!fast && n_unique > 2 && *best_err > 8) || (fast && *best_err > 8);
    if do_ls {
        if let Some((r0, r1)) = ls_alpha_endpoints_u(samples, best) {
            consider_alpha_u(r0, r1, samples, best, best_err);
            consider_alpha_u(r1, r0, samples, best, best_err);
            if *best_err == 0 {
                return;
            }
        }
    }
    // Fast: no neighborhood. Simple low-err: no neighborhood.
    if fast || (!full && n_unique <= 5 && *best_err <= 24) {
        return;
    }
    let d = if *best_err > 64 {
        2
    } else if *best_err > 8 {
        1
    } else {
        0
    };
    if d == 0 {
        return;
    }

    let harvesting = super::harvest::enabled();
    let score = neighborhood_score(*best_err, span);
    if !full && !harvesting && skip_neighborhood(*best_err, score, n_unique) {
        return;
    }

    let axis = n_unique > 5;
    let null_err = *best_err;
    refine_alpha_neighborhood_u(samples, best, best_err, d, axis);
    if harvesting {
        super::harvest::record(false, n_unique, span, null_err, *best_err, axis);
    }
}

/// `null_err * 16 / span` — span-normalized LS residual (corpus harvest feature).
#[inline]
fn neighborhood_score(null_err: i32, span: i32) -> i32 {
    (null_err * 16) / span.max(1)
}

/// Skip ±N when LS residual is span-small.
/// Ceiling (ambientCG busy blocks reaching neighborhood, 512k rows):
/// - score<=8 → 26.7% skip / 96.13% gain-kept (prior ship)
/// - score<=9 → 35.5% skip / 93.76% gain-kept
/// - score<=10 → 43.7% skip / 90.95% gain-kept  ← busy ship (holdout maps)
#[inline]
fn skip_neighborhood(null_err: i32, score: i32, _n_unique: usize) -> bool {
    null_err <= 4 || score <= 10
}

fn refine_alpha_neighborhood_u(
    samples: &[u8; 16],
    best: &mut [u8; 8],
    best_err: &mut i32,
    d: i32,
    axis_aligned: bool,
) {
    let b0 = best[0];
    let b1 = best[1];
    if axis_aligned {
        let six = b0 > b1;
        for d0 in -d..=d {
            if d0 == 0 {
                continue;
            }
            let a0 = (b0 as i32 + d0).clamp(0, 255) as u8;
            if (a0 > b1) != six {
                continue;
            }
            consider_alpha_u(a0, b1, samples, best, best_err);
            if *best_err == 0 {
                return;
            }
        }
        let b0 = best[0];
        let b1 = best[1];
        let six = b0 > b1;
        for d1 in -d..=d {
            if d1 == 0 {
                continue;
            }
            let a1 = (b1 as i32 + d1).clamp(0, 255) as u8;
            if (b0 > a1) != six {
                continue;
            }
            consider_alpha_u(b0, a1, samples, best, best_err);
            if *best_err == 0 {
                return;
            }
        }
    } else {
        for d0 in -d..=d {
            for d1 in -d..=d {
                if d0 == 0 && d1 == 0 {
                    continue;
                }
                let a0 = (b0 as i32 + d0).clamp(0, 255) as u8;
                let a1 = (b1 as i32 + d1).clamp(0, 255) as u8;
                consider_alpha_u(a0, a1, samples, best, best_err);
                if *best_err == 0 {
                    return;
                }
            }
        }
    }
}

fn consider_alpha_u(a0: u8, a1: u8, samples: &[u8; 16], best: &mut [u8; 8], best_err: &mut i32) {
    if a0 == a1 {
        return;
    }
    let palette = if a0 > a1 {
        alpha_palette6_u(a0, a1)
    } else {
        alpha_palette4_u(a0, a1)
    };
    if let Some((packed, err)) = pack_alpha_indices_err(a0, a1, &palette, samples, *best_err) {
        *best_err = err;
        *best = packed;
    }
}

/// Pack indices + SSE; returns `None` if SSE cannot beat `err_limit` (early abort).
fn pack_alpha_indices_err(
    a0: u8,
    a1: u8,
    palette: &[u8; 8],
    samples: &[u8; 16],
    err_limit: i32,
) -> Option<([u8; 8], i32)> {
    let mut indices = [0u8; 16];
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
        indices[i] = best;
        let diff = palette[best as usize] as i32 - s as i32;
        err += diff * diff;
        if err >= err_limit {
            return None;
        }
    }
    let mut out = [0u8; 8];
    out[0] = a0;
    out[1] = a1;
    let mut bits: u64 = 0;
    for (i, idx) in indices.iter().enumerate() {
        bits |= (*idx as u64) << (3 * i);
    }
    for b in 0..6 {
        out[2 + b] = ((bits >> (8 * b)) & 0xFF) as u8;
    }
    Some((out, err))
}

/// Least-squares endpoints from current indices (6-lerp weights). Falls back to None on 4-lerp.
fn ls_alpha_endpoints_u(samples: &[u8; 16], block: &[u8; 8]) -> Option<(u8, u8)> {
    let a0 = block[0];
    let a1 = block[1];
    if a0 <= a1 {
        return None; // 4-lerp uses 0/255 sentinels — skip LS.
    }
    let mut bits = 0u64;
    for b in 0..6 {
        bits |= (block[2 + b] as u64) << (8 * b);
    }
    // BC4 6-lerp: idx 0→e0, 1→e1, 2..7 → (6..1)*e0 + (1..6)*e1 over 7.
    const W: [f32; 8] = [0.0, 1.0, 1.0 / 7.0, 2.0 / 7.0, 3.0 / 7.0, 4.0 / 7.0, 5.0 / 7.0, 6.0 / 7.0];
    let mut sw = 0.0f32;
    let mut sw2 = 0.0f32;
    let mut sx = 0.0f32;
    let mut sxw = 0.0f32;
    let mut n = 0.0f32;
    for i in 0..16 {
        let idx = ((bits >> (3 * i)) & 7) as usize;
        let w = W[idx]; // weight toward e1
        let x = samples[i] as f32;
        sw += w;
        sw2 += w * w;
        sx += x;
        sxw += x * w;
        n += 1.0;
    }
    let det = n * sw2 - sw * sw;
    if det.abs() < 1e-3 {
        return None;
    }
    let e0 = (sx * sw2 - sxw * sw) / det;
    let e1 = (n * sxw - sw * sx) / det;
    Some((
        e0.round().clamp(0.0, 255.0) as u8,
        e1.round().clamp(0.0, 255.0) as u8,
    ))
}

/// Integer UNORM→SNORM matching `round(((u/255)*2-1)*127)` clamped to [-127,127].
#[inline]
fn unorm_u8_to_snorm_i32(u: u8) -> i32 {
    (((u as i32) * 253 + 127) / 254 - 127).clamp(-127, 127)
}

/// SNORM i32 → UNORM u8 LUT (index = s + 127 for s in [-127, 127]).
const SNORM_TO_UNORM: [u8; 255] = {
    let mut t = [0u8; 255];
    let mut i = 0;
    while i < 255 {
        let s = i as i32 - 127;
        t[i] = ((((s + 127) * 255) + 127) / 254) as u8;
        i += 1;
    }
    t
};

/// SNORM i32 → UNORM u8, matching the corpus scoreboard (`snorm_bits_to_unorm`).
#[inline]
fn snorm_i32_to_unorm_u8(s: i32) -> u8 {
    SNORM_TO_UNORM[(s.clamp(-127, 127) + 127) as usize]
}

fn encode_alpha_block_signed(samples: [u8; 16]) -> [u8; 8] {
    // Search endpoints in SNORM, but score (and assign indices) by UNORM recon —
    // the bake-off PSNR is UNORM after snorm→unorm, not SNORM-domain SSE.
    let mut vals = [0i32; 16];
    let mut lo = 127i32;
    let mut hi = -127i32;
    for (i, &s) in samples.iter().enumerate() {
        let v = unorm_u8_to_snorm_i32(s);
        vals[i] = v;
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if lo == hi {
        return pack_alpha_indices_s(hi, lo, &alpha_palette4_s(hi, lo), &samples);
    }

    let mut best = pack_alpha_indices_s(hi, lo, &alpha_palette6_s(hi, lo), &samples);
    let mut best_err = alpha_sse_s(&samples, &best);
    consider_alpha_s(lo, hi, &samples, &mut best, &mut best_err);
    let span = hi - lo;

    let fast = quality_is_fast();
    let (n_unique, uniq) = unique_values_s_capped(&vals, 5);
    if !fast && n_unique <= 5 {
        if best_err > 4 && (n_unique <= 4 || best_err > 16) {
            consider_unique_pairs_s(&samples, &uniq[..n_unique], &mut best, &mut best_err);
        }
    } else if !fast && n_unique > 5 && span > 4 {
        consider_alpha_s(hi - 1, lo + 1, &samples, &mut best, &mut best_err);
        consider_alpha_s(lo + 1, hi - 1, &samples, &mut best, &mut best_err);
    }
    refine_alpha_s(&samples, &vals, n_unique, span, &mut best, &mut best_err);
    best
}

fn unique_values_s_capped(vals: &[i32; 16], cap: usize) -> (usize, [i32; 16]) {
    let mut uniq = [0i32; 16];
    let mut n = 0usize;
    'outer: for &s in vals {
        for i in 0..n {
            if uniq[i] == s {
                continue 'outer;
            }
        }
        if n >= cap {
            return (cap + 1, uniq);
        }
        uniq[n] = s;
        n += 1;
    }
    (n, uniq)
}

fn consider_unique_pairs_s(
    samples: &[u8; 16],
    uniq: &[i32],
    best: &mut [u8; 8],
    best_err: &mut i32,
) {
    for i in 0..uniq.len() {
        for j in 0..uniq.len() {
            if i == j {
                continue;
            }
            consider_alpha_s(uniq[i], uniq[j], samples, best, best_err);
            if *best_err == 0 {
                return;
            }
        }
    }
}

fn refine_alpha_s(
    samples: &[u8; 16],
    vals: &[i32; 16],
    n_unique: usize,
    span: i32,
    best: &mut [u8; 8],
    best_err: &mut i32,
) {
    if *best_err == 0 {
        return;
    }
    let full = super::harvest::full_refine();
    let fast = quality_is_fast();
    let do_ls = full || (!fast && n_unique > 2 && *best_err > 8) || (fast && *best_err > 8);
    if do_ls {
        if let Some((r0, r1)) = ls_alpha_endpoints_s(vals, best) {
            consider_alpha_s(r0, r1, samples, best, best_err);
            consider_alpha_s(r1, r0, samples, best, best_err);
            if *best_err == 0 {
                return;
            }
        }
    }
    if fast || (!full && n_unique <= 5 && *best_err <= 24) {
        return;
    }
    let d = if *best_err > 64 {
        2
    } else if *best_err > 8 {
        1
    } else {
        0
    };
    if d == 0 {
        return;
    }

    let harvesting = super::harvest::enabled();
    let score = neighborhood_score(*best_err, span);
    if !full && !harvesting && skip_neighborhood(*best_err, score, n_unique) {
        return;
    }

    let axis = n_unique > 5;
    let null_err = *best_err;
    refine_alpha_neighborhood_s(samples, best, best_err, d, axis);
    if harvesting {
        super::harvest::record(true, n_unique, span, null_err, *best_err, axis);
    }
}

fn refine_alpha_neighborhood_s(
    samples: &[u8; 16],
    best: &mut [u8; 8],
    best_err: &mut i32,
    d: i32,
    axis_aligned: bool,
) {
    let b0 = best[0] as i8 as i32;
    let b1 = best[1] as i8 as i32;
    if axis_aligned {
        let six = b0 > b1;
        for d0 in -d..=d {
            if d0 == 0 {
                continue;
            }
            let a0 = (b0 + d0).clamp(-127, 127);
            if (a0 > b1) != six {
                continue;
            }
            consider_alpha_s(a0, b1, samples, best, best_err);
            if *best_err == 0 {
                return;
            }
        }
        let b0 = best[0] as i8 as i32;
        let b1 = best[1] as i8 as i32;
        let six = b0 > b1;
        for d1 in -d..=d {
            if d1 == 0 {
                continue;
            }
            let a1 = (b1 + d1).clamp(-127, 127);
            if (b0 > a1) != six {
                continue;
            }
            consider_alpha_s(b0, a1, samples, best, best_err);
            if *best_err == 0 {
                return;
            }
        }
    } else {
        for d0 in -d..=d {
            for d1 in -d..=d {
                if d0 == 0 && d1 == 0 {
                    continue;
                }
                consider_alpha_s(
                    (b0 + d0).clamp(-127, 127),
                    (b1 + d1).clamp(-127, 127),
                    samples,
                    best,
                    best_err,
                );
                if *best_err == 0 {
                    return;
                }
            }
        }
    }
}

fn consider_alpha_s(
    a0: i32,
    a1: i32,
    samples: &[u8; 16],
    best: &mut [u8; 8],
    best_err: &mut i32,
) {
    if a0 == a1 {
        return;
    }
    let a0 = a0.clamp(-127, 127);
    let a1 = a1.clamp(-127, 127);
    if a0 == a1 {
        return;
    }
    let palette = if a0 > a1 {
        alpha_palette6_s(a0, a1)
    } else {
        alpha_palette4_s(a0, a1)
    };
    if let Some((packed, err)) = pack_alpha_indices_s_err(a0, a1, &palette, samples, *best_err) {
        *best_err = err;
        *best = packed;
    }
}

fn pack_alpha_indices_s_err(
    a0: i32,
    a1: i32,
    palette: &[i32; 8],
    samples: &[u8; 16],
    err_limit: i32,
) -> Option<([u8; 8], i32)> {
    let mut pal_u = [0u8; 8];
    for (i, &p) in palette.iter().enumerate() {
        pal_u[i] = snorm_i32_to_unorm_u8(p);
    }
    let mut indices = [0u8; 16];
    let mut err = 0i32;
    for (i, &s) in samples.iter().enumerate() {
        let mut best = 0u8;
        let mut best_d = i32::MAX;
        for (j, &pu) in pal_u.iter().enumerate() {
            let d = (pu as i32 - s as i32).abs();
            if d < best_d {
                best_d = d;
                best = j as u8;
            }
        }
        indices[i] = best;
        let diff = pal_u[best as usize] as i32 - s as i32;
        err += diff * diff;
        if err >= err_limit {
            return None;
        }
    }
    let mut out = [0u8; 8];
    out[0] = a0 as i8 as u8;
    out[1] = a1 as i8 as u8;
    let mut bits: u64 = 0;
    for (i, idx) in indices.iter().enumerate() {
        bits |= (*idx as u64) << (3 * i);
    }
    for b in 0..6 {
        out[2 + b] = ((bits >> (8 * b)) & 0xFF) as u8;
    }
    Some((out, err))
}

fn ls_alpha_endpoints_s(vals: &[i32; 16], block: &[u8; 8]) -> Option<(i32, i32)> {
    let a0 = block[0] as i8 as i32;
    let a1 = block[1] as i8 as i32;
    if a0 <= a1 {
        return None;
    }
    let mut bits = 0u64;
    for b in 0..6 {
        bits |= (block[2 + b] as u64) << (8 * b);
    }
    const W: [f32; 8] = [0.0, 1.0, 1.0 / 7.0, 2.0 / 7.0, 3.0 / 7.0, 4.0 / 7.0, 5.0 / 7.0, 6.0 / 7.0];
    let mut sw = 0.0f32;
    let mut sw2 = 0.0f32;
    let mut sx = 0.0f32;
    let mut sxw = 0.0f32;
    let mut n = 0.0f32;
    for i in 0..16 {
        let idx = ((bits >> (3 * i)) & 7) as usize;
        let w = W[idx];
        let x = vals[i] as f32;
        sw += w;
        sw2 += w * w;
        sx += x;
        sxw += x * w;
        n += 1.0;
    }
    let det = n * sw2 - sw * sw;
    if det.abs() < 1e-3 {
        return None;
    }
    let e0 = (sx * sw2 - sxw * sw) / det;
    let e1 = (n * sxw - sw * sx) / det;
    Some((
        e0.round().clamp(-127.0, 127.0) as i32,
        e1.round().clamp(-127.0, 127.0) as i32,
    ))
}

fn alpha_sse_u(samples: &[u8; 16], block: &[u8; 8]) -> i32 {
    let a0 = block[0];
    let a1 = block[1];
    let palette = if a0 > a1 {
        alpha_palette6_u(a0, a1)
    } else {
        alpha_palette4_u(a0, a1)
    };
    let mut bits = 0u64;
    for b in 0..6 {
        bits |= (block[2 + b] as u64) << (8 * b);
    }
    let mut err = 0i32;
    for i in 0..16 {
        let idx = ((bits >> (3 * i)) & 7) as usize;
        let d = palette[idx] as i32 - samples[i] as i32;
        err += d * d;
    }
    err
}

fn alpha_sse_s(samples: &[u8; 16], block: &[u8; 8]) -> i32 {
    let a0 = block[0] as i8 as i32;
    let a1 = block[1] as i8 as i32;
    let palette = if a0 > a1 {
        alpha_palette6_s(a0, a1)
    } else {
        alpha_palette4_s(a0, a1)
    };
    let mut bits = 0u64;
    for b in 0..6 {
        bits |= (block[2 + b] as u64) << (8 * b);
    }
    let mut err = 0i32;
    for i in 0..16 {
        let idx = ((bits >> (3 * i)) & 7) as usize;
        let d = snorm_i32_to_unorm_u8(palette[idx]) as i32 - samples[i] as i32;
        err += d * d;
    }
    err
}

fn pack_alpha_indices(a0: u8, a1: u8, palette: &[u8; 8], samples: &[u8; 16]) -> [u8; 8] {
    let mut indices = [0u8; 16];
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
        indices[i] = best;
    }
    let mut out = [0u8; 8];
    out[0] = a0;
    out[1] = a1;
    let mut bits: u64 = 0;
    for (i, idx) in indices.iter().enumerate() {
        bits |= (*idx as u64) << (3 * i);
    }
    for b in 0..6 {
        out[2 + b] = ((bits >> (8 * b)) & 0xFF) as u8;
    }
    out
}

fn pack_alpha_indices_s(a0: i32, a1: i32, palette: &[i32; 8], samples: &[u8; 16]) -> [u8; 8] {
    let mut pal_u = [0u8; 8];
    for (i, &p) in palette.iter().enumerate() {
        pal_u[i] = snorm_i32_to_unorm_u8(p);
    }
    let mut indices = [0u8; 16];
    for (i, &s) in samples.iter().enumerate() {
        let mut best = 0u8;
        let mut best_d = i32::MAX;
        for (j, &pu) in pal_u.iter().enumerate() {
            let d = (pu as i32 - s as i32).abs();
            if d < best_d {
                best_d = d;
                best = j as u8;
            }
        }
        indices[i] = best;
    }
    let mut out = [0u8; 8];
    out[0] = a0 as i8 as u8;
    out[1] = a1 as i8 as u8;
    let mut bits: u64 = 0;
    for (i, idx) in indices.iter().enumerate() {
        bits |= (*idx as u64) << (3 * i);
    }
    for b in 0..6 {
        out[2 + b] = ((bits >> (8 * b)) & 0xFF) as u8;
    }
    out
}

fn alpha_palette6_u(max_v: u8, min_v: u8) -> [u8; 8] {
    // Match bcdec_rs / DirectXTex: fixed-point weights with +32768 rounding.
    const W6: [i32; 6] = [9363, 18724, 28086, 37450, 46812, 56173];
    let max = max_v as i32;
    let min = min_v as i32;
    [
        max_v,
        min_v,
        ((W6[5] * max + W6[0] * min + 32768) >> 16) as u8,
        ((W6[4] * max + W6[1] * min + 32768) >> 16) as u8,
        ((W6[3] * max + W6[2] * min + 32768) >> 16) as u8,
        ((W6[2] * max + W6[3] * min + 32768) >> 16) as u8,
        ((W6[1] * max + W6[4] * min + 32768) >> 16) as u8,
        ((W6[0] * max + W6[5] * min + 32768) >> 16) as u8,
    ]
}

fn alpha_palette4_u(max_v: u8, min_v: u8) -> [u8; 8] {
    const W4: [i32; 4] = [13107, 26215, 39321, 52429];
    let max = max_v as i32;
    let min = min_v as i32;
    [
        max_v,
        min_v,
        ((W4[3] * max + W4[0] * min + 32768) >> 16) as u8,
        ((W4[2] * max + W4[1] * min + 32768) >> 16) as u8,
        ((W4[1] * max + W4[2] * min + 32768) >> 16) as u8,
        ((W4[0] * max + W4[3] * min + 32768) >> 16) as u8,
        0,
        255,
    ]
}

fn alpha_palette6_s(max_v: i32, min_v: i32) -> [i32; 8] {
    const W6: [i32; 6] = [9363, 18724, 28086, 37450, 46812, 56173];
    [
        max_v,
        min_v,
        (W6[5] * max_v + W6[0] * min_v + 32768) >> 16,
        (W6[4] * max_v + W6[1] * min_v + 32768) >> 16,
        (W6[3] * max_v + W6[2] * min_v + 32768) >> 16,
        (W6[2] * max_v + W6[3] * min_v + 32768) >> 16,
        (W6[1] * max_v + W6[4] * min_v + 32768) >> 16,
        (W6[0] * max_v + W6[5] * min_v + 32768) >> 16,
    ]
}

fn alpha_palette4_s(max_v: i32, min_v: i32) -> [i32; 8] {
    const W4: [i32; 4] = [13107, 26215, 39321, 52429];
    [
        max_v,
        min_v,
        (W4[3] * max_v + W4[0] * min_v + 32768) >> 16,
        (W4[2] * max_v + W4[1] * min_v + 32768) >> 16,
        (W4[1] * max_v + W4[2] * min_v + 32768) >> 16,
        (W4[0] * max_v + W4[3] * min_v + 32768) >> 16,
        -127,
        127,
    ]
}


// ---------------------------------------------------------------------------
// BC7 mode 6
// ---------------------------------------------------------------------------

/// BC7 mode 6: single subset, RGBA 7-bit endpoints + P-bits, 4-bit indices.
pub fn encode_bc7_mode6(pixels: [[u8; 4]; 16], out: &mut [u8]) {
    let mut best_bits = [0u8; 16];
    let mut best_err = i64::MAX;
    let mut best_seed = extrema_rgba(&pixels);
    let mut have = false;

    let (seeds, n_seeds) = bc7_mode6_seeds(&pixels);
    for &(ep0, ep1) in &seeds[..n_seeds] {
        if let Some((bits, err)) = try_bc7_mode6(&pixels, ep0, ep1, false) {
            if err < best_err {
                best_err = err;
                best_bits = bits;
                best_seed = (ep0, ep1);
                have = true;
            }
        }
    }
    // Skip LS on near-solid blocks — seed endpoints already win.
    let do_ls = rgba_span_sum(&pixels) > 8;
    if do_ls {
        if let Some((bits, err)) = try_bc7_mode6(&pixels, best_seed.0, best_seed.1, true) {
            if err <= best_err {
                best_bits = bits;
                have = true;
            }
        }
    }
    if !have {
        if let Some((bits, _)) = try_bc7_mode6(&pixels, best_seed.0, best_seed.1, true) {
            best_bits = bits;
        }
    }
    out[..16].copy_from_slice(&best_bits);
}

fn rgba_span_sum(pixels: &[[u8; 4]; 16]) -> i32 {
    let (mx, mn) = channel_minmax_rgba(pixels);
    (0..4).map(|c| (mx[c] - mn[c]) as i32).sum()
}

type Seed = ([u8; 4], [u8; 4]);

/// Push with dedup: a duplicate trial can never win under strict `<`, so
/// skipping it is byte-identical and saves a whole index-fit pass.
#[inline]
fn push_seed(seeds: &mut [Seed; 5], n: &mut usize, s: Seed) {
    for seed in seeds[..*n].iter() {
        if *seed == s {
            return;
        }
    }
    seeds[*n] = s;
    *n += 1;
}

fn bc7_mode6_seeds(pixels: &[[u8; 4]; 16]) -> ([Seed; 5], usize) {
    let mut seeds = [([0u8; 4], [0u8; 4]); 5];
    let mut n = 0usize;
    push_seed(&mut seeds, &mut n, extrema_rgba(pixels));
    push_seed(&mut seeds, &mut n, channel_minmax_rgba(pixels));

    let span = rgba_span_sum(pixels);
    // Low variance: extrema + channel minmax are enough.
    if span <= 16 {
        return (seeds, n);
    }

    let (mx, mn) = extrema_rgba(pixels);
    let mut mean = [0u32; 4];
    for p in pixels {
        for c in 0..4 {
            mean[c] += p[c] as u32;
        }
    }
    let mean = mean.map(|v| (v / 16) as u8);
    push_seed(&mut seeds, &mut n, (mx, mean));
    push_seed(&mut seeds, &mut n, (mean, mn));

    // Farthest-pair only on busy blocks (O(16²)).
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
        push_seed(&mut seeds, &mut n, (pa, pb));
    }
    (seeds, n)
}

fn channel_minmax_rgba(pixels: &[[u8; 4]; 16]) -> ([u8; 4], [u8; 4]) {
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
const W6M: [u32; 16] = [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64];

/// Reconstructed 16-entry palette for one (c0, c1) pair — computed ONCE per
/// trial instead of re-lerping per pixel per candidate index.
#[inline]
fn palette_mode6(c0: [u8; 4], c1: [u8; 4]) -> [[u8; 4]; 16] {
    let mut pal = [[0u8; 4]; 16];
    for (k, &w) in W6M.iter().enumerate() {
        for c in 0..4 {
            pal[k][c] = (((64 - w) * c0[c] as u32 + w * c1[c] as u32 + 32) / 64) as u8;
        }
    }
    pal
}

/// Nearest palette entry (strict `<`: lowest index wins ties) + its SSE.
#[inline]
fn best_index_pal(px: &[u8; 4], pal: &[[u8; 4]; 16]) -> (u8, i32) {
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

/// Index-fit a whole block against one palette; returns (indices, total SSE).
#[inline]
fn fit_indices_mode6(pixels: &[[u8; 4]; 16], pal: &[[u8; 4]; 16]) -> ([u8; 16], i64) {
    let mut indices = [0u8; 16];
    let mut err = 0i64;
    for (i, px) in pixels.iter().enumerate() {
        let (idx, e) = best_index_pal(px, pal);
        indices[i] = idx;
        err += e as i64;
    }
    (indices, err)
}

fn try_bc7_mode6(
    pixels: &[[u8; 4]; 16],
    ep0: [u8; 4],
    ep1: [u8; 4],
    refine: bool,
) -> Option<([u8; 16], i64)> {
    let (mut q0, mut p0) = quantize_7p(ep0);
    let (mut q1, mut p1) = quantize_7p(ep1);
    let pal = palette_mode6(unquantize_7p(q0, p0), unquantize_7p(q1, p1));
    // SSE is accumulated during the index fit — the recon after an endpoint
    // swap + index inversion is identical (W6M symmetry), so no re-walk.
    let (mut indices, mut err) = fit_indices_mode6(pixels, &pal);
    if indices[0] > 7 {
        std::mem::swap(&mut q0, &mut q1);
        std::mem::swap(&mut p0, &mut p1);
        for idx in indices.iter_mut() {
            *idx = 15 - *idx;
        }
    }

    // Least-squares refine endpoints given indices (then re-quantize).
    if refine {
        if let Some((r0, r1)) = ls_endpoints_mode6(pixels, &indices) {
            let (nq0, np0) = quantize_7p(r0);
            let (nq1, np1) = quantize_7p(r1);
            let npal = palette_mode6(unquantize_7p(nq0, np0), unquantize_7p(nq1, np1));
            let (mut nidx, nerr) = fit_indices_mode6(pixels, &npal);
            if nidx[0] > 7 {
                for idx in nidx.iter_mut() {
                    *idx = 15 - *idx;
                }
                q0 = nq1;
                p0 = np1;
                q1 = nq0;
                p1 = np0;
            } else {
                q0 = nq0;
                p0 = np0;
                q1 = nq1;
                p1 = np1;
            }
            indices = nidx;
            err = nerr;
        }
    }

    Some((pack_bc7_mode6(q0, p0, q1, p1, indices), err))
}

fn ls_endpoints_mode6(pixels: &[[u8; 4]; 16], indices: &[u8; 16]) -> Option<([u8; 4], [u8; 4])> {
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

fn pack_bc7_mode6(q0: [u8; 4], p0: u8, q1: [u8; 4], p1: u8, indices: [u8; 16]) -> [u8; 16] {
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

fn quantize_7p(c: [u8; 4]) -> ([u8; 4], u8) {
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

fn unquantize_7p(q: [u8; 4], p: u8) -> [u8; 4] {
    [
        unquantize_7p_chan(q[0], p),
        unquantize_7p_chan(q[1], p),
        unquantize_7p_chan(q[2], p),
        unquantize_7p_chan(q[3], p),
    ]
}

fn unquantize_7p_chan(q: u8, p: u8) -> u8 {
    let v = ((q as u32) << 1) | (p as u32);
    v as u8
}

fn extrema_opaque(pixels: &[[u8; 4]; 16]) -> ([u8; 3], [u8; 3]) {
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

fn extrema_rgba(pixels: &[[u8; 4]; 16]) -> ([u8; 4], [u8; 4]) {
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

fn to_565(c: [u8; 3]) -> u16 {
    let r = (c[0] as u16 >> 3) & 31;
    let g = (c[1] as u16 >> 2) & 63;
    let b = (c[2] as u16 >> 3) & 31;
    (r << 11) | (g << 5) | b
}

fn from_565(c: u16) -> [u8; 3] {
    let r = ((c >> 11) & 31) as u8;
    let g = ((c >> 5) & 63) as u8;
    let b = (c & 31) as u8;
    [(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2)]
}

fn lerp_rgb(a: [u8; 3], b: [u8; 3], aw: u32, bw: u32) -> [u8; 3] {
    [
        ((aw * a[0] as u32 + bw * b[0] as u32) / (aw + bw)) as u8,
        ((aw * a[1] as u32 + bw * b[1] as u32) / (aw + bw)) as u8,
        ((aw * a[2] as u32 + bw * b[2] as u32) / (aw + bw)) as u8,
    ]
}

fn pack_indices_2bit(pixels: &[[u8; 4]; 16], colors: &[[u8; 3]; 4], alpha_punch: bool) -> u32 {
    let mut table = 0u32;
    for (i, p) in pixels.iter().enumerate() {
        let idx = if alpha_punch && p[3] < 128 {
            3
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
            best
        };
        table |= (idx as u32) << (2 * i);
    }
    table
}

fn sqr_rgb(a: [u8; 3], b: [u8; 3]) -> i32 {
    let mut s = 0i32;
    for i in 0..3 {
        let d = a[i] as i32 - b[i] as i32;
        s += d * d;
    }
    s
}

#[derive(Default)]
struct BitWriter {
    low: u64,
    high: u64,
    pos: u32,
}

impl BitWriter {
    fn write_bits(&mut self, value: u32, n: u32) {
        debug_assert!(n <= 32);
        let mask = if n == 32 {
            u64::MAX
        } else {
            (1u64 << n) - 1
        };
        let v = (value as u64) & mask;
        if self.pos < 64 {
            self.low |= v << self.pos;
            if self.pos + n > 64 {
                let overflow = self.pos + n - 64;
                self.high |= v >> (n - overflow);
            }
        } else {
            self.high |= v << (self.pos - 64);
        }
        self.pos += n;
    }

    fn into_array(self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0..8].copy_from_slice(&self.low.to_le_bytes());
        out[8..16].copy_from_slice(&self.high.to_le_bytes());
        out
    }
}
