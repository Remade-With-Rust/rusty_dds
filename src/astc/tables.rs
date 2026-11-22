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
    /// `taps[texel]` = (number of taps, [(index, contribution); 4]).
    pub taps: Vec<(u8, [(u8, u8); 4])>,
    /// Transpose: the texels each stored weight feeds and how much, flattened;
    /// stored weight j owns `feed[feed_at[j]..feed_at[j + 1]]`.
    pub feed: Vec<(u8, u8)>,
    pub feed_at: Vec<u16>,
    /// The grid equals the footprint (infill is the identity).
    pub full: bool,
    /// Mean over texels of sum((contribution / 16)^2): how much infill
    /// averages away independent quantisation noise on the stored weights.
    pub noise: f32,
}

pub(crate) fn build_grid(fp: AstcFootprint, gw: u32, gh: u32, gd: u32) -> Grid {
    let (bw, bh, bd) = (fp.width(), fp.height(), fp.depth());
    let ds = (1024 + bw / 2) / (bw - 1);
    let dt = (1024 + bh / 2) / (bh - 1);
    let dr = if bd > 1 { (1024 + bd / 2) / (bd - 1) } else { 0 };
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
    let mut noise = 0f32;
    for &(n, tp) in taps.iter() {
        for &(_, c) in &tp[..n as usize] {
            let f = c as f32 / 16.0;
            noise += f * f;
        }
    }
    noise /= taps.len().max(1) as f32;
    Grid {
        noise,
        w: gw as u8,
        h: gh as u8,
        d: gd as u8,
        count,
        taps,
        feed,
        feed_at,
        full: gw == bw && gh == bh && gd == bd,
    }
}

