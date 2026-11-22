//! HDR encode (plan E7), first version: one partition, HDR endpoint modes 11
//! (RGB), 14 (RGB + LDR alpha) and 15 (RGB + HDR alpha).
//!
//! ASTC interpolates HDR components linearly in a 16-bit pseudo-logarithmic
//! (LNS) domain and converts to FP16 afterwards. The encoder therefore works
//! in that domain: every texel is mapped to the LNS value whose conversion
//! gives its FP16 value, lines are fitted there, and the exact error is the
//! squared difference of the decoder's interpolated LNS value and the target
//! (LDR alpha lanes use the UNORM16 domain). Endpoint values are produced for
//! every mode-11 sub-mode and major component (and the direct form), decoded
//! with the decoder's own endpoint unpacking, and the best kept. Like the LDR
//! encoder, the final choice is made on the exact decoded error.

use alloc::vec::Vec;

use super::block::{decimate, infill_eff, quantize_weights, Best, MAX_TEXELS};
use super::context::Ctx;
use crate::astc::cem;
use crate::astc::ise::LEVELS;
use crate::astc::parse::{BlockKind, SymbolicBlock};
use crate::astc::quant::COLOR_NEAREST;
use crate::astc::texel::lerp;
use crate::astc::AstcProfile;

/// f32 to binary16 bits, round to nearest even; NaN and negatives become 0,
/// values above the half range clamp to 65504 (ASTC HDR has no negatives).
pub(crate) fn f32_to_f16_clamped(v: f32) -> u16 {
    if v.is_nan() || v <= 0.0 {
        return 0;
    }
    let b = v.to_bits();
    let e = ((b >> 23) & 0xFF) as i32 - 127 + 15;
    let m = b & 0x7F_FFFF;
    if e >= 31 {
        return 0x7BFF;
    }
    if e <= 0 {
        // Subnormal half.
        if e < -10 {
            return 0;
        }
        let m = m | 0x80_0000;
        let shift = (14 - e) as u32;
        let half = m >> shift;
        let rem = m & ((1 << shift) - 1);
        let mid = 1 << (shift - 1);
        let r = if rem > mid || (rem == mid && half & 1 == 1) {
            half + 1
        } else {
            half
        };
        return r as u16;
    }
    let half = ((e as u32) << 10) | (m >> 13);
    let rem = m & 0x1FFF;
    let r = if rem > 0x1000 || (rem == 0x1000 && half & 1 == 1) {
        half + 1
    } else {
        half
    };
    (r.min(0x7BFF)) as u16
}

/// The LNS value (16-bit domain, continuous) whose conversion gives the
/// half `h` (inverse of the decoder's piecewise mantissa map, bucket centre).
fn f16_to_lns(h: u16) -> f32 {
    let e = (h >> 10) as f32;
    let mt = ((h & 0x3FF) as f32) * 8.0 + 3.5;
    let m = if mt < 1536.0 {
        mt / 3.0
    } else if mt < 5632.0 {
        (mt + 512.0) / 4.0
    } else {
        (mt + 2048.0) / 5.0
    };
    e * 2048.0 + m.min(2047.0)
}

/// Field bit destinations of the six extra bits X0..X5 per mode-11 sub-mode:
/// (field, bit), fields 0 a, 1 b0, 2 b1, 3 c, 4 d0, 5 d1 (from the
/// specification's endpoint bit mode table, as the decoder reads them).
const XDEST: [[(u8, u8); 6]; 8] = [
    [(1, 6), (2, 6), (4, 6), (5, 6), (4, 5), (5, 5)],
    [(1, 6), (2, 6), (1, 7), (2, 7), (4, 5), (5, 5)],
    [(0, 9), (3, 6), (4, 6), (5, 6), (4, 5), (5, 5)],
    [(1, 6), (2, 6), (0, 9), (3, 6), (4, 5), (5, 5)],
    [(1, 6), (2, 6), (1, 7), (2, 7), (0, 9), (0, 10)],
    [(0, 9), (0, 10), (3, 7), (3, 6), (4, 5), (5, 5)],
    [(1, 6), (2, 6), (0, 11), (3, 6), (0, 9), (0, 10)],
    [(0, 9), (0, 10), (0, 11), (3, 6), (4, 5), (5, 5)],
];

