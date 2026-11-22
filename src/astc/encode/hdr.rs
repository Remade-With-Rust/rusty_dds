//! HDR encode (plan E7): one partition, HDR endpoint modes 11 (RGB), 7 (RGB
//! base + scale, opaque blocks), 14 (RGB + LDR alpha) and 15 (RGB + HDR
//! alpha).
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
// One index walks several parallel per-texel / per-channel arrays (and in the encoder fixes the floating-point operation order); iterator chains would obscure that.
#[allow(clippy::needless_range_loop)]
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

/// Where mode 7 keeps each field bit, per sub-mode: `bit[mode][field][i]` is
/// the (value, bit) holding bit `i` of field `field` (red, green, blue,
/// scale, before the shift), and `width[mode][field]` how many bits the
/// field has. Derived by probing the decoder's own field assembly
/// (`cem::mode7_fields`), so it cannot disagree with it.
pub(crate) struct Mode7 {
    bit: [[[(u8, u8); 12]; 4]; 6],
    width: [[u8; 4]; 6],
}

/// `modeval` (the four mode bits) for a major component and sub-mode, if
/// the pair is encodable.
fn mode7_modeval(maj: usize, mode: usize) -> Option<u32> {
    match (maj, mode) {
        (0..=2, 0..=3) => Some(((maj as u32) << 2) | mode as u32),
        (0..=2, 4) => Some(0xC | maj as u32),
        (0, 5) => Some(0xF),
        _ => None,
    }
}

/// The four values with only the mode bits of `modeval` set.
fn mode7_base(modeval: u32) -> [u8; 4] {
    [
        ((modeval & 3) << 6) as u8,
        (((modeval >> 2) & 1) << 7) as u8,
        (((modeval >> 3) & 1) << 7) as u8,
        0,
    ]
}

impl Mode7 {
    pub(crate) fn new() -> Self {
        let mut t = Mode7 {
            bit: [[[(0, 0); 12]; 4]; 6],
            width: [[0; 4]; 6],
        };
        for mode in 0..6 {
            let base = mode7_base(mode7_modeval(0, mode).unwrap());
            let as_i = |v: [u8; 4]| [v[0] as i32, v[1] as i32, v[2] as i32, v[3] as i32];
            let (_, _, f0) = cem::mode7_fields(&as_i(base));
            // Free bits: every bit but the four mode bits.
            for (k, mask) in [(0usize, 0x3Fu8), (1, 0x7F), (2, 0x7F), (3, 0xFF)] {
                for b in 0..8u8 {
                    if mask >> b & 1 == 0 {
                        continue;
                    }
                    let mut v = base;
                    v[k] |= 1 << b;
                    let (maj, m, f) = cem::mode7_fields(&as_i(v));
                    debug_assert!(maj == 0 && m == mode as i32);
                    for fi in 0..4 {
                        let d = f[fi] ^ f0[fi];
                        if d != 0 {
                            let i = d.trailing_zeros() as usize;
                            t.bit[mode][fi][i] = (k as u8, b);
                            t.width[mode][fi] = t.width[mode][fi].max(i as u8 + 1);
                        }
                    }
                }
            }
        }
        t
    }
}

