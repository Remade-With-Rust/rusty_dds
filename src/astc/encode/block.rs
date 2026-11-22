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

use super::context::{Ctx, Mode};
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
/// (baked literals: float const fn needs Rust 1.82).
#[rustfmt::skip]
static QSTEP2: [f32; 21] = [65025.0, 16256.25, 7225.0, 4064.0625, 2601.0, 1327.0408163265308, 802.7777777777777, 537.3966942148761, 289.0, 180.12465373961217, 122.92060491493385, 67.66389177939648, 42.751479289940825, 29.4363965595292, 16.383219954648524, 10.419003364845377, 7.204986149584487, 4.031558063116126, 2.5720897116411536, 1.7824346920314682, 1.0];

/// Endpoint classes searched (the value count decides the colour range).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Lum,        // CEM 0: 2 values
    LumAlpha,   // CEM 4: 4 values
    RgbScale,   // CEM 6: 4 values
    Rgb,        // CEM 8 / 9: 6 values
    RgbScaleA,  // CEM 10: 6 values
    Rgba,       // CEM 12 / 13: 8 values
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

/// One partitioning with its ideal fit.
struct Fit {
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
    /// Texel indices of each partition, and their counts.
    idx: [[u8; MAX_TEXELS]; 4],
    cnt: [usize; 4],
}

impl Fit {
    fn part(&self, p: usize) -> &[u8] {
        &self.idx[p][..self.cnt[p]]
    }
}

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
            nd[a] = cov[a][0] * dir[0] + cov[a][1] * dir[1] + cov[a][2] * dir[2] + cov[a][3] * dir[3];
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
        w[i] = if span > 0.0 { (ts[i] - tmin) / span } else { 0.0 };
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
        w[i] = if span > 0.0 { (src.px[i][c] - lo) as f32 / span } else { 0.0 };
    }
    line.e0[c] = lo as f32;
    line.e1[c] = hi as f32;
    let d = span * src.tgt.cw[c];
    line.len2b = d * d;
}

fn make_fit(src: &Src, parts: u8, seed: u16, labels: &[u8], ccs: usize) -> Fit {
    let mut f = Fit {
        parts,
        seed,
        labels: [0; MAX_TEXELS],
        lines: [Line::default(); 4],
        w1: [0.0; MAX_TEXELS],
        w2: [0.0; MAX_TEXELS],
        ccs,
        perp: 0.0,
        idx: [[0; MAX_TEXELS]; 4],
        cnt: [0; 4],
    };
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
    for p in 0..parts {
        let (idx, cnt) = (f.idx[p as usize], f.cnt[p as usize]);
        let idx = &idx[..cnt];
        let (mut line, perp) = fit_line(src, idx, mask, &mut f.w1);
        if ccs < 4 {
            fit_channel(src, idx, ccs, &mut line, &mut f.w2);
        }
        f.lines[p as usize] = line;
        f.perp += perp;
    }
    f
}

