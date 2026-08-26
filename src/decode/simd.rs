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
    __m128i, _mm_add_epi16, _mm_and_si128, _mm_cvtsi64_si128, _mm_loadl_epi64, _mm_setr_epi16,
    _mm_setr_epi8, _mm_srli_epi16,
    _mm_or_si128, _mm_set1_epi32, _mm_set_epi32, _mm_unpacklo_epi64, _mm_loadu_si128, _mm_mullo_epi16, _mm_packus_epi16,
    _mm_set1_epi16, _mm_set_epi16, _mm_set_epi64x, _mm_shuffle_epi8, _mm_srai_epi16,
    _mm_storel_epi64, _mm_storeu_si128, _mm_unpackhi_epi16, _mm_unpackhi_epi8,
    _mm_unpacklo_epi16, _mm_unpacklo_epi8,
};

// The register pre-packing (`pack4`, `pack_bd3`, `pack_bd4`, ...) and the
// 16-bit-lane safety argument moved to `super::interp_pack`, shared with the
// aarch64 NEON mirror (`decode::neon`) so the layout has one definition.
pub(super) use super::interp_pack::{pack4, pack_bd3, pack_bd4};
// The opaque-alpha packers have no production caller on this side — `bcn`
// reaches them through `pack_bd3` — but the oracle below spells them out.
#[cfg(test)]
pub(super) use super::interp_pack::{pack3_opaque_base, pack3_opaque_delta};

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
/// Cached feature probe.
///
/// A relaxed load is sufficient: the value is a property of the CPU, so it
/// never changes and every racing thread computes the SAME answer. The worst a
/// race can do is probe twice and store the same byte twice, and there is
/// nothing to publish besides the byte, so no ordering is needed to read it.
///
/// This replaced `OnceLock<bool>`, which is acquire-ordered and carries a slow
/// path LLVM cannot prove unreachable — so every probe kept a call site for
/// `OnceLock::initialize`. The decode block loop pays a probe on EVERY 4x4
/// block, which is what makes the difference worth the byte.
#[inline(always)]
fn probe(cache: &std::sync::atomic::AtomicU8, detect: impl FnOnce() -> bool) -> bool {
    use std::sync::atomic::Ordering;
    // 0 = not yet probed, 1 = absent, 2 = present.
    match cache.load(Ordering::Relaxed) {
        0 => {
            let v = if detect() { 2 } else { 1 };
            cache.store(v, Ordering::Relaxed);
            v == 2
        }
        v => v == 2,
    }
}

/// Needs SSSE3 (`pshufb`) and BMI2 (`pdep`) — neither is baseline — and needs
/// `pdep` to be fast rather than microcoded. See [`has_fast_pdep`]. Cached, and
/// the scalar twin covers every CPU that fails this.
#[inline]
pub(super) fn has_ssse3() -> bool {
    static OK: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
    probe(&OK, || {
        // SSE4.1 joined the requirement with the in-register palette build
        // (`bc4_palette_xmm` uses `pmulld`/`pblendw`).
        //
        // BMI2 and the fast-`pdep` CPUID probe LEFT it (inline-exe §5.2k.2):
        // the index unpack in `bc5_gather_ssse3` no longer uses `pdep`, so
        // there is nothing left to microcode. That admits AMD Zen 1 and
        // Zen 2 — which this gate previously excluded outright, sending them
        // down the scalar block path for every BC4 and BC5 surface.
        std::arch::is_x86_feature_detected!("ssse3")
            && std::arch::is_x86_feature_detected!("sse4.1")
    })
}

// RETIRED 2026-08-26 (inline-exe §5.2k.2): `has_fast_pdep`.
//
// It existed because BMI2 being *present* was not the question: on AMD Zen 1
// and Zen 2 `pdep`/`pext` are microcoded at roughly 18 cycles latency and
// 1/18 throughput, against 3 cycles on Intel Haswell-and-later and Zen
// 3-and-later, and the BC5 kernel issued four per block against a block
// budget near 100 cycles. So the probe read CPUID for the vendor string and
// family (Zen 1/2 are 0x17, Zen 3 is 0x19) and excluded those parts — which
// meant they decoded every BC4 and BC5 surface on the SCALAR path, forfeiting
// the whole vector win rather than a fraction of it.
//
// The cure was not a better probe but removing the instruction: the index
// unpack is now `pshufb` + `mullo` + `srli` (see `bc5_gather_ssse3`), so no
// `pdep` is issued on any CPU and there is nothing left to detect. A
// microarchitecture-specific CPUID hazard is a maintenance liability with a
// shelf life — prefer deleting the dependency to modelling the hardware.


/// Sixteen 3-bit indices, one per byte, entirely in registers — and WITHOUT
/// `pdep` (inline-exe §5.2k.2).
///
/// Writing the indices to a `[u8; 16]` and loading it back is the classic
/// store-forwarding stall — sixteen narrow stores feeding one wide load —
/// and it ate most of the gather win when measured that way. The previous
/// form used `pdep` with mask `0x0707..07`, which is the perfect instruction
/// for this on Intel and Zen 3+ — and MICROCODED at ~18 cycles on Zen 1 and
/// Zen 2, so [`has_ssse3`] had to exclude those parts entirely and they
/// decoded every BC4/BC5 surface on the scalar path (which the D8 probe
/// measures at ~6.3x slower for BC4, ~3.6x for BC5). Needing only SSSE3
/// retires that whole CPUID hazard — and it is the same rewrite the NEON
/// port needs, ARM having no `pdep` either.
///
/// # The extract
///
/// Per sixteen-bit lane: `pshufb` gathers the source byte (or two) its field
/// spans, then `mullo_epi16` by `2 ^ (13 - off)` lands that field at bits
/// `[13, 16)` — the multiply truncates everything at bit 16 and above, and
/// the shift discards everything below 13 — so one uniform `srli_epi16(13)`
/// both extracts and masks it. SSE2 has no per-lane variable shift; this is
/// the way around that, and it needs no mask constant.
///
/// Field `p` lives at bit `3p`, so within its group of eight it starts at
/// byte `(3p)/8` with offset `(3p)%8` — bytes `[0,0,0,1,1,1,2,2]`, offsets
/// `[0,3,6,1,4,7,2,5]`. Only offsets 6 and 7 (lanes 2 and 5) reach into a
/// second byte; every other field ends at bit 8 or below and fits in one.
///
/// Those single-byte lanes therefore select `-1`, so `pshufb` writes a zero
/// instead of a byte nobody reads. That is deliberate: it keeps EVERY
/// selector entry pointing at a real index byte (`0..=5`), so the constants
/// carry no implication that the source extends past the forty-eight bits
/// that exist. The arithmetic would survive junk there — its contribution
/// lands at bit 16 and `mullo` truncates it — but a selector reading byte 6
/// invites the next person to widen this into a memory load that runs off
/// the end of the payload.
///
/// Proven equal to `(w >> 3p) & 7` by `idx_spread_matches_scalar`.
#[target_feature(enable = "ssse3")]
unsafe fn idx_spread_ssse3(w: u64) -> __m128i {
    let sel_lo = _mm_setr_epi8(0, -1, 0, -1, 0, 1, 1, -1, 1, -1, 1, 2, 2, -1, 2, -1);
    let sel_hi = _mm_setr_epi8(3, -1, 3, -1, 3, 4, 4, -1, 4, -1, 4, 5, 5, -1, 5, -1);
    let mult = _mm_setr_epi16(8192, 1024, 128, 4096, 512, 64, 2048, 256);
    let src = _mm_cvtsi64_si128(w as i64);
    let lo = _mm_srli_epi16(_mm_mullo_epi16(_mm_shuffle_epi8(src, sel_lo), mult), 13);
    let hi = _mm_srli_epi16(_mm_mullo_epi16(_mm_shuffle_epi8(src, sel_hi), mult), 13);
    // Values are 0..=7, so `packus` never saturates.
    _mm_packus_epi16(lo, hi)
}