/// Mode 7 candidate values for HDR RGB endpoints `e0`, `e1` (12-bit units):
/// every encodable (major component, sub-mode), with e1 as the base and the
/// mean per-channel drop to e0 as the scale.
// One index walks parallel per-channel arrays (and fixes the operation order).
#[allow(clippy::needless_range_loop)]
fn mode7_candidates(t: &Mode7, e0: [f32; 3], e1: [f32; 3], out: &mut Vec<[u8; 4]>) {
    out.clear();
    let scale = ((e1[0] - e0[0]) + (e1[1] - e0[1]) + (e1[2] - e0[2])) * (1.0 / 3.0);
    for maj in 0..3usize {
        let mut a = e1;
        a.swap(0, maj);
        for mode in 0..6usize {
            let Some(modeval) = mode7_modeval(maj, mode) else {
                continue;
            };
            const SHAMTS: [u32; 6] = [1, 1, 2, 3, 4, 5];
            let unit = (1u32 << SHAMTS[mode]) as f32;
            let f = if mode == 5 {
                [a[0], a[1], a[2], scale]
            } else {
                [a[0], a[0] - a[1], a[0] - a[2], scale]
            };
            let mut v = mode7_base(modeval);
            for (fi, &x) in f.iter().enumerate() {
                let w = t.width[mode][fi] as u32;
                let q = ((x / unit + 0.5).max(0.0) as u32).min((1 << w) - 1);
                for i in 0..w as usize {
                    if q >> i & 1 != 0 {
                        let (k, b) = t.bit[mode][fi][i];
                        v[k as usize] |= 1 << b;
                    }
                }
            }
            out.push(v);
        }
    }
}

/// Squared distance of the endpoints `v` decode to from the fitted
/// endpoints `target` (interpolation domain), over `chans` channels.
fn endpoint_err(
    cem_v: u8,
    profile: AstcProfile,
    v: &[u8; 8],
    target: ([f32; 4], [f32; 4]),
    chans: usize,
) -> f64 {
    let ep = cem::expand(&cem::decode(cem_v, v), profile);
    let mut e = 0f64;
    for c in 0..chans {
        let a = ep.e0[c] as f64 - target.0[c] as f64;
        let b = ep.e1[c] as f64 - target.1[c] as f64;
        e += a * a + b * b;
    }
    e
}

/// The representable value of `range` next above (`up`) or below `x`.
fn step_level(range: usize, x: u8, up: bool) -> Option<u8> {
    let row = &COLOR_NEAREST[range];
    if up {
        (x as usize + 1..256).map(|y| row[y]).find(|&q| q > x)
    } else {
        (0..x as usize).rev().map(|y| row[y]).find(|&q| q < x)
    }
}

/// Coordinate search over the packed values `v` (each moved one
/// representable level at a time) minimising [`endpoint_err`]; `e` is the
/// error of `v` on entry. True if `v` changed.
#[allow(clippy::too_many_arguments)]
fn repair(
    cem_v: u8,
    profile: AstcProfile,
    range: usize,
    nvals: usize,
    v: &mut [u8; 8],
    mut e: f64,
    target: ([f32; 4], [f32; 4]),
    chans: usize,
) -> bool {
    let mut changed = false;
    for _ in 0..3 {
        let mut moved = false;
        for k in 0..nvals {
            for up in [false, true] {
                let Some(q) = step_level(range, v[k], up) else {
                    continue;
                };
                let mut w = *v;
                w[k] = q;
                let ew = endpoint_err(cem_v, profile, &w, target, chans);
                if ew < e {
                    *v = w;
                    e = ew;
                    moved = true;
                    changed = true;
                }
            }
        }
        if !moved {
            break;
        }
    }
    changed
}

/// Least-squares endpoints under mode 7's model, value = e1 - (1 - w) s
/// with one scale s for R, G and B (interpolation domain, weights `eff`):
/// with u = 1 - w, s = (U X / n - sum u x) / (3 (sum u^2 - U^2 / n)) and
/// e1 = (X + s U) / n per channel (U = sum u, X = sum x). Returns (e0, e1)
/// with alpha 0 (mode 7 has none).
fn mode7_fit(t: &[[f32; 4]], eff: &[u8; MAX_TEXELS]) -> ([f32; 4], [f32; 4]) {
    let n = t.len() as f32;
    let (mut su, mut su2, mut sux) = (0f32, 0f32, 0f32);
    let mut sx = [0f32; 3];
    for (i, ti) in t.iter().enumerate() {
        let u = 1.0 - eff[i] as f32 / 64.0;
        su += u;
        su2 += u * u;
        for c in 0..3 {
            sx[c] += ti[c];
            sux += u * ti[c];
        }
    }
    let den = 3.0 * (su2 - su * su / n);
    let s = if den > 1e-6 {
        ((su * (sx[0] + sx[1] + sx[2]) / n - sux) / den).max(0.0)
    } else {
        0.0
    };
    let mut e0 = [0f32; 4];
    let mut e1 = [0f32; 4];
    for c in 0..3 {
        e1[c] = ((sx[c] + s * su) / n).clamp(0.0, 65535.0);
        e0[c] = (e1[c] - s).max(0.0);
    }
    (e0, e1)
}

