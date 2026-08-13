//! BC7 mode 1: 2 subsets, 6-bit endpoints + one shared p-bit per subset,
//! 3-bit indices, opaque (decodes A=255 — the caller gates on opaque
//! blocks). Search: rank all 64 partitions by SSE-to-subset-mean (the
//! 1-color bound), full-fit the top `RANK_K`.

use super::*;

/// BC7 2-subset partition table (64 shapes); the MSB marks each subset's
/// anchor (fix-up) index. Copied verbatim from bcdec_rs (MIT) — the same
/// table our decode oracle uses.
#[rustfmt::skip]
const P2: [[[u8; 4]; 4]; 64] = [
    [[128, 0, 1, 1], [0, 0, 1, 1], [0, 0, 1, 1], [0, 0, 1, 129]],
    [[128, 0, 0, 1], [0, 0, 0, 1], [0, 0, 0, 1], [0, 0, 0, 129]],
    [[128, 1, 1, 1], [0, 1, 1, 1], [0, 1, 1, 1], [0, 1, 1, 129]],
    [[128, 0, 0, 1], [0, 0, 1, 1], [0, 0, 1, 1], [0, 1, 1, 129]],
    [[128, 0, 0, 0], [0, 0, 0, 1], [0, 0, 0, 1], [0, 0, 1, 129]],
    [[128, 0, 1, 1], [0, 1, 1, 1], [0, 1, 1, 1], [1, 1, 1, 129]],
    [[128, 0, 0, 1], [0, 0, 1, 1], [0, 1, 1, 1], [1, 1, 1, 129]],
    [[128, 0, 0, 0], [0, 0, 0, 1], [0, 0, 1, 1], [0, 1, 1, 129]],
    [[128, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 1], [0, 0, 1, 129]],
    [[128, 0, 1, 1], [0, 1, 1, 1], [1, 1, 1, 1], [1, 1, 1, 129]],
    [[128, 0, 0, 0], [0, 0, 0, 1], [0, 1, 1, 1], [1, 1, 1, 129]],
    [[128, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 1], [0, 1, 1, 129]],
    [[128, 0, 0, 1], [0, 1, 1, 1], [1, 1, 1, 1], [1, 1, 1, 129]],
    [[128, 0, 0, 0], [0, 0, 0, 0], [1, 1, 1, 1], [1, 1, 1, 129]],
    [[128, 0, 0, 0], [1, 1, 1, 1], [1, 1, 1, 1], [1, 1, 1, 129]],
    [[128, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0], [1, 1, 1, 129]],
    [[128, 0, 0, 0], [1, 0, 0, 0], [1, 1, 1, 0], [1, 1, 1, 129]],
    [[128, 1, 129, 1], [0, 0, 0, 1], [0, 0, 0, 0], [0, 0, 0, 0]],
    [[128, 0, 0, 0], [0, 0, 0, 0], [129, 0, 0, 0], [1, 1, 1, 0]],
    [[128, 1, 129, 1], [0, 0, 1, 1], [0, 0, 0, 1], [0, 0, 0, 0]],
    [[128, 0, 129, 1], [0, 0, 0, 1], [0, 0, 0, 0], [0, 0, 0, 0]],
    [[128, 0, 0, 0], [1, 0, 0, 0], [129, 1, 0, 0], [1, 1, 1, 0]],
    [[128, 0, 0, 0], [0, 0, 0, 0], [129, 0, 0, 0], [1, 1, 0, 0]],
    [[128, 1, 1, 1], [0, 0, 1, 1], [0, 0, 1, 1], [0, 0, 0, 129]],
    [[128, 0, 129, 1], [0, 0, 0, 1], [0, 0, 0, 1], [0, 0, 0, 0]],
    [[128, 0, 0, 0], [1, 0, 0, 0], [129, 0, 0, 0], [1, 1, 0, 0]],
    [[128, 1, 129, 0], [0, 1, 1, 0], [0, 1, 1, 0], [0, 1, 1, 0]],
    [[128, 0, 129, 1], [0, 1, 1, 0], [0, 1, 1, 0], [1, 1, 0, 0]],
    [[128, 0, 0, 1], [0, 1, 1, 1], [129, 1, 1, 0], [1, 0, 0, 0]],
    [[128, 0, 0, 0], [1, 1, 1, 1], [129, 1, 1, 1], [0, 0, 0, 0]],
    [[128, 1, 129, 1], [0, 0, 0, 1], [1, 0, 0, 0], [1, 1, 1, 0]],
    [[128, 0, 129, 1], [1, 0, 0, 1], [1, 0, 0, 1], [1, 1, 0, 0]],
    [[128, 1, 0, 1], [0, 1, 0, 1], [0, 1, 0, 1], [0, 1, 0, 129]],
    [[128, 0, 0, 0], [1, 1, 1, 1], [0, 0, 0, 0], [1, 1, 1, 129]],
    [[128, 1, 0, 1], [1, 0, 129, 0], [0, 1, 0, 1], [1, 0, 1, 0]],
    [[128, 0, 1, 1], [0, 0, 1, 1], [129, 1, 0, 0], [1, 1, 0, 0]],
    [[128, 0, 129, 1], [1, 1, 0, 0], [0, 0, 1, 1], [1, 1, 0, 0]],
    [[128, 1, 0, 1], [0, 1, 0, 1], [129, 0, 1, 0], [1, 0, 1, 0]],
    [[128, 1, 1, 0], [1, 0, 0, 1], [0, 1, 1, 0], [1, 0, 0, 129]],
    [[128, 1, 0, 1], [1, 0, 1, 0], [1, 0, 1, 0], [0, 1, 0, 129]],
    [[128, 1, 129, 1], [0, 0, 1, 1], [1, 1, 0, 0], [1, 1, 1, 0]],
    [[128, 0, 0, 1], [0, 0, 1, 1], [129, 1, 0, 0], [1, 0, 0, 0]],
    [[128, 0, 129, 1], [0, 0, 1, 0], [0, 1, 0, 0], [1, 1, 0, 0]],
    [[128, 0, 129, 1], [1, 0, 1, 1], [1, 1, 0, 1], [1, 1, 0, 0]],
    [[128, 1, 129, 0], [1, 0, 0, 1], [1, 0, 0, 1], [0, 1, 1, 0]],
    [[128, 0, 1, 1], [1, 1, 0, 0], [1, 1, 0, 0], [0, 0, 1, 129]],
    [[128, 1, 1, 0], [0, 1, 1, 0], [1, 0, 0, 1], [1, 0, 0, 129]],
    [[128, 0, 0, 0], [0, 1, 129, 0], [0, 1, 1, 0], [0, 0, 0, 0]],
    [[128, 1, 0, 0], [1, 1, 129, 0], [0, 1, 0, 0], [0, 0, 0, 0]],
    [[128, 0, 129, 0], [0, 1, 1, 1], [0, 0, 1, 0], [0, 0, 0, 0]],
    [[128, 0, 0, 0], [0, 0, 129, 0], [0, 1, 1, 1], [0, 0, 1, 0]],
    [[128, 0, 0, 0], [0, 1, 0, 0], [129, 1, 1, 0], [0, 1, 0, 0]],
    [[128, 1, 1, 0], [1, 1, 0, 0], [1, 0, 0, 1], [0, 0, 1, 129]],
    [[128, 0, 1, 1], [0, 1, 1, 0], [1, 1, 0, 0], [1, 0, 0, 129]],
    [[128, 1, 129, 0], [0, 0, 1, 1], [1, 0, 0, 1], [1, 1, 0, 0]],
    [[128, 0, 129, 1], [1, 0, 0, 1], [1, 1, 0, 0], [0, 1, 1, 0]],
    [[128, 1, 1, 0], [1, 1, 0, 0], [1, 1, 0, 0], [1, 0, 0, 129]],
    [[128, 1, 1, 0], [0, 0, 1, 1], [0, 0, 1, 1], [1, 0, 0, 129]],
    [[128, 1, 1, 1], [1, 1, 1, 0], [1, 0, 0, 0], [0, 0, 0, 129]],
    [[128, 0, 0, 1], [1, 0, 0, 0], [1, 1, 1, 0], [0, 1, 1, 129]],
    [[128, 0, 0, 0], [1, 1, 1, 1], [0, 0, 1, 1], [0, 0, 1, 129]],
    [[128, 0, 129, 1], [0, 0, 1, 1], [1, 1, 1, 1], [0, 0, 0, 0]],
    [[128, 0, 129, 0], [0, 0, 1, 0], [1, 1, 1, 0], [1, 1, 1, 0]],
    [[128, 1, 0, 0], [0, 1, 0, 0], [0, 1, 1, 1], [0, 1, 1, 129]],
];