/// Candidate value sextets for HDR RGB endpoints `e0`, `e1` (12-bit units).
fn mode11_candidates(e0: [f32; 3], e1: [f32; 3], out: &mut Vec<[u8; 6]>) {
    out.clear();
    let r = |x: f32, s: f32, max: i32| ((x / s + 0.5) as i32).clamp(0, max);
    // Direct form: R and G 8-bit, B 7-bit.
    out.push([
        r(e0[0], 16.0, 255) as u8,
        r(e1[0], 16.0, 255) as u8,
        r(e0[1], 16.0, 255) as u8,
        r(e1[1], 16.0, 255) as u8,
        0x80 | r(e0[2], 32.0, 127) as u8,
        0x80 | r(e1[2], 32.0, 127) as u8,
    ]);
    for maj in 0..3usize {
        let (mut a0, mut a1) = (e0, e1);
        if maj == 1 {
            a0.swap(0, 1);
            a1.swap(0, 1);
        } else if maj == 2 {
            a0.swap(0, 2);
            a1.swap(0, 2);
        }
        for mode in 0..8usize {
            let mut nb = [9u32, 6, 6, 6, 5, 5];
            for &(f, _) in XDEST[mode].iter() {
                nb[f as usize] += 1;
            }
            let unit = (1u32 << ((mode as u32 >> 1) ^ 3)) as f32;
            let cval = a1[0] - a0[0];
            let f = [
                a1[0],
                a1[0] - a1[1],
                a1[0] - a1[2],
                cval,
                (a1[1] - a0[1]) - cval,
                (a1[2] - a0[2]) - cval,
            ];
            let mut q = [0i32; 6];
            for k in 0..6 {
                let v = (f[k] / unit + if f[k] >= 0.0 { 0.5 } else { -0.5 }) as i32;
                q[k] = if k >= 4 {
                    let h = 1i32 << (nb[k] - 1);
                    v.clamp(-h, h - 1)
                } else {
                    v.clamp(0, (1 << nb[k]) - 1)
                };
            }
            let bit = |field: usize, b: u8| -> u8 { ((q[field] >> b) & 1) as u8 };
            let mut v = [0u8; 6];
            v[0] = (q[0] & 0xFF) as u8;
            v[1] = ((mode as u8 & 1) << 7) | (bit(0, 8) << 6) | (q[3] & 0x3F) as u8;
            v[2] = (((mode as u8 >> 1) & 1) << 7) | (q[1] & 0x3F) as u8;
            v[3] = (((mode as u8 >> 2) & 1) << 7) | (q[2] & 0x3F) as u8;
            v[4] = ((maj as u8 & 1) << 7) | (q[4] & 0x1F) as u8;
            v[5] = ((maj as u8 >> 1) << 7) | (q[5] & 0x1F) as u8;
            let xd = XDEST[mode];
            let xb = |k: usize| bit(xd[k].0 as usize, xd[k].1);
            v[2] |= xb(0) << 6;
            v[3] |= xb(1) << 6;
            v[4] |= xb(2) << 6;
            v[5] |= xb(3) << 6;
            v[4] |= xb(4) << 5;
            v[5] |= xb(5) << 5;
            out.push(v);
        }
    }
}

