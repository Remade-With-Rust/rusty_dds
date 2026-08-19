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
            let blk = &data[bi..bi + 16];
            if !bc6h_mode11_half(blk, &mut scratch, signed) {
                bcdec_rs::bc6h_half(blk, &mut scratch, 4 * 3, signed);
            }
            // Convert in one tight straight-line pass, NOT folded into the
            // strided scatter below. Folding them looks like the obvious win —
            // one pass instead of two — and MEASURED SLOWER (1024^2 24-thread:
            // 1.72 ms fused against 1.61 ms here). 48 independent conversions
            // vectorise; a strided read/write with the conversion inline does
            // not, and the vectoriser is worth more than the extra pass.
            // Same shape as the refuted backward widen in the crate's history.
            // `vcvtph2ps` does eight halves per instruction; 48 of them is six
            // instructions against 48 scalar conversions. Measured at ~19% of
            // the BC6H call by doubling the conversion work.
            #[cfg(all(feature = "simd", target_arch = "x86_64"))]
            let converted = crate::decode::simd::half48_to_f32(&scratch, &mut fscratch);
            #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
            let converted = false;
            if !converted {
                for k in 0..4 * 4 * 3 {
                    fscratch[k] = half_to_f32(scratch[k]);
                }
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
                if n == 4 {
                    // A whole block row per store: one slice range-check instead
                    // of sixteen indexed writes. The RGB-to-RGBA widen and the
                    // stride change happen while building the row, which is a
                    // register-resident array, not while addressing `out`.
                    let px = [
                        fscratch[s],
                        fscratch[s + 1],
                        fscratch[s + 2],
                        1.0,
                        fscratch[s + 3],
                        fscratch[s + 4],
                        fscratch[s + 5],
                        1.0,
                        fscratch[s + 6],
                        fscratch[s + 7],
                        fscratch[s + 8],
                        1.0,
                        fscratch[s + 9],
                        fscratch[s + 10],
                        fscratch[s + 11],
                        1.0,
                    ];
                    out[d..d + 16].copy_from_slice(&px);
                } else {
                    for i in 0..n {
                        out[d + i * 4] = fscratch[s + i * 3];
                        out[d + i * 4 + 1] = fscratch[s + i * 3 + 1];
                        out[d + i * 4 + 2] = fscratch[s + i * 3 + 2];
                        out[d + i * 4 + 3] = 1.0;
                    }
                }
            }
        }
    }
    Ok(())
}

/// BC6H interpolation weights for 4-bit indices.
const BC6H_W4: [i32; 16] = [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64];

/// Decode one **mode 11** BC6H block to the 48 half-float components the caller
/// expects, laid out with a pitch of 12.
///
/// Mode 11 is one subset with both endpoints stored explicitly at 10 bits — no
/// partition table, no delta compression, no sign extension — and sixteen 4-bit
/// indices. **It is 100% of the blocks this crate's encoder produces**, and it is
/// the shape most BC6H content uses for smooth HDR gradients.
///
/// The general decoder reaches it through a stateful `Bitstream` whose every
/// read mutates the cursor, so the mode dispatch, the six endpoint reads and the
/// sixteen index reads are one long serial dependency chain. Reading each field
/// by computed offset from an immutable `u128` makes them independent — the same
/// fix that was worth +19% to +73% across BC1-BC5 and every BC7 mode.
///
/// Unlike BC5, BC6H measured **throughput** bound: doubling the block-decode
/// work cost 1.7x (113.8 -> 67.5 Mpx/s), and removing it entirely doubled the
/// call (113.8 -> 227.3). Work removed here is work saved.
///
/// Returns `false` for signed content or any other mode, which falls back.
#[inline]
fn bc6h_mode11_half(blk: &[u8], out: &mut [u16; 4 * 4 * 3], signed: bool) -> bool {
    // The 5-bit mode field; 0b00011 is the 10.10/10.10/10.10 single-subset mode.
    if signed || blk[0] & 0x1f != 0x03 {
        return false;
    }
    let Ok(bytes) = <[u8; 16]>::try_from(&blk[..16]) else {
        return false;
    };
    let b = u128::from_le_bytes(bytes);

    // Six 10-bit endpoint components, component-major: rw gw bw rx gx bx.
    let f = |sh: u32| ((b >> sh) & 0x3ff) as i32;
    // Unquantize at 10 bits, unsigned: the endpoints saturate rather than scale.
    let uq = |v: i32| {
        if v == 0 {
            0
        } else if v == 1023 {
            0xFFFF
        } else {
            ((v << 16) + 0x8000) >> 10
        }
    };
    let a = [uq(f(5)), uq(f(15)), uq(f(25))];
    let c = [uq(f(35)), uq(f(45)), uq(f(55))];

    // `(a*(64-w) + c*w + 32) >> 6` factored to one multiply, as everywhere else
    // in this crate: base and delta do not depend on the weight.
    let base = [a[0] * 64 + 32, a[1] * 64 + 32, a[2] * 64 + 32];
    let delta = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];

    // 63 bits of indices; pixel 0 is the fix-up and stores one bit fewer.
    let idx = (b >> 65) as u64;
    for p in 0..16usize {
        let w = if p == 0 {
            BC6H_W4[(idx & 0x7) as usize]
        } else {
            BC6H_W4[((idx >> (3 + (p - 1) * 4)) & 0xf) as usize]
        };
        let o = (p / 4) * 12 + (p % 4) * 3;
        for ch in 0..3 {
            let v = (base[ch] + w * delta[ch]) >> 6;
            // finish_unquantize, unsigned: scale the magnitude by 31/64. The
            // result IS the half bit pattern.
            out[o + ch] = ((v * 31) >> 6) as u16;
        }
    }
    true
}

#[cfg(test)]
mod mode11_tests {
    use super::bc6h_mode11_half;

    /// Bit-identical to the general decoder across randomised blocks, including
    /// the endpoint extremes where `unquantize` takes its saturating branches
    /// (`v == 0` and `v == 1023`) and the all-ones payload.
    #[test]
    fn mode11_matches_the_general_decoder() {
        let mut state = 0x6bc6_1111_2222_3333u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..40_000 {
            let mut blk = [0u8; 16];
            match case {
                0 => {}
                1 => blk.iter_mut().for_each(|x| *x = 0xff),
                _ => {
                    blk[..8].copy_from_slice(&next().to_le_bytes());
                    blk[8..].copy_from_slice(&next().to_le_bytes());
                }
            }
            // Force the mode field; leave everything above it random.
            blk[0] = (blk[0] & !0x1f) | 0x03;

            let mut ours = [0u16; 4 * 4 * 3];
            assert!(bc6h_mode11_half(&blk, &mut ours, false), "case {case}");
            let mut theirs = [0u16; 4 * 4 * 3];
            bcdec_rs::bc6h_half(&blk, &mut theirs, 4 * 3, false);
            assert_eq!(ours, theirs, "case {case}: block {blk:02x?}");
        }
    }

    /// Signed content and every other mode must be declined, not mis-decoded.
    #[test]
    fn other_modes_and_signed_are_declined() {
        let mut out = [0u16; 4 * 4 * 3];
        let mut blk = [0u8; 16];
        blk[0] = 0x03;
        assert!(bc6h_mode11_half(&blk, &mut out, false));
        // Same bits, signed: declined.
        assert!(!bc6h_mode11_half(&blk, &mut out, true));
        // Every other 5-bit mode field.
        for m in 0..32u8 {
            if m == 0x03 {
                continue;
            }
            blk[0] = m;
            assert!(!bc6h_mode11_half(&blk, &mut out, false), "mode field {m:#07b}");
        }
    }
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