/// Harvest-chosen shape shortlist (357k wins over the bc7 corpus): shape 2
/// alone carries 83.2% of the total mode-1 gain and these eight carry 95%.
/// Trying a fixed shortlist replaces the 64-shape ranking entirely.
const SHORTLIST: [u8; 8] = [2, 10, 13, 16, 0, 23, 15, 14];

pub(super) fn try_bc7_mode1(pixels: &[[u8; 4]; 16], err_limit: i64) -> Option<([u8; 16], i64)> {
    // NOTE: a color-space 2-cluster pre-gate was tried and REVERTED — the
    // biggest winners (startscreen +13.5 dB) are GRADIENT blocks where a
    // spatial split lets two shorter LINES fit; point-cluster bounds are
    // blind to that structure and killed 100% of those gains.
    let mut sq = 0i64;
    for p in pixels {
        for c in 0..3 {
            sq += (p[c] as i64) * (p[c] as i64);
        }
    }
    let mut best: Option<([u8; 16], i64)> = None;
    let mut best_err = err_limit;
    for &part in &SHORTLIST {
        let tbl = &P2[part as usize];
        let mut sum = [[0i64; 3]; 2];
        let mut cnt = [0i64; 2];
        for (i, p) in pixels.iter().enumerate() {
            let s = (tbl[i / 4][i % 4] & 0x7F) as usize;
            for c in 0..3 {
                sum[s][c] += p[c] as i64;
            }
            cnt[s] += 1;
        }
        let mut term = 0i64;
        for s in 0..2 {
            for c in 0..3 {
                term += sum[s][c] * sum[s][c] / cnt[s];
            }
        }
        // Promise gate: the 2-cluster bound must project a >=2x reduction —
        // a marginal promise never survives quantization + 3-bit indices.
        let est = sq - term;
        if est * 2 >= best_err {
            continue;
        }
        if let Some((bits, err)) = fit_partition(pixels, part, best_err) {
            if err < best_err {
                best_err = err;
                best = Some((bits, err));
            }
        }
    }
    best
}