/// Encode one HDR block. `t` holds the targets in the interpolation domain
/// (LNS for HDR lanes, UNORM16 for LDR alpha); `h` the half values (for void
/// extent); `opaque` means alpha is 1.0 everywhere.
pub(crate) fn encode_block_hdr(
    ctx: &Ctx,
    profile: AstcProfile,
    exact: usize,
    t: &[[f32; 4]],
    h: &[[u16; 4]],
    opaque: bool,
) -> Best {
    let n = t.len();
    if h.iter().all(|p| *p == h[0]) {
        let mut s = SymbolicBlock::empty(BlockKind::VoidHdr, 0);
        s.void_color = h[0];
        return Best { err: 0.0, sym: s };
    }
    let cem_v: u8 = if opaque {
        11
    } else if profile == AstcProfile::Hdr {
        15
    } else {
        14
    };
    let nvals = cem::value_count(cem_v) as u32;
    let chans = if opaque { 3 } else { 4 };
    // Principal-axis line in the domain.
    let mut mean = [0f32; 4];
    for p in t {
        for c in 0..chans {
            mean[c] += p[c];
        }
    }
    for m in mean.iter_mut() {
        *m /= n as f32;
    }
    let mut cov = [[0f32; 4]; 4];
    for p in t {
        let d: [f32; 4] = core::array::from_fn(|c| if c < chans { p[c] - mean[c] } else { 0.0 });
        for a in 0..4 {
            for b in 0..4 {
                cov[a][b] += d[a] * d[b];
            }
        }
    }
    let mut dir = [1f32, 1.0, 1.0, if opaque { 0.0 } else { 1.0 }];
    for _ in 0..8 {
        let nd: [f32; 4] = core::array::from_fn(|a| (0..4).map(|b| cov[a][b] * dir[b]).sum());
        let mx = nd.iter().fold(0f32, |m, &v| m.max(v.abs()));
        if mx <= 1e-12 {
            break;
        }
        dir = nd.map(|v| v / mx);
    }
    if dir[0] + dir[1] + dir[2] < 0.0 {
        dir = dir.map(|v| -v);
    }
    let dd: f32 = dir.iter().map(|v| v * v).sum();
    let mut ts = [0f32; MAX_TEXELS];
    let (mut tmin, mut tmax) = (f32::MAX, f32::MIN);
    for (i, p) in t.iter().enumerate() {
        let v: f32 = if dd > 0.0 {
            (0..chans).map(|c| (p[c] - mean[c]) * dir[c]).sum::<f32>() / dd
        } else {
            0.0
        };
        ts[i] = v;
        tmin = tmin.min(v);
        tmax = tmax.max(v);
    }
    let span = tmax - tmin;
    let mut ideal = [0f32; MAX_TEXELS];
    for i in 0..n {
        ideal[i] = if span > 0.0 {
            (ts[i] - tmin) / span
        } else {
            0.0
        };
    }
    // Candidate modes: single plane, ranked by decimation residual plus the
    // weight quantisation noise; colour range from the bit budget.
    let mut cands: Vec<(f32, usize, usize)> = Vec::new();
    let mut dec = alloc::vec![[0f32; 64]; ctx.grids.len()];
    let mut res = alloc::vec![-1f32; ctx.grids.len()];
    let len2: f32 = (0..chans).map(|c| (dir[c] * span) * (dir[c] * span)).sum();
    for (mi, m) in ctx.modes.iter().enumerate() {
        if m.dual {
            continue;
        }
        let g = m.grid as usize;
        if res[g] < 0.0 {
            decimate(ctx, g, &ideal, 2, &mut dec[g]);
            let mut e = 0f32;
            for (ti, &(nt, tp)) in ctx.grids[g].taps.iter().enumerate() {
                let v: f32 = tp[..nt as usize]
                    .iter()
                    .map(|&(k, c)| dec[g][k as usize] * c as f32)
                    .sum();
                let d = v / (16.0 * 64.0) - ideal[ti];
                e += d * d;
            }
            res[g] = e * len2;
        }
        let r = m.crange[0][(nvals / 2 - 1) as usize];
        if r == 0xFF {
            continue;
        }
        let lw = LEVELS[m.range as usize] as f32;
        let wq = n as f32 * len2 * ctx.grids[g].noise / (12.0 * (lw - 1.0) * (lw - 1.0));
        let lc = LEVELS[r as usize] as f32;
        let step = 65535.0 / (lc - 1.0);
        let cq = n as f32 * step * step / 12.0 * 0.5;
        cands.push((res[g] + wq + cq, mi, r as usize));
    }
    cands.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(core::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });
    cands.truncate(exact.max(1));
    let mut best = Best {
        err: f64::MAX,
        sym: SymbolicBlock::empty(BlockKind::VoidHdr, 0),
    };
    let mut six: Vec<[u8; 6]> = Vec::new();
    for &(_, mi, range) in &cands {
        let m = ctx.modes[mi];
        let g = m.grid as usize;
        let count = ctx.grids[g].count;
        let mut gw = [0u8; 64];
        quantize_weights(m.range as usize, &dec[g][..count], &mut gw[..count]);
        let mut eff = [0u8; MAX_TEXELS];
        for _pass in 0..2 {
            infill_eff(ctx, g, &gw, &mut eff);
            // Least-squares endpoints per channel in the domain.
            let mut e0 = [0f32; 4];
            let mut e1 = [0f32; 4];
            for c in 0..chans {
                let (mut a, mut b, mut d, mut x0, mut x1) = (0f32, 0f32, 0f32, 0f32, 0f32);
                for i in 0..n {
                    let w = eff[i] as f32 / 64.0;
                    let u = 1.0 - w;
                    a += u * u;
                    b += u * w;
                    d += w * w;
                    x0 += u * t[i][c];
                    x1 += w * t[i][c];
                }
                let det = a * d - b * b;
                if det.abs() > 1e-6 {
                    e0[c] = ((x0 * d - x1 * b) / det).clamp(0.0, 65535.0);
                    e1[c] = ((x1 * a - x0 * b) / det).clamp(0.0, 65535.0);
                } else {
                    let mean_c = t.iter().map(|p| p[c]).sum::<f32>() / n as f32;
                    e0[c] = mean_c;
                    e1[c] = mean_c;
                }
            }
            let to12 = |v: f32| v / 16.0;
            mode11_candidates(
                [to12(e0[0]), to12(e0[1]), to12(e0[2])],
                [to12(e1[0]), to12(e1[1]), to12(e1[2])],
                &mut six,
            );
            let mut pass_best: Option<(f64, [u8; 8])> = None;
            for s6 in &six {
                let mut v = [0u8; 8];
                v[..6].copy_from_slice(s6);
                if cem_v == 14 {
                    v[6] = (e0[3] / 257.0 + 0.5).clamp(0.0, 255.0) as u8;
                    v[7] = (e1[3] / 257.0 + 0.5).clamp(0.0, 255.0) as u8;
                } else if cem_v == 15 {
                    v[6] = 0x80 | (to12(e0[3]) / 32.0 + 0.5).clamp(0.0, 127.0) as u8;
                    v[7] = 0x80 | (to12(e1[3]) / 32.0 + 0.5).clamp(0.0, 127.0) as u8;
                }
                for x in v.iter_mut().take(nvals as usize) {
                    *x = COLOR_NEAREST[range][*x as usize];
                }
                let ep = cem::expand(&cem::decode(cem_v, &v), profile);
                let mut err = 0f64;
                for i in 0..n {
                    for c in 0..chans {
                        let d = lerp(ep.e0[c], ep.e1[c], eff[i]) as f64 - t[i][c] as f64;
                        err += d * d;
                    }
                }
                if pass_best.map_or(true, |(b, _)| err < b) {
                    pass_best = Some((err, v));
                }
            }
            let Some((err, v)) = pass_best else { break };
            if err < best.err {
                let mut s = SymbolicBlock::empty(BlockKind::Normal, m.mode);
                let gr = &ctx.grids[g];
                s.grid = [gr.w, gr.h, gr.d];
                s.weight_range = m.range;
                s.partitions = 1;
                s.cem = [cem_v; 4];
                s.color_range = range as u8;
                s.color_count = nvals as u8;
                s.color_values[..nvals as usize].copy_from_slice(&v[..nvals as usize]);
                s.weights[..count].copy_from_slice(&gw[..count]);
                best = Best { err, sym: s };
            }
            // Re-derive ideal weights from the decoded endpoints.
            let ep = cem::expand(&cem::decode(cem_v, &v), profile);
            let mut w = [0f32; MAX_TEXELS];
            for i in 0..n {
                let (mut num, mut den) = (0f32, 0f32);
                for c in 0..chans {
                    let d = ep.e1[c] as f32 - ep.e0[c] as f32;
                    num += d * (t[i][c] - ep.e0[c] as f32);
                    den += d * d;
                }
                w[i] = if den > 0.0 {
                    (num / den).clamp(0.0, 1.0)
                } else {
                    0.0
                };
            }
            let mut d1 = [0f32; 64];
            decimate(ctx, g, &w, 2, &mut d1);
            quantize_weights(m.range as usize, &d1[..count], &mut gw[..count]);
        }
    }
    best
}