/// Exact error of the values `v` (endpoint mode `cem_v`) against the
/// targets `t` with texel weights `eff`: squared differences of the
/// decoder's interpolated values (interpolation domain), over `chans`.
// One index walks parallel per-channel arrays (and fixes the operation order).
#[allow(clippy::needless_range_loop)]
fn texel_err(
    cem_v: u8,
    profile: AstcProfile,
    v: &[u8; 8],
    eff: &[u8; MAX_TEXELS],
    t: &[[f32; 4]],
    chans: usize,
) -> f64 {
    let ep = cem::expand(&cem::decode(cem_v, v), profile);
    let mut err = 0f64;
    for (i, ti) in t.iter().enumerate() {
        for c in 0..chans {
            let d = lerp(ep.e0[c], ep.e1[c], eff[i]) as f64 - ti[c] as f64;
            err += d * d;
        }
    }
    err
}

/// Below 256 levels, rounding each packed value on its own can flip the
/// mode and field bits it carries. Repair the two candidates whose decoded
/// endpoints land nearest the fitted ones by a coordinate search over
/// neighbouring levels ([`repair`]), and append the repaired values; the
/// exact texel error then chooses among the originals and the repaired.
#[allow(clippy::too_many_arguments)]
fn repair_best(
    cem_v: u8,
    profile: AstcProfile,
    range: usize,
    nvals: usize,
    quantized: &mut Vec<[u8; 8]>,
    target: ([f32; 4], [f32; 4]),
    chans: usize,
) {
    if range >= COLOR_NEAREST.len() - 1 {
        return;
    }
    let mut ranked: Vec<(f64, usize)> = quantized
        .iter()
        .enumerate()
        .map(|(j, v)| (endpoint_err(cem_v, profile, v, target, chans), j))
        .collect();
    ranked.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    for &(e, j) in ranked.iter().take(2) {
        let mut v = quantized[j];
        if repair(cem_v, profile, range, nvals, &mut v, e, target, chans) {
            quantized.push(v);
        }
    }
}