/// Build the eight-entry BC4 palette directly in the low eight bytes of an
/// `__m128i` — no scalar pack tree and no `movq` handoff (inline-exe D1).
///
/// The BC5 block is LATENCY bound (see `bc5_block_rgba`): one ~25-cycle
/// serial chain, of which the scalar palette build plus the register
/// transfer measured ~32%. This shortens the chain instead of thinning it —
/// the file's three recorded throughput refutations all failed that test.
///
/// Lane recipe: entry `k` is `(W[k] * delta + (e0 << 16) + 32768) >> 16`,
/// with `W[0] = 0` yielding exactly `e0` and `W[1] = 65536` exactly `e1`
/// (the `+32768` floors away under the shift for every delta), so all eight
/// entries share one formula; the four-interpolant arm then overwrites lanes
/// 6 and 7 with its constants via `pblendw`. Bytes are taken by `pshufb`
/// truncation, which is saturation-free and sign-agnostic — signed palettes
/// wrap to their two's-complement bytes exactly as the scalar `as u8` does.
/// Nothing here can overflow an i32 lane: `|W * delta| <= 65536 * 255` and
/// `e0 << 16` add to well under 2^31, and every final value is in
/// `-127 ..= 255`, inside `packs_epi32`'s exact range.
///
/// Proven equal to [`crate::decode::bcn::bc4_palette_packed`] over the
/// EXHAUSTIVE domain — all 65 536 endpoint pairs, both signs — by
/// `bc4_palette_xmm_matches_scalar_exhaustively`.
#[target_feature(enable = "ssse3,sse4.1")]
unsafe fn bc4_palette_xmm(a0: u8, a1: u8, is_signed: bool) -> __m128i {
    // Only the names this kernel adds; `_mm_setr_epi8`/`_mm_setr_epi16` come
    // from the module import. Re-importing them here shadowed it, and the
    // unused-import lint resolved that shadowing differently on 1.73 (the
    // declared MSRV) than on stable — six warnings on one, zero on the other.
    use core::arch::x86_64::{
        _mm_add_epi32, _mm_blend_epi16, _mm_mullo_epi32, _mm_packs_epi32, _mm_setr_epi32,
        _mm_srai_epi32,
    };
    let (e0, e1) = if is_signed {
        ((a0 as i8 as i32).max(-127), (a1 as i8 as i32).max(-127))
    } else {
        (a0 as i32, a1 as i32)
    };
    let dv = _mm_set1_epi32(e1 - e0);
    let add = _mm_set1_epi32((e0 << 16) + 32768);
    let take = _mm_setr_epi8(0, 2, 4, 6, 8, 10, 12, 14, -1, -1, -1, -1, -1, -1, -1, -1);
    let entries = |w: __m128i| _mm_srai_epi32(_mm_add_epi32(_mm_mullo_epi32(dv, w), add), 16);
    if e0 > e1 {
        let lo = entries(_mm_setr_epi32(0, 65536, 9363, 18724));
        let hi = entries(_mm_setr_epi32(28086, 37450, 46812, 56173));
        _mm_shuffle_epi8(_mm_packs_epi32(lo, hi), take)
    } else {
        let lo = entries(_mm_setr_epi32(0, 65536, 13107, 26215));
        // Lanes 6 and 7 are the mode's constants, not interpolants; the two
        // zero weights below are placeholders the blend overwrites.
        let hi = entries(_mm_setr_epi32(39321, 52429, 0, 0));
        let tail = if is_signed {
            _mm_setr_epi16(0, 0, 0, 0, 0, 0, -127, 127)
        } else {
            _mm_setr_epi16(0, 0, 0, 0, 0, 0, 0, 255)
        };
        let v = _mm_blend_epi16(_mm_packs_epi32(lo, hi), tail, 0b1100_0000);
        _mm_shuffle_epi8(v, take)
    }
}

/// Gather both channels of a BC5 block and write all four RGBA rows, building
/// the palettes in-register on the way.
///
/// The measured cost in BC5 was the **table lookup**, not the index arithmetic:
/// with the lookup stubbed out the block runs at ~655 Mpx/s against ~371 with it,
/// so thirty-two dependent byte loads were 43% of the call. `pshufb` is a
/// sixteen-entry byte gather in one instruction, which is exactly the shape of an
/// eight-entry palette lookup done sixteen times.
///
/// Endpoints arrive as raw block bytes and the palettes are built HERE, by
/// [`bc4_palette_xmm`] — the caller no longer runs the scalar build at all on
/// this path (D1: that build fed a pack tree and a `movq`, ~32% of the
/// latency chain). `green` is `None` for BC4, which expands with a zero
/// second channel.
///
/// Returns `false` when the ISA gate fails, so the caller keeps its scalar path.
///
/// `out` must span the four block rows, i.e. at least `3 * pitch + 16` bytes.
#[allow(clippy::too_many_arguments)]
pub(super) fn bc5_gather(
    r0: u8,
    r1: u8,
    green: Option<(u8, u8)>,
    ir: u64,
    ig: u64,
    is_signed: bool,
    out: &mut [u8],
    pitch: usize,
) -> bool {
    if !has_ssse3() {
        return false;
    }
    debug_assert!(out.len() >= 3 * pitch + 16);
    // SAFETY: guarded by the `has_ssse3` check above (which asserts SSSE3 and
    // SSE4.1), so every intrinsic used is available. The four stores
    // write sixteen bytes at `0, pitch, 2*pitch, 3*pitch`, all within the
    // `3 * pitch + 16` the caller guarantees; nothing is aligned-assuming and
    // no pointer escapes.
    unsafe { bc5_gather_ssse3(r0, r1, green, ir, ig, is_signed, out.as_mut_ptr(), pitch) }
    true
}

