//! Per-footprint tables shared by the fast decoder and the encoder: the
//! infill taps of a weight grid. Built at run time into caller-owned
//! contexts (no global state).

use alloc::vec::Vec;

use crate::astc::footprint::AstcFootprint;

/// Infill of one weight grid: for each texel up to four (stored-weight index,
/// contribution 0..=16) pairs, so that the texel's weight is
/// `(sum(grid[idx] * contrib) + 8) >> 4`, exactly the specification's infill.
pub(crate) struct Grid {
    pub w: u8,
    pub h: u8,
    pub d: u8,
    /// Stored weights per plane.
    pub count: usize,
    /// `taps[texel]` = (number of taps, [(index, contribution); 4]). The
    /// encoder reads the padded forms below; this one is the oracle of the
    /// infill test (`taps_equal_reference_infill`).
    #[cfg_attr(not(test), allow(dead_code))]
    pub taps: Vec<(u8, [(u8, u8); 4])>,
    /// `taps` padded to exactly four entries with (0, 0): fixed-length loops
    /// for the encoder (a zero contribution adds exactly nothing).
    pub taps4: Vec<[(u8, u8); 4]>,
    /// `taps4` split for the encoder's float loops: indices, and the
    /// contributions as f32 (exact conversions of the u8 values).
    pub tap_i: Vec<[u8; 4]>,
    pub tap_c: Vec<[f32; 4]>,
    /// Transpose: the texels each stored weight feeds and how much, flattened
    /// (`feed_t`/`feed_c` below); stored weight j owns entries
    /// `feed_at[j]..feed_at[j + 1]`.
    pub feed_at: Vec<u16>,
    /// Per stored weight: the sum of its contributions, and the sum of the
    /// squared contributions / 16, each accumulated in `feed` order (the
    /// encoder's decimation uses them bit for bit as it would recompute them).
    pub feed_cs: Vec<f32>,
    pub feed_den: Vec<f32>,
    /// `feed` split: texel index, contribution as f32, and contribution / 16
    /// as f32 (both exact).
    pub feed_t: Vec<u8>,
    pub feed_c: Vec<f32>,
    pub feed_c16: Vec<f32>,
    /// The grid equals the footprint (infill is the identity).
    pub full: bool,
    /// Padded, transposed copies of `tap_i`/`tap_c` and of the feeds, for the
    /// encoder's eight-lane twins (`encode::simd`). Texels are padded to
    /// `tp` (a multiple of 8, at most 216): tap `k` of texel `t` is at
    /// `k * tp + t`, index (masked to 0..64) as `i32` and contribution as
    /// `f32`; padding texels have (0, 0.0). Stored weights are padded to
    /// `jp` (a multiple of 8, at most 64): feed `f` of weight `j` is at
    /// `f * jp + j` (texel, contribution, contribution / 16), each weight's
    /// feeds in `feed` order and then (0, 0.0, 0.0) up to `fmax`; the
    /// per-weight sums `feed_cs`/`feed_den` padded with 0.0.
    #[cfg_attr(not(all(feature = "simd", target_arch = "x86_64")), allow(dead_code))]
    pub tp: usize,
    #[cfg_attr(not(all(feature = "simd", target_arch = "x86_64")), allow(dead_code))]
    pub tap_iv: Vec<i32>,
    #[cfg_attr(not(all(feature = "simd", target_arch = "x86_64")), allow(dead_code))]
    pub tap_cv: Vec<f32>,
    #[cfg_attr(not(all(feature = "simd", target_arch = "x86_64")), allow(dead_code))]
    pub jp: usize,
    #[cfg_attr(not(all(feature = "simd", target_arch = "x86_64")), allow(dead_code))]
    pub fmax: usize,
    #[cfg_attr(not(all(feature = "simd", target_arch = "x86_64")), allow(dead_code))]
    pub feed_tv: Vec<i32>,
    #[cfg_attr(not(all(feature = "simd", target_arch = "x86_64")), allow(dead_code))]
    pub feed_cv: Vec<f32>,
    #[cfg_attr(not(all(feature = "simd", target_arch = "x86_64")), allow(dead_code))]
    pub feed_c16v: Vec<f32>,
    #[cfg_attr(not(all(feature = "simd", target_arch = "x86_64")), allow(dead_code))]
    pub feed_csv: Vec<f32>,
    #[cfg_attr(not(all(feature = "simd", target_arch = "x86_64")), allow(dead_code))]
    pub feed_denv: Vec<f32>,
    /// Mean over texels of sum((contribution / 16)^2): how much infill
    /// averages away independent quantisation noise on the stored weights.
    pub noise: f32,
}