/// Encode one HDR block. `t` holds the targets in the interpolation domain
/// (LNS for HDR lanes, UNORM16 for LDR alpha); `h` the half values (for void
/// extent); `opaque` means alpha is 1.0 everywhere.
// One index walks several parallel per-texel / per-channel arrays (and in the encoder fixes the floating-point operation order); iterator chains would obscure that.
#[allow(clippy::needless_range_loop)]
pub(crate) fn encode_block_hdr(
    ctx: &Ctx,
    m7: &Mode7,
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
    let mut quantized: Vec<[u8; 8]> = Vec::new();
    let mut quads: Vec<[u8; 4]> = Vec::new();
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
            // The best encoding of this pass: (error, values, CEM, colour range).
            let mut pass_best: Option<(f64, [u8; 8], u8, usize)> = None;
            quantized.clear();
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
                quantized.push(v);
            }
            let target = (e0, e1);
            repair_best(
                cem_v,
                profile,
                range,
                nvals as usize,
                &mut quantized,
                target,
                chans,
            );
            for v in &quantized {
                let err = texel_err(cem_v, profile, v, &eff, t, chans);
                if pass_best.map_or(true, |b| err < b.0) {
                    pass_best = Some((err, *v, cem_v, range));
                }
            }
            // Mode 7 (RGB base + scale: four values, so a finer colour range
            // than mode 11's six at the same block mode) for opaque blocks.
            let r4 = m.crange[0][1];
            if cem_v == 11 && r4 != 0xFF {
                let r4 = r4 as usize;
                // Two starting points: the per-channel fit above (its mean
                // drop as the scale) and the least-squares fit of mode 7's
                // own model. Neither wins on every image (measured), so both
                // are tried and the exact error chooses.
                for (f0, f1) in [(e0, e1), mode7_fit(t, &eff)] {
                    mode7_candidates(
                        m7,
                        [to12(f0[0]), to12(f0[1]), to12(f0[2])],
                        [to12(f1[0]), to12(f1[1]), to12(f1[2])],
                        &mut quads,
                    );
                    quantized.clear();
                    for q in &quads {
                        let mut v = [0u8; 8];
                        for k in 0..4 {
                            v[k] = COLOR_NEAREST[r4][q[k] as usize];
                        }
                        quantized.push(v);
                    }
                    repair_best(7, profile, r4, 4, &mut quantized, (f0, f1), chans);
                    for v in &quantized {
                        let err = texel_err(7, profile, v, &eff, t, chans);
                        if pass_best.map_or(true, |b| err < b.0) {
                            pass_best = Some((err, *v, 7, r4));
                        }
                    }
                }
            }
            let Some((err, v, pc, pr)) = pass_best else {
                break;
            };
            if err < best.err {
                let pn = cem::value_count(pc);
                let mut s = SymbolicBlock::empty(BlockKind::Normal, m.mode);
                let gr = &ctx.grids[g];
                s.grid = [gr.w, gr.h, gr.d];
                s.weight_range = m.range;
                s.partitions = 1;
                s.cem = [pc; 4];
                s.color_range = pr as u8;
                s.color_count = pn as u8;
                s.color_values[..pn].copy_from_slice(&v[..pn]);
                s.weights[..count].copy_from_slice(&gw[..count]);
                best = Best { err, sym: s };
            }
            // Re-derive ideal weights from the decoded endpoints.
            let ep = cem::expand(&cem::decode(pc, &v), profile);
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
    fn mode7_table_is_a_bit_permutation_and_candidates_decode_near() {
        let t = Mode7::new();
        for mode in 0..6 {
            let mut seen = std::collections::BTreeSet::new();
            for f in 0..4 {
                assert!(t.width[mode][f] >= 5, "mode {mode} field {f}");
                for i in 0..t.width[mode][f] as usize {
                    assert!(
                        seen.insert(t.bit[mode][f][i]),
                        "mode {mode} field {f} bit {i}"
                    );
                }
            }
        }
        // A grey-ish ramp and a coloured one: the best candidate decodes
        // within a few quantisation units of the target endpoints.
        // (e0, e1, bound): the bound is the base+scale model's own error
        // (the three drops differ) plus a few quantisation units.
        for (e0, e1, bound) in [
            (
                [800.0f32, 820.0, 790.0],
                [1600.0f32, 1650.0, 1580.0],
                3.0 * 64.0 * 64.0,
            ),
            (
                [100.0, 300.0, 200.0],
                [900.0, 2000.0, 1300.0],
                420_000.0 + 3.0 * 64.0 * 64.0,
            ),
        ] {
            let mut v = Vec::new();
            mode7_candidates(&t, e0, e1, &mut v);
            assert!(!v.is_empty());
            let best = v
                .iter()
                .map(|q| {
                    let w = [q[0], q[1], q[2], q[3], 0, 0, 0, 0];
                    let raw = cem::decode(7, &w);
                    (0..3)
                        .map(|c| {
                            let a = raw.e0[c] as f32 - e0[c];
                            let b = raw.e1[c] as f32 - e1[c];
                            (a * a + b * b) as f64
                        })
                        .sum::<f64>()
                })
                .fold(f64::MAX, f64::min);
            assert!(best < bound, "{best}");
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
