//! Interpolation and output conversion ("Weight Application"), all in integer
//! arithmetic so the half-float results are exact bit patterns.

/// `C = floor((C0 * (64 - i) + C1 * i + 32) / 64)` on 16-bit endpoints.
#[inline]
pub(crate) fn lerp(c0: u16, c1: u16, w: u8) -> u16 {
    let w = w as u32;
    ((c0 as u32 * (64 - w) + c1 as u32 * w + 32) >> 6) as u16
}

/// `decode_float16` for an LDR (UNORM16) value: 65535 is 1.0; anything else
/// is `C / 65536` rounded toward zero to binary16.
#[inline]
pub(crate) fn unorm16_to_f16(c: u16) -> u16 {
    if c == 0xFFFF {
        return 0x3C00;
    }
    if c < 4 {
        // Subnormal: c * 2^-16 = (c << 8) * 2^-24.
        return c << 8;
    }
    let e = 15 - c.leading_zeros(); // floor(log2 c), 2..=15
    let mant = if e >= 10 {
        (c as u32 >> (e - 10)) & 0x3FF
    } else {
        ((c as u32) << (10 - e)) & 0x3FF
    };
    (((e - 1) << 10) | mant) as u16
}

/// `decode_float16` for an HDR (LNS) value: the piecewise mantissa map, then
/// Inf/NaN clamped to the largest finite half, `0x7BFF`.
#[inline]
pub(crate) fn lns_to_f16(c: u16) -> u16 {
    let e = (c as u32) >> 11;
    let m = (c as u32) & 0x7FF;
    let mt = if m < 512 {
        3 * m
    } else if m >= 1536 {
        5 * m - 2048
    } else {
        4 * m - 512
    };
    let cf = (e << 10) + (mt >> 3);
    cf.min(0x7BFF) as u16
}

/// Exact widening of a binary16 bit pattern to `f32` (NaN payloads are
/// preserved in the top mantissa bits).
#[inline]
pub(crate) fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h as u32) & 0x8000) << 16;
    let exp = ((h as u32) >> 10) & 0x1F;
    let mant = (h as u32) & 0x3FF;
    let bits = if exp == 0 {
        if mant == 0 {
            sign
        } else {
            // Subnormal: normalise.
            let shift = mant.leading_zeros() - 21; // bring the top bit to bit 10
            let m = (mant << shift) & 0x3FF;
            let e = 127 - 15 + 1 - shift;
            sign | (e << 23) | (m << 13)
        }
    } else if exp == 0x1F {
        sign | 0x7F80_0000 | (mant << 13)
    } else {
        sign | ((exp + 127 - 15) << 23) | (mant << 13)
    };
    f32::from_bits(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unorm16_conversion_is_round_toward_zero() {
        for c in 0..=0xFFFEu32 {
            let h = unorm16_to_f16(c as u16);
            let f = f16_to_f32(h) as f64;
            let exact = c as f64 / 65536.0;
            assert!(f <= exact, "c={c}");
            // The next half up must exceed the exact value.
            let up = f16_to_f32(h + 1) as f64;
            assert!(up > exact, "c={c}");
        }
        assert_eq!(unorm16_to_f16(0xFFFF), 0x3C00);
        assert_eq!(unorm16_to_f16(0), 0);
    }

    #[test]
    fn f16_widening_matches_reference_points() {
        assert_eq!(f16_to_f32(0x3C00), 1.0);
        assert_eq!(f16_to_f32(0x7BFF), 65504.0);
        assert_eq!(f16_to_f32(0x0001), 2f32.powi(-24));
        assert_eq!(f16_to_f32(0x0200), 2f32.powi(-15));
        assert_eq!(f16_to_f32(0xC000), -2.0);
        assert!(f16_to_f32(0xFFFF).is_nan());
        assert_eq!(f16_to_f32(0x7C00), f32::INFINITY);
    }

    #[test]
    fn lns_clamps_to_max_finite() {
        assert_eq!(lns_to_f16(0xFFFF), 0x7BFF);
        assert_eq!(lns_to_f16(0x7800), 0x3C00);
        assert_eq!(lns_to_f16(0), 0);
    }
}
