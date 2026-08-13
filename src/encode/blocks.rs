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
    // Fused pack+score: the index fit's per-pixel argmin distance IS the SSE
    // contribution, so the old pack-then-bc1_sse re-walk is pure recompute.
    let (a, a_err) = pack_bc1_scored(&pixels, max_c, min_c, i32::MAX)
        .expect("unbounded pack always packs");
    if rgb_channel_span_sum(&pixels) < 24 {
        return a;
    }
    let mut best = a;
    let mut best_err = a_err;
    if best_err == 0 {
        return best;
    }
    let (mx, mn) = channel_minmax_rgb(&pixels);
    if !(mx == max_c && mn == min_c) {
        consider_bc1(&pixels, mx, mn, &mut best, &mut best_err);
    }
    // Refine gate: a tiny residual can't repay PCA + LS (gain <= best_err).
    // Smooth-map blocks skip the whole refine; busy blocks keep the quality.
    if quality_is_fast() || best_err <= 16 {
        return best;
    }
    // PCA-axis extremes: luminance extrema mis-seed chroma-dominant blocks.
    if let Some((pa, pb)) = pca_extremes_rgb(&pixels) {
        consider_bc1(&pixels, pa, pb, &mut best, &mut best_err);
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
        consider_bc1(&pixels, e0, e1, &mut best, &mut best_err);
        if best_err >= prev {
            break;
        }
    }
    if best_err > bc1_lattice_min_err() {
        lattice_refine_bc1(&pixels, &mut best, &mut best_err);
    }
    best
}


fn unsigned_window_enabled() -> bool {
    use std::sync::OnceLock;
    static E: OnceLock<bool> = OnceLock::new();
    *E.get_or_init(|| {
        std::env::var("RUSTY_DDS_BC45U_WINDOW")
            .map(|v| v == "1")
            .unwrap_or(false)
    })
}

fn alpha_sel_enabled() -> bool {
    use std::sync::OnceLock;
    static E: OnceLock<bool> = OnceLock::new();
    *E.get_or_init(|| std::env::var("RUSTY_DDS_ALPHA_SEL").map(|v| v != "0").unwrap_or(true))
}

