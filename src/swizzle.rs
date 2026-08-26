//! R↔B channel swap for 32-bit pixels (BGRA8 ↔ RGBA8), vectorised.
//!
//! The per-pixel permutation `[2,1,0,3]` is self-inverse, so ONE kernel serves
//! both the decoder's BGRA8→RGBA8 path and the encoder's RGBA8→BGRA8
//! passthrough arm. Gated like `simd_tables`: outside both feature trees,
//! because `decode` and `encode` are independently optional and both sides
//! call it.
//!
//! `pshufb` is SSSE3 — above the x86-64 compile baseline (SSE2) — so the
//! kernel is runtime-detected, with the scalar loop at each call site kept as
//! both the fallback and the oracle. The dispatch is hoisted: one feature
//! check per surface, and the whole loop lives inside the `#[target_feature]`
//! function. The alternative shape — a per-pixel-group call into the kernel —
//! is the one that measured 47.8% SLOWER on BC1 decode (see `decode::simd`),
//! because a `#[target_feature]` function cannot inline into a caller without
//! the feature.

/// Copy `src` into `dst`, swapping bytes 0 and 2 of every 4-byte pixel.
///
/// Returns `false` — having written nothing — when SSSE3 is unavailable or
/// the slices are not equal-length whole pixels; the caller then runs its
/// scalar loop. Both shipping callers pass `width * height * 4` bytes on each
/// side, so the strict check is a debug-only concern, not a hot-path branch.
pub(crate) fn swap_rb(src: &[u8], dst: &mut [u8]) -> bool {
    if src.len() != dst.len() || src.len() % 4 != 0 || !has_ssse3() {
        return false;
    }
    // SAFETY: SSSE3 confirmed by the runtime check above; equal lengths and
    // whole-pixel size checked above, and the kernel touches only bytes
    // `0..src.len()` of each slice.
    unsafe { swap_rb_ssse3(src, dst) };
    true
}

/// Four pixels per `pshufb`; sub-16-byte remainder handled per pixel.
///
/// # Safety
///
/// Caller guarantees SSSE3 and `src.len() == dst.len()` with
/// `src.len() % 4 == 0`.
#[target_feature(enable = "ssse3")]
unsafe fn swap_rb_ssse3(src: &[u8], dst: &mut [u8]) {
    use std::arch::x86_64::*;
    let n = src.len();
    let sp = src.as_ptr();
    let dp = dst.as_mut_ptr();
    let sel = _mm_setr_epi8(2, 1, 0, 3, 6, 5, 4, 7, 10, 9, 8, 11, 14, 13, 12, 15);
    let mut i = 0usize;
    while i + 16 <= n {
        let v = _mm_loadu_si128(sp.add(i) as *const __m128i);
        _mm_storeu_si128(dp.add(i) as *mut __m128i, _mm_shuffle_epi8(v, sel));
        i += 16;
    }
    while i < n {
        // Fewer than four whole pixels left.
        let p = [*sp.add(i + 2), *sp.add(i + 1), *sp.add(i), *sp.add(i + 3)];
        std::ptr::copy_nonoverlapping(p.as_ptr(), dp.add(i), 4);
        i += 4;
    }
}

/// Cached SSSE3 detection, one relaxed byte — same shape and reasoning as
/// `encode::blocks::simd::has_avx2`. `pub(crate)`: the mip box filter's
/// kernel (`encode::mips`) shares the probe.
#[inline]
pub(crate) fn has_ssse3() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    // 0 = not yet probed, 1 = absent, 2 = present.
    static F: AtomicU8 = AtomicU8::new(0);
    match F.load(Ordering::Relaxed) {
        0 => {
            let v = if std::is_x86_feature_detected!("ssse3") { 2 } else { 1 };
            F.store(v, Ordering::Relaxed);
            v == 2
        }
        v => v == 2,
    }
}

#[cfg(test)]
mod tests {
    /// The scalar form both call sites keep as their fallback.
    fn swap_rb_scalar(src: &[u8], dst: &mut [u8]) {
        for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
            d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
        }
    }

    /// Byte-identical against the scalar loop across random contents and every
    /// length residue the tail loop can see (0..4 whole pixels beyond a
    /// 16-byte boundary), plus large buffers.
    #[test]
    fn swap_rb_matches_scalar() {
        if !super::has_ssse3() {
            eprintln!("SSSE3 not available; skipping");
            return;
        }
        let mut state = 0x5111_22ab_c0ff_ee01u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..60_000u32 {
            // 0..=67 pixels: covers empty, every %4 pixel-tail residue, and
            // multi-vector bodies.
            let px = (next() % 68) as usize;
            let mut src = vec![0u8; px * 4];
            for b in src.iter_mut() {
                *b = next() as u8;
            }
            let mut fast = vec![0u8; px * 4];
            let mut slow = vec![0u8; px * 4];
            assert!(super::swap_rb(&src, &mut fast), "dispatch declined, case {case}");
            swap_rb_scalar(&src, &mut slow);
            assert_eq!(fast, slow, "case {case}, {px} px");
        }
        // A surface-sized buffer, once: 512x512.
        let mut state2 = 0x9e37_79b9_7f4a_7c15u64;
        let mut src = vec![0u8; 512 * 512 * 4];
        for b in src.iter_mut() {
            state2 ^= state2 << 13;
            state2 ^= state2 >> 7;
            state2 ^= state2 << 17;
            *b = state2 as u8;
        }
        let mut fast = vec![0u8; src.len()];
        let mut slow = vec![0u8; src.len()];
        assert!(super::swap_rb(&src, &mut fast));
        swap_rb_scalar(&src, &mut slow);
        assert_eq!(fast, slow);
        // Self-inverse: swapping twice is the identity.
        let mut twice = vec![0u8; src.len()];
        assert!(super::swap_rb(&fast, &mut twice));
        assert_eq!(twice, src);
    }

    /// Best-of-N micro A/B of exactly the changed code, scalar arm against the
    /// kernel on a 1024x1024 surface. Run with:
    /// `cargo test --release swizzle -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn probe_swap_rb_ab() {
        assert!(super::has_ssse3(), "SSSE3 required for the A/B");
        const PX: usize = 1024 * 1024;
        let mut state = 0xdead_beef_cafe_f00du64;
        let mut src = vec![0u8; PX * 4];
        for b in src.iter_mut() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *b = state as u8;
        }
        let mut out = vec![0u8; PX * 4];
        let best = |f: &mut dyn FnMut() -> u64| {
            let mut best = u64::MAX;
            for _ in 0..31 {
                let t = std::time::Instant::now();
                let sink = f();
                let dt = t.elapsed().as_nanos() as u64;
                assert_ne!(sink, 0); // keep the work observable
                best = best.min(dt);
            }
            best
        };
        let scalar_ns = best(&mut || {
            swap_rb_scalar(&src, &mut out);
            out[123] as u64 + 1
        });
        let simd_ns = best(&mut || {
            assert!(super::swap_rb(&src, &mut out));
            out[123] as u64 + 1
        });
        eprintln!(
            "swap_rb 1024x1024: scalar {:.3} ns/px, pshufb {:.3} ns/px, ratio {:.2}x",
            scalar_ns as f64 / PX as f64,
            simd_ns as f64 / PX as f64,
            scalar_ns as f64 / simd_ns as f64,
        );
    }
}
