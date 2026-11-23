//! One-block LDR encoder.
//!
//! Search (written from the specification and the HPG 2012 paper; see
//! docs/plans/astc.md section 7):
//!
//! 1. A constant block becomes an LDR void-extent block.
//! 2. For each partitioning considered (one partition, then the best-ranked
//!    two-partition patterns after a two-means clustering), each partition
//!    gets a principal-axis line fit: ideal endpoints and an ideal weight per
//!    texel. Dual-plane candidates fit one channel on its own line.
//! 3. For every legal block mode, the ideal weights are fitted to the mode's
//!    weight grid (least squares through the exact infill), quantised to its
//!    weight range, and an error estimate (weight error along each line plus
//!    the expected endpoint quantisation error at the colour range the
//!    remaining bits allow) ranks the candidate.
//! 4. The best few candidates are encoded for real: endpoints are refitted to
//!    the quantised weights (least squares), quantised in every applicable
//!    endpoint mode, and the block is evaluated EXACTLY with the decoder's own
//!    endpoint and interpolation primitives. Weights are then re-derived from
//!    the decoded endpoints and the loop repeats.
//!
//! Estimates only rank; the final choice is the exact decoded error, so the
//! encoder's reconstruction is the decoder's output by construction.

use alloc::vec::Vec;

use super::context::{label_bits, Ctx, Mode, PartBits};
use crate::astc::cem;
use crate::astc::ise::LEVELS;
use crate::astc::parse::{BlockKind, SymbolicBlock};
use crate::astc::quant::COLOR_NEAREST;
use crate::astc::texel::lerp;
use crate::astc::AstcProfile;

/// Preset-derived search limits.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Params {
    /// Highest partition count tried (1 or 2 today).
    pub max_partitions: u8,
    /// Patterns fully searched for 2, 3 and 4 partitions, best clustering
    /// match first.
    pub part_candidates: [usize; 3],
    /// Of those, how many are fully searched, ranked by their line-fit
    /// error (cheap); a candidate whose line-fit error alone already
    /// exceeds the best block is skipped.
    pub part_search: [usize; 3],
    /// Candidates (mode x endpoint class) encoded exactly per partitioning.
    pub exact_candidates: usize,
    /// Weight/endpoint refinement passes, for the best `refine_top`
    /// candidates after one unrefined pass over all of them.
    pub refine: u8,
    pub refine_top: usize,
    /// Try dual-plane modes when one channel is poorly correlated.
    pub dual_plane: bool,
    /// Lowest inter-channel correlation below which dual plane is tried.
    pub dual_corr_limit: f32,
    /// Least-squares iterations when fitting weights to a decimated grid.
    pub grid_iters: u8,
    /// Stop after the one-partition search when its per-channel MSE (8-bit
    /// domain) is at most this times (texels / 16)^2.
    pub part2_skip_err: f32,
    /// Search only the most used block modes covering this share of blocks
    /// (per mille, see `shortlist.rs`); 1000 searches every mode.
    pub mode_coverage: u16,
}

/// What the error is measured against.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Target {
    pub profile: AstcProfile,
    /// Measure after the `decode_unorm8` rule (top 8 bits). Always true for sRGB.
    pub unorm8: bool,
    pub cw: [f32; 4],
}

pub(super) const MAX_TEXELS: usize = 216;

/// Squared colour quantisation step (255 / (levels - 1))^2 per range
/// (baked literals: float const fn needs Rust 1.82; each is the shortest
/// decimal for the f32 nearest the f64 quotient, `qstep2_table` checks).
#[rustfmt::skip]
static QSTEP2: [f32; 21] = [65025.0, 16256.25, 7225.0, 4064.0625, 2601.0, 1327.0408, 802.7778, 537.39667, 289.0, 180.12465, 122.92061, 67.663895, 42.75148, 29.436396, 16.38322, 10.4190035, 7.204986, 4.031558, 2.5720897, 1.7824347, 1.0];

/// Endpoint classes searched (the value count decides the colour range).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Lum,       // CEM 0: 2 values
    LumAlpha,  // CEM 4: 4 values
    RgbScale,  // CEM 6: 4 values
    Rgb,       // CEM 8 / 9: 6 values
    RgbScaleA, // CEM 10: 6 values
    Rgba,      // CEM 12 / 13: 8 values
}

impl Class {
    fn values(self) -> u32 {
        match self {
            Class::Lum => 2,
            Class::LumAlpha | Class::RgbScale => 4,
            Class::Rgb | Class::RgbScaleA => 6,
            Class::Rgba => 8,
        }
    }
    /// Endpoint channels the quantisation error lands on.
    fn channels(self) -> f32 {
        match self {
            Class::Lum => 3.0,
            _ => 4.0,
        }
    }
}

/// A line fit of one partition.
#[derive(Clone, Copy, Default)]
struct Line {
    e0: [f32; 4],
    e1: [f32; 4],
    /// Weighted squared length of e1 - e0 over the plane-1 channels.
    len2: f32,
    /// Squared length along the plane-2 channel (dual plane only).
    len2b: f32,
}

/// One partitioning with its ideal fit. Lives in [`FitPool`] and is
/// refilled in place for each block: only the cells a reader can reach are
/// written (`labels`, `w1`, `tw1`, `tw2` for texels `0..n`, each partition's
/// `idx` row up to its count, and `w2` when there is a second plane, the only
/// case that reads it).
pub(crate) struct Fit {
    parts: u8,
    seed: u16,
    labels: [u8; MAX_TEXELS],
    lines: [Line; 4],
    /// Ideal weights in [0, 1]: plane 1, plane 2.
    w1: [f32; MAX_TEXELS],
    w2: [f32; MAX_TEXELS],
    /// Dual-plane channel, 4 = none.
    ccs: usize,
    /// Fixed error off the fitted lines (not reducible by weights).
    perp: f32,
    /// Per texel: its partition line's len2 and len2b (residual weights).
    tw1: [f32; MAX_TEXELS],
    tw2: [f32; MAX_TEXELS],
    /// Texel indices of each partition, and their counts.
    idx: [[u8; MAX_TEXELS]; 4],
    cnt: [usize; 4],
}

impl Fit {
    fn part(&self, p: usize) -> &[u8] {
        &self.idx[p][..self.cnt[p]]
    }

    fn new() -> Self {
        Fit {
            parts: 0,
            seed: 0,
            labels: [0; MAX_TEXELS],
            lines: [Line::default(); 4],
            w1: [0.0; MAX_TEXELS],
            w2: [0.0; MAX_TEXELS],
            ccs: 4,
            perp: 0.0,
            tw1: [0.0; MAX_TEXELS],
            tw2: [0.0; MAX_TEXELS],
            idx: [[0; MAX_TEXELS]; 4],
            cnt: [0; 4],
        }
    }
}

/// Fits reused across the blocks of one encode call (the context owns it),
/// so a fit is neither zero-filled nor moved per block: slot 0 is the
/// one-partition fit, slot 1 the dual-plane one, slots 2.. the candidate
/// partitionings. `order` and `top` are the partition ranking's scratch.
#[derive(Default)]
pub(crate) struct FitPool {
    fits: Vec<Fit>,
    order: Vec<usize>,
    top: Vec<(u32, usize)>,
    search: SearchScratch,
}

/// The mode search's working vectors, reused across calls (cleared, never
/// shrunk): one allocation each per encode call instead of per fit / per
/// candidate.
#[derive(Default)]
struct SearchScratch {
    cands: Vec<Cand>,
    top: Vec<usize>,
    keep: Vec<usize>,
    tried: Vec<Tried>,
    encs: Vec<(u8, [u8; 8])>,
    /// `exact_candidate`'s best encoding per (CEM, partition). Not cleared
    /// between passes: a cell is valid only while its CEM's bit is set in
    /// that pass's per-partition mask.
    chosen: [[(f64, [u8; 8]); 4]; 16],
}

/// The encoding a pass of `exact_candidate` chose: its CEM and each
/// partition's values.
type Pick = (usize, [[u8; 8]; 4]);

/// One unrefined candidate of `search_fit`: its exact error, candidate
/// index, grid weights (planes 1 and 2) and chosen encoding.
type Tried = (f64, usize, [u8; 64], [u8; 64], Option<Pick>);

/// The best block found so far.
pub(crate) struct Best {
    pub err: f64,
    pub sym: SymbolicBlock,
}

struct Src<'a> {
    px: &'a [[u8; 4]],
    n: usize,
    tgt: Target,
    opaque: bool,
    grey: bool,
}

fn dot4(a: [f32; 4], b: [f32; 4]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3]
}

