//! Register pre-packing for the BC7 two-pixel interpolation kernels — the
//! arch-neutral half shared by `decode::simd` (x86-64/SSE2) and
//! `decode::neon` (aarch64), so the packing layout has exactly one
//! definition and the two mirrors cannot drift.
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
//! | `w * delta` | `-16_320 ..= 16_320` | yes, so a 16-bit multiply is exact |
//! | `base + w * delta` | `32 ..= 16_352` | yes |
//!
//! The sum is the original `e0 * (64 - w) + e1 * w + 32`, which cannot leave
//! `32 ..= 16_352`, so `>> 6` lands in `0..=255` and the saturating
//! narrow/pack in either mirror is never reached. Sixteen-bit lanes therefore
//! hold **eight** channels per register instead of four, which is where the
//! gain comes from — the same rearrangement that halved the multiply count
//! also doubled the lane count.

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

/// Pre-pack the base/delta pairs of an opaque-alpha mode into register form.
#[inline(always)]
pub(super) fn pack_bd3(bd: &[([i32; 3], [i32; 3])], pairs: usize) -> [(i64, i64); 4] {
    // FOUR slots for three subsets. Callers index this with a two-bit field
    // (`(subsets >> 2 * p) & 0x3`), and although a three-subset partition never
    // names subset 3, nothing tells the compiler that — so a three-slot array
    // left a live bounds check on every lookup. The fourth slot is never read;
    // it exists so the mask itself proves the index.
    let mut out = [(0i64, 0i64); 4];
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
