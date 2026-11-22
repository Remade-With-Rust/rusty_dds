//! Weight infill ("Weight Infill"): stored weight grid to per-texel weights,
//! bilinear in 2D and simplex in 3D, with the specification's exact integer
//! arithmetic.

use super::footprint::AstcFootprint;

/// Per-texel effective weights (0..=64) for one plane.
///
/// `grid` holds the plane's stored, unquantised weights in raster order
/// (x fastest); it is padded so that the neighbour reads of edge texels (whose
/// contribution is always zero) stay in bounds.
pub fn infill(
    fp: AstcFootprint,
    gw: u32,
    gh: u32,
    gd: u32,
    grid: &[u8; GRID_PAD],
    out: &mut [u8; 216],
) {
    let (bw, bh, bd) = (fp.width(), fp.height(), fp.depth());
    let ds = (1024 + bw / 2) / (bw - 1);
    let dt = (1024 + bh / 2) / (bh - 1);
    // A 2D footprint has depth 1; its r axis never varies.
    let dr = if bd > 1 { (1024 + bd / 2) / (bd - 1) } else { 0 };
    let mut i = 0;
    for r in 0..bd {
        for t in 0..bh {
            for s in 0..bw {
                let gs = (ds * s * (gw - 1) + 32) >> 6;
                let gt = (dt * t * (gh - 1) + 32) >> 6;
                let (js, fs) = (gs >> 4, gs & 0xF);
                let (jt, ft) = (gt >> 4, gt & 0xF);
                let w = if bd == 1 {
                    let v0 = (js + jt * gw) as usize;
                    let p00 = grid[v0] as u32;
                    let p01 = grid[v0 + 1] as u32;
                    let p10 = grid[v0 + gw as usize] as u32;
                    let p11 = grid[v0 + gw as usize + 1] as u32;
                    let w11 = (fs * ft + 8) >> 4;
                    let w10 = ft - w11;
                    let w01 = fs - w11;
                    let w00 = 16 + w11 - fs - ft;
                    (p00 * w00 + p01 * w01 + p10 * w10 + p11 * w11 + 8) >> 4
                } else {
                    let gr = (dr * r * (gd - 1) + 32) >> 6;
                    let (jr, fr) = (gr >> 4, gr & 0xF);
                    let wh = gw * gh;
                    let (s1, s2, w0, w1, w2, w3);
                    let (fs_i, ft_i, fr_i) = (fs as i32, ft as i32, fr as i32);
                    if fs > ft && ft > fr {
                        s1 = 1;
                        s2 = gw;
                        w0 = 16 - fs_i;
                        w1 = fs_i - ft_i;
                        w2 = ft_i - fr_i;
                        w3 = fr_i;
                    } else if ft >= fs && fs > fr {
                        // fs > ft false, ft > fr true, fs > fr true
                        s1 = gw;
                        s2 = 1;
                        w0 = 16 - ft_i;
                        w1 = ft_i - fs_i;
                        w2 = fs_i - fr_i;
                        w3 = fr_i;
                    } else if fs > fr && fr >= ft {
                        // fs > ft true, ft > fr false, fs > fr true
                        s1 = 1;
                        s2 = wh;
                        w0 = 16 - fs_i;
                        w1 = fs_i - fr_i;
                        w2 = fr_i - ft_i;
                        w3 = ft_i;
                    } else if fr >= fs && fs > ft {
                        // fs > ft true, ft > fr false, fs > fr false
                        s1 = wh;
                        s2 = 1;
                        w0 = 16 - fr_i;
                        w1 = fr_i - fs_i;
                        w2 = fs_i - ft_i;
                        w3 = ft_i;
                    } else if ft > fr && fr >= fs {
                        // fs > ft false, ft > fr true, fs > fr false
                        s1 = gw;
                        s2 = wh;
                        w0 = 16 - ft_i;
                        w1 = ft_i - fr_i;
                        w2 = fr_i - fs_i;
                        w3 = fs_i;
                    } else {
                        // fs > ft false, ft > fr false, fs > fr false
                        s1 = wh;
                        s2 = gw;
                        w0 = 16 - fr_i;
                        w1 = fr_i - ft_i;
                        w2 = ft_i - fs_i;
                        w3 = fs_i;
                    }
                    let v0 = (js + jt * gw + jr * wh) as usize;
                    let p0 = grid[v0] as i32;
                    let p1 = grid[v0 + s1 as usize] as i32;
                    let p2 = grid[v0 + (s1 + s2) as usize] as i32;
                    let p3 = grid[v0 + (wh + gw + 1) as usize] as i32;
                    ((p0 * w0 + p1 * w1 + p2 * w2 + p3 * w3 + 8) >> 4) as u32
                };
                out[i] = w as u8;
                i += 1;
            }
        }
    }
}

/// Size of the padded stored-weight array: 64 weights plus the largest
/// out-of-grid neighbour offset (3D: `W*H + W + 1` with W, H <= 6).
pub const GRID_PAD: usize = 64 + 64;