/// Principal-axis line fit of the texels `idx` over the channels in `mask`.
/// Returns the line and fills `w` (ideal weights) for those texels; returns
/// the residual error off the line.
// One index walks several parallel per-texel / per-channel arrays (and in the encoder fixes the floating-point operation order); iterator chains would obscure that.
#[allow(clippy::needless_range_loop)]
fn fit_line(src: &Src, idx: &[u8], mask: [bool; 4], w: &mut [f32; MAX_TEXELS]) -> (Line, f32) {
    let cw = src.tgt.cw;
    let n = idx.len().max(1) as f32;
    let mut mean = [0f32; 4];
    for &i in idx {
        let i = i as usize;
        for c in 0..4 {
            mean[c] += src.px[i][c] as f32;
        }
    }
    for m in mean.iter_mut() {
        *m /= n;
    }
    // Covariance over the masked channels, weighted.
    let mut cov = [[0f32; 4]; 4];
    for &i in idx {
        let i = i as usize;
        let mut d = [0f32; 4];
        for c in 0..4 {
            if mask[c] {
                d[c] = (src.px[i][c] as f32 - mean[c]) * cw[c];
            }
        }
        for a in 0..4 {
            for b in 0..4 {
                cov[a][b] += d[a] * d[b];
            }
        }
    }
    // Power iteration from the dominant-variance axis; normalised by the max
    // component (no square root, so no libm in the determinism surface).
    let mut dir = [0f32; 4];
    let mut best = -1.0f32;
    for c in 0..4 {
        if mask[c] && cov[c][c] > best {
            best = cov[c][c];
            dir = [0.0; 4];
            dir[c] = 1.0;
        }
    }
    for _ in 0..8 {
        let mut nd = [0f32; 4];
        for a in 0..4 {
            nd[a] =
                cov[a][0] * dir[0] + cov[a][1] * dir[1] + cov[a][2] * dir[2] + cov[a][3] * dir[3];
        }
        let mx = nd.iter().fold(0f32, |m, &v| m.max(v.abs()));
        if mx <= 1e-12 {
            break;
        }
        for v in nd.iter_mut() {
            *v /= mx;
        }
        dir = nd;
    }
    // Undo the channel weighting in the direction, then orient so the RGB sum
    // grows from e0 to e1 (direct endpoint modes need sum(e1) >= sum(e0)).
    for c in 0..4 {
        if cw[c] > 0.0 {
            dir[c] /= cw[c];
        }
        if !mask[c] {
            dir[c] = 0.0;
        }
    }
    let s = dir[0] + dir[1] + dir[2];
    if s < 0.0 || (s == 0.0 && dir[3] < 0.0) {
        for v in dir.iter_mut() {
            *v = -*v;
        }
    }
    let dd = dot4(dir, dir);
    let mut tmin = f32::MAX;
    let mut tmax = f32::MIN;
    let mut ts = [0f32; MAX_TEXELS];
    for &i in idx {
        let i = i as usize;
        let mut d = [0f32; 4];
        for c in 0..4 {
            if mask[c] {
                d[c] = src.px[i][c] as f32 - mean[c];
            }
        }
        let t = if dd > 0.0 { dot4(d, dir) / dd } else { 0.0 };
        ts[i] = t;
        tmin = tmin.min(t);
        tmax = tmax.max(t);
    }
    if idx.is_empty() {
        tmin = 0.0;
        tmax = 0.0;
    }
    let span = tmax - tmin;
    let mut perp = 0f32;
    for &i in idx {
        let i = i as usize;
        w[i] = if span > 0.0 {
            (ts[i] - tmin) / span
        } else {
            0.0
        };
        for c in 0..4 {
            if mask[c] {
                let p = mean[c] + dir[c] * ts[i];
                let e = (src.px[i][c] as f32 - p) * cw[c];
                perp += e * e;
            }
        }
    }
    let mut line = Line::default();
    for c in 0..4 {
        if mask[c] {
            line.e0[c] = (mean[c] + dir[c] * tmin).clamp(0.0, 255.0);
            line.e1[c] = (mean[c] + dir[c] * tmax).clamp(0.0, 255.0);
            let d = (line.e1[c] - line.e0[c]) * cw[c];
            line.len2 += d * d;
        } else {
            line.e0[c] = mean[c];
            line.e1[c] = mean[c];
        }
    }
    (line, perp)
}

/// Fit one channel on its own (dual plane): min/max endpoints, ideal weights.
fn fit_channel(src: &Src, idx: &[u8], c: usize, line: &mut Line, w: &mut [f32; MAX_TEXELS]) {
    let mut lo = 255u8;
    let mut hi = 0u8;
    for &i in idx {
        let i = i as usize;
        lo = lo.min(src.px[i][c]);
        hi = hi.max(src.px[i][c]);
    }
    if idx.is_empty() {
        lo = 0;
        hi = 0;
    }
    let span = (hi - lo) as f32;
    for &i in idx {
        let i = i as usize;
        w[i] = if span > 0.0 {
            (src.px[i][c] - lo) as f32 / span
        } else {
            0.0
        };
    }
    line.e0[c] = lo as f32;
    line.e1[c] = hi as f32;
    let d = span * src.tgt.cw[c];
    line.len2b = d * d;
}

/// Fill `f` with the ideal fit of one partitioning (see [`Fit`] for which
/// cells are written).
fn make_fit(f: &mut Fit, src: &Src, parts: u8, seed: u16, labels: &[u8], ccs: usize) {
    f.parts = parts;
    f.seed = seed;
    f.ccs = ccs;
    f.perp = 0.0;
    f.lines = [Line::default(); 4];
    f.cnt = [0; 4];
    f.labels[..src.n].copy_from_slice(&labels[..src.n]);
    for i in 0..src.n {
        let p = f.labels[i] as usize;
        f.idx[p][f.cnt[p]] = i as u8;
        f.cnt[p] += 1;
    }
    let mut mask = [true, true, true, !src.opaque];
    if ccs < 4 {
        mask[ccs] = false;
    }
    let Fit {
        idx,
        cnt,
        w1,
        w2,
        lines,
        perp: total,
        ..
    } = f;
    for p in 0..parts as usize {
        let idx = &idx[p][..cnt[p]];
        let (mut line, perp) = fit_line(src, idx, mask, w1);
        if ccs < 4 {
            fit_channel(src, idx, ccs, &mut line, w2);
        }
        lines[p] = line;
        *total += perp;
    }
    for i in 0..src.n {
        let l = &f.lines[f.labels[i] as usize];
        f.tw1[i] = l.len2;
        f.tw2[i] = l.len2b;
    }
}

/// Least-squares fit of per-texel ideal weights (scaled to 0..=64) to a
/// grid, through the infill taps. Returns grid weights in 0..=64 (float).
pub(super) fn decimate(
    ctx: &Ctx,
    grid: usize,
    ideal: &[f32; MAX_TEXELS],
    iters: u8,
    out: &mut [f32; 64],
) {
    let g = &ctx.grids[grid];
    if g.full {
        for (o, &w) in out[..g.count].iter_mut().zip(ideal.iter()) {
            *o = w * 64.0;
        }
        return;
    }
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if let Some(t) = ctx.avx2 {
        if super::simd::decimate(t, g, ideal, iters, out) {
            return;
        }
    }
    decimate_scalar(g, ideal, iters, out);
}

/// The scalar body of [`decimate`] for a grid that is not the identity: the
/// oracle of `simd::decimate`, and the kernel without AVX2.
// One index walks several parallel per-texel / per-channel arrays (and in the encoder fixes the floating-point operation order); iterator chains would obscure that.
#[allow(clippy::needless_range_loop)]
pub(super) fn decimate_scalar(
    g: &crate::astc::tables::Grid,
    ideal: &[f32; MAX_TEXELS],
    iters: u8,
    out: &mut [f32; 64],
) {
    // Start from the contribution-weighted average of the texels each stored
    // weight feeds.
    for j in 0..g.count {
        let mut s = 0f32;
        let r = g.feed_at[j] as usize..g.feed_at[j + 1] as usize;
        for (&t, &c) in g.feed_t[r.clone()].iter().zip(&g.feed_c[r]) {
            s += ideal[t as usize] * c;
        }
        let cs = g.feed_cs[j];
        out[j] = if cs > 0.0 { 64.0 * s / cs } else { 32.0 };
    }
    // Jacobi steps on sum_t (sum_k c_tk/16 g_k - 64 w_t)^2. The ranking
    // call asks for none: return before zero-filling the residual buffer.
    if iters == 0 {
        return;
    }
    let mut resid = [0f32; MAX_TEXELS];
    for _ in 0..iters {
        for (t, (ti, tc)) in g.tap_i.iter().zip(&g.tap_c).enumerate() {
            let mut v = 0f32;
            for k in 0..4 {
                // Grid indices are < 64 (mask: no bounds check).
                v += out[(ti[k] & 63) as usize] * tc[k];
            }
            resid[t] = v * (1.0 / 16.0) - 64.0 * ideal[t];
        }
        for j in 0..g.count {
            let mut num = 0f32;
            let r = g.feed_at[j] as usize..g.feed_at[j + 1] as usize;
            for (&t, &cf) in g.feed_t[r.clone()].iter().zip(&g.feed_c16[r]) {
                num += cf * resid[t as usize];
            }
            let den = g.feed_den[j];
            if den > 0.0 {
                out[j] = (out[j] - num / den).clamp(0.0, 64.0);
            }
        }
    }
}

