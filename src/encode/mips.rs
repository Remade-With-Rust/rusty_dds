//! Box-filter mip generation for encode.

use crate::error::Error;

pub fn downsample_rgba8(
    src: &[u8],
    width: u32,
    height: u32,
    depth: u32,
) -> Result<Vec<u8>, Error> {
    let dw = (width / 2).max(1);
    let dh = (height / 2).max(1);
    let dd = (depth / 2).max(1);
    let mut out = vec![0u8; (dw * dh * dd * 4) as usize];
    let sw = width as usize;
    let sh = height as usize;
    let sd = depth as usize;

    for z in 0..dd as usize {
        for y in 0..dh as usize {
            for x in 0..dw as usize {
                let x0 = (x * 2).min(sw - 1);
                let y0 = (y * 2).min(sh - 1);
                let z0 = (z * 2).min(sd - 1);
                let x1 = (x0 + 1).min(sw - 1);
                let y1 = (y0 + 1).min(sh - 1);
                let z1 = (z0 + 1).min(sd - 1);

                let mut acc = [0u32; 4];
                let mut n = 0u32;
                for zz in z0..=z1 {
                    for yy in y0..=y1 {
                        for xx in x0..=x1 {
                            let i = ((zz * sh + yy) * sw + xx) * 4;
                            for c in 0..4 {
                                acc[c] += src[i + c] as u32;
                            }
                            n += 1;
                        }
                    }
                }
                let o = ((z * dh as usize + y) * dw as usize + x) * 4;
                for c in 0..4 {
                    out[o + c] = ((acc[c] + n / 2) / n) as u8;
                }
            }
        }
    }
    Ok(out)
}
