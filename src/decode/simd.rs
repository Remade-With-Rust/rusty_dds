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
    __m128i, _mm_add_epi16, _mm_set_epi32, _mm_loadu_si128, _mm_mullo_epi16, _mm_packus_epi16,
    _mm_set1_epi16, _mm_set_epi16, _mm_set_epi64x, _mm_shuffle_epi8, _mm_srai_epi16,
    _mm_set_epi64x as _set64, _mm_storel_epi64, _mm_storeu_si128, _mm_unpackhi_epi16, _mm_unpackhi_epi8,
    _mm_unpacklo_epi16, _mm_unpacklo_epi8,
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


/// Can this CPU run the BC5 gather profitably?
///
/// Needs SSSE3 (`pshufb`) and BMI2 (`pdep`) — neither is baseline — and needs
/// `pdep` to be fast rather than microcoded. See [`has_fast_pdep`]. Cached, and
/// the scalar twin covers every CPU that fails this.
#[inline]
pub(super) fn has_ssse3() -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| {
        std::arch::is_x86_feature_detected!("ssse3")
            && std::arch::is_x86_feature_detected!("bmi2")
            && has_fast_pdep()
    })
}

/// Is `pdep` a real instruction on this CPU, or microcode?
///
/// BMI2 being *present* is not the question. On AMD Zen 1 and Zen 2 `pdep` and
/// `pext` are microcoded at roughly **18 cycles** latency and 1/18 throughput,
/// against 3 cycles on Intel Haswell-and-later and AMD Zen 3-and-later. The BC5
/// kernel issues four of them per block against a block budget near 100 cycles,
/// so enabling this path on Zen 1/2 would be a large *regression* on hardware
/// that advertises the feature.
///
/// Zen 3 is family 0x19; Zen 1 and Zen 2 are 0x17. Anything not AMD is fine.
fn has_fast_pdep() -> bool {
    // `__cpuid` is safe on x86_64: leaves 0 and 1 are architecturally defined
    // and supported everywhere, and it touches no memory.
    let (vendor, family) = {
        let v = core::arch::x86_64::__cpuid(0);
        let f = core::arch::x86_64::__cpuid(1);
        ((v.ebx, v.edx, v.ecx), f.eax)
    };
    // "AuthenticAMD" as three little-endian dwords.
    let is_amd = vendor == (0x6874_7541, 0x6974_6e65, 0x444d_4163);
    if !is_amd {
        return true;
    }
    let base = (family >> 8) & 0xf;
    let display = if base == 0xf {
        base + ((family >> 20) & 0xff)
    } else {
        base
    };
    display >= 0x19
}

/// Gather both channels of a BC5 block and write all four RGBA rows.
///
/// The measured cost in BC5 was the **table lookup**, not the index arithmetic:
/// with the lookup stubbed out the block runs at ~655 Mpx/s against ~371 with it,
/// so thirty-two dependent byte loads were 43% of the call. `pshufb` is a
/// sixteen-entry byte gather in one instruction, which is exactly the shape of an
/// eight-entry palette lookup done sixteen times.
///
/// Returns `false` when SSSE3 is absent, so the caller keeps its scalar path.
///
/// `out` must span the four block rows, i.e. at least `3 * pitch + 16` bytes.
pub(super) fn bc5_gather(
    pr: u64,
    pg: u64,
    ir: u64,
    ig: u64,
    out: &mut [u8],
    pitch: usize,
) -> bool {
    if !has_ssse3() {
        return false;
    }
    debug_assert!(out.len() >= 3 * pitch + 16);
    // SAFETY: guarded by the `has_ssse3` check above, so every intrinsic used is
    // available. The four stores write sixteen bytes at `0, pitch, 2*pitch,
    // 3*pitch`, all within the `3 * pitch + 16` the caller guarantees; the loads
    // read eight bytes from `[u8; 8]` palettes and sixteen from local arrays.
    // Nothing is aligned-assuming and no pointer escapes.
    unsafe { bc5_gather_ssse3(pr, pg, ir, ig, out, pitch) }
    true
}