const fn build_wq_lut() -> [[u8; 257]; 12] {
    // WQ_LUT[range][round(4 g)] = the weight level nearest g (0..=64), ties to
    // the lower level.
    let mut t = [[0u8; 257]; 12];
    let mut r = 0;
    while r < 12 {
        let n = LEVELS[r] as usize;
        let mut i = 0;
        while i < 257 {
            let mut best = 0u8;
            let mut bd = u32::MAX;
            let mut v = 0;
            while v < n {
                let l = crate::astc::quant::WEIGHT_UNQUANT_C[r][v] as u32;
                let x = i as u32;
                let d = (4 * l).abs_diff(x);
                if d < bd || (d == bd && (l as u8) < best) {
                    bd = d;
                    best = l as u8;
                }
                v += 1;
            }
            t[r][i] = best;
            i += 1;
        }
        r += 1;
    }
    t
}

static WQ_LUT: [[u8; 257]; 12] = build_wq_lut();

/// Quantise grid weights (0..=64 float) to the weight range's levels.
///
/// The index is `((v * 4 + 0.5) as usize).min(256)`, computed through a
/// saturating `i32` cast and an integer clamp: both casts truncate toward
/// zero and send NaN to 0; a negative value is 0 as `usize` and at most 0 as
/// `i32` (clamped to 0); a value past `i32::MAX` saturates and is clamped to
/// 256 either way. Same index for every input, without the 64-bit unsigned
/// conversion sequence.
pub(super) fn quantize_weights(range: usize, g: &[f32], out: &mut [u8]) {
    let lut = &WQ_LUT[range];
    for (o, &v) in out.iter_mut().zip(g) {
        *o = lut[((v * 4.0 + 0.5) as i32).clamp(0, 256) as usize];
    }
}

pub(super) fn infill_eff(ctx: &Ctx, grid: usize, gw: &[u8], out: &mut [u8; MAX_TEXELS]) {
    let g = &ctx.grids[grid];
    let gw: &[u8; 64] = gw[..64].try_into().unwrap();
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if let Some(t) = ctx.avx2 {
        if super::simd::infill_eff(t, g, gw, out) {
            return;
        }
    }
    infill_eff_scalar(g, gw, out);
}

/// The scalar body of [`infill_eff`]: the oracle of `simd::infill_eff`, and
/// the kernel without AVX2.
pub(super) fn infill_eff_scalar(
    g: &crate::astc::tables::Grid,
    gw: &[u8; 64],
    out: &mut [u8; MAX_TEXELS],
) {
    for (t, tp) in g.taps4.iter().enumerate() {
        let mut s = 0u32;
        for &(k, c) in tp {
            s += gw[(k & 63) as usize] as u32 * c as u32;
        }
        out[t] = ((s + 8) >> 4) as u8;
    }
}

/// The least-squares sums of [`refit_part`]: per plane, `a = sum u^2`,
/// `b = sum u t`, `d = sum t^2` (t = weight / 64, u = 1 - t), and per
/// channel `x0 = sum u x`, `x1 = sum t x`. Plane 2 sums stay 0 without a
/// second plane; channels at or past `chans` are not meaningful.
#[derive(Debug, PartialEq)]
pub(super) struct RefitSums {
    pub a: [f32; 2],
    pub b: [f32; 2],
    pub d: [f32; 2],
    pub x0: [f32; 4],
    pub x1: [f32; 4],
}

/// [`RefitSums`] one texel at a time: the oracle of `simd::refit_sums`, and
/// the kernel without it. The weight sums (a, b, d) depend only on the
/// plane, not the channel: accumulated once per plane, in the same texel
/// order as a per-channel fit would, so every result is bit-identical to
/// fitting each channel on its own.
#[cfg_attr(all(feature = "simd", target_arch = "x86_64"), allow(dead_code))]
pub(super) fn refit_sums_scalar(
    px: &[[u8; 4]],
    idx: &[u8],
    w1: &[u8; MAX_TEXELS],
    w2: &[u8; MAX_TEXELS],
    ccs: usize,
    chans: usize,
) -> RefitSums {
    let (mut a, mut b, mut d) = ([0f32; 2], [0f32; 2], [0f32; 2]);
    let (mut x0, mut x1) = ([0f32; 4], [0f32; 4]);
    let dual = ccs < 4;
    for &i in idx {
        let i = i as usize;
        let t1 = w1[i] as f32 * (1.0 / 64.0);
        let u1 = 1.0 - t1;
        a[0] += u1 * u1;
        b[0] += u1 * t1;
        d[0] += t1 * t1;
        let (t2, u2) = if dual {
            let t2 = w2[i] as f32 * (1.0 / 64.0);
            let u2 = 1.0 - t2;
            a[1] += u2 * u2;
            b[1] += u2 * t2;
            d[1] += t2 * t2;
            (t2, u2)
        } else {
            (t1, u1)
        };
        for c in 0..chans {
            let (t, u) = if c == ccs { (t2, u2) } else { (t1, u1) };
            let x = px[i][c] as f32;
            x0[c] += u * x;
            x1[c] += t * x;
        }
    }
    RefitSums { a, b, d, x0, x1 }
}

/// Least-squares endpoints for each channel of one partition given integer
/// texel weights (0..=64) per plane.
fn refit_part(
    src: &Src,
    idx: &[u8],
    w1: &[u8; MAX_TEXELS],
    w2: &[u8; MAX_TEXELS],
    ccs: usize,
    chans: usize,
) -> [Option<(f32, f32)>; 4] {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    let RefitSums { a, b, d, x0, x1 } = super::simd::refit_sums(src.px, idx, w1, w2, ccs);
    #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
    let RefitSums { a, b, d, x0, x1 } = refit_sums_scalar(src.px, idx, w1, w2, ccs, chans);
    let mut out = [None; 4];
    for c in 0..chans {
        let pl = usize::from(c == ccs);
        let det = a[pl] * d[pl] - b[pl] * b[pl];
        if det.abs() < 1e-6 {
            continue;
        }
        let e0 = (x0[c] * d[pl] - x1[c] * b[pl]) / det;
        let e1 = (x1[c] * a[pl] - x0[c] * b[pl]) / det;
        out[c] = Some((e0.clamp(0.0, 255.0), e1.clamp(0.0, 255.0)));
    }
    out
}

/// The colour level of `range` nearest `x`. After the float clamp the value
/// is NaN (cast to 0 either way) or in 0..=255, where an `i32` cast
/// truncates exactly as the former `usize` cast did: same index, without
/// the 64-bit unsigned conversion sequence (`(v as usize).min(255)` was
/// already a no-op clamp).
fn near(range: usize, x: f32) -> u8 {
    let v = (x + 0.5).clamp(0.0, 255.0) as i32;
    COLOR_NEAREST[range][(v as usize).min(255)]
}

/// Candidate endpoint encodings (CEM, values) for a class.
fn encode_endpoints(
    class: Class,
    range: usize,
    e0: [f32; 4],
    e1: [f32; 4],
    out: &mut Vec<(u8, [u8; 8])>,
) {
    out.clear();
    let lum = |e: [f32; 4]| (e[0] + e[1] + e[2]) * (1.0 / 3.0);
    match class {
        Class::Lum => {
            out.push((
                0,
                [near(range, lum(e0)), near(range, lum(e1)), 0, 0, 0, 0, 0, 0],
            ));
        }
        Class::LumAlpha => {
            out.push((
                4,
                [
                    near(range, lum(e0)),
                    near(range, lum(e1)),
                    near(range, e0[3]),
                    near(range, e1[3]),
                    0,
                    0,
                    0,
                    0,
                ],
            ));
        }
        Class::Rgb | Class::Rgba => {
            let alpha = class == Class::Rgba;
            let cem = if alpha { 12 } else { 8 };
            // Direct.
            let mut v = [0u8; 8];
            for c in 0..3 {
                v[2 * c] = near(range, e0[c]);
                v[2 * c + 1] = near(range, e1[c]);
            }
            if alpha {
                v[6] = near(range, e0[3]);
                v[7] = near(range, e1[3]);
            }
            out.push((cem, v));
            // Blue contraction: stored (2R - B, 2G - B, B), endpoints swapped.
            let inv = |e: [f32; 4]| [2.0 * e[0] - e[2], 2.0 * e[1] - e[2], e[2], e[3]];
            let (i0, i1) = (inv(e0), inv(e1));
            if (0..3).all(|c| (-0.5..=255.5).contains(&i0[c]) && (-0.5..=255.5).contains(&i1[c])) {
                let mut v = [0u8; 8];
                for c in 0..3 {
                    v[2 * c] = near(range, i1[c]);
                    v[2 * c + 1] = near(range, i0[c]);
                }
                if alpha {
                    v[6] = near(range, e1[3]);
                    v[7] = near(range, e0[3]);
                }
                out.push((cem, v));
            }
            // Base + offset (bit transfer): offsets in -32..=31.
            let mut ok = true;
            let mut v = [0u8; 8];
            let chans = if alpha { 4 } else { 3 };
            for c in 0..chans {
                let base = (e0[c] + 0.5).clamp(0.0, 255.0) as i32;
                let off = (e1[c] + 0.5).clamp(0.0, 255.0) as i32 - base;
                if !(-32..=31).contains(&off) {
                    ok = false;
                    break;
                }
                let lo = ((base & 0x7F) << 1) as f32;
                let hi = ((base & 0x80) | ((off & 0x3F) << 1)) as f32;
                v[2 * c] = near(range, lo);
                v[2 * c + 1] = near(range, hi);
            }
            if ok {
                out.push((if alpha { 13 } else { 9 }, v));
            }
        }
        Class::RgbScale | Class::RgbScaleA => {
            let l1 = dot4([e1[0], e1[1], e1[2], 0.0], [e1[0], e1[1], e1[2], 0.0]);
            let s = if l1 > 0.0 {
                256.0 * dot4([e0[0], e0[1], e0[2], 0.0], [e1[0], e1[1], e1[2], 0.0]) / l1
            } else {
                0.0
            };
            let mut v = [0u8; 8];
            v[0] = near(range, e1[0]);
            v[1] = near(range, e1[1]);
            v[2] = near(range, e1[2]);
            v[3] = near(range, s.min(255.0));
            if class == Class::RgbScaleA {
                v[4] = near(range, e0[3]);
                v[5] = near(range, e1[3]);
                out.push((10, v));
            } else {
                out.push((6, v));
            }
        }
    }
}

