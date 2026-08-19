//! SIMD interpolation for BCn block decode (x86_64).
//!
//! # Why this is SSE2 and not AVX2
//!
//! SSE2 is **baseline on x86_64** — it needs no runtime detection and no
//! fallback path, so there is exactly one code path to test and it is the one
//! that ships. The encoder's AVX2 kernels are runtime-detected because AVX2 is
//! not guaranteed; nothing here needs that.
//!
//! # Why 16-bit lanes are safe
//!
//! [`crate::decode::bcn`] rearranges BC7 interpolation to `base + w * delta`,
//! where `base = e0 * 64 + 32` and `delta = e1 - e0`. For endpoints in `0..=255`
//! and weights in `0..=64`:
//!
//! | term | range | fits `i16` |
//! |---|---|---|
//! | `base` | `32 ..= 16_352` | yes |
//! | `delta` | `-255 ..= 255` | yes |
//! | `w * delta` | `-16_320 ..= 16_320` | yes, so `mullo` is exact |
//! | `base + w * delta` | `32 ..= 16_352` | yes |
//!
//! The sum is the original `e0 * (64 - w) + e1 * w + 32`, which cannot leave
//! `32 ..= 16_352`, so `>> 6` lands in `0..=255` and the `packus` saturation is
//! never reached. Sixteen-bit lanes therefore hold **eight** channels per
//! register instead of four, which is where the gain comes from — the same
//! rearrangement that halved the multiply count also doubled the lane count.

use core::arch::x86_64::{
    __m128i, _mm_add_epi16, _mm_mullo_epi16, _mm_packus_epi16, _mm_set_epi16, _mm_set_epi64x,
    _mm_srai_epi16, _mm_storel_epi64,
};

/// Pack four per-channel values into one register-ready `i64` of four `i16`
/// lanes, in RGBA order.
#[inline(always)]
pub(super) fn pack4(v: [i32; 4]) -> i64 {
    ((v[0] as u16 as u64)
        | ((v[1] as u16 as u64) << 16)
        | ((v[2] as u16 as u64) << 32)
        | ((v[3] as u16 as u64) << 48)) as i64
}

/// Pack three per-channel values plus an opaque alpha, for the modes that do
/// not carry one.
#[inline(always)]
pub(super) fn pack3_opaque_base(v: [i32; 3]) -> i64 {
    // Alpha is written as a constant 255, which after `>> 6` means a base of
    // `255 << 6` with a zero delta.
    pack4([v[0], v[1], v[2], 255 << 6])
}

/// [`pack3_opaque_base`]'s delta twin: alpha must not move with the weight.
#[inline(always)]
pub(super) fn pack3_opaque_delta(v: [i32; 3]) -> i64 {
    pack4([v[0], v[1], v[2], 0])
}

/// Interpolate **two adjacent pixels** and write eight RGBA bytes.
///
/// `b0`/`d0` are the base and delta for the first pixel's subset, `b1`/`d1` for
/// the second's; single-subset modes pass the same pair twice. `dst` must have
/// at least eight bytes, which the callers guarantee by construction — two
/// pixels of a four-pixel block row are always contiguous.
#[inline(always)]
pub(super) fn write2(b0: i64, d0: i64, b1: i64, d1: i64, w0: i16, w1: i16, dst: &mut [u8]) {
    debug_assert!(dst.len() >= 8, "write2 needs eight bytes");
    // SAFETY: every intrinsic below is SSE2, which is unconditionally present on
    // x86_64. `_mm_storel_epi64` writes exactly eight bytes and does not require
    // alignment; the `debug_assert` above plus the callers' construction (two
    // pixels within one block row) guarantee the slice is that long. No pointer
    // outlives this call.
    unsafe {
        let base = _mm_set_epi64x(b1, b0);
        let delta = _mm_set_epi64x(d1, d0);
        let w = _mm_set_epi16(w1, w1, w1, w1, w0, w0, w0, w0);
        let v = _mm_add_epi16(base, _mm_mullo_epi16(delta, w));
        let v = _mm_srai_epi16(v, 6);
        // Saturating pack is exact here: the shifted values are always 0..=255.
        let packed = _mm_packus_epi16(v, v);
        _mm_storel_epi64(dst.as_mut_ptr() as *mut __m128i, packed);
    }
}

/// Pre-pack the base/delta pairs of an opaque-alpha mode into register form.
#[inline(always)]
pub(super) fn pack_bd3(bd: &[([i32; 3], [i32; 3])], pairs: usize) -> [(i64, i64); 3] {
    let mut out = [(0i64, 0i64); 3];
    for (k, slot) in out.iter_mut().enumerate().take(pairs) {
        slot.0 = pack3_opaque_base(bd[k].0);
        slot.1 = pack3_opaque_delta(bd[k].1);
    }
    out
}

/// [`pack_bd3`] for the modes that carry alpha.
#[inline(always)]
pub(super) fn pack_bd4(bd: &[([i32; 4], [i32; 4])], pairs: usize) -> [(i64, i64); 2] {
    let mut out = [(0i64, 0i64); 2];
    for (k, slot) in out.iter_mut().enumerate().take(pairs) {
        slot.0 = pack4(bd[k].0);
        slot.1 = pack4(bd[k].1);
    }
    out
}