fn fit_partition(pixels: &[[u8; 4]; 16], part: u8, err_limit: i64) -> Option<([u8; 16], i64)> {
    let tbl = &P2[part as usize];
    let mut members: [([usize; 16], usize); 2] = [([0; 16], 0); 2];
    let mut anchor1 = 0usize;
    for i in 0..16 {
        let v = tbl[i / 4][i % 4];
        let s = (v & 0x7F) as usize;
        let slot = members[s].1;
        members[s].0[slot] = i;
        members[s].1 += 1;
        if v & 0x80 != 0 && s == 1 {
            anchor1 = i;
        }
    }

    let mut q = [[[0u8; 3]; 2]; 2]; // [subset][endpoint][chan]
    let mut pbits = [0u8; 2];
    let mut indices = [0u8; 16];
    let mut total_err = 0i64;

    for s in 0..2 {
        let (idxs, n) = (members[s].0, members[s].1);
        // seed endpoints: luminance extrema over the subset
        let mut min_l = i32::MAX;
        let mut max_l = i32::MIN;
        let mut e0 = [0u8; 3];
        let mut e1 = [0u8; 3];
        for &i in &idxs[..n] {
            let p = pixels[i];
            let l = p[0] as i32 * 2 + p[1] as i32 * 3 + p[2] as i32;
            if l > max_l {
                max_l = l;
                e0 = [p[0], p[1], p[2]];
            }
            if l < min_l {
                min_l = l;
                e1 = [p[0], p[1], p[2]];
            }
        }
        // Abort budget: whatever the other subset hasn't spent yet.
        let budget = (err_limit - total_err).clamp(0, i32::MAX as i64) as i32;
        let (bq, bp, bidx, berr) = fit_subset(pixels, &idxs[..n], e0, e1, budget);
        if berr == i32::MAX {
            return None; // no p/seed combination stayed under the budget
        }
        q[s] = bq;
        pbits[s] = bp;
        for (k, &i) in idxs[..n].iter().enumerate() {
            indices[i] = bidx[k];
        }
        total_err += berr as i64;
        if total_err >= err_limit {
            return None;
        }
    }

    // Anchor constraints: pixel 0 (subset 0) and anchor1 (subset 1) need
    // index MSB 0; W3 symmetry keeps recon identical under swap+invert.
    for &(s, anchor) in &[(0usize, 0usize), (1, anchor1)] {
        if indices[anchor] >= 4 {
            q[s].swap(0, 1);
            let (idxs, n) = (members[s].0, members[s].1);
            for &i in &idxs[..n] {
                indices[i] = 7 - indices[i];
            }
        }
    }

    Some((pack(part, &q, &pbits, &indices, anchor1), total_err))
}