/// Exact error of one partition's texels for decoded endpoints `ep` and
/// texel weights (plane 1, plane 2).
fn exact_part_err(
    ctx: &Ctx,
    src: &Src,
    idx: &[u8],
    ep: &cem::Endpoints,
    w1: &[u8; MAX_TEXELS],
    w2: &[u8; MAX_TEXELS],
    ccs: usize,
) -> f64 {
    let cw = src.tgt.cw;
    // Opaque blocks are only ever given alpha-free endpoint classes, whose
    // decoded alpha is 0xFFFF: the alpha error is exactly 0, so skip it.
    let chans = if src.opaque { 3 } else { 4 };
    let (px, unorm8) = (src.px, src.tgt.unorm8);
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    let acc = super::simd::part_err_acc(
        ctx.quads,
        px,
        idx,
        ep,
        w1,
        w2,
        ccs,
        chans,
        unorm8,
        src.tgt.profile == AstcProfile::LdrSrgb,
    )
    .unwrap_or_else(|| part_err_acc_scalar(px, idx, ep, w1, w2, ccs, chans, unorm8));
    #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
    let acc = {
        let _ = ctx;
        part_err_acc_scalar(px, idx, ep, w1, w2, ccs, chans, unorm8)
    };
    let scale = if src.tgt.unorm8 {
        1.0
    } else {
        1.0 / (257.0 * 257.0)
    };
    (0..4).map(|c| acc[c] as f64 * cw[c] as f64 * scale).sum()
}

/// Per-channel sums of squared errors (8-bit target domain under
/// `decode_unorm8`, 16-bit otherwise) of the texels `idx`; channels at or
/// past `chans` stay 0. The oracle of `simd::part_err_acc`, and the kernel
/// on targets without it.
#[allow(clippy::too_many_arguments)]
pub(super) fn part_err_acc_scalar(
    px: &[[u8; 4]],
    idx: &[u8],
    ep: &cem::Endpoints,
    w1: &[u8; MAX_TEXELS],
    w2: &[u8; MAX_TEXELS],
    ccs: usize,
    chans: usize,
    unorm8: bool,
) -> [u64; 4] {
    let mut acc = [0u64; 4];
    for &i in idx {
        let i = i as usize;
        for c in 0..chans {
            let w = if c == ccs { w2[i] } else { w1[i] };
            let v = lerp(ep.e0[c], ep.e1[c], w) as i64;
            let s = px[i][c] as i64;
            let d = if unorm8 { (v >> 8) - s } else { v - s * 257 };
            acc[c] += (d * d) as u64;
        }
    }
    acc
}

/// Candidate classes for this block.
fn classes(src: &Src) -> &'static [Class] {
    match (src.opaque, src.grey) {
        (true, true) => &[Class::Lum, Class::Rgb],
        (true, false) => &[Class::Rgb, Class::RgbScale],
        (false, true) => &[Class::LumAlpha, Class::Rgba],
        (false, false) => &[Class::Rgba, Class::RgbScaleA],
    }
}

struct Cand {
    est: f32,
    mode: usize,
    class: Class,
    range: usize,
}

/// Encode one partitioning: rank modes by estimate, then encode the best few
/// exactly. Updates `best`.
///
/// The estimate of a mode is the grid's decimation residual (computed once per
/// grid) plus the expected quantisation noise of its weight range (scaled by
/// how much the grid's infill averages it away) plus the expected endpoint
/// quantisation error at the colour range the remaining bits allow.
fn search_fit(
    ctx: &Ctx,
    prm: &Params,
    src: &Src,
    fit: &Fit,
    best: &mut Best,
    ss: &mut SearchScratch,
) {
    let dual = fit.ccs < 4;
    let n = src.n;
    let cls = classes(src);
    // Sum of squared line lengths over texels (plane 1 and plane 2).
    let mut sum_len2 = 0f32;
    let mut sum_len2b = 0f32;
    for i in 0..n {
        let l = &fit.lines[fit.labels[i] as usize];
        sum_len2 += l.len2;
        sum_len2b += l.len2b;
    }
    // Model error of each class: base+scale forces e0 onto the line through
    // the origin and e1 (s * e1); weigh the endpoint miss by the texels near e0.
    let mut scale_err = [0f32; 2];
    for (ci, &class) in cls.iter().enumerate() {
        if matches!(class, Class::RgbScale | Class::RgbScaleA) {
            for p in 0..fit.parts as usize {
                let l = &fit.lines[p];
                let e1 = [l.e1[0], l.e1[1], l.e1[2], 0.0];
                let e0 = [l.e0[0], l.e0[1], l.e0[2], 0.0];
                let ll = dot4(e1, e1);
                let sc = if ll > 0.0 { dot4(e0, e1) / ll } else { 0.0 };
                let mut miss = 0f32;
                for c in 0..3 {
                    let d = (e0[c] - sc * e1[c]) * src.tgt.cw[c];
                    miss += d * d;
                }
                let mut wsum = 0f32;
                for i in 0..n {
                    if fit.labels[i] as usize == p {
                        let u = 1.0 - fit.w1[i];
                        wsum += u * u;
                    }
                }
                scale_err[ci] += miss * wsum;
            }
        }
    }
    // Per grid: decimated weights and the residual of their (unquantised) infill.
    let mut sc = ctx.scratch.borrow_mut();
    let sc = &mut *sc;
    sc.dec_err.iter_mut().for_each(|v| *v = -1.0);
    sc.refined.iter_mut().for_each(|v| *v = false);
    let (dec_err, dec1, dec2, refined) =
        (&mut sc.dec_err, &mut sc.dec1, &mut sc.dec2, &mut sc.refined);
    let SearchScratch {
        cands,
        top,
        keep,
        tried,
        encs,
        chosen,
    } = ss;
    cands.clear();
    let cw_mean = (src.tgt.cw[0] + src.tgt.cw[1] + src.tgt.cw[2] + src.tgt.cw[3]) * 0.25;
    // The best k by estimate, ties by candidate index (a total order; index
    // order is mode order), kept while the candidates are generated. The grid
    // residual (decimation + infill, the expensive part) is computed only
    // for a candidate that could still enter the top k: its estimate is the
    // residual (>= 0) plus a cheap rest, and once the k-th estimate is below
    // that rest the candidate cannot get in (the k-th estimate only falls).
    // The selected set is exactly the one a full evaluation selects.
    let k = prm.exact_candidates.max(1);
    top.clear();
    for (mi, m) in ctx.modes.iter().enumerate() {
        if m.dual != dual {
            continue;
        }
        let g = m.grid as usize;
        let levels_w = LEVELS[m.range as usize] as f32;
        let step_w = 1.0 / (levels_w - 1.0);
        let wq = step_w * step_w * (1.0 / 12.0) * ctx.grids[g].noise * (sum_len2 + sum_len2b);
        for (ci, &class) in cls.iter().enumerate() {
            let r = m.crange[fit.parts as usize - 1][(class.values() / 2 - 1) as usize];
            if r == 0xFF {
                continue;
            }
            let range = r as usize;
            let step = QSTEP2[range];
            let qerr = n as f32 * (2.0 / 3.0) * step * (1.0 / 12.0) * class.channels() * cw_mean;
            let i = cands.len();
            if top.len() == k && dec_err[g] < 0.0 {
                let rest = wq + qerr + fit.perp + scale_err[ci];
                if rest * (1.0 - 1e-5) > cands[top[k - 1]].est {
                    cands.push(Cand {
                        est: f32::INFINITY,
                        mode: mi,
                        class,
                        range,
                    });
                    continue;
                }
            }
            if dec_err[g] < 0.0 {
                // Rank on the one-shot (transpose-average) decimation; the
                // kept candidates get the least-squares refinement below.
                decimate(ctx, g, &fit.w1, 0, &mut dec1[g]);
                let mut e = residual(ctx, g, &dec1[g], &fit.w1, fit, false);
                if dual {
                    decimate(ctx, g, &fit.w2, 0, &mut dec2[g]);
                    e += residual(ctx, g, &dec2[g], &fit.w2, fit, true);
                }
                dec_err[g] = e;
            }
            let est = dec_err[g] + wq + qerr + fit.perp + scale_err[ci];
            cands.push(Cand {
                est,
                mode: mi,
                class,
                range,
            });
            let better = |a: usize, b: usize| {
                cands[a].est < cands[b].est || (cands[a].est == cands[b].est && a < b)
            };
            if top.len() == k && !better(i, top[k - 1]) {
                continue;
            }
            let pos = top.iter().position(|&t| better(i, t)).unwrap_or(top.len());
            top.insert(pos, i);
            top.truncate(k);
        }
    }
    // Each kept mode also gets its other endpoint classes (adjacent entries:
    // candidates are pushed per mode, class after class), each at the colour
    // range its own value count allows.
    keep.clear();
    keep.extend_from_slice(top);
    for &t in top.iter() {
        let m = cands[t].mode;
        let mut j = t;
        while j > 0 && cands[j - 1].mode == m {
            j -= 1;
        }
        while j < cands.len() && cands[j].mode == m {
            if !keep.contains(&j) {
                keep.push(j);
            }
            j += 1;
        }
    }
    tried.clear();
    for &i in keep.iter() {
        let c = &cands[i];
        let g = ctx.modes[c.mode].grid as usize;
        if !refined[g] && prm.grid_iters > 0 {
            decimate(ctx, g, &fit.w1, prm.grid_iters, &mut dec1[g]);
            if dual {
                decimate(ctx, g, &fit.w2, prm.grid_iters, &mut dec2[g]);
            }
            refined[g] = true;
        }
        let count = ctx.grids[g].count;
        let mut gw1 = [0u8; 64];
        let mut gw2 = [0u8; 64];
        quantize_weights(
            ctx.modes[c.mode].range as usize,
            &dec1[g][..count],
            &mut gw1[..count],
        );
        if dual {
            quantize_weights(
                ctx.modes[c.mode].range as usize,
                &dec2[g][..count],
                &mut gw2[..count],
            );
        }
        // One unrefined pass for every kept candidate ...
        let (e, pick) =
            exact_candidate(ctx, prm, src, fit, c, gw1, gw2, 0, None, best, encs, chosen);
        tried.push((e, i, gw1, gw2, pick));
    }
    // ... and the refinement passes only for the best few.
    if prm.refine > 0 {
        tried.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(core::cmp::Ordering::Equal)
                .then(a.1.cmp(&b.1))
        });
        // A refinement's first pass would repeat the unrefined pass above
        // (same weights, same candidate): it resumes from that pass's
        // encoding instead. No encoding there means the first pass would
        // have returned at once.
        for &(_, i, gw1, gw2, pick) in tried.iter().take(prm.refine_top.max(1)) {
            if pick.is_some() {
                exact_candidate(
                    ctx, prm, src, fit, &cands[i], gw1, gw2, prm.refine, pick, best, encs, chosen,
                );
            }
        }
    }
}