/// Interpolate two adjacent pixels where **colour and alpha take different
/// weights** — BC7 modes 4 and 5, which carry two independent index sets.
///
/// `alpha_lane` is the output byte within the pixel that holds the
/// alpha-weighted value. Those modes also carry a rotation that moves alpha into
/// a colour channel, so the caller permutes `base`/`delta` into output order and
/// passes the resulting lane here; the four possibilities are matched rather
/// than computed so each arm builds a constant shuffle.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
pub(super) fn write2_split(
    b0: i64,
    d0: i64,
    b1: i64,
    d1: i64,
    wc: (i16, i16),
    wa: (i16, i16),
    alpha_lane: usize,
    dst: &mut [u8],
) {
    debug_assert!(dst.len() >= 8, "write2_split needs eight bytes");
    debug_assert!(alpha_lane < 4);
    // SAFETY: as `write2` — SSE2 only, an unaligned eight-byte store into a
    // slice the caller has sized, and no pointer escapes.
    unsafe {
        let base = _mm_set_epi64x(b1, b0);
        let delta = _mm_set_epi64x(d1, d0);
        // Lanes are little-endian in `_mm_set_epi16`: the last argument is lane 0.
        let (c0, a0, c1, a1) = (wc.0, wa.0, wc.1, wa.1);
        let w = match alpha_lane {
            0 => _mm_set_epi16(c1, c1, c1, a1, c0, c0, c0, a0),
            1 => _mm_set_epi16(c1, c1, a1, c1, c0, c0, a0, c0),
            2 => _mm_set_epi16(c1, a1, c1, c1, c0, a0, c0, c0),
            _ => _mm_set_epi16(a1, c1, c1, c1, a0, c0, c0, c0),
        };
        let v = _mm_add_epi16(base, _mm_mullo_epi16(delta, w));
        let v = _mm_srai_epi16(v, 6);
        let packed = _mm_packus_epi16(v, v);
        _mm_storel_epi64(dst.as_mut_ptr() as *mut __m128i, packed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vector path must agree with the scalar expression it replaces across
    /// the whole endpoint and weight domain, including the endpoints where a
    /// 16-bit lane would overflow if the range analysis were wrong.
    #[test]
    fn write2_matches_scalar_over_the_full_domain() {
        const WEIGHTS: [i16; 6] = [0, 9, 21, 43, 60, 64];
        for &e0 in &[0u32, 1, 63, 127, 128, 254, 255] {
            for &e1 in &[0u32, 1, 63, 127, 128, 254, 255] {
                let base = [e0 as i32 * 64 + 32; 4];
                let delta = [e1 as i32 - e0 as i32; 4];
                let (bp, dp) = (pack4(base), pack4(delta));
                for &w0 in &WEIGHTS {
                    for &w1 in &WEIGHTS {
                        let mut got = [0u8; 8];
                        write2(bp, dp, bp, dp, w0, w1, &mut got);
                        for (k, w) in [w0, w1].into_iter().enumerate() {
                            let want =
                                ((base[0] + w as i32 * delta[0]) >> 6) as u8;
                            for c in 0..4 {
                                assert_eq!(
                                    got[k * 4 + c],
                                    want,
                                    "e0={e0} e1={e1} w={w} channel {c}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// Opaque-alpha packing must produce exactly 255 for every weight, since
    /// alpha in those modes does not interpolate at all.
    #[test]
    fn opaque_alpha_is_constant_across_weights() {
        let base = pack3_opaque_base([100 * 64 + 32, 0 + 32, 255 * 64 + 32]);
        let delta = pack3_opaque_delta([50, -50, 0]);
        for w in 0..=64i16 {
            let mut got = [0u8; 8];
            write2(base, delta, base, delta, w, w, &mut got);
            assert_eq!(got[3], 255, "alpha moved at w={w}");
            assert_eq!(got[7], 255, "alpha moved at w={w}");
        }
    }

    /// The split-weight path must place the alpha-weighted value in the lane the
    /// rotation names, and colour weights everywhere else, for all four lanes.
    #[test]
    fn write2_split_honours_the_alpha_lane() {
        let base = pack4([10 * 64 + 32, 20 * 64 + 32, 30 * 64 + 32, 40 * 64 + 32]);
        let delta = pack4([100, 100, 100, 100]);
        for alpha_lane in 0..4usize {
            let mut got = [0u8; 8];
            write2_split(base, delta, base, delta, (0, 0), (64, 64), alpha_lane, &mut got);
            let starts = [10, 20, 30, 40];
            for lane in 0..4usize {
                let want = if lane == alpha_lane {
                    ((starts[lane] * 64 + 32 + 64 * 100) >> 6) as u8
                } else {
                    starts[lane] as u8
                };
                assert_eq!(got[lane], want, "alpha_lane {alpha_lane}, lane {lane}");
            }
        }
    }
}
