//! NEON (aarch64) mirrors of the BC7 two-pixel interpolation kernels in
//! [`crate::decode::simd`] — inline-exe §5.3 port stage 1: one pair of
//! kernels unlocks the vector path for all eight BC7 modes at once.
//!
//! The register pre-packing and the 16-bit-lane safety argument live in
//! [`super::interp_pack`], shared with the x86 mirror so the two cannot
//! drift. AdvSIMD (NEON) is baseline on every aarch64 Rust target, so unlike
//! x86's AVX2 kernels there is NO runtime detection and no scalar fallback
//! arm on this architecture — the vector path IS the aarch64 path.
//!
//! STATUS: oracle-verified ON aarch64 under emulation (qemu-user 10.2.1,
//! 2026-08-26): the two `*_matches_scalar` sweeps below PLUS all eight BC7
//! `*_matches_the_general_decoder` bcdec-oracle tests — which route through
//! these kernels on this architecture — pass 25/25
//! (`cargo test --lib --target aarch64-unknown-linux-musl --no-default-features
//! --features decode,simd`, run with `qemu-aarch64`). Codegen: the
//! interpolation compiles to `mla v.8h` + `sqshrun #6` — two vector
//! instructions per two pixels. NOT yet timed on real ARM hardware; emulation
//! proves correctness, never speed.

pub(super) use super::interp_pack::{pack4, pack_bd3, pack_bd4};

/// Interpolate **two adjacent pixels** and write eight RGBA bytes.
///
/// NEON mirror of [`crate::decode::simd::write2`] — same contract: `b0`/`d0`
/// are the packed base and delta for the first pixel's subset, `b1`/`d1` for
/// the second's; single-subset modes pass the same pair twice. `dst` must
/// have at least eight bytes, which the callers guarantee by construction.
#[inline(always)]
pub(super) fn write2(b0: i64, d0: i64, b1: i64, d1: i64, w0: i16, w1: i16, dst: &mut [u8]) {
    debug_assert!(dst.len() >= 8, "write2 needs eight bytes");
    // SAFETY: AdvSIMD is statically enabled on every aarch64 Rust target, so
    // the intrinsics are always available. `vst1_u8` writes exactly eight
    // bytes and does not require alignment; the `debug_assert` above plus the
    // callers' construction (two pixels within one block row) guarantee the
    // slice is that long. No pointer outlives this call.
    unsafe {
        use core::arch::aarch64::*;
        let base = vcombine_s16(
            vreinterpret_s16_u64(vcreate_u64(b0 as u64)),
            vreinterpret_s16_u64(vcreate_u64(b1 as u64)),
        );
        let delta = vcombine_s16(
            vreinterpret_s16_u64(vcreate_u64(d0 as u64)),
            vreinterpret_s16_u64(vcreate_u64(d1 as u64)),
        );
        let w = vcombine_s16(vdup_n_s16(w0), vdup_n_s16(w1));
        let v = vaddq_s16(base, vmulq_s16(delta, w));
        let v = vshrq_n_s16::<6>(v);
        // Saturating narrow is exact here: the shifted values are always
        // 0..=255, the same argument as the x86 `packus`.
        let packed = vqmovun_s16(v);
        vst1_u8(dst.as_mut_ptr(), packed);
    }
}

/// Interpolate two adjacent pixels where **colour and alpha take different
/// weights** — BC7 modes 4 and 5. NEON mirror of
/// [`crate::decode::simd::write2_split`]; `alpha_lane` is the output byte
/// within the pixel that holds the alpha-weighted value, matched rather than
/// computed so each arm builds a constant weight vector.
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
    // SAFETY: as `write2` — baseline AdvSIMD, an unaligned eight-byte store
    // into a slice the caller has sized, and no pointer escapes.
    unsafe {
        use core::arch::aarch64::*;
        let base = vcombine_s16(
            vreinterpret_s16_u64(vcreate_u64(b0 as u64)),
            vreinterpret_s16_u64(vcreate_u64(b1 as u64)),
        );
        let delta = vcombine_s16(
            vreinterpret_s16_u64(vcreate_u64(d0 as u64)),
            vreinterpret_s16_u64(vcreate_u64(d1 as u64)),
        );
        let (c0, a0, c1, a1) = (wc.0, wa.0, wc.1, wa.1);
        // Lane order 0..=7 — the transcription of the x86 `_mm_set_epi16`
        // arms, whose argument order is lane 7 down to lane 0.
        let warr: [i16; 8] = match alpha_lane {
            0 => [a0, c0, c0, c0, a1, c1, c1, c1],
            1 => [c0, a0, c0, c0, c1, a1, c1, c1],
            2 => [c0, c0, a0, c0, c1, c1, a1, c1],
            _ => [c0, c0, c0, a0, c1, c1, c1, a1],
        };
        let w = vld1q_s16(warr.as_ptr());
        let v = vaddq_s16(base, vmulq_s16(delta, w));
        let v = vshrq_n_s16::<6>(v);
        let packed = vqmovun_s16(v);
        vst1_u8(dst.as_mut_ptr(), packed);
    }
}

#[cfg(test)]
mod tests {
    use super::super::interp_pack::pack4;
    use super::*;

    /// The NEON path must agree with the scalar expression it replaces across
    /// the whole endpoint and weight domain — the same sweep the x86 mirror
    /// runs, including the endpoints where a 16-bit lane would overflow if
    /// the range analysis were wrong.
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
                            let want = ((base[0] + w as i32 * delta[0]) >> 6) as u8;
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

    /// `write2_split` must place the alpha-weighted value in exactly the
    /// named lane and the colour weight everywhere else, across all four
    /// rotations and the full endpoint/weight domain.
    #[test]
    fn write2_split_matches_scalar_all_lanes() {
        const WEIGHTS: [i16; 5] = [0, 9, 43, 60, 64];
        for lane in 0..4usize {
            for &e0 in &[0u32, 1, 128, 255] {
                for &e1 in &[0u32, 127, 255] {
                    let base = [e0 as i32 * 64 + 32; 4];
                    let delta = [e1 as i32 - e0 as i32; 4];
                    let (bp, dp) = (pack4(base), pack4(delta));
                    for &wc in &WEIGHTS {
                        for &wa in &WEIGHTS {
                            let mut got = [0u8; 8];
                            write2_split(bp, dp, bp, dp, (wc, wc), (wa, wa), lane, &mut got);
                            for px in 0..2 {
                                for c in 0..4 {
                                    let w = if c == lane { wa } else { wc } as i32;
                                    let want = ((base[c] + w * delta[c]) >> 6) as u8;
                                    assert_eq!(
                                        got[px * 4 + c],
                                        want,
                                        "lane={lane} e0={e0} e1={e1} wc={wc} wa={wa} px={px} c={c}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