/// Weighted squared error of the unquantised infill of decimated weights
/// against the ideal per-texel weights.
fn residual(
    ctx: &Ctx,
    grid: usize,
    dec: &[f32; 64],
    ideal: &[f32; MAX_TEXELS],
    fit: &Fit,
    plane2: bool,
) -> f32 {
    let g = &ctx.grids[grid];
    let tw = if plane2 { &fit.tw2 } else { &fit.tw1 };
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if let Some(t) = ctx.avx2 {
        if let Some(e) = super::simd::residual(t, g, dec, ideal, tw) {
            return e;
        }
    }
    residual_scalar(g, dec, ideal, tw)
}

/// [`residual`] with explicit per-texel residual weights `tw` (the HDR
/// encoder's): the same kernels, AVX2 twin when available.
pub(super) fn residual_tw(
    ctx: &Ctx,
    grid: usize,
    dec: &[f32; 64],
    ideal: &[f32; MAX_TEXELS],
    tw: &[f32; MAX_TEXELS],
) -> f32 {
    let g = &ctx.grids[grid];
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if let Some(t) = ctx.avx2 {
        if let Some(e) = super::simd::residual(t, g, dec, ideal, tw) {
            return e;
        }
    }
    residual_scalar(g, dec, ideal, tw)
}

/// The scalar body of [`residual`]: the oracle of `simd::residual`, and the
/// kernel without AVX2.
pub(super) fn residual_scalar(
    g: &crate::astc::tables::Grid,
    dec: &[f32; 64],
    ideal: &[f32; MAX_TEXELS],
    tw: &[f32; MAX_TEXELS],
) -> f32 {
    let mut e = 0f32;
    for (t, (ti, tc)) in g.tap_i.iter().zip(&g.tap_c).enumerate() {
        let mut v = 0f32;
        for k in 0..4 {
            v += dec[(ti[k] & 63) as usize] * tc[k];
        }
        let d = v * (1.0 / (16.0 * 64.0)) - ideal[t];
        e += d * d * tw[t];
    }
    e
}