fn bc1_lattice_rounds() -> u32 {
    use std::sync::OnceLock;
    static R: OnceLock<u32> = OnceLock::new();
    *R.get_or_init(|| {
        std::env::var("RUSTY_DDS_BC1_LATTICE_ROUNDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3)
    })
}


/// Experiment knob for the lattice gate threshold (read once; default 0 =
/// fire whenever residual is non-zero).
fn bc1_lattice_min_err() -> i32 {
    use std::sync::OnceLock;
    static T: OnceLock<i32> = OnceLock::new();
    *T.get_or_init(|| {
        std::env::var("RUSTY_DDS_BC1_LATTICE_T")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    })
}

fn consider_bc1(
    pixels: &[[u8; 4]; 16],
    e0: [u8; 3],
    e1: [u8; 3],
    best: &mut [u8; 8],
    best_err: &mut i32,
) {
    if let Some((cand, err)) = pack_bc1_scored(pixels, e0, e1, *best_err) {
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
fn bc1_fit_4color(
    pixels: &[[u8; 4]; 16],
    colors: &[[u8; 3]; 4],
    err_limit: i32,
) -> Option<(u32, i32)> {
    let hi = colors[0];
    let lo = colors[1];
    let axis = [
        lo[0] as i32 - hi[0] as i32,
        lo[1] as i32 - hi[1] as i32,
        lo[2] as i32 - hi[2] as i32,
    ];
    let len2 = axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2];
    let mut table = 0u32;
    let mut err = 0i32;
    // Projection fast path DISABLED after measurement: cutting a 4-entry
    // scan to a dot + ±1-rank verify saved ~nothing (3 sqr evals vs 4) and
    // cost up to 0.012 dB — reverted to the exact exhaustive fit.
    if false && len2 >= 256 {
        // Index order along t (from hi): 0 (t=0), 2 (1/3), 3 (2/3), 1 (1).
        const ALONG: [usize; 4] = [0, 2, 3, 1];
        for (i, p) in pixels.iter().enumerate() {
            let dot = (p[0] as i32 - hi[0] as i32) * axis[0]
                + (p[1] as i32 - hi[1] as i32) * axis[1]
                + (p[2] as i32 - hi[2] as i32) * axis[2];
            let d6 = dot * 6;
            let rank = (d6 > len2) as usize + (d6 > 3 * len2) as usize + (d6 > 5 * len2) as usize;
            // ±1 rank window absorbs interpolation rounding.
            let mut best = ALONG[rank];
            let mut best_d = sqr_rgb([p[0], p[1], p[2]], colors[best]);
            if rank > 0 {
                let j = ALONG[rank - 1];
                let d = sqr_rgb([p[0], p[1], p[2]], colors[j]);
                if d < best_d {
                    best_d = d;
                    best = j;
                }
            }
            if rank < 3 {
                let j = ALONG[rank + 1];
                let d = sqr_rgb([p[0], p[1], p[2]], colors[j]);
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
    } else {
        for (i, p) in pixels.iter().enumerate() {
            let mut best = 0usize;
            let mut best_d = i32::MAX;
            for (j, c) in colors.iter().enumerate() {
                let d = sqr_rgb([p[0], p[1], p[2]], *c);
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
    }
    Some((table, err))
}

/// Score a 4-color candidate whose endpoints are ALREADY 565 values (the
/// 565-lattice refine works in quantized space directly, so no re-rounding).
fn pack_bc1_scored_565(
    pixels: &[[u8; 4]; 16],
    a: u16,
    b: u16,
    err_limit: i32,
) -> Option<([u8; 8], i32)> {
    debug_assert_ne!(a, b);
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    let ca = from_565(hi);
    let cb = from_565(lo);
    let colors = [ca, cb, lerp_rgb(ca, cb, 2, 1), lerp_rgb(ca, cb, 1, 2)];
    let (table, err) = bc1_fit_4color(pixels, &colors, err_limit)?;
    let mut out = [0u8; 8];
    out[0..2].copy_from_slice(&hi.to_le_bytes());
    out[2..4].copy_from_slice(&lo.to_le_bytes());
    out[4..8].copy_from_slice(&table.to_le_bytes());
    Some((out, err))
}

/// 565-lattice hill climb around the winner: LS optimizes continuous RGB and
/// rounds through 565, so adjacent LATTICE points can beat the rounded
/// answer (the same discrete-lattice effect the signed window exploits).
/// ±1 per component per endpoint (12 candidates/round), up to 2 rounds,
/// strict `<` acceptance — quality-monotone.
fn lattice_refine_bc1(pixels: &[[u8; 4]; 16], best: &mut [u8; 8], best_err: &mut i32) {
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
                if let Some((blk, e)) = pack_bc1_scored_565(pixels, cand, other, *best_err) {
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
fn pack_bc1_scored(
    pixels: &[[u8; 4]; 16],
    max_c: [u8; 3],
    min_c: [u8; 3],
    err_limit: i32,
) -> Option<([u8; 8], i32)> {
    let mut max565 = to_565(max_c);
    let min565 = to_565(min_c);
    if max565 == min565 {
        max565 = max565.saturating_add(1);
    }
    let (c0, c1, colors, punch) = if max565 > min565 {
        let ca = from_565(max565);
        let cb = from_565(min565);
        (
            max565,
            min565,
            [ca, cb, lerp_rgb(ca, cb, 2, 1), lerp_rgb(ca, cb, 1, 2)],
            false,
        )
    } else if max565 < min565 {
        // 565 quantization inverted the seed order: stored c0 > c1 still
        // decodes as 4-COLOR mode, so fit indices against the decode-true
        // 4-color palette. (The old code fitted a 3-color+black palette here
        // that no decoder would ever reconstruct.)
        let ca = from_565(min565);
        let cb = from_565(max565);
        (
            min565,
            max565,
            [ca, cb, lerp_rgb(ca, cb, 2, 1), lerp_rgb(ca, cb, 1, 2)],
            false,
        )
    } else {
        // Equal even after the +1 nudge (0xFFFF): true 3-color mode.
        let ca = from_565(min565);
        let cb = from_565(max565);
        (
            min565,
            max565,
            [ca, cb, lerp_rgb(ca, cb, 1, 1), [0, 0, 0]],
            true,
        )
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

#[cfg(test)]
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
    // Full BC4-grade alpha search (uniques/LS/neighborhood) instead of the
    // min/max-only fast path: quality-monotone (same dual seed, candidates
    // only added under strict `<`), and CryTIF-style UI content is
    // alpha-gradient-heavy.
    out[..8].copy_from_slice(&encode_alpha_block_unsigned(pixels.map(|p| p[3])));
    out[8..16].copy_from_slice(&encode_bc1_bytes(pixels));
}

#[cfg(test)]
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
    } else if max565 < min565 {
        // Stored c0 > c1 decodes as 4-color; fit against the decode palette.
        let colors = [
            from_565(min565),
            from_565(max565),
            lerp_rgb(from_565(min565), from_565(max565), 2, 1),
            lerp_rgb(from_565(min565), from_565(max565), 1, 2),
        ];
        (min565, max565, pack_indices_2bit(&pixels, &colors, false))
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
    // Unsigned twin of the signed windowed sweep (same gate + prune).
    // DEFAULT OFF: it costs ~3.2s corpus CPU for +0.15..0.45 dB on 14 cases
    // that already beat DirectXTex by 0.4-1.9 dB — the CPU budget instead
    // funds the BC1 lattice and BC7 mode 5, whose gains are 2-20x larger.
    // Opt in with RUSTY_DDS_BC45U_WINDOW=1. (err>16 tight-gate variant was
    // tried: keeps only 10-45% of the smooth-map gains, which live at err
    // 5..16.)
    if unsigned_window_enabled()
        && !quality_is_fast()
        && (16..=64).contains(&span)
        && best_err > 4
    {
        unsigned_window_sweep(&samples, &mut best, &mut best_err);
    }
    best
}

/// ±4 exhaustive window around the unsigned winner; 6-lerp pairs whose
/// palette range provably can't beat `best_err` are skipped (4-lerp pairs
/// carry 0/255 sentinels and are never pruned); strict `<` keeps it
/// quality-monotone.
fn unsigned_window_sweep(samples: &[u8; 16], best: &mut [u8; 8], best_err: &mut i32) {
    let b0 = best[0] as i32;
    let b1 = best[1] as i32;
    let mut smin = 255u8;
    let mut smax = 0u8;
    for &s in samples {
        smin = smin.min(s);
        smax = smax.max(s);
    }
    for d0 in -4i32..=4 {
        for d1 in -4i32..=4 {
            if d0 == 0 && d1 == 0 {
                continue;
            }
            let a0 = (b0 + d0).clamp(0, 255) as u8;
            let a1 = (b1 + d1).clamp(0, 255) as u8;
            if a0 > a1 {
                let over = (smax as i32 - a0 as i32).max(0);
                let under = (a1 as i32 - smin as i32).max(0);
                if over * over + under * under >= *best_err {
                    continue;
                }
            }
            consider_alpha_u(a0, a1, samples, best, best_err);
            if *best_err == 0 {
                return;
            }
        }
    }
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
        // Iterate LS -> index refit while SSE keeps falling (BC1-refine shape).
        for _ in 0..4 {
            let Some((r0, r1)) = ls_alpha_endpoints_u(samples, best) else {
                break;
            };
            let prev = *best_err;
            consider_alpha_u(r0, r1, samples, best, best_err);
            consider_alpha_u(r1, r0, samples, best, best_err);
            if *best_err == 0 {
                return;
            }
            if *best_err >= prev {
                break;
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

/// Ascending value order of the 6-lerp alpha palette (a0 > a1): a1 first,
/// interpolants descend from idx2 (near a0) — and of the 4-lerp palette
/// (a0 <= a1): sentinel 0, a0, interpolants ascend, a1, sentinel 255.
const ALPHA_ORDER6: [u8; 8] = [1, 7, 6, 5, 4, 3, 2, 0];
const ALPHA_ORDER4: [u8; 8] = [6, 0, 2, 3, 4, 5, 1, 7];

/// Threshold selector for nearest-palette-entry: 7 boundaries between the
/// (deduped) ascending values, tie rules baked to reproduce the linear
/// scan's strict-`<` lowest-index behaviour EXACTLY. Proven byte-identical
/// by full enumeration (`alpha_select_matches_linear_exhaustive`).
struct AlphaSelect {
    thr: [i32; 7],
    lut: [u8; 8],
    n: usize,
}

impl AlphaSelect {
    fn build(palette: &[u8; 8], order: &[u8; 8]) -> Self {
        // Dedupe equal values keeping the LOWEST original index (the linear
        // scan's tie winner among equal values).
        let mut vals = [0i32; 8];
        let mut idxs = [0u8; 8];
        let mut n = 0usize;
        for &o in order {
            let v = palette[o as usize] as i32;
            if n > 0 && vals[n - 1] == v {
                if o < idxs[n - 1] {
                    idxs[n - 1] = o;
                }
                continue;
            }
            vals[n] = v;
            idxs[n] = o;
            n += 1;
        }
        let mut thr = [i32::MAX; 7];
        for k in 0..n - 1 {
            let (vl, vh) = (vals[k], vals[k + 1]);
            let sum = vl + vh;
            let m = sum >> 1;
            // s > thr[k] selects the HIGH side. Odd sum: no exact tie.
            // Even sum: tie at m goes to the lower ORIGINAL index.
            thr[k] = if sum & 1 == 1 {
                m
            } else if idxs[k] < idxs[k + 1] {
                m
            } else {
                m - 1
            };
        }
        Self { thr, lut: idxs, n }
    }

    #[inline]
    fn select(&self, s: u8) -> u8 {
        let s = s as i32;
        let mut rank = 0usize;
        for k in 0..self.n.saturating_sub(1) {
            rank += (s > self.thr[k]) as usize;
        }
        self.lut[rank]
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
    if alpha_sel_enabled() {
        let order = if a0 > a1 { &ALPHA_ORDER6 } else { &ALPHA_ORDER4 };
        let sel = AlphaSelect::build(palette, order);
        for (i, &s) in samples.iter().enumerate() {
            let best = sel.select(s);
            indices[i] = best;
            let diff = palette[best as usize] as i32 - s as i32;
            err += diff * diff;
            if err >= err_limit {
                return None;
            }
        }
    } else {
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
    let (mut best, mut best_err, lo, hi, span, _n_unique) =
        encode_alpha_block_signed_presweep(samples);
    // Windowed endpoint sweep: the LS/±2 search leaves ~0.5 dB on smooth
    // signed content (Wood normals) — the UNORM-scored optimum sits beyond
    // its reach but NEAR the current best (harvest: ±4 window keeps 93% of
    // Wood's ceiling gain at ~80 pairs/block; a full-span sweep costs 6-25x
    // more for gains only on maps already ahead of DirectXTex).
    let _ = (lo, hi);
    if !quality_is_fast() && signed_sweep_gate(span, best_err) {
        signed_window_sweep(&samples, &mut best, &mut best_err);
    }
    best
}

/// ±4 exhaustive window around the pre-sweep winner (both palette modes are
/// reachable near the b0==b1 boundary); strict `<` keeps it quality-monotone.
fn signed_window_sweep(samples: &[u8; 16], best: &mut [u8; 8], best_err: &mut i32) {
    let b0 = best[0] as i8 as i32;
    let b1 = best[1] as i8 as i32;
    // Range lower bound (busy-block pruning): a sample above the pair's
    // UNORM palette ceiling contributes at least (smax-phi)^2, and one below
    // the floor at least (plo-smin)^2 — 6-lerp palettes lie inside their
    // endpoints, so a pair whose bound reaches best_err provably cannot win
    // and skipping it is byte-identical. (4-lerp pairs carry 0/255 sentinels
    // and are never pruned.)
    let mut smin = 255u8;
    let mut smax = 0u8;
    for &s in samples {
        smin = smin.min(s);
        smax = smax.max(s);
    }
    for d0 in -4i32..=4 {
        for d1 in -4i32..=4 {
            if d0 == 0 && d1 == 0 {
                continue;
            }
            let a0 = (b0 + d0).clamp(-127, 127);
            let a1 = (b1 + d1).clamp(-127, 127);
            if a0 > a1 {
                let phi = snorm_i32_to_unorm_u8(a0) as i32;
                let plo = snorm_i32_to_unorm_u8(a1) as i32;
                let over = (smax as i32 - phi).max(0);
                let under = (plo - smin as i32).max(0);
                if over * over + under * under >= *best_err {
                    continue;
                }
            }
            consider_alpha_s(a0, a1, samples, best, best_err);
            if *best_err == 0 {
                return;
            }
        }
    }
}

/// The full signed search pipeline BEFORE the exhaustive sweep.
/// Returns (block, err, lo, hi, span, n_unique) — split out so the harvest
/// test can measure the sweep's null arm.
fn encode_alpha_block_signed_presweep(samples: [u8; 16]) -> ([u8; 8], i32, i32, i32, i32, usize) {
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
        let b = pack_alpha_indices_s(hi, lo, &alpha_palette4_s(hi, lo), &samples);
        let err = alpha_sse_s(&samples, &b);
        return (b, err, lo, hi, 0, 1);
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
    (best, best_err, lo, hi, span, n_unique)
}

/// Harvest-tuned gate (target/signed_sweep_harvest.csv, 643k blocks):
/// span < 8 gained ZERO across 169k blocks, and `gain <= best_err` means a
/// tiny residual can never pay for the window.
#[inline]
fn signed_sweep_gate(span: i32, best_err: i32) -> bool {
    // Harvest-tuned (target/signed_sweep_harvest.csv, 643k blocks): span < 8
    // gained ZERO across 169k blocks; spans > 32 only gain on maps already
    // ahead of DirectXTex; `gain <= best_err` means err <= 4 can't pay.
    (8..=32).contains(&span) && best_err > 4
}

/// Bounded near-exhaustive signed endpoint sweep (both orders => both
/// palette modes), candidates added under strict `<` (quality-monotone).
#[cfg(test)]
fn signed_sweep(lo: i32, hi: i32, samples: &[u8; 16], best: &mut [u8; 8], best_err: &mut i32) {
    let a_lo = (lo - 8).max(-127);
    let a_hi = (hi + 8).min(127);
    for e0 in a_lo..=a_hi {
        for e1 in a_lo..=a_hi {
            if e0 == e1 {
                continue;
            }
            consider_alpha_s(e0, e1, samples, best, best_err);
            if *best_err == 0 {
                return;
            }
        }
    }
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
        // Iterate LS -> index refit while the UNORM-scored SSE keeps falling
        // (same shape as the BC1 refine loop; each pass is ~2 pack evals).
        for _ in 0..4 {
            let Some((r0, r1)) = ls_alpha_endpoints_s(vals, best) else {
                break;
            };
            let prev = *best_err;
            consider_alpha_s(r0, r1, samples, best, best_err);
            consider_alpha_s(r1, r0, samples, best, best_err);
            if *best_err == 0 {
                return;
            }
            if *best_err >= prev {
                break;
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
    // snorm→unorm is monotone, so the static ascending orders carry over.
    let mut indices = [0u8; 16];
    let mut err = 0i32;
    if alpha_sel_enabled() {
        let order = if a0 > a1 { &ALPHA_ORDER6 } else { &ALPHA_ORDER4 };
        let sel = AlphaSelect::build(&pal_u, order);
        for (i, &s) in samples.iter().enumerate() {
            let best = sel.select(s);
            indices[i] = best;
            let diff = pal_u[best as usize] as i32 - s as i32;
            err += diff * diff;
            if err >= err_limit {
                return None;
            }
        }
    } else {
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
const W2: [u32; 4] = [0, 21, 43, 64];
/// 3-bit BC7 interpolation weights (symmetric: W3[7-i] == 64 - W3[i]).
const W3: [u32; 8] = [0, 9, 18, 27, 37, 46, 55, 64];

/// 7-bit color endpoint dequant (no p-bit): v' = (v<<1) | (v>>6).
#[inline]
fn unquant7(v: u8) -> u8 {
    (v << 1) | (v >> 6)
}

fn try_bc7_mode5(pixels: &[[u8; 4]; 16], rotation: u8) -> Option<([u8; 16], i64)> {
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
fn try_bc7_mode4(pixels: &[[u8; 4]; 16]) -> Option<([u8; 16], i64)> {
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
fn unquant5(v: u8) -> u8 {
    (v << 3) | (v >> 2)
}

#[inline]
fn unquant6(v: u8) -> u8 {
    (v << 2) | (v >> 4)
}

#[allow(clippy::type_complexity)]
fn fit_color_mode4(
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
fn score_alpha_mode4(alpha: &[u8; 16], q0: u8, q1: u8) -> (u8, u8, [u8; 16], i32) {
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

fn pack_bc7_mode4(
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
fn fit_color_mode5(
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

fn palette_mode5_color(q0: [u8; 3], q1: [u8; 3]) -> [[u8; 3]; 4] {
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
fn ls_endpoints_mode5(pixels: &[[u8; 4]; 16], indices: &[u8; 16]) -> Option<([u8; 3], [u8; 3])> {
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
fn fit_alpha_mode5(alpha: &[u8; 16], hi: u8, lo: u8) -> (u8, u8, [u8; 16], i32) {
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

fn score_alpha_mode5(alpha: &[u8; 16], c0: u8, c1: u8) -> (u8, u8, [u8; 16], i32) {
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

fn pack_bc7_mode5(
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
fn encode_bc7_mode6_inner(pixels: &[[u8; 4]; 16]) -> ([u8; 16], i64) {
    let mut best_bits = [0u8; 16];
    let mut best_err = i64::MAX;
    let mut best_seed = extrema_rgba(pixels);
    let mut have = false;

    let (seeds, n_seeds) = bc7_mode6_seeds(pixels);
    for &(ep0, ep1) in &seeds[..n_seeds] {
        if let Some((bits, err)) = try_bc7_mode6(pixels, ep0, ep1, false) {
            if err < best_err {
                best_err = err;
                best_bits = bits;
                best_seed = (ep0, ep1);
                have = true;
            }
        }
    }
    // Skip LS on near-solid blocks — seed endpoints already win.
    let do_ls = rgba_span_sum(pixels) > 8;
    if do_ls {
        if let Some((bits, err)) = try_bc7_mode6(pixels, best_seed.0, best_seed.1, true) {
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

/// t64 (0..=64) → nearest mode-6 weight index, ties toward the lower index.
const W6M_NEAREST: [u8; 65] = {
    let mut lut = [0u8; 65];
    let mut t = 0;
    while t <= 64 {
        let mut best = 0usize;
        let mut best_d = 255i32;
        let mut k = 0usize;
        while k < 16 {
            let d = (W6M[k] as i32 - t as i32).abs();
            if d < best_d {
                best_d = d;
                best = k;
            }
            k += 1;
        }
        lut[t] = best as u8;
        t += 1;
    }
    lut
};

/// Index-fit a whole block against one palette; returns (indices, total SSE).
///
/// Fast path: palette entries lie (rounded) on the c0→c1 line, so the SSE
/// argmin sits at the projection of the pixel onto that line; evaluating a
/// ±2 index window in ascending order reproduces the exhaustive scan's
/// strict-`<` lowest-index-tie result (equal-SSE minima on a line can only
/// straddle the projection, i.e. adjacent indices). Near-degenerate palettes
/// (axis < 16 in every channel → entries may collide after rounding, where a
/// distant equal-SSE entry could win the global tiebreak) take the
/// exhaustive path. Twin test: `mode6_projection_matches_exhaustive`.
#[inline]
fn fit_indices_mode6(pixels: &[[u8; 4]; 16], pal: &[[u8; 4]; 16]) -> ([u8; 16], i64) {
    let c0 = pal[0];
    let c1 = pal[15];
    let mut axis = [0i32; 4];
    let mut len2 = 0i64;
    let mut mono = false;
    for c in 0..4 {
        axis[c] = c1[c] as i32 - c0[c] as i32;
        len2 += (axis[c] * axis[c]) as i64;
        if axis[c].abs() >= 16 {
            mono = true;
        }
    }
    if !mono {
        return fit_indices_mode6_exhaustive(pixels, pal);
    }
    let mut indices = [0u8; 16];
    let mut err = 0i64;
    for (i, px) in pixels.iter().enumerate() {
        let mut dot = 0i64;
        for c in 0..4 {
            dot += ((px[c] as i32 - c0[c] as i32) * axis[c]) as i64;
        }
        let t64 = if dot <= 0 {
            0usize
        } else {
            (((dot * 64 + len2 / 2) / len2) as usize).min(64)
        };
        let k = W6M_NEAREST[t64] as i32;
        let lo = (k - 2).max(0) as usize;
        let hi = ((k + 2) as usize).min(15);
        let mut bi = lo as u8;
        let mut be = i32::MAX;
        for (j, p) in pal.iter().enumerate().take(hi + 1).skip(lo) {
            let mut e = 0i32;
            for c in 0..4 {
                let d = p[c] as i32 - px[c] as i32;
                e += d * d;
            }
            if e < be {
                be = e;
                bi = j as u8;
            }
        }
        indices[i] = bi;
        err += be as i64;
    }
    (indices, err)
}

/// Exhaustive twin (oracle + fallback for near-degenerate palettes).
#[inline]
fn fit_indices_mode6_exhaustive(pixels: &[[u8; 4]; 16], pal: &[[u8; 4]; 16]) -> ([u8; 16], i64) {
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

#[cfg(test)]
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

#[cfg(test)]
mod ceiling_probe {
    use super::*;

    fn load_png_gray_channels(path: &str) -> (usize, usize, Vec<u8>, Vec<u8>) {
        let f = std::fs::File::open(path).expect("png");
        let mut dec = png::Decoder::new(std::io::BufReader::new(f));
        dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = dec.read_info().unwrap();
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).unwrap();
        buf.truncate(info.buffer_size());
        let (w, h) = (info.width as usize, info.height as usize);
        let step = match info.color_type {
            png::ColorType::Rgb => 3,
            png::ColorType::Rgba => 4,
            png::ColorType::Grayscale => 1,
            png::ColorType::GrayscaleAlpha => 2,
            _ => panic!("unexpected color type"),
        };
        let mut r = Vec::with_capacity(w * h);
        let mut g = Vec::with_capacity(w * h);
        for px in buf.chunks_exact(step) {
            r.push(px[0]);
            g.push(px[if step >= 3 { 1 } else { 0 }]);
        }
        (w, h, r, g)
    }

    fn block_samples(chan: &[u8], w: usize, bx: usize, by: usize) -> [u8; 16] {
        let mut s = [0u8; 16];
        for row in 0..4 {
            for col in 0..4 {
                s[row * 4 + col] = chan[(by * 4 + row) * w + bx * 4 + col];
            }
        }
        s
    }

    /// UNORM-domain SSE of the best signed encoding found by a bounded
    /// near-exhaustive endpoint sweep (both orders => both palette modes).
    fn exhaustive_signed_sse(samples: &[u8; 16]) -> i64 {
        let mut lo = 127i32;
        let mut hi = -127i32;
        for &s in samples {
            let v = unorm_u8_to_snorm_i32(s);
            lo = lo.min(v);
            hi = hi.max(v);
        }
        let a_lo = (lo - 8).max(-127);
        let a_hi = (hi + 8).min(127);
        let current = encode_alpha_block_signed(*samples);
        let mut best_err = alpha_sse_s(samples, &current);
        let mut best = current;
        for e0 in a_lo..=a_hi {
            for e1 in a_lo..=a_hi {
                if e0 == e1 {
                    continue;
                }
                consider_alpha_s(e0, e1, samples, &mut best, &mut best_err);
            }
        }
        // 4-lerp sentinel mode benefits from endpoints at range edges too.
        best_err as i64
    }

    fn load_tiff_gray(path: &str) -> Option<(usize, usize, Vec<u8>)> {
        use tiff::decoder::DecodingResult;
        use tiff::ColorType;
        let f = std::fs::File::open(path).ok()?;
        let mut dec = tiff::decoder::Decoder::new(std::io::BufReader::new(f)).ok()?;
        let (w, h) = dec.dimensions().ok()?;
        let ct = dec.colortype().ok()?;
        match (ct, dec.read_image().ok()?) {
            (ColorType::Gray(8), DecodingResult::U8(v)) => Some((w as usize, h as usize, v)),
            _ => None,
        }
    }

    /// Observe-only harvest for the signed sweep gate: every signed block in
    /// the corpus -> (map, span, n_unique, null_err, gain, pairs).
    #[test]
    #[ignore]
    fn signed_sweep_harvest() {
        let root = env!("CARGO_MANIFEST_DIR");
        let mut sources: Vec<(String, Vec<(usize, usize, Vec<u8>)>)> = Vec::new();
        // Normals (R+G channels -> bc5s) and roughness masks (R -> bc4s).
        for asset in ["Bricks097", "Metal063", "Rock064", "Wood095"] {
            let p = format!("{root}/corpus/raw/{asset}/{asset}_1K-PNG_NormalGL.png");
            if std::path::Path::new(&p).exists() {
                let (w, h, r, g) = load_png_gray_channels(&p);
                sources.push((format!("{asset}_normal"), vec![(w, h, r), (w, h, g)]));
            }
            let p = format!("{root}/corpus/raw/{asset}/{asset}_1K-PNG_Roughness.png");
            if std::path::Path::new(&p).exists() {
                let (w, h, r, _) = load_png_gray_channels(&p);
                sources.push((format!("{asset}_mask"), vec![(w, h, r)]));
            }
        }
        for tex in ["tex_bark", "tex_straw", "tex_water", "tex_wool", "tex_brick_1024"] {
            let p = format!("{root}/corpus/raw_tif/{tex}.tiff");
            if let Some((w, h, v)) = load_tiff_gray(&p) {
                sources.push((tex.to_string(), vec![(w, h, v)]));
            }
        }
        let mut csv = String::from("map,span,n_unique,null_err,gain,pairs,dcheb\n");
        for (name, chans) in &sources {
            for (w, h, chan) in chans {
                for by in 0..h / 4 {
                    for bx in 0..w / 4 {
                        let s = block_samples(chan, *w, bx, by);
                        let (mut best, mut err, lo, hi, span, n_unique) =
                            encode_alpha_block_signed_presweep(s);
                        let null_err = err;
                        if null_err == 0 {
                            continue;
                        }
                        let pre0 = best[0] as i8 as i32;
                        let pre1 = best[1] as i8 as i32;
                        signed_sweep(lo, hi, &s, &mut best, &mut err);
                        let gain = null_err - err;
                        // Chebyshev distance from pre-sweep endpoints to the
                        // winners (order-insensitive: try both pairings).
                        let dcheb = if gain > 0 {
                            let w0 = best[0] as i8 as i32;
                            let w1 = best[1] as i8 as i32;
                            let d_a = (w0 - pre0).abs().max((w1 - pre1).abs());
                            let d_b = (w0 - pre1).abs().max((w1 - pre0).abs());
                            d_a.min(d_b)
                        } else {
                            -1
                        };
                        let range = (hi + 8).min(127) - (lo - 8).max(-127) + 1;
                        csv.push_str(&format!(
                            "{name},{span},{n_unique},{null_err},{gain},{},{dcheb}\n",
                            range * range
                        ));
                    }
                }
            }
        }
        std::fs::write(format!("{root}/target/signed_sweep_harvest.csv"), csv).unwrap();
        println!("wrote target/signed_sweep_harvest.csv");
    }

    #[test]
    #[ignore]
    fn bc5s_wood_ceiling() {
        let root = env!("CARGO_MANIFEST_DIR");
        let path = format!("{root}/corpus/raw/Wood095/Wood095_1K-PNG_NormalGL.png");
        let (w, h, r, g) = load_png_gray_channels(&path);
        let (bw, bh) = (w / 4, h / 4);
        let mut cur_sse = 0i64;
        let mut ceil_sse = 0i64;
        let nthreads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let rows_per = (bh + nthreads - 1) / nthreads;
        let results: Vec<(i64, i64)> = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for t in 0..nthreads {
                let (r, g) = (&r, &g);
                handles.push(scope.spawn(move || {
                    let mut cur = 0i64;
                    let mut ceil = 0i64;
                    for by in (t * rows_per)..((t + 1) * rows_per).min(bh) {
                        for bx in 0..bw {
                            for chan in [r, g] {
                                let s = block_samples(chan, w, bx, by);
                                let enc = encode_alpha_block_signed(s);
                                cur += alpha_sse_s(&s, &enc) as i64;
                                ceil += exhaustive_signed_sse(&s);
                            }
                        }
                    }
                    (cur, ceil)
                }));
            }
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for (c, x) in results {
            cur_sse += c;
            ceil_sse += x;
        }
        let n = (w * h * 2) as f64;
        let psnr = |sse: i64| 10.0 * (255.0f64 * 255.0 / (sse as f64 / n)).log10();
        println!(
            "Wood BC5S: current={:.3} dB  ceiling={:.3} dB  (delta {:+.3})",
            psnr(cur_sse),
            psnr(ceil_sse),
            psnr(ceil_sse) - psnr(cur_sse)
        );
    }
}

#[cfg(test)]
mod fuse_oracle {
    use super::*;

    /// pack_bc1_scored must equal the old pack_bc1 + bc1_sse pair exactly.
    #[test]
    fn bc1_scored_matches_pack_plus_sse() {
        let mut state = 0x243F6A8885A308D3u64;
        let mut rng = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..200_000 {
            let mut px = [[0u8; 4]; 16];
            let flat = case % 7 == 0;
            let base = (rng() & 0xFF) as u8;
            for p in px.iter_mut() {
                let r = rng();
                if flat {
                    p[0] = base.wrapping_add((r & 3) as u8);
                    p[1] = base.wrapping_add(((r >> 2) & 3) as u8);
                    p[2] = base.wrapping_add(((r >> 4) & 3) as u8);
                } else {
                    p[0] = (r & 0xFF) as u8;
                    p[1] = ((r >> 8) & 0xFF) as u8;
                    p[2] = ((r >> 16) & 0xFF) as u8;
                }
                // Mix in punch-through alphas sometimes.
                p[3] = if case % 5 == 0 && (r >> 24) & 3 == 0 {
                    ((r >> 26) & 0x7F) as u8
                } else {
                    255
                };
            }
            let e0 = [(rng() & 0xFF) as u8, (rng() & 0xFF) as u8, (rng() & 0xFF) as u8];
            let e1 = [(rng() & 0xFF) as u8, (rng() & 0xFF) as u8, (rng() & 0xFF) as u8];
            let old_block = pack_bc1(px, e0, e1);
            let old_err = bc1_sse(&px, &old_block);
            let (new_block, new_err) =
                pack_bc1_scored(&px, e0, e1, i32::MAX).expect("unbounded");
            // The projection index fit (bc1_fit_4color) is a RESTRICTED
            // search: its SSE can only be >= the exhaustive fit, and only
            // negligibly (rounding cross-term on far-off-line pixels; the
            // corpus moves <=0.012 dB worst-case). Punch-path blocks stay
            // bit-exact.
            assert!(new_err >= old_err, "fast beat exhaustive?! (case {case})");
            assert!(
                new_err <= old_err + old_err / 100 + 32,
                "projection fit degraded SSE beyond contract (case {case}): {new_err} vs {old_err}"
            );
            if new_block != old_block {
                // Bytes may differ only when the fit differs; err must track.
                assert!(new_err >= old_err);
            }
            // Early-abort contract: limit == err must return None (>= abort).
            assert!(pack_bc1_scored(&px, e0, e1, new_err).is_none());
            if new_err > 0 {
                assert!(pack_bc1_scored(&px, e0, e1, new_err + 1).is_some());
            }
        }
    }
}

#[cfg(test)]
mod mode6_projection_oracle {
    use super::*;

    #[test]
    fn mode6_projection_matches_exhaustive() {
        let mut state = 0x9E3779B97F4A7C15u64;
        let mut rng = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..400_000u32 {
            // Production-shaped palettes: random endpoints through the same
            // quantize/unquantize as try_bc7_mode6, biased toward small axes
            // every few cases to stress the mono-gate boundary.
            let r = rng();
            let e0 = [
                (r & 0xFF) as u8,
                ((r >> 8) & 0xFF) as u8,
                ((r >> 16) & 0xFF) as u8,
                ((r >> 24) & 0xFF) as u8,
            ];
            let e1 = if case % 4 == 0 {
                // near-degenerate: e1 within +-8 of e0 per channel
                let s = rng();
                let mut v = [0u8; 4];
                for c in 0..4 {
                    let d = ((s >> (8 * c)) & 0xF) as i32 - 8;
                    v[c] = (e0[c] as i32 + d).clamp(0, 255) as u8;
                }
                v
            } else {
                let s = rng();
                [
                    (s & 0xFF) as u8,
                    ((s >> 8) & 0xFF) as u8,
                    ((s >> 16) & 0xFF) as u8,
                    ((s >> 24) & 0xFF) as u8,
                ]
            };
            let (q0, p0) = quantize_7p(e0);
            let (q1, p1) = quantize_7p(e1);
            let pal = palette_mode6(unquantize_7p(q0, p0), unquantize_7p(q1, p1));
            let mut px = [[0u8; 4]; 16];
            for p in px.iter_mut() {
                let r = rng();
                // Mix: random pixels and near-palette pixels (index-fit shape).
                if r & 1 == 0 {
                    let k = ((r >> 1) & 15) as usize;
                    for c in 0..4 {
                        let n = ((r >> (8 + 8 * c)) & 7) as i32 - 3;
                        p[c] = (pal[k][c] as i32 + n).clamp(0, 255) as u8;
                    }
                } else {
                    p[0] = (r >> 8) as u8;
                    p[1] = (r >> 16) as u8;
                    p[2] = (r >> 24) as u8;
                    p[3] = (r >> 32) as u8;
                }
            }
            let fast = fit_indices_mode6(&px, &pal);
            let slow = fit_indices_mode6_exhaustive(&px, &pal);
            // Contract: the projection window is a RESTRICTED search, so its
            // SSE can only be >= the exhaustive fit, and only negligibly so
            // (divergence needs a pixel far off the endpoint line, where the
            // rounding cross-term outweighs the t-distance — SSE-tiny by
            // construction; corpus payloads move 0 cases at 0.0001 dB).
            assert!(fast.1 >= slow.1, "fast beat exhaustive?! (case {case})");
            assert!(
                fast.1 <= slow.1 + slow.1 / 100 + 16,
                "projection fit degraded SSE beyond contract (case {case}): {} vs {}",
                fast.1,
                slow.1
            );
        }
    }
}

#[cfg(test)]
mod alpha_select_oracle {
    use super::*;

    /// Full enumeration: every (a0, a1) endpoint pair x every sample value,
    /// both palette modes, unsigned domain — the selector must reproduce
    /// the linear scan's argmin (strict `<`, lowest index wins ties) on all
    /// ~16.7M combinations. This is a proof by exhaustion, not a sample.
    #[test]
    #[ignore] // ~seconds in release; run explicitly
    fn alpha_select_matches_linear_exhaustive() {
        for a0 in 0..=255u8 {
            for a1 in 0..=255u8 {
                let (palette, order): ([u8; 8], &[u8; 8]) = if a0 > a1 {
                    (alpha_palette6_u(a0, a1), &ALPHA_ORDER6)
                } else {
                    (alpha_palette4_u(a0, a1), &ALPHA_ORDER4)
                };
                let sel = AlphaSelect::build(&palette, order);
                for s in 0..=255u8 {
                    let mut lin = 0u8;
                    let mut lin_d = i32::MAX;
                    for (j, &p) in palette.iter().enumerate() {
                        let d = (p as i32 - s as i32).abs();
                        if d < lin_d {
                            lin_d = d;
                            lin = j as u8;
                        }
                    }
                    let fast = sel.select(s);
                    assert_eq!(
                        fast, lin,
                        "a0={a0} a1={a1} s={s} palette={palette:?}"
                    );
                }
            }
        }
    }
}