#[allow(clippy::too_many_arguments)]
#[target_feature(enable = "ssse3,sse4.1")]
unsafe fn bc5_gather_ssse3(
    r0: u8,
    r1: u8,
    green: Option<(u8, u8)>,
    ir: u64,
    ig: u64,
    is_signed: bool,
    dst: *mut u8,
    pitch: usize,
) {
    // Sixteen 3-bit indices to sixteen bytes — see `idx_spread_ssse3`.
    let idx_vec = |w: u64| idx_spread_ssse3(w);

    // Palettes built in-register — see `bc4_palette_xmm`. Same feature set,
    // so both builds inline into this chain; no scalar pack tree, no `movq`.
    let pal_r = bc4_palette_xmm(r0, r1, is_signed);
    let pal_g = match green {
        Some((g0, g1)) => bc4_palette_xmm(g0, g1, is_signed),
        None => core::arch::x86_64::_mm_setzero_si128(),
    };
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
        _mm_storeu_si128(dst.add(r * pitch) as *mut __m128i, row);
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
    static OK: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
    probe(&OK, || {
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
    static OK: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
    probe(&OK, || std::arch::is_x86_feature_detected!("ssse3"))
}

// The `pshufb` selector table for four BC1 pixels lives in `crate::simd_tables`
// (shared with the encoder's fixed-table BC1 SSE kernel, which must build
// without the `decode` feature).
pub(crate) use crate::simd_tables::BC1_SEL;

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
/// function cannot be inlined across: the call plus its feature check cost
/// 27% of BC1 decode on their own (that check was a `OnceLock` when this was
/// measured; it is a relaxed byte now, which shrinks the second term but not
/// the call), and passing the palette by value made the
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
    grid_x: usize,
    run_x: usize,
    run_y: usize,
    out: &mut [u8],
    out_w: usize,
) {
    let pitch = out_w * 4;
    let src = data.as_ptr();
    let dst = out.as_mut_ptr();
    for by in 0..run_y {
        for bx in 0..run_x {
            let bi = (by * grid_x + bx) * 8;
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


/// Per-pixel mask keeping RGB and clearing alpha, so a decoded alpha byte can be
/// `or`ed straight in.
const RGB_MASK: i32 = 0x00ff_ffff;

/// Two BC2 alpha pixels, indexed by the byte holding their two 4-bit values.
///
/// Laid out at the alpha positions of two RGBA pixels with the colour bytes
/// zeroed, so `unpacklo_epi64` of two of these is a whole row's alpha, ready to
/// `or` into the colour vector. `* 17` is the reference's 4-bit-to-8-bit scale,
/// exactly `0x0F -> 0xFF`.
const fn build_bc2_alpha() -> [[u8; 8]; 256] {
    let mut t = [[0u8; 8]; 256];
    let mut b = 0usize;
    while b < 256 {
        t[b][3] = ((b & 0x0f) * 17) as u8;
        t[b][7] = (((b >> 4) & 0x0f) * 17) as u8;
        b += 1;
    }
    t
}

static BC2_ALPHA: [[u8; 8]; 256] = build_bc2_alpha();

/// `pshufb` selectors for two BC3 alpha pixels, indexed by the six bits holding
/// their two 3-bit palette indices.
///
/// `0x80` makes `pshufb` emit zero, so the colour bytes come out clear and only
/// the alpha positions carry a palette entry. Six bits rather than twelve keeps
/// the table at 512 bytes instead of 64 KiB — two loads and an `unpacklo_epi64`
/// per row are far cheaper than leaving L1.
const fn build_bc3_sel() -> [[u8; 8]; 64] {
    let mut t = [[0x80u8; 8]; 64];
    let mut b = 0usize;
    while b < 64 {
        t[b][3] = (b & 0x7) as u8;
        t[b][7] = ((b >> 3) & 0x7) as u8;
        b += 1;
    }
    t
}

static BC3_SEL: [[u8; 8]; 64] = build_bc3_sel();

/// Decode a whole BC2 surface: BC1 colour, with 4-bit alpha folded in before the
/// store.
///
/// The scalar path decodes colour, stores four RGBA words per row, and then
/// performs **sixteen single-byte read-modify-writes** back into those same
/// words. That is a store-forwarding hazard per pixel on top of a doubled store
/// stream, and a ceiling probe put it at **37% of BC2 decode** (0.2305 ms
/// against 0.1445 with the alpha pass stubbed). Merging the alpha into the
/// colour vector makes the block one store per row again.
///
/// # Safety
///
/// As [`bc1_blocks_ssse3`], with sixteen-byte blocks.
#[target_feature(enable = "ssse3")]
pub(super) unsafe fn bc2_blocks_ssse3(
    data: &[u8],
    grid_x: usize,
    run_x: usize,
    run_y: usize,
    out: &mut [u8],
    out_w: usize,
) {
    let pitch = out_w * 4;
    let src = data.as_ptr();
    let dst = out.as_mut_ptr();
    let keep = _mm_set1_epi32(RGB_MASK);
    for by in 0..run_y {
        for bx in 0..run_x {
            let bi = (by * grid_x + bx) * 16;
            let blk = core::slice::from_raw_parts(src.add(bi), 16);
            // `true`: BC2 colour blocks are always four-colour, whatever the
            // endpoint order.
            let pal = super::bcn::bc1_palette(&blk[8..16], true);
            let p = _mm_set_epi32(pal[3] as i32, pal[2] as i32, pal[1] as i32, pal[0] as i32);
            let idx = u32::from_le_bytes([blk[12], blk[13], blk[14], blk[15]]);
            let o = (by * 4 * out_w + bx * 4) * 4;
            for row in 0..4usize {
                let colour = _mm_shuffle_epi8(
                    p,
                    _mm_loadu_si128(
                        BC1_SEL[((idx >> (8 * row)) & 0xff) as usize].as_ptr() as *const __m128i
                    ),
                );
                let alpha = _mm_unpacklo_epi64(
                    _mm_loadl_epi64(
                        BC2_ALPHA[blk[row * 2] as usize].as_ptr() as *const __m128i
                    ),
                    _mm_loadl_epi64(
                        BC2_ALPHA[blk[row * 2 + 1] as usize].as_ptr() as *const __m128i
                    ),
                );
                _mm_storeu_si128(
                    dst.add(o + row * pitch) as *mut __m128i,
                    _mm_or_si128(_mm_and_si128(colour, keep), alpha),
                );
            }
        }
    }
}

/// Build the eight-entry BC3 alpha palette directly in the low eight bytes
/// of an `__m128i` — the D1 recipe carried to the DIVISION-form palette
/// (inline-exe D3; the build ceiling-probes at ~22% of BC3 decode).
///
/// This is NOT the refuted scalar `base + k*delta` rewrite recorded on
/// `bc3_alpha_palette_packed` — that one serialised six scalar entries
/// behind computing `delta`; here all eight lanes issue together and the
/// exact division becomes ONE `pmulhi_epu16` against a reciprocal:
///
/// * entry k is `(WA[k]*a0 + WB[k]*a1 + 1) / 7` (or `/ 5`), and
///   `WA = 7, WB = 0` / `WA = 0, WB = 7` reproduce `a0` and `a1` exactly
///   through the same formula — `(7*a0 + 1) / 7 == a0` — so lanes 0 and 1
///   need no special casing;
/// * `N * 9363 >> 16` equals `N / 7` exactly for `N < 1872` (numerators
///   reach 1786), and `N * 13108 >> 16` equals `N / 5` exactly for
///   `N < 3276` (numerators reach 1276);
/// * the four-interpolant arm's constant lane 6 falls out of the formula
///   (`N = 1` divides to 0) and lane 7 is one `insert_epi16`.
///
/// Everything is SSE2 except the caller's `pshufb`, so the surface kernel's
/// SSSE3 gate is unchanged. Proven equal to the scalar build over the
/// EXHAUSTIVE domain — all 65 536 endpoint pairs — by
/// `bc3_alpha_xmm_matches_scalar_exhaustively`.
#[target_feature(enable = "ssse3")]
unsafe fn bc3_alpha_xmm(a0: u8, a1: u8) -> __m128i {
    // As `bc4_palette_xmm`: only the names not already imported at module
    // scope, so nothing is shadowed and the MSRV and stable lints agree.
    use core::arch::x86_64::{_mm_insert_epi16, _mm_mulhi_epu16};
    let a0v = _mm_set1_epi16(a0 as i16);
    let a1v = _mm_set1_epi16(a1 as i16);
    let take = _mm_setr_epi8(0, 2, 4, 6, 8, 10, 12, 14, -1, -1, -1, -1, -1, -1, -1, -1);
    let numer = |wa: __m128i, wb: __m128i| {
        _mm_add_epi16(
            _mm_add_epi16(_mm_mullo_epi16(wa, a0v), _mm_mullo_epi16(wb, a1v)),
            _mm_set1_epi16(1),
        )
    };
    if a0 > a1 {
        let n = numer(
            _mm_setr_epi16(7, 0, 6, 5, 4, 3, 2, 1),
            _mm_setr_epi16(0, 7, 1, 2, 3, 4, 5, 6),
        );
        _mm_shuffle_epi8(_mm_mulhi_epu16(n, _mm_set1_epi16(9363)), take)
    } else {
        let n = numer(
            _mm_setr_epi16(5, 0, 4, 3, 2, 1, 0, 0),
            _mm_setr_epi16(0, 5, 1, 2, 3, 4, 0, 0),
        );
        let e = _mm_mulhi_epu16(n, _mm_set1_epi16(13108));
        _mm_shuffle_epi8(_mm_insert_epi16(e, 255, 7), take)
    }
}

/// Decode a whole BC3 surface: BC1 colour, with an interpolated alpha block
/// gathered by a second `pshufb` and folded in before the store.
///
/// Same defect as BC2 — sixteen byte read-modify-writes over the colour words,
/// measured at **26% of BC3 decode** (0.2734 ms against 0.2031 stubbed) — and
/// the same fix. The alpha palette is eight bytes, so it rides in the low half
/// of a register and `pshufb` selects from it directly.
///
/// # Safety
///
/// As [`bc1_blocks_ssse3`], with sixteen-byte blocks.
#[target_feature(enable = "ssse3")]
pub(super) unsafe fn bc3_blocks_ssse3(
    data: &[u8],
    grid_x: usize,
    run_x: usize,
    run_y: usize,
    out: &mut [u8],
    out_w: usize,
) {
    let pitch = out_w * 4;
    let src = data.as_ptr();
    let dst = out.as_mut_ptr();
    let keep = _mm_set1_epi32(RGB_MASK);
    for by in 0..run_y {
        for bx in 0..run_x {
            let bi = (by * grid_x + bx) * 16;
            let blk = core::slice::from_raw_parts(src.add(bi), 16);
            let pal = super::bcn::bc1_palette(&blk[8..16], true);
            let p = _mm_set_epi32(pal[3] as i32, pal[2] as i32, pal[1] as i32, pal[0] as i32);
            let cidx = u32::from_le_bytes([blk[12], blk[13], blk[14], blk[15]]);
            // Alpha palette built in-register — see `bc3_alpha_xmm`. Same
            // feature set, so it inlines here; no scalar build, no `movq`.
            let apal = bc3_alpha_xmm(blk[0], blk[1]);
            let aidx = u64::from_le_bytes([
                blk[0], blk[1], blk[2], blk[3], blk[4], blk[5], blk[6], blk[7],
            ]) >> 16;
            let o = (by * 4 * out_w + bx * 4) * 4;
            for row in 0..4usize {
                let colour = _mm_shuffle_epi8(
                    p,
                    _mm_loadu_si128(
                        BC1_SEL[((cidx >> (8 * row)) & 0xff) as usize].as_ptr() as *const __m128i
                    ),
                );
                // Twelve index bits per row, split into two six-bit lookups.
                let sh = 12 * row;
                let sel = _mm_unpacklo_epi64(
                    _mm_loadl_epi64(
                        BC3_SEL[((aidx >> sh) & 0x3f) as usize].as_ptr() as *const __m128i
                    ),
                    _mm_loadl_epi64(
                        BC3_SEL[((aidx >> (sh + 6)) & 0x3f) as usize].as_ptr() as *const __m128i
                    ),
                );
                let alpha = _mm_shuffle_epi8(apal, sel);
                _mm_storeu_si128(
                    dst.add(o + row * pitch) as *mut __m128i,
                    _mm_or_si128(_mm_and_si128(colour, keep), alpha),
                );
            }
        }
    }
}


/// Is AVX2 available?
///
/// Decode keeps its own check rather than borrowing the encoder's, so the
/// decoder stands alone when `encode` is compiled out.
#[inline]
pub(super) fn has_avx2() -> bool {
    static OK: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
    probe(&OK, || std::arch::is_x86_feature_detected!("avx2"))
}

/// Interpolate one BC6H mode-11 block: sixteen weights against three channels,
/// eight lanes at a time.
///
/// A ceiling probe puts this at **~37% of BC6H decode**, the largest single
/// share left in it — 0.65 ms full against 0.41 with the arithmetic stubbed.
///
/// # Why 32-bit lanes
///
/// `base` is `a * 64 + 32` for an unquantized endpoint up to 0xFFFF, so it
/// reaches 4 194 336 and `w * delta` spans +/-4 194 240. Both need `i32`; the
/// sum is the original `a * (64 - w) + c * w + 32`, so it stays in
/// `0 ..= 4 194 336`, `>> 6` lands in `0 ..= 65 535`, and `(v * 31) >> 6` in
/// `0 ..= 31 743`. `packus_epi32` therefore never saturates, and the arithmetic
/// shifts are exact because nothing is ever negative.
///
/// # Why the output is planar
///
/// Writing `r * 16, g * 16, b * 16` means the kernel never interleaves: three
/// broadcasts and six store-ready vectors, no cross-lane shuffling at all. The
/// f32 conversion downstream is layout-agnostic, and the RGBA widen that
/// follows was already a strided read — a ceiling probe puts it at ~8%, and
/// reading three planes costs it nothing.
pub(super) fn bc6h_interp_avx2(
    base: &[i32; 3],
    delta: &[i32; 3],
    w: &[i32; 16],
    out: &mut [u16; 48],
) -> bool {
    if !has_avx2() {
        return false;
    }
    // SAFETY: guarded above. Every load and store is a fixed offset inside the
    // three fixed-size arrays, and `loadu`/`storeu` impose no alignment
    // requirement.
    unsafe { bc6h_interp_avx2_impl(base, delta, w, out) }
    true
}

#[target_feature(enable = "avx2")]
unsafe fn bc6h_interp_avx2_impl(
    base: &[i32; 3],
    delta: &[i32; 3],
    w: &[i32; 16],
    out: &mut [u16; 48],
) {
    use core::arch::x86_64::{
        __m256i, _mm256_add_epi32, _mm256_castsi256_si128, _mm256_loadu_si256,
        _mm256_mullo_epi32, _mm256_packus_epi32, _mm256_permute4x64_epi64, _mm256_set1_epi32,
        _mm256_srai_epi32,
    };
    let wv = [
        _mm256_loadu_si256(w.as_ptr() as *const __m256i),
        _mm256_loadu_si256(w.as_ptr().add(8) as *const __m256i),
    ];
    let s31 = _mm256_set1_epi32(31);
    for ch in 0..3usize {
        let bv = _mm256_set1_epi32(base[ch]);
        let dv = _mm256_set1_epi32(delta[ch]);
        for half in 0..2usize {
            let v = _mm256_srai_epi32(
                _mm256_add_epi32(bv, _mm256_mullo_epi32(dv, wv[half])),
                6,
            );
            // finish_unquantize: scale by 31/64. The result IS the half pattern.
            let v = _mm256_srai_epi32(_mm256_mullo_epi32(v, s31), 6);
            // `packus` folds within 128-bit lanes, so qwords 0 and 2 hold the
            // eight values we want; `permute4x64` brings them together.
            let packed = _mm256_permute4x64_epi64(_mm256_packus_epi32(v, v), 0b0000_1000);
            _mm_storeu_si128(
                out.as_mut_ptr().add(ch * 16 + half * 8) as *mut __m128i,
                _mm256_castsi256_si128(packed),
            );
        }
    }
}


/// Decode a whole BC5 surface, both channels gathered per block.
///
/// Same reason the BC1/BC2/BC3 loops live here: a `#[target_feature]` function
/// cannot be inlined into a caller that lacks the feature, so dispatching inside
/// the block loop pays a real call plus a feature check on **every 4x4
/// block**. 0.3.28 measured that boundary at 26.7% of BC1 decode, when the
/// check was a `OnceLock`; it is a relaxed byte now, so the check is nearly
/// free and the CALL is what remains. BC4 and BC5
/// won their gathers in 0.3.28 *despite* paying it every block, because those
/// gathers are heavy enough to carry it — which is exactly why it went
/// unnoticed until the dispatch sites were listed side by side.
///
/// `bc5_gather_ssse3` is itself a `#[target_feature]` function with the same
/// features, so it inlines into this loop rather than being called.
///
/// # Safety
///
/// The caller must have checked SSSE3 and fast `pdep`, must pass a `data` long
/// enough for `blocks_x * blocks_y` sixteen-byte blocks, and an `out` long
/// enough for `blocks_y * 4` rows of `out_w` pixels — the aligned case its
/// caller validates.
#[target_feature(enable = "ssse3,sse4.1")]
pub(super) unsafe fn bc5_blocks_ssse3(
    data: &[u8],
    grid_x: usize,
    run_x: usize,
    run_y: usize,
    out: &mut [u8],
    out_w: usize,
    is_signed: bool,
) {
    let pitch = out_w * 4;
    let src = data.as_ptr();
    let dst = out.as_mut_ptr();
    for by in 0..run_y {
        for bx in 0..run_x {
            let bi = (by * grid_x + bx) * 16;
            let blk = core::slice::from_raw_parts(src.add(bi), 16);
            let ir = super::bcn::bc4_indices(&blk[..8]);
            let ig = super::bcn::bc4_indices(&blk[8..16]);
            let o = (by * 4 * out_w + bx * 4) * 4;
            bc5_gather_ssse3(
                blk[0],
                blk[1],
                Some((blk[8], blk[9])),
                ir,
                ig,
                is_signed,
                dst.add(o),
                pitch,
            );
        }
    }
}

/// Decode a whole BC4 surface.
///
/// BC4 is BC5 with a zero second channel, so it runs the same gather with an
/// all-zero green palette and a zero index word — reusing that kernel and its
/// oracle rather than duplicating either, exactly as the per-block path did.
///
/// # Safety
///
/// As [`bc5_blocks_ssse3`], with eight-byte blocks.
#[target_feature(enable = "ssse3,sse4.1")]
pub(super) unsafe fn bc4_blocks_ssse3(
    data: &[u8],
    grid_x: usize,
    run_x: usize,
    run_y: usize,
    out: &mut [u8],
    out_w: usize,
    is_signed: bool,
) {
    let pitch = out_w * 4;
    let src = data.as_ptr();
    let dst = out.as_mut_ptr();
    for by in 0..run_y {
        for bx in 0..run_x {
            let bi = (by * grid_x + bx) * 8;
            let blk = core::slice::from_raw_parts(src.add(bi), 8);
            let ir = super::bcn::bc4_indices(blk);
            let o = (by * 4 * out_w + bx * 4) * 4;
            bc5_gather_ssse3(blk[0], blk[1], None, ir, 0, is_signed, dst.add(o), pitch);
        }
    }
}


/// Convert one BC6H block's planar halves straight to RGBA `f32` rows, without
/// ever materialising an intermediate `f32` array.
///
/// # The stall this removes
///
/// The two-pass shape was: convert 48 halves into an `[f32; 48]` scratch with
/// 256-bit stores, then build each output row by reading that scratch back with
/// **scalar four-byte loads**. A vector store feeding a scalar load is this
/// crate's recurring store-forwarding stall, and a decomposition probe found the
/// pair costing **34% (conversion) + 29% (widen)** of BC6H decode — against 8%
/// for the interpolation that 0.3.30 vectorised.
///
/// Planar halves (0.3.30) are what make the fusion cheap: eight reds, eight
/// greens and eight blues are each one contiguous 128-bit load, so `vcvtph2ps`
/// yields three vectors of eight floats that transpose to eight RGBA pixels with
/// four unpacks, four shuffles and four `permute2f128`s. Alpha is a constant
/// `1.0` vector, never loaded.
///
/// Two groups of eight pixels cover the block; group 0 is rows 0-1 and group 1
/// is rows 2-3, because planar pixel order is row-major.
///
/// Returns `false` when F16C/AVX are absent, so the caller keeps its scalar
/// two-pass path.
///
/// # Safety
///
/// `dst` must have room for four rows of sixteen `f32` at `pitch` stride, i.e.
/// `3 * pitch + 16` elements.
pub(super) unsafe fn bc6h_planar_to_rgba(src: &[u16; 48], dst: *mut f32, pitch: usize) -> bool {
    if !has_f16c() {
        return false;
    }
    bc6h_planar_to_rgba_f16c(src, dst, pitch);
    true
}

#[target_feature(enable = "f16c,avx")]
unsafe fn bc6h_planar_to_rgba_f16c(src: &[u16; 48], dst: *mut f32, pitch: usize) {
    use core::arch::x86_64::{
        _mm256_cvtph_ps, _mm256_permute2f128_ps, _mm256_set1_ps, _mm256_shuffle_ps,
        _mm256_storeu_ps, _mm256_unpackhi_ps, _mm256_unpacklo_ps,
    };
    let one = _mm256_set1_ps(1.0);
    for g in 0..2usize {
        let ld = |ch: usize| {
            _mm256_cvtph_ps(_mm_loadu_si128(
                src.as_ptr().add(ch * 16 + g * 8) as *const __m128i,
            ))
        };
        let (rf, gf, bf) = (ld(0), ld(1), ld(2));

        // 8x4 transpose: r0g0r1g1 / r2g2r3g3 / b0a0b1a1 / b2a2b3a3 per lane.
        let t0 = _mm256_unpacklo_ps(rf, gf);
        let t1 = _mm256_unpackhi_ps(rf, gf);
        let t2 = _mm256_unpacklo_ps(bf, one);
        let t3 = _mm256_unpackhi_ps(bf, one);
        // Each `q` now holds one pixel per 128-bit lane: q0 = px0 | px4, etc.
        let q0 = _mm256_shuffle_ps(t0, t2, 0x44);
        let q1 = _mm256_shuffle_ps(t0, t2, 0xEE);
        let q2 = _mm256_shuffle_ps(t1, t3, 0x44);
        let q3 = _mm256_shuffle_ps(t1, t3, 0xEE);

        // Lane-lows are the first row of the group, lane-highs the second.
        let row = dst.add(g * 2 * pitch);
        _mm256_storeu_ps(row, _mm256_permute2f128_ps(q0, q1, 0x20));
        _mm256_storeu_ps(row.add(8), _mm256_permute2f128_ps(q2, q3, 0x20));
        let row = row.add(pitch);
        _mm256_storeu_ps(row, _mm256_permute2f128_ps(q0, q1, 0x31));
        _mm256_storeu_ps(row.add(8), _mm256_permute2f128_ps(q2, q3, 0x31));
    }
}

// REFUTED (2026-08-26): a whole-surface `#[target_feature(avx2,f16c)]` BC6H
// loop — `bc6h_blocks_avx2`, one dispatch per surface with all three kernels
// inlined into it, mirroring the BC1-BC5 surface loops — was built, verified
// byte-identical, and MEASURED SLOWER: 2.05 -> 2.9-4.5 ns/px at 1024²,
// interleaved ABBA against a HEAD-worktree baseline, replicated across seven
// rounds and two orderings, with the call boundaries confirmed gone in the
// generated code. Outlining the transpose again recovered only part of it,
// and even the "obviously free" probes-only variant inherited damage — the
// merged/refactored loop compiled to 2462 lines against the incumbent's
// 1280, with thirteen bounds-check panic paths against four and both interp
// flavours inlined into one stack-heavy frame. This loop's codegen is
// precariously tuned; the 26.7%-of-BC1 boundary law does NOT transfer:
// kernels this heavy use the per-block call boundaries as register and
// scheduling barriers. Everything reverted to the shape that measures
// fastest — the one below and in `bc6h.rs`. Two laws worth the dig:
// (1) returning an 88-byte tuple from a non-inlined helper cost more per
// block than all three call boundaries combined; (2) the episode left
// `bc6h_interp_avx2` with its first direct oracle
// (`mode11_interp_vector_matches_scalar`), closing a §4.5 gap.

#[cfg(test)]
mod tests {
    use super::*;

    /// The in-register palette build must equal the scalar
    /// `bc4_palette_packed` over the EXHAUSTIVE domain — all 65 536 endpoint
    /// pairs, both signs, both mode arms and every saturating clamp. Proof by
    /// exhaustion, not sampling: the whole input domain is two bytes.
    #[test]
    fn bc4_palette_xmm_matches_scalar_exhaustively() {
        if !has_ssse3() {
            eprintln!("SSSE3/SSE4.1/BMI2 gate not passed; skipping");
            return;
        }
        for a0 in 0..=255u8 {
            for a1 in 0..=255u8 {
                for signed in [false, true] {
                    let want = super::super::bcn::bc4_palette_packed(a0, a1, signed);
                    // SAFETY: the gate above asserts SSSE3 and SSE4.1.
                    let got = unsafe {
                        let v = bc4_palette_xmm(a0, a1, signed);
                        let mut out = [0u8; 16];
                        _mm_storeu_si128(out.as_mut_ptr() as *mut __m128i, v);
                        u64::from_le_bytes(out[..8].try_into().unwrap())
                    };
                    assert_eq!(
                        got, want,
                        "a0={a0} a1={a1} signed={signed}: {got:#018x} != {want:#018x}"
                    );
                }
            }
        }
    }

    /// The `pdep`-free index unpack must equal the field extraction it
    /// replaces — byte `p` is `(w >> 3p) & 7` — for every bit pattern shape
    /// that matters (inline-exe §5.2k.2).
    ///
    /// The domain is 2^48, so this is structured plus random rather than
    /// exhaustive: each index in isolation at its maximum (which walks a
    /// `7` across all sixteen fields, covering every byte boundary the
    /// two-byte lanes straddle), all-zero, all-ones, and 200k random words.
    #[test]
    fn idx_spread_matches_scalar() {
        if !has_ssse3() {
            eprintln!("SSSE3/SSE4.1 gate not passed; skipping");
            return;
        }
        let check = |w: u64| {
            // SAFETY: SSSE3 checked above.
            let got = unsafe {
                let v = idx_spread_ssse3(w);
                let mut out = [0u8; 16];
                _mm_storeu_si128(out.as_mut_ptr() as *mut __m128i, v);
                out
            };
            let mut want = [0u8; 16];
            for (p, slot) in want.iter_mut().enumerate() {
                *slot = ((w >> (3 * p)) & 7) as u8;
            }
            assert_eq!(got, want, "w = {w:#014x}");
        };
        check(0);
        check(0x0000_FFFF_FFFF_FFFF);
        // One field at a time, every value, at every position.
        for p in 0..16u32 {
            for v in 0..8u64 {
                check(v << (3 * p));
                // ...and against an all-ones background, so a lane that
                // wrongly picks up a neighbour's bits cannot hide.
                check((0x0000_FFFF_FFFF_FFFFu64 & !(7 << (3 * p))) | (v << (3 * p)));
            }
        }
        let mut state = 0x52c2_9e37_79b9u64 | 1;
        for _ in 0..200_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            check(state & 0x0000_FFFF_FFFF_FFFF);
        }
    }

    /// The in-register BC3 alpha palette must equal the scalar division-form
    /// build over the EXHAUSTIVE domain — all 65 536 endpoint pairs, both
    /// mode arms, every reciprocal-rounding edge. Proof by exhaustion: the
    /// whole input domain is two bytes.
    #[test]
    fn bc3_alpha_xmm_matches_scalar_exhaustively() {
        if !has_pshufb() {
            eprintln!("SSSE3 not available; skipping");
            return;
        }
        for a0 in 0..=255u8 {
            for a1 in 0..=255u8 {
                let want = super::super::bcn::bc3_alpha_palette_packed(a0, a1);
                // SAFETY: SSSE3 checked above.
                let got = unsafe {
                    let v = bc3_alpha_xmm(a0, a1);
                    let mut out = [0u8; 16];
                    _mm_storeu_si128(out.as_mut_ptr() as *mut __m128i, v);
                    u64::from_le_bytes(out[..8].try_into().unwrap())
                };
                assert_eq!(got, want, "a0={a0} a1={a1}: {got:#018x} != {want:#018x}");
            }
        }
    }

    /// The AVX2 mode-11 interpolator against the scalar expression it
    /// replaces, DIRECTLY — §4.5 oracle debt: until now `bc6h_interp_avx2`
    /// was covered only end-to-end through whichever path the dispatch
    /// happened to take. Inputs are generated exactly as `bc6h_mode11_half`
    /// derives them — 10-bit endpoints through the saturating unquantize,
    /// weights from the W4 table — including both saturation extremes, where
    /// `base` hits its range limits and `packus` would show any lane error.
    #[test]
    fn mode11_interp_vector_matches_scalar() {
        if !has_avx2() {
            eprintln!("AVX2 not available; skipping");
            return;
        }
        const W4: [i32; 16] = [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64];
        let uq = |v: i32| {
            if v == 0 {
                0
            } else if v == 1023 {
                0xFFFF
            } else {
                ((v << 16) + 0x8000) >> 10
            }
        };
        let mut state = 0x6bc6_d155_a7c4_2026u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..60_000u32 {
            let (e0, e1): ([i32; 3], [i32; 3]) = match case {
                0 => ([0; 3], [0; 3]),
                1 => ([1023; 3], [1023; 3]),
                2 => ([0; 3], [1023; 3]),
                3 => ([1023; 3], [0; 3]),
                _ => {
                    let r = next();
                    let s = next();
                    (
                        [
                            (r & 0x3ff) as i32,
                            ((r >> 10) & 0x3ff) as i32,
                            ((r >> 20) & 0x3ff) as i32,
                        ],
                        [
                            (s & 0x3ff) as i32,
                            ((s >> 10) & 0x3ff) as i32,
                            ((s >> 20) & 0x3ff) as i32,
                        ],
                    )
                }
            };
            let a = [uq(e0[0]), uq(e0[1]), uq(e0[2])];
            let c = [uq(e1[0]), uq(e1[1]), uq(e1[2])];
            let base = [a[0] * 64 + 32, a[1] * 64 + 32, a[2] * 64 + 32];
            let delta = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let mut w = [0i32; 16];
            for wp in w.iter_mut() {
                *wp = W4[(next() & 0xf) as usize];
            }
            let mut fast = [0u16; 48];
            assert!(bc6h_interp_avx2(&base, &delta, &w, &mut fast));
            let mut slow = [0u16; 48];
            for (p, &wp) in w.iter().enumerate() {
                for ch in 0..3 {
                    let v = (base[ch] + wp * delta[ch]) >> 6;
                    slow[ch * 16 + p] = ((v * 31) >> 6) as u16;
                }
            }
            assert_eq!(fast, slow, "case {case}: e0={e0:?} e1={e1:?} w={w:?}");
        }
    }

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
            unsafe { bc1_blocks_ssse3(&data, BX, BX, BY, &mut got, out_w) };

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

    /// Both surface loops must be byte-identical to their scalar block
    /// decoders. BC2 exercises the full 4-bit alpha byte range; BC3 exercises
    /// both alpha-palette branches (`a0 > a1` and its transparent-black twin).
    #[test]
    fn bc23_blocks_ssse3_match_scalar() {
        if !has_pshufb() {
            return;
        }
        let mut state = 0x0bad_c0de_1234_5678u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        const BX: usize = 2;
        const BY: usize = 2;
        const N: usize = BX * BY * 16;
        for case in 0..20_000u32 {
            let mut data = [0u8; N];
            match case {
                0 => {}
                1 => data.iter_mut().for_each(|x| *x = 0xff),
                // a0 <= a1 in every block: the six-entry branch with index 6
                // transparent and index 7 opaque.
                2 => {
                    for b in 0..BX * BY {
                        data[b * 16] = 3;
                        data[b * 16 + 1] = 200;
                    }
                }
                // c0 == c1: the colour half's degenerate case, which BC2 and BC3
                // must still read as four-colour.
                3 => {
                    for b in 0..BX * BY {
                        data[b * 16 + 8..b * 16 + 12]
                            .copy_from_slice(&[0x34, 0x12, 0x34, 0x12]);
                    }
                }
                _ => {
                    for c in data.chunks_exact_mut(8) {
                        c.copy_from_slice(&next().to_le_bytes());
                    }
                }
            }
            let out_w = BX * 4;
            let pitch = out_w * 4;
            let len = out_w * BY * 4 * 4;
            for which in 0..2 {
                let mut got = vec![0u8; len];
                unsafe {
                    if which == 0 {
                        bc2_blocks_ssse3(&data, BX, BX, BY, &mut got, out_w)
                    } else {
                        bc3_blocks_ssse3(&data, BX, BX, BY, &mut got, out_w)
                    }
                }
                let mut want = vec![0u8; len];
                for by in 0..BY {
                    for bx in 0..BX {
                        let bi = (by * BX + bx) * 16;
                        let o = (by * 4 * out_w + bx * 4) * 4;
                        if which == 0 {
                            super::super::bcn::bc2_block_rgba_for_test(
                                &data[bi..bi + 16],
                                &mut want[o..],
                                pitch,
                            );
                        } else {
                            super::super::bcn::bc3_block_rgba_for_test(
                                &data[bi..bi + 16],
                                &mut want[o..],
                                pitch,
                            );
                        }
                    }
                }
                assert_eq!(got, want, "case {case}, {}", if which == 0 { "bc2" } else { "bc3" });
            }
        }
    }

    /// The BC4 and BC5 surface loops must be byte-identical to the scalar block
    /// decoders, for both signedness conventions — the signed palette takes the
    /// other `unquantize` branch and its endpoints are `-127..=127`.
    #[test]
    fn bc45_blocks_ssse3_match_scalar() {
        if !has_ssse3() {
            return;
        }
        let mut state = 0x45_45_c0ffee_1234u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        const BX: usize = 2;
        const BY: usize = 2;
        for case in 0..20_000u32 {
            for (bb, is_bc5) in [(8usize, false), (16usize, true)] {
                let mut data = vec![0u8; BX * BY * bb];
                match case {
                    0 => {}
                    1 => data.iter_mut().for_each(|x| *x = 0xff),
                    // a0 <= a1 in every sub-block: the six-entry branch, where
                    // index 6 is the low bound and index 7 the high one.
                    2 => {
                        for b in 0..BX * BY {
                            data[b * bb] = 3;
                            data[b * bb + 1] = 200;
                            if is_bc5 {
                                data[b * bb + 8] = 5;
                                data[b * bb + 9] = 180;
                            }
                        }
                    }
                    _ => {
                        for c in data.chunks_exact_mut(8) {
                            c.copy_from_slice(&next().to_le_bytes());
                        }
                    }
                }
                for is_signed in [false, true] {
                    let out_w = BX * 4;
                    let pitch = out_w * 4;
                    let len = out_w * BY * 4 * 4;
                    let mut got = vec![0u8; len];
                    unsafe {
                        if is_bc5 {
                            bc5_blocks_ssse3(&data, BX, BX, BY, &mut got, out_w, is_signed)
                        } else {
                            bc4_blocks_ssse3(&data, BX, BX, BY, &mut got, out_w, is_signed)
                        }
                    }
                    let mut want = vec![0u8; len];
                    for by in 0..BY {
                        for bx in 0..BX {
                            let bi = (by * BX + bx) * bb;
                            let o = (by * 4 * out_w + bx * 4) * 4;
                            if is_bc5 {
                                super::super::bcn::bc5_block_rgba_for_test(
                                    &data[bi..bi + bb],
                                    &mut want[o..],
                                    pitch,
                                    is_signed,
                                );
                            } else {
                                super::super::bcn::bc4_block_rgba_for_test(
                                    &data[bi..bi + bb],
                                    &mut want[o..],
                                    pitch,
                                    is_signed,
                                );
                            }
                        }
                    }
                    assert_eq!(
                        got, want,
                        "case {case}, {} signed={is_signed}",
                        if is_bc5 { "bc5" } else { "bc4" }
                    );
                }
            }
        }
    }

    /// The fused convert-and-widen must be bit-identical to the two-pass path it
    /// replaces, **exhaustively over the domain BC6H can actually produce**.
    ///
    /// That domain is `0 ..= 0x7BFF`. `bc6h_mode11_half` emits
    /// `((v * 31) >> 6) as u16` for a non-negative `v <= 0xFFFF`, so the result
    /// is at most 31 743 — the largest *finite* half — and never negative. The
    /// encoder clamps to the same `0x7BFF`. BC6H therefore never produces a NaN,
    /// an infinity, or a negative half, and the exponent field is never all-ones.
    ///
    /// This matters: over the FULL `u16` range the in-house scalar
    /// [`super::super::bc6h::half_to_f32`] and hardware `vcvtph2ps` disagree on
    /// NaN payloads, which is why an earlier version of this test failed. That
    /// disagreement is real but unreachable, and testing it would have gated a
    /// correct kernel on values the codec cannot emit.
    ///
    /// 31 744 values is small enough to sweep completely, so this is exhaustive
    /// rather than sampled — and because the uniform sweep puts every value
    /// through every one of the 48 lane positions, it also covers lane
    /// placement. A randomised in-domain pass then mixes distinct values across
    /// lanes to catch a transpose that only shows with unequal channels.
    ///
    /// It doubles as the first oracle [`half48_to_f32`] has ever had: proving
    /// `vcvtph2ps` equals the scalar converter across the reachable domain
    /// covers the two-pass path too.
    #[test]
    fn bc6h_planar_to_rgba_matches_two_pass() {
        if !has_f16c() {
            return;
        }
        const PITCH: usize = 40;
        const MAX: u32 = 0x7BFF; // largest half BC6H can emit

        let check = |src: &[u16; 48], label: &str| {
            let mut got = vec![0f32; 3 * PITCH + 16];
            unsafe { assert!(bc6h_planar_to_rgba(src, got.as_mut_ptr(), PITCH)) };
            let mut want = vec![0f32; 3 * PITCH + 16];
            for p in 0..16usize {
                let o = (p / 4) * PITCH + (p % 4) * 4;
                want[o] = super::super::bc6h::half_to_f32(src[p]);
                want[o + 1] = super::super::bc6h::half_to_f32(src[16 + p]);
                want[o + 2] = super::super::bc6h::half_to_f32(src[32 + p]);
                want[o + 3] = 1.0;
            }
            for (i, (a, b)) in got.iter().zip(want.iter()).enumerate() {
                assert_eq!(a.to_bits(), b.to_bits(), "{label}, element {i}");
            }
        };

        // Exhaustive over the reachable domain, every value in every lane.
        for v in 0..=MAX {
            check(&[v as u16; 48], &format!("uniform {v:#06x}"));
        }

        // Distinct values across lanes, to catch a transpose the uniform sweep
        // cannot see.
        let mut state = 0x6b6b_f00d_1234_5678u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..20_000u32 {
            let mut src = [0u16; 48];
            match case {
                // Every lane a different value, in order: pins the transpose.
                0 => {
                    for (i, v) in src.iter_mut().enumerate() {
                        *v = (i as u16 + 1) * 97;
                    }
                }
                1 => src = [MAX as u16; 48],
                _ => {
                    for v in src.iter_mut() {
                        *v = (next() % (MAX as u64 + 1)) as u16;
                    }
                }
            }
            check(&src, &format!("case {case}"));
        }
    }
}