#[allow(clippy::too_many_arguments)]
/// Encode one candidate exactly with `refine` weight/endpoint passes; updates
/// `best` and returns the lowest exact error this candidate reached (of the
/// passes it ran) and, for an unrefined call, the encoding it chose.
/// `resume`: the encoding an earlier unrefined call chose with these same
/// weights; the first pass is then not re-run (it would compute exactly
/// that, and its error is already in `best`) and refinement starts from it.
// One index walks several parallel per-texel / per-channel arrays (and in the encoder fixes the floating-point operation order); iterator chains would obscure that.
#[allow(clippy::needless_range_loop)]
fn exact_candidate(
    ctx: &Ctx,
    prm: &Params,
    src: &Src,
    fit: &Fit,
    cand: &Cand,
    gw1: [u8; 64],
    gw2: [u8; 64],
    refine: u8,
    resume: Option<Pick>,
    best: &mut Best,
    encs: &mut Vec<(u8, [u8; 8])>,
    chosen: &mut [[(f64, [u8; 8]); 4]; 16],
) -> (f64, Option<Pick>) {
    let mut mine = f64::MAX;
    let mut resume = resume;
    let m: Mode = ctx.modes[cand.mode];
    let g = m.grid as usize;
    let count = ctx.grids[g].count;
    let dual = fit.ccs < 4;
    let parts = fit.parts as usize;
    let mut gw1 = gw1;
    let mut gw2 = gw2;
    let mut eff1 = [0u8; MAX_TEXELS];
    let mut eff2 = [0u8; MAX_TEXELS];
    for _pass in 0..=refine {
        let cemv = if let Some((c, vals)) = resume.take() {
            for p in 0..parts {
                chosen[c][p].1 = vals[p];
            }
            c
        } else {
            infill_eff(ctx, g, &gw1, &mut eff1);
            if dual {
                infill_eff(ctx, g, &gw2, &mut eff2);
            }
            // Refit endpoints to the quantised weights, then pick the best
            // encoding per partition (same CEM for all partitions: matched).
            let mut lines = fit.lines;
            // Opaque: alpha is overwritten with 255 below, so it is not refitted.
            let chans = if src.opaque { 3 } else { 4 };
            for p in 0..parts {
                let fits = refit_part(src, fit.part(p), &eff1, &eff2, fit.ccs, chans);
                for c in 0..chans {
                    if let Some((a, b)) = fits[c] {
                        lines[p].e0[c] = a;
                        lines[p].e1[c] = b;
                    }
                }
                if src.opaque {
                    lines[p].e0[3] = 255.0;
                    lines[p].e1[3] = 255.0;
                }
            }
            // Collect encodings per partition; choose the CEM with the lowest
            // total error over partitions. `chosen[cem][p]` is valid when bit
            // `cem` of that partition's `seen` mask is set (its first encoding
            // fills it, as a strict `<` against an f64::MAX start did: every
            // exact error is finite); `all` keeps the CEMs seen in every
            // partition, and `total` sums a CEM's partitions in partition order.
            let mut total = [0f64; 16];
            let mut all = u16::MAX;
            for p in 0..parts {
                encode_endpoints(cand.class, cand.range, lines[p].e0, lines[p].e1, encs);
                // Same-budget alternatives: a class with no more values fits the
                // candidate's colour range too.
                let mut extra: Vec<(u8, [u8; 8])> = Vec::new();
                for &alt in classes(src) {
                    if alt != cand.class && alt.values() == cand.class.values() {
                        encode_endpoints(alt, cand.range, lines[p].e0, lines[p].e1, &mut extra);
                        encs.extend_from_slice(&extra);
                    }
                }
                let mut seen = 0u16;
                for &(cemv, v) in encs.iter() {
                    let raw = cem::decode(cemv, &v);
                    let ep = cem::expand(&raw, src.tgt.profile);
                    let e = exact_part_err(ctx, src, fit.part(p), &ep, &eff1, &eff2, fit.ccs);
                    let bit = 1u16 << (cemv & 15);
                    let cell = &mut chosen[(cemv & 15) as usize][p];
                    if seen & bit == 0 || e < cell.0 {
                        *cell = (e, v);
                    }
                    seen |= bit;
                }
                all &= seen;
                let mut mk = seen;
                while mk != 0 {
                    let cemv = mk.trailing_zeros() as usize;
                    total[cemv] += chosen[cemv][p].0;
                    mk &= mk - 1;
                }
            }
            // Lowest CEM first, strict `<`: ties keep the lowest CEM.
            let mut pick = None;
            let mut mk = all;
            while mk != 0 {
                let cemv = mk.trailing_zeros() as usize;
                mk &= mk - 1;
                let e = total[cemv];
                if pick.map_or(true, |(be, _)| e < be) {
                    pick = Some((e, cemv));
                }
            }
            let Some((err, cemv)) = pick else {
                return (mine, None);
            };
            mine = mine.min(err);
            if err < best.err {
                let mut s = SymbolicBlock::empty_normal();
                s.mode = m.mode;
                let gm = ctx.grids[g].w;
                s.grid = [gm, ctx.grids[g].h, ctx.grids[g].d];
                s.weight_range = m.range;
                s.dual_plane = dual;
                s.plane2_component = if dual { fit.ccs as u8 } else { 0 };
                s.partitions = fit.parts;
                s.partition_seed = if parts > 1 { fit.seed } else { 0 };
                s.cem = [cemv as u8; 4];
                s.cem_matched = parts > 1;
                s.color_range = cand.range as u8;
                let vc = cem::value_count(cemv as u8);
                s.color_count = (vc * parts) as u8;
                for p in 0..parts {
                    s.color_values[p * vc..(p + 1) * vc].copy_from_slice(&chosen[cemv][p].1[..vc]);
                }
                if dual {
                    for i in 0..count {
                        s.weights[2 * i] = gw1[i];
                        s.weights[2 * i + 1] = gw2[i];
                    }
                } else {
                    s.weights[..count].copy_from_slice(&gw1[..count]);
                }
                best.err = err;
                best.sym = s;
            }
            cemv
        };
        if _pass == refine {
            if refine == 0 {
                let vals = core::array::from_fn(|p| chosen[cemv][p].1);
                return (mine, Some((cemv, vals)));
            }
            break;
        }
        // Re-derive ideal weights from the decoded endpoints of the chosen
        // encoding, then re-decimate and re-quantise.
        let mut w1 = [0f32; MAX_TEXELS];
        let mut w2 = [0f32; MAX_TEXELS];
        for p in 0..parts {
            let v = chosen[cemv][p].1;
            let ep = cem::expand(&cem::decode(cemv as u8, &v), src.tgt.profile);
            // Per channel: the line direction d and start e0 (in the 16-bit
            // domain). `den` depends on neither the texel nor x, so it is
            // summed once per partition, in the per-texel loop's channel
            // order: the same value the loop computed for every texel.
            let d: [f32; 4] = core::array::from_fn(|c| ep.e1[c] as f32 - ep.e0[c] as f32);
            let e0: [f32; 4] = core::array::from_fn(|c| ep.e0[c] as f32);
            let cw = src.tgt.cw;
            let mut den = 0f32;
            for c in 0..4 {
                if c != fit.ccs {
                    den += d[c] * d[c] * cw[c];
                }
            }
            for &i in fit.part(p) {
                let i = i as usize;
                let mut num = 0f32;
                for c in 0..4 {
                    if c == fit.ccs {
                        continue;
                    }
                    let x = src.px[i][c] as f32 * 257.0 - e0[c];
                    num += d[c] * x * cw[c];
                }
                w1[i] = if den > 0.0 {
                    (num / den).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                if dual {
                    let c = fit.ccs;
                    let x = src.px[i][c] as f32 * 257.0 - e0[c];
                    w2[i] = if d[c] != 0.0 {
                        (x / d[c]).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                }
            }
        }
        let (old1, old2) = (gw1, gw2);
        let mut d1 = [0f32; 64];
        decimate(ctx, g, &w1, prm.grid_iters, &mut d1);
        quantize_weights(m.range as usize, &d1[..count], &mut gw1[..count]);
        if dual {
            let mut d2 = [0f32; 64];
            decimate(ctx, g, &w2, prm.grid_iters, &mut d2);
            quantize_weights(m.range as usize, &d2[..count], &mut gw2[..count]);
        }
        // A pass is a function of the grid weights alone (everything else it
        // reads is fixed for the candidate). Weights unchanged: the next pass
        // would repeat this one exactly (same error, not strictly below
        // `best` any more, same `mine`) and re-derive the same weights again,
        // and so would every pass after it.
        if gw1[..count] == old1[..count] && (!dual || gw2[..count] == old2[..count]) {
            break;
        }
    }
    (mine, None)
}

/// k-means clustering (k = 2..=4) of the block's colours; deterministic
/// farthest-point seeding and a fixed iteration count. Returns labels.
// One index walks several parallel per-texel / per-channel arrays (and in the encoder fixes the floating-point operation order); iterator chains would obscure that.
#[allow(clippy::needless_range_loop)]
fn k_means(src: &Src, k: usize, labels: &mut [u8; MAX_TEXELS]) {
    let n = src.n;
    let col = |i: usize| -> [f32; 4] {
        let p = src.px[i];
        let cw = src.tgt.cw;
        [
            p[0] as f32 * cw[0],
            p[1] as f32 * cw[1],
            p[2] as f32 * cw[2],
            p[3] as f32 * cw[3],
        ]
    };
    let d2 = |a: [f32; 4], b: [f32; 4]| {
        let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2], a[3] - b[3]];
        dot4(d, d)
    };
    let mut mean = [0f32; 4];
    for i in 0..n {
        let c = col(i);
        for j in 0..4 {
            mean[j] += c[j];
        }
    }
    for v in mean.iter_mut() {
        *v /= n as f32;
    }
    // Seeds: the texel farthest from the mean, then repeatedly the texel
    // farthest from its nearest seed.
    let mut cent = [[0f32; 4]; 4];
    let mut bi = 0;
    let mut bd = -1.0;
    for i in 0..n {
        let d = d2(col(i), mean);
        if d > bd {
            bd = d;
            bi = i;
        }
    }
    cent[0] = col(bi);
    for s in 1..k {
        let mut bi = 0;
        let mut bd = -1.0;
        for i in 0..n {
            let c = col(i);
            let d = (0..s).map(|j| d2(c, cent[j])).fold(f32::MAX, f32::min);
            if d > bd {
                bd = d;
                bi = i;
            }
        }
        cent[s] = col(bi);
    }
    for _ in 0..4 {
        let mut sum = [[0f32; 4]; 4];
        let mut cnt = [0f32; 4];
        for i in 0..n {
            let c = col(i);
            let mut best = 0;
            let mut bd = f32::MAX;
            for (j, ce) in cent.iter().enumerate().take(k) {
                let d = d2(c, *ce);
                if d < bd {
                    bd = d;
                    best = j;
                }
            }
            labels[i] = best as u8;
            cnt[best] += 1.0;
            for q in 0..4 {
                sum[best][q] += c[q];
            }
        }
        for j in 0..k {
            if cnt[j] > 0.0 {
                for q in 0..4 {
                    cent[j][q] = sum[j][q] / cnt[j];
                }
            }
        }
    }
}

/// Permutations of 3 and 4 labels, for matching a clustering to a pattern.
const PERM3: [[u8; 3]; 6] = [
    [0, 1, 2],
    [0, 2, 1],
    [1, 0, 2],
    [1, 2, 0],
    [2, 0, 1],
    [2, 1, 0],
];
const PERM4: [[u8; 4]; 24] = [
    [0, 1, 2, 3],
    [0, 1, 3, 2],
    [0, 2, 1, 3],
    [0, 2, 3, 1],
    [0, 3, 1, 2],
    [0, 3, 2, 1],
    [1, 0, 2, 3],
    [1, 0, 3, 2],
    [1, 2, 0, 3],
    [1, 2, 3, 0],
    [1, 3, 0, 2],
    [1, 3, 2, 0],
    [2, 0, 1, 3],
    [2, 0, 3, 1],
    [2, 1, 0, 3],
    [2, 1, 3, 0],
    [2, 3, 0, 1],
    [2, 3, 1, 0],
    [3, 0, 1, 2],
    [3, 0, 2, 1],
    [3, 1, 0, 2],
    [3, 1, 2, 0],
    [3, 2, 0, 1],
    [3, 2, 1, 0],
];