/// The part of a [`Grid`] the decoder reads: the taps, the stored-weight
/// count and whether infill is the identity.
pub(crate) struct Taps {
    pub count: usize,
    /// `taps[texel]` = (number of taps, [(index, contribution); 4]).
    pub taps: Vec<(u8, [(u8, u8); 4])>,
    pub full: bool,
}

/// The infill taps of one weight grid, without the encoder's derived tables
/// (the decoder builds only this, once per grid it meets).
pub(crate) fn build_taps(fp: AstcFootprint, gw: u32, gh: u32, gd: u32) -> Taps {
    let (bw, bh, bd) = (fp.width(), fp.height(), fp.depth());
    let ds = (1024 + bw / 2) / (bw - 1);
    let dt = (1024 + bh / 2) / (bh - 1);
    let dr = if bd > 1 {
        (1024 + bd / 2) / (bd - 1)
    } else {
        0
    };
    let count = (gw * gh * gd) as usize;
    let mut taps = Vec::with_capacity(fp.texels() as usize);
    for r in 0..bd {
        for t in 0..bh {
            for s in 0..bw {
                let gs = (ds * s * (gw - 1) + 32) >> 6;
                let gt = (dt * t * (gh - 1) + 32) >> 6;
                let (js, fs) = (gs >> 4, gs & 0xF);
                let (jt, ft) = (gt >> 4, gt & 0xF);
                let raw: [(u32, u32); 4] = if bd == 1 {
                    let v0 = js + jt * gw;
                    let w11 = (fs * ft + 8) >> 4;
                    [
                        (v0, 16 + w11 - fs - ft),
                        (v0 + 1, fs - w11),
                        (v0 + gw, ft - w11),
                        (v0 + gw + 1, w11),
                    ]
                } else {
                    let gr = (dr * r * (gd - 1) + 32) >> 6;
                    let (jr, fr) = (gr >> 4, gr & 0xF);
                    let wh = gw * gh;
                    let v0 = js + jt * gw + jr * wh;
                    let (s1, s2, w) = if fs > ft && ft > fr {
                        (1, gw, [16 - fs, fs - ft, ft - fr, fr])
                    } else if ft >= fs && fs > fr {
                        (gw, 1, [16 - ft, ft - fs, fs - fr, fr])
                    } else if fs > fr && fr >= ft {
                        (1, wh, [16 - fs, fs - fr, fr - ft, ft])
                    } else if fr >= fs && fs > ft {
                        (wh, 1, [16 - fr, fr - fs, fs - ft, ft])
                    } else if ft > fr && fr >= fs {
                        (gw, wh, [16 - ft, ft - fr, fr - fs, fs])
                    } else {
                        (wh, gw, [16 - fr, fr - ft, ft - fs, fs])
                    };
                    [
                        (v0, w[0]),
                        (v0 + s1, w[1]),
                        (v0 + s1 + s2, w[2]),
                        (v0 + wh + gw + 1, w[3]),
                    ]
                };
                // Merge duplicate indices and drop zero contributions; a zero
                // tap may point past the grid, a non-zero one never does.
                let mut out = [(0u8, 0u8); 4];
                let mut n = 0usize;
                for &(idx, c) in raw.iter() {
                    if c == 0 {
                        continue;
                    }
                    debug_assert!((idx as usize) < count);
                    if let Some(e) = out[..n].iter_mut().find(|e| e.0 as u32 == idx) {
                        e.1 += c as u8;
                    } else {
                        out[n] = (idx as u8, c as u8);
                        n += 1;
                    }
                }
                taps.push((n as u8, out));
            }
        }
    }
    Taps {
        count,
        taps,
        full: gw == bw && gh == bh && gd == bd,
    }
}

