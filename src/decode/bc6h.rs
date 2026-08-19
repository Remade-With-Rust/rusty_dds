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

    // `bcdec_rs::bc6h_float` is `bc6h_half` followed by 48 calls to a
    // half->float converter carrying two branches each — measured at 15.5% of
    // the call. Taking the halves ourselves lets that conversion be branchless
    // *and* fold into the RGBA widen, so the two tail passes become one.
    let mut scratch = [0u16; 4 * 4 * 3];
    let mut fscratch = [0f32; 4 * 4 * 3];
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let bi = (by * blocks_x + bx) * 16;
            bcdec_rs::bc6h_half(&data[bi..bi + 16], &mut scratch, 4 * 3, signed);
            // Convert in one tight straight-line pass, NOT folded into the
            // strided scatter below. Folding them looks like the obvious win —
            // one pass instead of two — and MEASURED SLOWER (1024^2 24-thread:
            // 1.72 ms fused against 1.61 ms here). 48 independent conversions
            // vectorise; a strided read/write with the conversion inline does
            // not, and the vectoriser is worth more than the extra pass.
            // Same shape as the refuted backward widen in the crate's history.
            for k in 0..4 * 4 * 3 {
                fscratch[k] = half_to_f32(scratch[k]);
            }
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
                    out[d + i * 4] = fscratch[s + i * 3];
                    out[d + i * 4 + 1] = fscratch[s + i * 3 + 1];
                    out[d + i * 4 + 2] = fscratch[s + i * 3 + 2];
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

/// IEEE binary16 -> binary32, branchless.
///
/// BC6H's decoded value *is* a half bit pattern, so this conversion is on the
/// critical path of every HDR pixel — 48 per block. `bcdec_rs` spends two
/// branches per value on the Inf/NaN and denormal cases; both are rare in real
/// HDR content but the branches are paid on every pixel regardless. Selecting
/// arithmetically instead lets the whole widen loop vectorise.
///
/// Exhaustively verified against the reference for all 65 536 inputs by
/// `half_to_f32_matches_reference_for_every_bit_pattern`.
#[inline(always)]
fn half_to_f32(h: u16) -> f32 {
    const SHIFTED_EXP: u32 = 0x7c00 << 13;
    let h = h as u32;
    let sign = (h & 0x8000) << 16;
    let mut o = (h & 0x7fff) << 13;
    let exp = o & SHIFTED_EXP;
    o += (127 - 15) << 23;

    // Inf/NaN: nudge the exponent the rest of the way. `* mask` rather than a
    // branch, so this is a multiply by 0 or 1 the vectoriser can keep.
    o += ((exp == SHIFTED_EXP) as u32) * ((128 - 16) << 23);

    // Zero/denormal: renormalise by subtracting the magic constant. Computed
    // unconditionally and selected with a mask — the subtraction is harmless
    // for normal inputs because the result is discarded.
    let magic = f32::from_bits(113 << 23);
    let denorm = (f32::from_bits(o + (1 << 23)) - magic).to_bits();
    let is_denorm = 0u32.wrapping_sub((exp == 0) as u32);
    let o = (denorm & is_denorm) | (o & !is_denorm);

    f32::from_bits(o | sign)
}

#[cfg(test)]
mod tests {
    use super::half_to_f32;

    /// The conversion replaces `bcdec_rs`'s, so it must agree on **every** input,
    /// not merely on the values HDR skies happen to contain. Inf, NaN, denormals
    /// and negative zero all have exact bit patterns worth pinning.
    #[test]
    fn half_to_f32_matches_reference_for_every_bit_pattern() {
        for bits in 0..=u16::MAX {
            let ours = half_to_f32(bits);
            let theirs = reference(bits);
            if ours.is_nan() && theirs.is_nan() {
                continue;
            }
            assert_eq!(
                ours.to_bits(),
                theirs.to_bits(),
                "half {bits:#06x}: {ours} != {theirs}"
            );
        }
    }

    /// The branchy formulation, transcribed from the reference implementation.
    fn reference(half: u16) -> f32 {
        let magic = f32::from_bits(113 << 23);
        let shifted_exp = 0x7c00 << 13;
        let mut o = (half as u32 & 0x7fff) << 13;
        let exp = shifted_exp & o;
        o += (127 - 15) << 23;
        if exp == shifted_exp {
            o += (128 - 16) << 23;
        } else if exp == 0 {
            o += 1 << 23;
            o = (f32::from_bits(o) - magic).to_bits();
        }
        o |= (half as u32 & 0x8000) << 16;
        f32::from_bits(o)
    }
}