/// Texels a pattern disagrees with a clustering on, under the best label
/// matching (confusion matrix, all permutations). The per-texel walk: the
/// oracle of [`mismatch_bits`].
#[cfg(test)]
fn mismatch(pattern: &[u8], labels: &[u8], k: usize) -> u32 {
    let mut cm = [[0u32; 4]; 4];
    for (&a, &b) in pattern.iter().zip(labels) {
        cm[a as usize][b as usize] += 1;
    }
    best_matching(&cm, pattern.len() as u32, k)
}

/// [`mismatch`] from per-label bitsets and counts (pattern, clustering;
/// `words` 64-bit words of texels, every label < `k`).
/// Only the leading `(k - 1) x (k - 1)` cells of the confusion matrix are
/// popcounts; the last row and column follow from the row sums (pattern
/// partition sizes) and column sums (cluster sizes). Integer counts, so the
/// matrix, and the result, are exactly the walk's.
#[inline(always)]
pub(super) fn mismatch_bits(pat: &PartBits, lab: &PartBits, k: usize, words: usize) -> u32 {
    let (pb, pc, lb, lc) = (&pat.bits, &pat.cnt, &lab.bits, &lab.cnt);
    let mut cm = [[0u32; 4]; 4];
    let last = k - 1;
    for a in 0..last {
        let mut row = 0;
        for b in 0..last {
            let mut s = 0;
            for w in 0..words {
                s += (pb[a][w] & lb[b][w]).count_ones();
            }
            cm[a][b] = s;
            row += s;
        }
        cm[a][last] = pc[a] - row;
    }
    let mut row = 0;
    for b in 0..last {
        let col: u32 = (0..last).map(|a| cm[a][b]).sum();
        cm[last][b] = lc[b] - col;
        row += cm[last][b];
    }
    cm[last][last] = pc[last] - row;
    let n = (0..k).map(|a| pc[a]).sum();
    best_matching(&cm, n, k)
}

/// The best `m` patterns (three or four partitions) by [`mismatch_bits`]
/// with the clustering `labels`, ties by pattern index, into `top` (empty on
/// entry). Kept out of line: inlined, it re-rolled the two-partition ranking
/// loop beside it (+0.12% Ir at Fast). With the POPCNT instruction it runs
/// as `simd::rank_patterns` (the same body compiled for it).
#[inline(never)]
fn rank_patterns(
    ctx: &Ctx,
    bits: &[PartBits],
    labels: &[u8],
    k: usize,
    m: usize,
    top: &mut Vec<(u32, usize)>,
) {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if let Some(t) = ctx.popcnt {
        return super::simd::rank_patterns(t, bits, labels, k, m, top);
    }
    #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
    let _ = ctx;
    rank_patterns_body(bits, labels, k, m, top);
}

/// The best `m` two-partition patterns by bitset distance to the clustering
/// `lb` (either labelling), ties by pattern index, into `top` (empty on
/// entry). With the POPCNT instruction it runs as `simd::rank_two` (the
/// same body compiled for it), like [`rank_patterns`].
#[inline(never)]
fn rank_two(
    ctx: &Ctx,
    pats: &[super::context::Partitioning],
    lb: &[u64; 4],
    n: usize,
    m: usize,
    top: &mut Vec<(u32, usize)>,
) {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if let Some(t) = ctx.popcnt {
        return super::simd::rank_two(t, pats, lb, n, m, top);
    }
    #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
    let _ = ctx;
    rank_two_body(pats, lb, n, m, top);
}

/// The body of [`rank_two`] (inlined into each instruction-set variant).
#[inline(always)]
pub(super) fn rank_two_body(
    pats: &[super::context::Partitioning],
    lb: &[u64; 4],
    n: usize,
    m: usize,
    top: &mut Vec<(u32, usize)>,
) {
    // Words past the footprint's texels are zero in both bitsets.
    let words = n.div_ceil(64);
    for (pi, p) in pats.iter().enumerate() {
        // popcount of the XOR of bitsets, either labelling.
        let x = (0..words)
            .map(|w| (p.bits[w] ^ lb[w]).count_ones())
            .sum::<u32>();
        let key = (x.min(n as u32 - x), pi);
        if top.len() == m && key >= top[m - 1] {
            continue;
        }
        let pos = top.iter().position(|t| key < *t).unwrap_or(top.len());
        top.insert(pos, key);
        top.truncate(m);
    }
}

/// The body of [`rank_patterns`] (inlined into each instruction-set variant).
#[inline(always)]
pub(super) fn rank_patterns_body(
    bits: &[PartBits],
    labels: &[u8],
    k: usize,
    m: usize,
    top: &mut Vec<(u32, usize)>,
) {
    let lab = label_bits(labels);
    let words = labels.len().div_ceil(64);
    if k == 4 {
        return rank4_bounded(bits, &lab, words, m, top);
    }
    for (pi, pb) in bits.iter().enumerate() {
        let key = (mismatch_bits(pb, &lab, k, words), pi);
        if top.len() == m && key >= top[m - 1] {
            continue;
        }
        let pos = top.iter().position(|t| key < *t).unwrap_or(top.len());
        top.insert(pos, key);
        top.truncate(m);
    }
}

/// [`rank_patterns_body`] for four partitions, where the exact matching
/// search tries 24 permutations: a pattern whose mismatch is provably at
/// least the last kept entry's skips the search. Any matching keeps at most
/// one cell per row of the confusion matrix, so the matched count is at most
/// the sum of the row maxima and the mismatch at least `n` minus it; once
/// the list is full a pattern is kept only below its last mismatch (a tie
/// loses on the later index), so a bound that reaches it decides the same.
/// (For three partitions, 6 permutations, the bound and the shared shape
/// cost more than they saved: Medium +2.0..+4.4% Ir, three shapes measured.)
#[inline(always)]
fn rank4_bounded(
    bits: &[PartBits],
    lab: &PartBits,
    words: usize,
    m: usize,
    top: &mut Vec<(u32, usize)>,
) {
    for (pi, pb) in bits.iter().enumerate() {
        let (cm, n) = confusion4(pb, lab, words);
        if top.len() == m {
            let mut rows = 0u32;
            for r in &cm {
                rows += r[0].max(r[1]).max(r[2].max(r[3]));
            }
            if n - rows >= top[m - 1].0 {
                continue;
            }
        }
        let key = (best_matching(&cm, n, 4), pi);
        if top.len() == m && key >= top[m - 1] {
            continue;
        }
        let pos = top.iter().position(|t| key < *t).unwrap_or(top.len());
        top.insert(pos, key);
        top.truncate(m);
    }
}

/// The confusion matrix of [`mismatch_bits`] for k = 4, and the texel count.
#[inline(always)]
fn confusion4(pat: &PartBits, lab: &PartBits, words: usize) -> ([[u32; 4]; 4], u32) {
    let (pb, pc, lb, lc) = (&pat.bits, &pat.cnt, &lab.bits, &lab.cnt);
    let mut cm = [[0u32; 4]; 4];
    for a in 0..3 {
        let mut row = 0;
        for b in 0..3 {
            let mut s = 0;
            for w in 0..words {
                s += (pb[a][w] & lb[b][w]).count_ones();
            }
            cm[a][b] = s;
            row += s;
        }
        cm[a][3] = pc[a] - row;
    }
    let mut row = 0;
    for b in 0..3 {
        let col: u32 = (0..3).map(|a| cm[a][b]).sum();
        cm[3][b] = lc[b] - col;
        row += cm[3][b];
    }
    cm[3][3] = pc[3] - row;
    (cm, pc[0] + pc[1] + pc[2] + pc[3])
}

/// `n` minus the largest diagonal sum of the confusion matrix over all
/// label permutations (k = 3 or 4).
fn best_matching(cm: &[[u32; 4]; 4], n: u32, k: usize) -> u32 {
    let mut best = 0u32;
    if k == 3 {
        for p in PERM3.iter() {
            best = best.max(cm[0][p[0] as usize] + cm[1][p[1] as usize] + cm[2][p[2] as usize]);
        }
    } else {
        for p in PERM4.iter() {
            best = best.max(
                cm[0][p[0] as usize]
                    + cm[1][p[1] as usize]
                    + cm[2][p[2] as usize]
                    + cm[3][p[3] as usize],
            );
        }
    }
    n - best
}

