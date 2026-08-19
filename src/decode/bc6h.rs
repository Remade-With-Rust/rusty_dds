//! BC6H (HDR half-float) decode via [`bcdec_rs`] — float output.
//!
//! `bcdec_rs::bc6h_float` writes RGB `f32` triplets; we decode the whole
//! slice into an RGB plane and expand to RGBA (`A = 1.0`) in one pass, so
//! NPOT edges use the same clamped-block scratch discipline as the LDR
//! tilers.

use crate::error::Error;

/// Decode a BC6H slice to tightly packed RGBA `f32` in caller memory.
///
/// `out` must be exactly `width * height * 4` floats.
///
/// One fused pass. `bcdec_rs::bc6h_float` writes contiguous RGB, so widening to
/// RGBA is unavoidable — but doing it over a full-surface RGB plane is not, and
/// that plane is what made this the slowest decode in the crate. At 1024^2 it
/// cost 12 MiB written, 12 MiB read back and 16 MiB written again, for a 16 MiB
/// result. Decoding a block at a time into a 192-byte scratch that never leaves
/// L1, and widening straight into `out`, deletes both extra streams.
pub fn decode_bc6h_into(
    data: &[u8],
    width: u32,
    height: u32,
    signed: bool,
    out: &mut [f32],
) -> Result<(), Error> {
    let (blocks_x, blocks_y, w, h) = validate(data, width, height)?;
    if out.len() != w.checked_mul(h).and_then(|n| n.checked_mul(4)).ok_or(Error::OutOfBounds)? {
        return Err(Error::OutOfBounds);
    }

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
                let s = row * 4 * 3;
                let d = (y * w + px0) * 4;
                for i in 0..n {
                    out[d + i * 4] = scratch[s + i * 3];
                    out[d + i * 4 + 1] = scratch[s + i * 3 + 1];
                    out[d + i * 4 + 2] = scratch[s + i * 3 + 2];
                    out[d + i * 4 + 3] = 1.0;
                }
            }
        }
    }
    Ok(())
}

/// Shared entry checks. Returns `(blocks_x, blocks_y, width, height)` as usize.
fn validate(data: &[u8], width: u32, height: u32) -> Result<(usize, usize, usize, usize), Error> {
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
    Ok((blocks_x, blocks_y, width as usize, height as usize))
}

pub fn decode_bc6h(
    data: &[u8],
    width: u32,
    height: u32,
    signed: bool,
) -> Result<Vec<f32>, Error> {
    let (_, _, w, h) = validate(data, width, height)?;
    let mut out = vec![
        0f32;
        w.checked_mul(h)
            .and_then(|n| n.checked_mul(4))
            .ok_or(Error::OutOfBounds)?
    ];
    decode_bc6h_into(data, width, height, signed, &mut out)?;
    Ok(out)
}