/// Per-texel targets for a block of f32 RGBA texels.
pub(crate) fn targets(
    px: &[[f32; 4]],
    profile: AstcProfile,
    t: &mut [[f32; 4]],
    h: &mut [[u16; 4]],
) -> bool {
    let mut opaque = true;
    for (i, p) in px.iter().enumerate() {
        for c in 0..4 {
            h[i][c] = f32_to_f16_clamped(p[c]);
        }
        opaque &= h[i][3] == 0x3C00;
        for c in 0..3 {
            t[i][c] = f16_to_lns(h[i][c]);
        }
        t[i][3] = if profile == AstcProfile::Hdr {
            f16_to_lns(h[i][3])
        } else {
            (p[3].clamp(0.0, 1.0) * 65535.0 + 0.5).floor()
        };
    }
    opaque
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::astc::texel::{f16_to_f32, lns_to_f16};

    #[test]
    fn f16_conversion_round_trips_halves() {
        for h in 0..0x7C00u16 {
            assert_eq!(f32_to_f16_clamped(f16_to_f32(h)), h, "{h:#x}");
        }
        assert_eq!(f32_to_f16_clamped(-1.0), 0);
        assert_eq!(f32_to_f16_clamped(1e9), 0x7BFF);
    }

    #[test]
    fn lns_inverse_lands_in_the_bucket() {
        for h in 0..0x7C00u16 {
            let l = f16_to_lns(h).round() as u32;
            assert_eq!(lns_to_f16(l.min(0xFFFF) as u16), h, "{h:#x}");
        }
    }

    #[test]
    fn mode11_candidates_decode_near_target() {
        // The best candidate decodes within a few 12-bit steps of the target.
        let e0 = [1000.0, 1500.0, 900.0];
        let e1 = [3000.0, 2900.0, 2000.0];
        let mut v = Vec::new();
        mode11_candidates(e0, e1, &mut v);
        let best = v
            .iter()
            .map(|s| {
                let raw = cem::decode(11, s);
                (0..3)
                    .map(|c| {
                        let a = raw.e0[c] as f32 - e0[c];
                        let b = raw.e1[c] as f32 - e1[c];
                        (a * a + b * b) as f64
                    })
                    .sum::<f64>()
            })
            .fold(f64::MAX, f64::min);
        assert!(best < 6.0 * 16.0 * 16.0, "{best}");
    }
}