/// [`build_taps`] plus the encoder's derived tables.
pub(crate) fn build_grid(fp: AstcFootprint, gw: u32, gh: u32, gd: u32) -> Grid {
    let Taps { count, taps, full } = build_taps(fp, gw, gh, gd);
    let taps4: Vec<[(u8, u8); 4]> = taps.iter().map(|&(_, tp)| tp).collect();
    let tap_i: Vec<[u8; 4]> = taps4.iter().map(|tp| tp.map(|e| e.0)).collect();
    let tap_c: Vec<[f32; 4]> = taps4.iter().map(|tp| tp.map(|e| e.1 as f32)).collect();
    let mut feeds = alloc::vec![Vec::new(); count];
    for (t, &(n, tp)) in taps.iter().enumerate() {
        for &(idx, c) in &tp[..n as usize] {
            feeds[idx as usize].push((t as u8, c));
        }
    }
    let mut feed = Vec::new();
    let mut feed_at = Vec::with_capacity(count + 1);
    for f in &feeds {
        feed_at.push(feed.len() as u16);
        feed.extend_from_slice(f);
    }
    feed_at.push(feed.len() as u16);
    let feed_t: Vec<u8> = feed.iter().map(|e| e.0).collect();
    let feed_c: Vec<f32> = feed.iter().map(|e| e.1 as f32).collect();
    let feed_c16: Vec<f32> = feed.iter().map(|e| e.1 as f32 * (1.0 / 16.0)).collect();
    let mut feed_cs = Vec::with_capacity(count);
    let mut feed_den = Vec::with_capacity(count);
    for j in 0..count {
        let (mut cs, mut den) = (0f32, 0f32);
        for &(_, c) in &feed[feed_at[j] as usize..feed_at[j + 1] as usize] {
            cs += c as f32;
            let cf = c as f32 * (1.0 / 16.0);
            den += cf * cf;
        }
        feed_cs.push(cs);
        feed_den.push(den);
    }
    let mut noise = 0f32;
    for &(n, tp) in taps.iter() {
        for &(_, c) in &tp[..n as usize] {
            let f = c as f32 / 16.0;
            noise += f * f;
        }
    }
    noise /= taps.len().max(1) as f32;
    let tp = taps.len().div_ceil(8) * 8;
    let mut tap_iv = alloc::vec![0i32; 4 * tp];
    let mut tap_cv = alloc::vec![0f32; 4 * tp];
    for t in 0..taps.len() {
        for k in 0..4 {
            tap_iv[k * tp + t] = (tap_i[t][k] & 63) as i32;
            tap_cv[k * tp + t] = tap_c[t][k];
        }
    }
    let jp = count.div_ceil(8) * 8;
    let fmax = (0..count)
        .map(|j| (feed_at[j + 1] - feed_at[j]) as usize)
        .max()
        .unwrap_or(0);
    let mut feed_tv = alloc::vec![0i32; fmax * jp];
    let mut feed_cv = alloc::vec![0f32; fmax * jp];
    let mut feed_c16v = alloc::vec![0f32; fmax * jp];
    let mut feed_csv = alloc::vec![0f32; jp];
    let mut feed_denv = alloc::vec![0f32; jp];
    for j in 0..count {
        for (f, e) in (feed_at[j] as usize..feed_at[j + 1] as usize).enumerate() {
            feed_tv[f * jp + j] = feed_t[e] as i32;
            feed_cv[f * jp + j] = feed_c[e];
            feed_c16v[f * jp + j] = feed_c16[e];
        }
        feed_csv[j] = feed_cs[j];
        feed_denv[j] = feed_den[j];
    }
    Grid {
        noise,
        w: gw as u8,
        h: gh as u8,
        d: gd as u8,
        count,
        taps,
        taps4,
        tap_i,
        tap_c,
        feed_at,
        feed_cs,
        feed_den,
        feed_t,
        feed_c,
        feed_c16,
        full,
        tp,
        tap_iv,
        tap_cv,
        jp,
        fmax,
        feed_tv,
        feed_cv,
        feed_c16v,
        feed_csv,
        feed_denv,
    }
}