#[target_feature(enable = "ssse3,bmi2")]
unsafe fn bc5_gather_ssse3(
    pr: u64,
    pg: u64,
    ir: u64,
    ig: u64,
    out: &mut [u8],
    pitch: usize,
) {
    // Sixteen 3-bit indices per channel, one byte each. These extractions are
    // already independent — the index arithmetic measured at only ~10% of the
    // call — so they stay scalar rather than fighting a 3-bit field unpack in
    // vector form.
    // Sixteen 3-bit indices per channel, one per byte, built entirely in
    // registers. Writing them to a `[u8; 16]` and loading it back is the classic
    // store-forwarding stall — sixteen narrow stores feeding one wide load —
    // and it ate most of the gather win when measured that way.
    //
    // `pdep` with mask 0x0707..07 deposits each 3-bit group into its own byte,
    // which is exactly the unpack needed, at two instructions per eight pixels.
    const SPREAD: u64 = 0x0707_0707_0707_0707;
    let idx_vec = |w: u64| {
        _set64(
            core::arch::x86_64::_pdep_u64(w >> 24, SPREAD) as i64,
            core::arch::x86_64::_pdep_u64(w, SPREAD) as i64,
        )
    };

    // `movq` from a register, not a load from a stack array — see the caller.
    let pal_r = core::arch::x86_64::_mm_cvtsi64_si128(pr as i64);
    let pal_g = core::arch::x86_64::_mm_cvtsi64_si128(pg as i64);
    let rv = _mm_shuffle_epi8(pal_r, idx_vec(ir));
    let gv = _mm_shuffle_epi8(pal_g, idx_vec(ig));

    // Interleave to RGBA. `ba` is 0x00,0xFF per 16-bit lane: blue zero, alpha
    // opaque, which is what BC5 expands to.
    let ba = _mm_set1_epi16(0xFF00u16 as i16);
    let rg_lo = _mm_unpacklo_epi8(rv, gv); // pixels 0..8 as (r,g) pairs
    let rg_hi = _mm_unpackhi_epi8(rv, gv); // pixels 8..16
    let rows = [
        _mm_unpacklo_epi16(rg_lo, ba),
        _mm_unpackhi_epi16(rg_lo, ba),
        _mm_unpacklo_epi16(rg_hi, ba),
        _mm_unpackhi_epi16(rg_hi, ba),
    ];
    for (r, row) in rows.into_iter().enumerate() {
        _mm_storeu_si128(out.as_mut_ptr().add(r * pitch) as *mut __m128i, row);
    }
}


/// Is hardware half-float conversion available?
///
/// `vcvtph2ps` converts **eight** halves per instruction. BC6H decode spends
/// ~19% of its call converting 48 halves per block to `f32` (measured by
/// doubling that work: 121.3 -> 98.8 Mpx/s), so this is the largest remaining
/// piece of that format. F16C implies AVX in practice, but both are asserted.
#[inline]
pub(super) fn has_f16c() -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| {
        std::arch::is_x86_feature_detected!("f16c") && std::arch::is_x86_feature_detected!("avx")
    })
}

/// Convert one BC6H block's 48 half components to `f32`.
///
/// Returns `false` when F16C is absent, so the caller keeps its scalar twin.
pub(super) fn half48_to_f32(src: &[u16; 48], dst: &mut [f32; 48]) -> bool {
    if !has_f16c() {
        return false;
    }
    // SAFETY: guarded by `has_f16c`. Both buffers are fixed 48-element arrays and
    // the loop reads/writes exactly six aligned-agnostic 8-element groups within
    // them; `loadu`/`storeu` impose no alignment requirement.
    unsafe { half48_to_f32_f16c(src, dst) }
    true
}

#[target_feature(enable = "f16c,avx")]
unsafe fn half48_to_f32_f16c(src: &[u16; 48], dst: &mut [f32; 48]) {
    use core::arch::x86_64::{_mm256_cvtph_ps, _mm256_storeu_ps};
    for i in 0..6usize {
        let h = _mm_loadu_si128(src.as_ptr().add(i * 8) as *const __m128i);
        _mm256_storeu_ps(dst.as_mut_ptr().add(i * 8), _mm256_cvtph_ps(h));
    }
}


/// Is plain SSSE3 available?
///
/// Separate from [`has_ssse3`], which additionally demands a *fast* `pdep`
/// because the BC5 gather uses one. The BC1 gather needs only `pshufb`.
#[inline]
pub(super) fn has_pshufb() -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| std::arch::is_x86_feature_detected!("ssse3"))
}

/// `pshufb` selectors for four BC1 pixels, indexed by the byte holding their
/// four 2-bit indices.
///
/// A BC1 palette is four RGBA entries — exactly sixteen bytes, exactly one
/// register — so one `pshufb` produces four whole pixels. All that is needed is
/// the byte selector, and there are only 256 of them: `SEL[b][4k + c]` is
/// `4 * ((b >> 2k) & 3) + c`. 4 KiB, L1-resident, built at compile time.
const fn build_bc1_sel() -> [[u8; 16]; 256] {
    let mut t = [[0u8; 16]; 256];
    let mut b = 0usize;
    while b < 256 {
        let mut k = 0usize;
        while k < 4 {
            let e = ((b >> (2 * k)) & 3) as u8;
            let mut c = 0usize;
            while c < 4 {
                t[b][k * 4 + c] = e * 4 + c as u8;
                c += 1;
            }
            k += 1;
        }
        b += 1;
    }
    t
}

