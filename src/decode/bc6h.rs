//! BC6H (HDR half-float) decode via [`bcdec_rs`] — float output.
//!
//! `bcdec_rs::bc6h_float` writes RGB `f32` triplets; we decode the whole
//! slice into an RGB plane and expand to RGBA (`A = 1.0`) in one pass, so
//! NPOT edges use the same clamped-block scratch discipline as the LDR
//! tilers.

use crate::error::Error;

pub fn decode_bc6h(
    data: &[u8],
    width: u32,
    height: u32,
    signed: bool,
) -> Result<Vec<f32>, Error> {
    if width == 0 || height == 0 {
        return Err(Error::InvalidField("zero image dimension".into()));
    }
    let blocks_x = (width as usize + 3) / 4;
    let blocks_y = (height as usize + 3) / 4;
    let expected = blocks_x
        .checked_mul(blocks_y)
        .and_then(|n| n.checked_mul(16))
        .ok_or(Error::OutOfBounds)?;
    if data.len() < expected {
        return Err(Error::TruncatedData);
    }
    let w = width as usize;
    let h = height as usize;

    let mut rgb = vec![0f32; w * h * 3];
    if width % 4 == 0 && height % 4 == 0 {
        let pitch = w * 3; // pitch in floats
        for by in 0..blocks_y {
            for bx in 0..blocks_x {
                let bi = (by * blocks_x + bx) * 16;
                let offset = (by * 4 * w + bx * 4) * 3;
                bcdec_rs::bc6h_float(&data[bi..bi + 16], &mut rgb[offset..], pitch, signed);
            }
        }
    } else {
        let mut scratch = [0f32; 4 * 4 * 3];
        for by in 0..blocks_y {
            for bx in 0..blocks_x {
                let bi = (by * blocks_x + bx) * 16;
                bcdec_rs::bc6h_float(&data[bi..bi + 16], &mut scratch, 4 * 3, signed);
                let px0 = bx * 4;
                let py0 = by * 4;
                for row in 0..4 {
                    let y = py0 + row;
                    if y >= h {
                        break;
                    }
                    let n = (w - px0).min(4);
                    let src = row * 4 * 3;
                    let dst = (y * w + px0) * 3;
                    rgb[dst..dst + n * 3].copy_from_slice(&scratch[src..src + n * 3]);
                }
            }
        }
    }

    let mut out = Vec::with_capacity(w * h * 4);
    for px in rgb.chunks_exact(3) {
        out.extend_from_slice(&[px[0], px[1], px[2], 1.0]);
    }
    Ok(out)
}