/// Lowest absolute correlation between a channel and the others, and that
/// channel (for the dual-plane gate).
// One index walks several parallel per-texel / per-channel arrays (and in the encoder fixes the floating-point operation order); iterator chains would obscure that.
#[allow(clippy::needless_range_loop)]
fn weakest_channel(src: &Src) -> (usize, f32) {
    let n = src.n as f32;
    let mut mean = [0f32; 4];
    for i in 0..src.n {
        for c in 0..4 {
            mean[c] += src.px[i][c] as f32;
        }
    }
    for m in mean.iter_mut() {
        *m /= n;
    }
    let mut cov = [[0f32; 4]; 4];
    for i in 0..src.n {
        let d: [f32; 4] = core::array::from_fn(|c| src.px[i][c] as f32 - mean[c]);
        for a in 0..4 {
            for b in 0..4 {
                cov[a][b] += d[a] * d[b];
            }
        }
    }
    let chans = if src.opaque { 3 } else { 4 };
    let mut best = (4usize, 1.0f32);
    for c in 0..chans {
        if cov[c][c] <= 1.0 {
            continue;
        }
        // Max |corr| with any other varying channel (squared, no sqrt).
        let mut mx = 0f32;
        for o in 0..chans {
            if o == c || cov[o][o] <= 1.0 {
                continue;
            }
            let r2 = cov[c][o] * cov[c][o] / (cov[c][c] * cov[o][o]);
            mx = mx.max(r2);
        }
        if mx < best.1 {
            best = (c, mx);
        }
    }
    best
}

/// Encode one block of `n` texels (z, y, x order; edge texels already
/// clamped). Returns the symbolic block and its exact error.
// One index walks several parallel per-texel / per-channel arrays (and in the encoder fixes the floating-point operation order); iterator chains would obscure that.
#[allow(clippy::needless_range_loop)]
pub(crate) fn encode_block(ctx: &Ctx, prm: &Params, tgt: Target, px: &[[u8; 4]]) -> Best {
    let n = ctx.texels;
    let first = px[0];
    if px[..n].iter().all(|&p| p == first) {
        let mut s = SymbolicBlock::empty_void_ldr();
        for c in 0..4 {
            s.void_color[c] = first[c] as u16 * 257;
        }
        return Best { err: 0.0, sym: s };
    }
    let opaque = px[..n].iter().all(|p| p[3] == 255);
    let grey = px[..n].iter().all(|p| p[0] == p[1] && p[1] == p[2]);
    let src = Src {
        px,
        n,
        tgt,
        opaque,
        grey,
    };
    let mut best = Best {
        err: f64::MAX,
        sym: SymbolicBlock::empty_void_ldr(),
    };
    let zeros = [0u8; MAX_TEXELS];
    let mut pool = ctx.fits.borrow_mut();
    let FitPool {
        fits,
        order,
        top,
        search,
    } = &mut *pool;
    let most = prm.part_candidates.iter().copied().max().unwrap_or(0);
    // Grows once per encode call (the slot count depends only on the preset).
    while fits.len() < 2 + most {
        fits.push(Fit::new());
    }
    make_fit(&mut fits[0], &src, 1, 0, &zeros, 4);
    search_fit(ctx, prm, &src, &fits[0], &mut best, search);
    // Early out: stop after the one-partition search when the block is
    // already good enough. The threshold is a per-channel MSE that grows with
    // the square of the texel count (larger blocks are allowed more error),
    // set per preset.
    let chans = if opaque { 3.0 } else { 4.0 };
    let r = n as f64 / 16.0;
    let good_enough = prm.part2_skip_err as f64 * r * r;
    if best.err / (n as f64 * chans) <= good_enough {
        return best;
    }
    if prm.dual_plane {
        let (c, r2) = weakest_channel(&src);
        if c < 4 && r2 < prm.dual_corr_limit * prm.dual_corr_limit {
            make_fit(&mut fits[1], &src, 1, 0, &zeros, c);
            search_fit(ctx, prm, &src, &fits[1], &mut best, search);
        }
    }
    for k in 2..=prm.max_partitions.min(4) as usize {
        let m = prm.part_candidates[k - 2];
        if m == 0 || best.err / (n as f64 * chans) <= good_enough {
            break;
        }
        let mut labels = [0u8; MAX_TEXELS];
        k_means(&src, k, &mut labels);
        // Rank patterns by mismatch with the clustering; best M by insertion.
        top.clear();
        let pats = match k {
            2 => &ctx.part2,
            3 => &ctx.part3,
            _ => &ctx.part4,
        };
        if k == 2 {
            let mut lb = [0u64; 4];
            for i in 0..n {
                if labels[i] == 1 {
                    lb[i / 64] |= 1 << (i % 64);
                }
            }
            rank_two(ctx, pats, &lb, n, m, top);
        } else {
            let bits = if k == 3 { &ctx.bits3 } else { &ctx.bits4 };
            rank_patterns(ctx, bits, &labels[..n], k, m, top);
        }
        for (j, &(_, pi)) in top.iter().enumerate() {
            let p = &pats[pi];
            make_fit(&mut fits[2 + j], &src, k as u8, p.seed, &p.labels, 4);
        }
        // Order the fits by line error without moving them. Stable sort:
        // ties keep the clustering-match order.
        order.clear();
        order.extend(2..2 + top.len());
        order.sort_by(|&a, &b| fits[a].perp.total_cmp(&fits[b].perp));
        for &j in order.iter().take(prm.part_search[k - 2].max(1)) {
            let fitk = &fits[j];
            if fitk.perp as f64 >= best.err {
                break;
            }
            search_fit(ctx, prm, &src, fitk, &mut best, search);
        }
    }
    best
}

impl SymbolicBlock {
    pub(crate) fn empty_normal() -> Self {
        Self::empty(BlockKind::Normal, 0)
    }
    pub(crate) fn empty_void_ldr() -> Self {
        Self::empty(BlockKind::VoidLdr, 0)
    }
}

#[cfg(test)]
mod oracle_tests {
    use super::*;

    /// QSTEP2 holds (255 / (levels - 1))^2 computed in f64 and rounded to f32.
    /// The bounded four-partition ranking keeps the same patterns, in the
    /// same order, as ranking every pattern by its exact mismatch.
    #[test]
    fn rank4_bounded_equals_exhaustive() {
        let mut rng = Rng::new(0x4A11_0004);
        for n in [16usize, 36, 64, 100, 144, 216] {
            let pats: Vec<PartBits> = (0..300)
                .map(|_| label_bits(&(0..n).map(|_| rng.below(4) as u8).collect::<Vec<_>>()))
                .collect();
            for m in [1usize, 2, 4, 8] {
                let labels: Vec<u8> = (0..n).map(|_| rng.below(4) as u8).collect();
                let lab = label_bits(&labels);
                let mut want: Vec<(u32, usize)> = pats
                    .iter()
                    .enumerate()
                    .map(|(pi, pb)| (mismatch_bits(pb, &lab, 4, n.div_ceil(64)), pi))
                    .collect();
                want.sort();
                want.truncate(m);
                let mut got = Vec::new();
                rank_patterns_body(&pats, &labels, 4, m, &mut got);
                assert_eq!(got, want, "n {n} m {m}");
            }
        }
    }

    /// The weight-quantisation index equals the `usize` form for every
    /// class of input: NaN, infinities, negatives, the clamp, huge values
    /// and a dense sweep of the working range.
    #[test]
    fn quantize_index_equals_usize_cast() {
        let old = |v: f32| ((v * 4.0 + 0.5) as usize).min(256);
        let new = |v: f32| ((v * 4.0 + 0.5) as i32).clamp(0, 256) as usize;
        let mut xs = alloc::vec![
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::MAX,
            f32::MIN,
            -0.0,
            0.0,
            -0.124_999_99,
            -0.125,
            -0.4,
            -1.0e9,
            1.0e9,
            3.0e9,
            64.0,
            64.125,
            1.0e30,
        ];
        // Every 65537th bit pattern (all exponents, both signs), then the
        // working range at a fine step.
        xs.extend((0..=u32::MAX).step_by(65_537).map(f32::from_bits));
        xs.extend((0..=720_000).map(|i| -2.0 + i as f32 * 1.0e-4));
        for x in xs {
            assert_eq!(new(x), old(x), "{x:e}");
        }
    }

    #[test]
    fn qstep2_table() {
        for (r, &q) in QSTEP2.iter().enumerate() {
            let levels = LEVELS[r] as f64;
            let x = 255.0 / (levels - 1.0);
            assert_eq!(q.to_bits(), ((x * x) as f32).to_bits(), "range {r}");
        }
    }
    use crate::astc::testgen::Rng;

    /// The bitset mismatch equals the per-texel walk, for every texel count
    /// (1..=216, so every word count and partial word) and k = 3, 4,
    /// including labellings that leave some labels empty.
    #[test]
    fn mismatch_bits_equals_walk() {
        let mut rng = Rng::new(0x5EED_3A11);
        for n in 1..=MAX_TEXELS {
            for k in [3usize, 4] {
                for trial in 0..24 {
                    // Some trials use fewer distinct labels than k.
                    let used = if trial % 4 == 0 {
                        1 + rng.below(k as u64)
                    } else {
                        k as u64
                    };
                    let pat: Vec<u8> = (0..n).map(|_| rng.below(used) as u8).collect();
                    let lab: Vec<u8> = (0..n).map(|_| rng.below(k as u64) as u8).collect();
                    assert_eq!(
                        mismatch_bits(&label_bits(&pat), &label_bits(&lab), k, n.div_ceil(64)),
                        mismatch(&pat, &lab, k),
                        "n {n} k {k} trial {trial}"
                    );
                }
            }
        }
    }
}