/// Least-squares fit of per-texel ideal weights (scaled to 0..=64) to a
/// grid, through the infill taps. Returns grid weights in 0..=64 (float).
pub(super) fn decimate(ctx: &Ctx, grid: usize, ideal: &[f32; MAX_TEXELS], iters: u8, out: &mut [f32; 64]) {
    let g = &ctx.grids[grid];
    if g.full {
        for (o, &w) in out[..g.count].iter_mut().zip(ideal.iter()) {
            *o = w * 64.0;
        }
        return;
    }
    // Start from the contribution-weighted average of the texels each stored
    // weight feeds.
    for j in 0..g.count {
        let mut s = 0f32;
        let mut cs = 0f32;
        for &(t, c) in &g.feed[g.feed_at[j] as usize..g.feed_at[j + 1] as usize] {
            s += ideal[t as usize] * c as f32;
            cs += c as f32;
        }
        out[j] = if cs > 0.0 { 64.0 * s / cs } else { 32.0 };
    }
    // Jacobi steps on sum_t (sum_k c_tk/16 g_k - 64 w_t)^2.
    let mut resid = [0f32; MAX_TEXELS];
    for _ in 0..iters {
        for (t, &(n, tp)) in g.taps.iter().enumerate() {
            let mut v = 0f32;
            for &(k, c) in &tp[..n as usize] {
                v += out[k as usize] * c as f32;
            }
            resid[t] = v * (1.0 / 16.0) - 64.0 * ideal[t];
        }
        for j in 0..g.count {
            let mut num = 0f32;
            let mut den = 0f32;
            for &(t, c) in &g.feed[g.feed_at[j] as usize..g.feed_at[j + 1] as usize] {
                let cf = c as f32 * (1.0 / 16.0);
                num += cf * resid[t as usize];
                den += cf * cf;
            }
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
                let d = if 4 * l > x { 4 * l - x } else { x - 4 * l };
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
pub(super) fn quantize_weights(range: usize, g: &[f32], out: &mut [u8]) {
    let lut = &WQ_LUT[range];
    for (o, &v) in out.iter_mut().zip(g) {
        *o = lut[((v * 4.0 + 0.5) as usize).min(256)];
    }
}

pub(super) fn infill_eff(ctx: &Ctx, grid: usize, gw: &[u8], out: &mut [u8; MAX_TEXELS]) {
    for (t, &(n, tp)) in ctx.grids[grid].taps.iter().enumerate() {
        let mut s = 0u32;
        for &(k, c) in &tp[..n as usize] {
            s += gw[k as usize] as u32 * c as u32;
        }
        out[t] = ((s + 8) >> 4) as u8;
    }
}


/// Least-squares endpoints for one channel given integer weights (0..=64).
fn refit_channel(src: &Src, idx: &[u8], c: usize, w: &[u8; MAX_TEXELS]) -> Option<(f32, f32)> {
    let (mut a, mut b, mut d, mut x0, mut x1) = (0f32, 0f32, 0f32, 0f32, 0f32);
    for &i in idx {
        let i = i as usize;
        let t = w[i] as f32 * (1.0 / 64.0);
        let u = 1.0 - t;
        let x = src.px[i][c] as f32;
        a += u * u;
        b += u * t;
        d += t * t;
        x0 += u * x;
        x1 += t * x;
    }
    let det = a * d - b * b;
    if det.abs() < 1e-6 {
        return None;
    }
    let e0 = (x0 * d - x1 * b) / det;
    let e1 = (x1 * a - x0 * b) / det;
    Some((e0.clamp(0.0, 255.0), e1.clamp(0.0, 255.0)))
}

fn near(range: usize, x: f32) -> u8 {
    let v = (x + 0.5).clamp(0.0, 255.0) as usize;
    COLOR_NEAREST[range][v.min(255)]
}

/// Candidate endpoint encodings (CEM, values) for a class.
fn encode_endpoints(class: Class, range: usize, e0: [f32; 4], e1: [f32; 4], out: &mut Vec<(u8, [u8; 8])>) {
    out.clear();
    let lum = |e: [f32; 4]| (e[0] + e[1] + e[2]) * (1.0 / 3.0);
    match class {
        Class::Lum => {
            out.push((0, [near(range, lum(e0)), near(range, lum(e1)), 0, 0, 0, 0, 0, 0]));
        }
        Class::LumAlpha => {
            out.push((
                4,
                [near(range, lum(e0)), near(range, lum(e1)), near(range, e0[3]), near(range, e1[3]), 0, 0, 0, 0],
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
    src: &Src,
    idx: &[u8],
    ep: &cem::Endpoints,
    w1: &[u8; MAX_TEXELS],
    w2: &[u8; MAX_TEXELS],
    ccs: usize,
) -> f64 {
    let cw = src.tgt.cw;
    let mut acc = [0u64; 4];
    for &i in idx {
        let i = i as usize;
        for c in 0..4 {
            let w = if c == ccs { w2[i] } else { w1[i] };
            let v = lerp(ep.e0[c], ep.e1[c], w) as i64;
            let s = src.px[i][c] as i64;
            let d = if src.tgt.unorm8 { (v >> 8) - s } else { v - s * 257 };
            acc[c] += (d * d) as u64;
        }
    }
    let scale = if src.tgt.unorm8 { 1.0 } else { 1.0 / (257.0 * 257.0) };
    (0..4).map(|c| acc[c] as f64 * cw[c] as f64 * scale).sum()
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
fn search_fit(ctx: &Ctx, prm: &Params, src: &Src, fit: &Fit, best: &mut Best) {
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
    let ng = ctx.grids.len();
    let mut dec_err: Vec<f32> = alloc::vec![-1.0; ng];
    let mut dec1: Vec<[f32; 64]> = alloc::vec![[0.0; 64]; ng];
    let mut dec2: Vec<[f32; 64]> = alloc::vec![[0.0; 64]; ng];
    let mut cands: Vec<Cand> = Vec::new();
    let cw_mean = (src.tgt.cw[0] + src.tgt.cw[1] + src.tgt.cw[2] + src.tgt.cw[3]) * 0.25;
    for (mi, m) in ctx.modes.iter().enumerate() {
        if m.dual != dual {
            continue;
        }
        let g = m.grid as usize;
        if dec_err[g] < 0.0 {
            // Rank on the one-shot (transpose-average) decimation; the kept
            // candidates get the least-squares refinement below.
            decimate(ctx, g, &fit.w1, 0, &mut dec1[g]);
            let mut e = residual(ctx, g, &dec1[g], &fit.w1, fit, false);
            if dual {
                decimate(ctx, g, &fit.w2, 0, &mut dec2[g]);
                e += residual(ctx, g, &dec2[g], &fit.w2, fit, true);
            }
            dec_err[g] = e;
        }
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
            let est = dec_err[g] + wq + qerr + fit.perp + scale_err[ci];
            cands.push(Cand { est, mode: mi, class, range });
        }
    }
    // The best k by estimate, ties by mode index (a total order), selected
    // without sorting the whole list.
    let k = prm.exact_candidates.max(1).min(cands.len());
    let better = |a: &Cand, b: &Cand| a.est < b.est || (a.est == b.est && a.mode < b.mode);
    let mut top: Vec<usize> = Vec::with_capacity(k + 1);
    for (i, c) in cands.iter().enumerate() {
        if top.len() == k && !better(c, &cands[top[k - 1]]) {
            continue;
        }
        let pos = top.iter().position(|&t| better(c, &cands[t])).unwrap_or(top.len());
        top.insert(pos, i);
        top.truncate(k);
    }
    // Each kept mode also gets its other endpoint classes (adjacent entries:
    // candidates are pushed per mode, class after class), each at the colour
    // range its own value count allows.
    let mut keep: Vec<usize> = top.clone();
    for &t in &top {
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
    let mut refined = alloc::vec![false; ng];
    let mut tried: Vec<(f64, usize, [u8; 64], [u8; 64])> = Vec::with_capacity(keep.len());
    for &i in &keep {
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
        quantize_weights(ctx.modes[c.mode].range as usize, &dec1[g][..count], &mut gw1[..count]);
        if dual {
            quantize_weights(ctx.modes[c.mode].range as usize, &dec2[g][..count], &mut gw2[..count]);
        }
        // One unrefined pass for every kept candidate ...
        let e = exact_candidate(ctx, prm, src, fit, c, gw1, gw2, 0, best);
        tried.push((e, i, gw1, gw2));
    }
    // ... and the refinement passes only for the best few.
    if prm.refine > 0 {
        tried.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(core::cmp::Ordering::Equal).then(a.1.cmp(&b.1)));
        for &(_, i, gw1, gw2) in tried.iter().take(prm.refine_top.max(1)) {
            exact_candidate(ctx, prm, src, fit, &cands[i], gw1, gw2, prm.refine, best);
        }
    }
}

/// Weighted squared error of the unquantised infill of decimated weights
/// against the ideal per-texel weights.
fn residual(ctx: &Ctx, grid: usize, dec: &[f32; 64], ideal: &[f32; MAX_TEXELS], fit: &Fit, plane2: bool) -> f32 {
    let g = &ctx.grids[grid];
    let mut e = 0f32;
    for (t, &(n, tp)) in g.taps.iter().enumerate() {
        let mut v = 0f32;
        for &(k, c) in &tp[..n as usize] {
            v += dec[k as usize] * c as f32;
        }
        let d = v * (1.0 / (16.0 * 64.0)) - ideal[t];
        let l = &fit.lines[fit.labels[t] as usize];
        e += d * d * if plane2 { l.len2b } else { l.len2 };
    }
    e
}

#[allow(clippy::too_many_arguments)]
/// Encode one candidate exactly with `refine` weight/endpoint passes; updates
/// `best` and returns the lowest exact error this candidate reached.
fn exact_candidate(
    ctx: &Ctx,
    prm: &Params,
    src: &Src,
    fit: &Fit,
    cand: &Cand,
    gw1: [u8; 64],
    gw2: [u8; 64],
    refine: u8,
    best: &mut Best,
) -> f64 {
    let mut mine = f64::MAX;
    let m: Mode = ctx.modes[cand.mode];
    let g = m.grid as usize;
    let count = ctx.grids[g].count;
    let dual = fit.ccs < 4;
    let parts = fit.parts as usize;
    let mut gw1 = gw1;
    let mut gw2 = gw2;
    let mut encs: Vec<(u8, [u8; 8])> = Vec::new();
    let mut eff1 = [0u8; MAX_TEXELS];
    let mut eff2 = [0u8; MAX_TEXELS];
    for _pass in 0..=refine {
        infill_eff(ctx, g, &gw1, &mut eff1);
        if dual {
            infill_eff(ctx, g, &gw2, &mut eff2);
        }
        // Refit endpoints to the quantised weights, then pick the best
        // encoding per partition (same CEM for all partitions: matched).
        let mut lines = fit.lines;
        for p in 0..parts {
            for c in 0..4 {
                let w = if c == fit.ccs { &eff2 } else { &eff1 };
                if let Some((a, b)) = refit_channel(src, fit.part(p), c, w) {
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
        // total error over partitions.
        let mut total_by_cem: [(f64, bool); 16] = [(0.0, true); 16];
        let mut chosen: [[(f64, [u8; 8]); 4]; 16] = [[(f64::MAX, [0; 8]); 4]; 16];
        for p in 0..parts {
            encode_endpoints(cand.class, cand.range, lines[p].e0, lines[p].e1, &mut encs);
            // Same-budget alternatives: a class with no more values fits the
            // candidate's colour range too.
            let mut extra: Vec<(u8, [u8; 8])> = Vec::new();
            for &alt in classes(src) {
                if alt != cand.class && alt.values() == cand.class.values() {
                    encode_endpoints(alt, cand.range, lines[p].e0, lines[p].e1, &mut extra);
                    encs.extend_from_slice(&extra);
                }
            }
            let mut seen = [false; 16];
            for &(cemv, v) in encs.iter() {
                let raw = cem::decode(cemv, &v);
                let ep = cem::expand(&raw, src.tgt.profile);
                let e = exact_part_err(src, fit.part(p), &ep, &eff1, &eff2, fit.ccs);
                seen[cemv as usize] = true;
                if e < chosen[cemv as usize][p].0 {
                    chosen[cemv as usize][p] = (e, v);
                }
            }
            for cemv in 0..16 {
                if !seen[cemv] {
                    total_by_cem[cemv].1 = false;
                } else {
                    total_by_cem[cemv].0 += chosen[cemv][p].0;
                }
            }
        }
        let mut pick = None;
        for cemv in 0..16 {
            if total_by_cem[cemv].1 && chosen[cemv][0].0 < f64::MAX {
                let e = total_by_cem[cemv].0;
                if pick.map_or(true, |(be, _)| e < be) {
                    pick = Some((e, cemv));
                }
            }
        }
        let Some((err, cemv)) = pick else { return mine };
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
        if _pass == refine {
            break;
        }
        // Re-derive ideal weights from the decoded endpoints of the chosen
        // encoding, then re-decimate and re-quantise.
        let mut w1 = [0f32; MAX_TEXELS];
        let mut w2 = [0f32; MAX_TEXELS];
        for p in 0..parts {
            let v = chosen[cemv][p].1;
            let ep = cem::expand(&cem::decode(cemv as u8, &v), src.tgt.profile);
            for &i in fit.part(p) {
                let i = i as usize;
                let (mut num, mut den) = (0f32, 0f32);
                for c in 0..4 {
                    if c == fit.ccs {
                        continue;
                    }
                    let d = ep.e1[c] as f32 - ep.e0[c] as f32;
                    let x = src.px[i][c] as f32 * 257.0 - ep.e0[c] as f32;
                    num += d * x * src.tgt.cw[c];
                    den += d * d * src.tgt.cw[c];
                }
                w1[i] = if den > 0.0 { (num / den).clamp(0.0, 1.0) } else { 0.0 };
                if dual {
                    let c = fit.ccs;
                    let d = ep.e1[c] as f32 - ep.e0[c] as f32;
                    let x = src.px[i][c] as f32 * 257.0 - ep.e0[c] as f32;
                    w2[i] = if d != 0.0 { (x / d).clamp(0.0, 1.0) } else { 0.0 };
                }
            }
        }
        let mut d1 = [0f32; 64];
        decimate(ctx, g, &w1, prm.grid_iters, &mut d1);
        quantize_weights(m.range as usize, &d1[..count], &mut gw1[..count]);
        if dual {
            let mut d2 = [0f32; 64];
            decimate(ctx, g, &w2, prm.grid_iters, &mut d2);
            quantize_weights(m.range as usize, &d2[..count], &mut gw2[..count]);
        }
    }
    mine
}

/// k-means clustering (k = 2..=4) of the block's colours; deterministic
/// farthest-point seeding and a fixed iteration count. Returns labels.
fn k_means(src: &Src, k: usize, labels: &mut [u8; MAX_TEXELS]) {
    let n = src.n;
    let col = |i: usize| -> [f32; 4] {
        let p = src.px[i];
        let cw = src.tgt.cw;
        [p[0] as f32 * cw[0], p[1] as f32 * cw[1], p[2] as f32 * cw[2], p[3] as f32 * cw[3]]
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
const PERM3: [[u8; 3]; 6] = [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]];
const PERM4: [[u8; 4]; 24] = [
    [0, 1, 2, 3], [0, 1, 3, 2], [0, 2, 1, 3], [0, 2, 3, 1], [0, 3, 1, 2], [0, 3, 2, 1],
    [1, 0, 2, 3], [1, 0, 3, 2], [1, 2, 0, 3], [1, 2, 3, 0], [1, 3, 0, 2], [1, 3, 2, 0],
    [2, 0, 1, 3], [2, 0, 3, 1], [2, 1, 0, 3], [2, 1, 3, 0], [2, 3, 0, 1], [2, 3, 1, 0],
    [3, 0, 1, 2], [3, 0, 2, 1], [3, 1, 0, 2], [3, 1, 2, 0], [3, 2, 0, 1], [3, 2, 1, 0],
];

/// Texels a pattern disagrees with a clustering on, under the best label
/// matching (confusion matrix, all permutations).
fn mismatch(pattern: &[u8], labels: &[u8], k: usize) -> u32 {
    let mut cm = [[0u32; 4]; 4];
    for (&a, &b) in pattern.iter().zip(labels) {
        cm[a as usize][b as usize] += 1;
    }
    let n = pattern.len() as u32;
    let mut best = 0u32;
    if k == 3 {
        for p in PERM3.iter() {
            best = best.max(cm[0][p[0] as usize] + cm[1][p[1] as usize] + cm[2][p[2] as usize]);
        }
    } else {
        for p in PERM4.iter() {
            best = best.max(
                cm[0][p[0] as usize] + cm[1][p[1] as usize] + cm[2][p[2] as usize] + cm[3][p[3] as usize],
            );
        }
    }
    n - best
}

/// Lowest absolute correlation between a channel and the others, and that
/// channel (for the dual-plane gate).
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
    let fit1 = make_fit(&src, 1, 0, &zeros, 4);
    search_fit(ctx, prm, &src, &fit1, &mut best);
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
            let fitd = make_fit(&src, 1, 0, &zeros, c);
            search_fit(ctx, prm, &src, &fitd, &mut best);
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
        let mut top: Vec<(u32, usize)> = Vec::with_capacity(m + 1);
        let pats = match k {
            2 => &ctx.part2,
            3 => &ctx.part3,
            _ => &ctx.part4,
        };
        let mut lb = [0u64; 4];
        if k == 2 {
            for i in 0..n {
                if labels[i] == 1 {
                    lb[i / 64] |= 1 << (i % 64);
                }
            }
        }
        for (pi, p) in pats.iter().enumerate() {
            let mis = if k == 2 {
                // popcount of the XOR of bitsets, either labelling.
                let x = (0..4).map(|w| (p.bits[w] ^ lb[w]).count_ones()).sum::<u32>();
                x.min(n as u32 - x)
            } else {
                mismatch(&p.labels, &labels[..n], k)
            };
            let key = (mis, pi);
            if top.len() == m && key >= top[m - 1] {
                continue;
            }
            let pos = top.iter().position(|t| key < *t).unwrap_or(top.len());
            top.insert(pos, key);
            top.truncate(m);
        }
        for &(_, pi) in top.iter() {
            let p = &pats[pi];
            let fitk = make_fit(&src, k as u8, p.seed, &p.labels, 4);
            search_fit(ctx, prm, &src, &fitk, &mut best);
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