static BC1_SEL: [[u8; 16]; 256] = build_bc1_sel();

/// Decode a whole BC1 surface, four pixels per `pshufb`.
///
/// A BC1 palette is four RGBA entries — exactly sixteen bytes, exactly one
/// register — so one `pshufb` expands four pixels, and one block is four
/// selector loads, four shuffles and four stores.
///
/// # Why the whole loop lives here
///
/// The obvious shape is a per-block gather called from the shared block loop.
/// That shape measured **0/16 wins, z = -4.00, 47.8% slower than scalar**. It
/// loses on two counts, both from the ABI boundary a `#[target_feature]`
/// function cannot be inlined across: the call plus its `OnceLock` check cost
/// 27% of BC1 decode on their own, and passing the palette by value made the
/// caller spill it to stack for the callee to reload — a store-forwarding
/// stall worth a further 14%. Hoisting the boundary above the loop removes
/// both: one feature check per surface, and [`bc1_palette`] inlines in, so the
/// palette is built in registers and never reaches memory.
///
/// # Safety
///
/// The caller must have checked SSSE3, must pass a `data` long enough for
/// `blocks_x * blocks_y` eight-byte blocks, and an `out` long enough for
/// `blocks_y * 4` rows of `out_w` pixels — i.e. the aligned case of
/// `decode_rgba_blocks_into`, which is where it is called from.
#[target_feature(enable = "ssse3")]
pub(super) unsafe fn bc1_blocks_ssse3(
    data: &[u8],
    blocks_x: usize,
    blocks_y: usize,
    out: &mut [u8],
    out_w: usize,
) {
    let pitch = out_w * 4;
    let src = data.as_ptr();
    let dst = out.as_mut_ptr();
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let bi = (by * blocks_x + bx) * 8;
            let blk = core::slice::from_raw_parts(src.add(bi), 8);
            let pal = super::bcn::bc1_palette(blk, false);
            // Four u32 in registers straight into one xmm: no stack round trip.
            let p = _mm_set_epi32(
                pal[3] as i32,
                pal[2] as i32,
                pal[1] as i32,
                pal[0] as i32,
            );
            let idx = u32::from_le_bytes([blk[4], blk[5], blk[6], blk[7]]);
            let o = (by * 4 * out_w + bx * 4) * 4;
            for row in 0..4usize {
                let sel = _mm_loadu_si128(
                    BC1_SEL[((idx >> (8 * row)) & 0xff) as usize].as_ptr() as *const __m128i,
                );
                _mm_storeu_si128(
                    dst.add(o + row * pitch) as *mut __m128i,
                    _mm_shuffle_epi8(p, sel),
                );
            }
        }
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

    /// The SSSE3 surface loop must be byte-identical to the scalar block
    /// decoder across random blocks, both endpoint orderings, and the
    /// degenerate `c0 == c1` case where index 3 must come out transparent.
    #[test]
    fn bc1_blocks_ssse3_matches_scalar() {
        if !has_pshufb() {
            return;
        }
        let mut state = 0x1357_9bdf_2468_ace0u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        // A 8x8 surface: four blocks across, four down, so the row/column
        // addressing is exercised rather than assumed.
        const BX: usize = 2;
        const BY: usize = 2;
        for case in 0..20_000u32 {
            let mut data = [0u8; BX * BY * 8];
            match case {
                0 => {}
                1 => data.iter_mut().for_each(|x| *x = 0xff),
                // c0 == c1 in every block: the three-colour branch.
                2 => {
                    for b in 0..BX * BY {
                        data[b * 8..b * 8 + 4].copy_from_slice(&[0x34, 0x12, 0x34, 0x12]);
                    }
                }
                _ => {
                    for b in 0..BX * BY {
                        data[b * 8..b * 8 + 8].copy_from_slice(&next().to_le_bytes());
                    }
                }
            }
            let out_w = BX * 4;
            let mut got = vec![0u8; out_w * BY * 4 * 4];
            unsafe { bc1_blocks_ssse3(&data, BX, BY, &mut got, out_w) };

            let mut want = vec![0u8; out_w * BY * 4 * 4];
            let pitch = out_w * 4;
            for by in 0..BY {
                for bx in 0..BX {
                    let bi = (by * BX + bx) * 8;
                    let o = (by * 4 * out_w + bx * 4) * 4;
                    super::super::bcn::bc1_color_block_for_test(
                        &data[bi..bi + 8],
                        &mut want[o..],
                        pitch,
                        false,
                    );
                }
            }
            assert_eq!(got, want, "case {case}");
        }
    }
}
