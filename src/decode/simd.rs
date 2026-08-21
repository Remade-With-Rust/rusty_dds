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
    __m128i, _mm_add_epi16, _mm_and_si128, _mm_cvtsi64_si128, _mm_loadl_epi64,
    _mm_or_si128, _mm_set1_epi32, _mm_set_epi32, _mm_unpacklo_epi64, _mm_loadu_si128, _mm_mullo_epi16, _mm_packus_epi16,
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
    unsafe { bc5_gather_ssse3(pr, pg, ir, ig, out.as_mut_ptr(), pitch) }
    true
}

#[target_feature(enable = "ssse3,bmi2")]
unsafe fn bc5_gather_ssse3(
    pr: u64,
    pg: u64,
    ir: u64,
    ig: u64,
    dst: *mut u8,
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

pub(crate) static BC1_SEL: [[u8; 16]; 256] = build_bc1_sel();

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
    blocks_x: usize,
    blocks_y: usize,
    out: &mut [u8],
    out_w: usize,
) {
    let pitch = out_w * 4;
    let src = data.as_ptr();
    let dst = out.as_mut_ptr();
    let keep = _mm_set1_epi32(RGB_MASK);
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let bi = (by * blocks_x + bx) * 16;
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
    blocks_x: usize,
    blocks_y: usize,
    out: &mut [u8],
    out_w: usize,
) {
    let pitch = out_w * 4;
    let src = data.as_ptr();
    let dst = out.as_mut_ptr();
    let keep = _mm_set1_epi32(RGB_MASK);
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let bi = (by * blocks_x + bx) * 16;
            let blk = core::slice::from_raw_parts(src.add(bi), 16);
            let pal = super::bcn::bc1_palette(&blk[8..16], true);
            let p = _mm_set_epi32(pal[3] as i32, pal[2] as i32, pal[1] as i32, pal[0] as i32);
            let cidx = u32::from_le_bytes([blk[12], blk[13], blk[14], blk[15]]);
            // One `movq` from a GPR, not a spilled array.
            let apal = _mm_cvtsi64_si128(super::bcn::bc3_alpha_palette_packed(blk[0], blk[1]) as i64);
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
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| std::arch::is_x86_feature_detected!("avx2"))
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
/// the block loop pays a real call plus a `OnceLock` check on **every 4x4
/// block**. 0.3.28 measured that boundary at 26.7% of BC1 decode. BC4 and BC5
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
#[target_feature(enable = "ssse3,bmi2")]
pub(super) unsafe fn bc5_blocks_ssse3(
    data: &[u8],
    blocks_x: usize,
    blocks_y: usize,
    out: &mut [u8],
    out_w: usize,
    is_signed: bool,
) {
    let pitch = out_w * 4;
    let src = data.as_ptr();
    let dst = out.as_mut_ptr();
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let bi = (by * blocks_x + bx) * 16;
            let blk = core::slice::from_raw_parts(src.add(bi), 16);
            let pr = super::bcn::bc4_palette_packed(blk[0], blk[1], is_signed);
            let pg = super::bcn::bc4_palette_packed(blk[8], blk[9], is_signed);
            let ir = super::bcn::bc4_indices(&blk[..8]);
            let ig = super::bcn::bc4_indices(&blk[8..16]);
            let o = (by * 4 * out_w + bx * 4) * 4;
            bc5_gather_ssse3(pr, pg, ir, ig, dst.add(o), pitch);
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
#[target_feature(enable = "ssse3,bmi2")]
pub(super) unsafe fn bc4_blocks_ssse3(
    data: &[u8],
    blocks_x: usize,
    blocks_y: usize,
    out: &mut [u8],
    out_w: usize,
    is_signed: bool,
) {
    let pitch = out_w * 4;
    let src = data.as_ptr();
    let dst = out.as_mut_ptr();
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let bi = (by * blocks_x + bx) * 8;
            let blk = core::slice::from_raw_parts(src.add(bi), 8);
            let pr = super::bcn::bc4_palette_packed(blk[0], blk[1], is_signed);
            let ir = super::bcn::bc4_indices(blk);
            let o = (by * 4 * out_w + bx * 4) * 4;
            bc5_gather_ssse3(pr, 0, ir, 0, dst.add(o), pitch);
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
                        bc2_blocks_ssse3(&data, BX, BY, &mut got, out_w)
                    } else {
                        bc3_blocks_ssse3(&data, BX, BY, &mut got, out_w)
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
                            bc5_blocks_ssse3(&data, BX, BY, &mut got, out_w, is_signed)
                        } else {
                            bc4_blocks_ssse3(&data, BX, BY, &mut got, out_w, is_signed)
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