/// Fit one subset: both shared p-bits, per-channel ±1 quantizer search,
/// W3 index fit, one LS refit round.
fn fit_subset(
    pixels: &[[u8; 4]; 16],
    idxs: &[usize],
    e0: [u8; 3],
    e1: [u8; 3],
    budget: i32,
) -> ([[u8; 3]; 2], u8, [u8; 16], i32) {
    let mut best_q = [[0u8; 3]; 2];
    let mut best_p = 0u8;
    let mut best_idx = [0u8; 16];
    let mut best_err = i32::MAX;
    let mut seeds = [(e0, e1); 2];
    for pass in 0..2 {
        let (s0, s1) = seeds[pass];
        for p in 0..2u8 {
            let q0 = quantize6p(s0, p);
            let q1 = quantize6p(s1, p);
            let c0 = unquant6p(q0, p);
            let c1 = unquant6p(q1, p);
            let mut pal = [[0u8; 3]; 8];
            for (k, &w) in W3.iter().enumerate() {
                for c in 0..3 {
                    pal[k][c] = (((64 - w) * c0[c] as u32 + w * c1[c] as u32 + 32) / 64) as u8;
                }
            }
            let limit = best_err.min(budget.saturating_add(1));
            let mut idx = [0u8; 16];
            let mut err = 0i32;
            for (k, &i) in idxs.iter().enumerate() {
                let px = pixels[i];
                let mut bi = 0u8;
                let mut be = i32::MAX;
                for (j, pc) in pal.iter().enumerate() {
                    let e = sqr_rgb([px[0], px[1], px[2]], *pc);
                    if e < be {
                        be = e;
                        bi = j as u8;
                    }
                }
                idx[k] = bi;
                err += be;
                if err >= limit {
                    err = i32::MAX;
                    break;
                }
            }
            if err < best_err {
                best_err = err;
                best_q = [q0, q1];
                best_p = p;
                best_idx = idx;
            }
        }
        if pass == 0 {
            if best_err == i32::MAX {
                break; // both p-bits blew the budget; LS has nothing to refine
            }
            if let Some((r0, r1)) = ls_endpoints(pixels, idxs, &best_idx) {
                seeds[1] = (r0, r1);
            } else {
                break;
            }
        }
    }
    (best_q, best_p, best_idx, best_err)
}

fn ls_endpoints(
    pixels: &[[u8; 4]; 16],
    idxs: &[usize],
    indices: &[u8; 16],
) -> Option<([u8; 3], [u8; 3])> {
    let mut a00 = 0f32;
    let mut a01 = 0f32;
    let mut a11 = 0f32;
    let mut b0 = [0f32; 3];
    let mut b1 = [0f32; 3];
    for (k, &i) in idxs.iter().enumerate() {
        let w = W3[indices[k] as usize] as f32 / 64.0;
        let u = 1.0 - w;
        a00 += u * u;
        a01 += u * w;
        a11 += w * w;
        for c in 0..3 {
            let x = pixels[i][c] as f32;
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

/// 6-bit quantizer under a shared p-bit: dequant v7=(q<<1)|p, v8=(v7<<1)|(v7>>6).
fn quantize6p(c: [u8; 3], p: u8) -> [u8; 3] {
    let mut q = [0u8; 3];
    for i in 0..3 {
        let base = c[i] >> 2;
        let mut bq = base.min(63);
        let mut be = i32::MAX;
        for cand in base.saturating_sub(1)..=(base + 1).min(63) {
            let r = unquant6p_chan(cand, p);
            let e = (r as i32 - c[i] as i32).pow(2);
            if e < be {
                be = e;
                bq = cand;
            }
        }
        q[i] = bq;
    }
    q
}

#[inline]
fn unquant6p_chan(q: u8, p: u8) -> u8 {
    let v7 = (q << 1) | p;
    (v7 << 1) | (v7 >> 6)
}

fn unquant6p(q: [u8; 3], p: u8) -> [u8; 3] {
    [
        unquant6p_chan(q[0], p),
        unquant6p_chan(q[1], p),
        unquant6p_chan(q[2], p),
    ]
}

fn pack(
    part: u8,
    q: &[[[u8; 3]; 2]; 2],
    pbits: &[u8; 2],
    indices: &[u8; 16],
    anchor1: usize,
) -> [u8; 16] {
    let mut bw = BitWriter::default();
    bw.write_bits(0, 1);
    bw.write_bits(1, 1); // mode 1
    bw.write_bits(part as u32, 6);
    for c in 0..3 {
        for s in 0..2 {
            for e in 0..2 {
                bw.write_bits(q[s][e][c] as u32, 6);
            }
        }
    }
    bw.write_bits(pbits[0] as u32, 1);
    bw.write_bits(pbits[1] as u32, 1);
    for (i, &v) in indices.iter().enumerate() {
        let bits = if i == 0 || i == anchor1 { 2 } else { 3 };
        bw.write_bits(v as u32, bits);
    }
    bw.into_array()
}
