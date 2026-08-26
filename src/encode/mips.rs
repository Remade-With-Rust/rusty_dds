//! Box-filter mip generation for encode.

use crate::error::Error;

/// Allocating form, kept for the oracles: the shipping driver recycles
/// buffers through [`downsample_rgba8_into`] and no production path allocates
/// per level any more.
#[cfg(test)]
pub fn downsample_rgba8(
    src: &[u8],
    width: u32,
    height: u32,
    depth: u32,
) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    downsample_rgba8_into(src, width, height, depth, &mut out)?;
    Ok(out)
}

/// [`downsample_rgba8`] into a caller-recycled buffer.
///
/// The mip chain calls this once per level per physical slice; with a fresh
/// `Vec` per level the allocation and its page faults were a fixed tax on
/// every level (level sizes only shrink, so a recycled buffer never
/// reallocates after the first level — the `resize` memset lands on warm
/// pages and the fault tax disappears).
pub fn downsample_rgba8_into(
    src: &[u8],
    width: u32,
    height: u32,
    depth: u32,
    out_buf: &mut Vec<u8>,
) -> Result<(), Error> {
    let dw = (width / 2).max(1);
    let dh = (height / 2).max(1);
    let dd = (depth / 2).max(1);
    // Every output byte is written by both paths below, so zeroing here is a
    // discarded pass. A recycled buffer shrinks level over level, so the
    // steady state is a pure `truncate` — no memset, no allocation; only a
    // first-use (or grown) buffer pays the zeroing `resize` once.
    let n = (dw * dh * dd * 4) as usize;
    if out_buf.len() >= n {
        out_buf.truncate(n);
    } else {
        out_buf.resize(n, 0);
    }
    let out: &mut [u8] = out_buf;
    let sw = width as usize;
    let sh = height as usize;
    let sd = depth as usize;

    // Any 2D level with both dimensions >= 2 — even OR odd — has no clamped
    // taps anywhere: a second tap exists in both axes for every output pixel
    // (for odd dimensions the last source column/row is simply never a second
    // tap), so every output pixel is exactly `(a + b + c + d + 2) >> 2` over a
    // full 2x2 quad. That whole case is one `pshufb` + two `pmaddubsw` per two
    // output pixels, bit-identical to the scalar arithmetic below (pair sums
    // cap at 510 and the quad sum at 1022, well inside i16 — no saturation is
    // reachable). The gate originally also demanded EVEN dimensions, which was
    // over-strict — it sent every NPOT level down the scalar path for no
    // arithmetic reason. Volumes, 1-wide/1-tall levels and non-SSSE3 CPUs take
    // the scalar loops below, which stay as the kernel's oracle.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if sd == 1
        && sw >= 2
        && sh >= 2
        && src.len() >= sw * sh * 4
        && crate::swizzle::has_ssse3()
    {
        let dh_us = sh / 2;
        // REFUTED (2026-08-26): splitting this kernel's output rows across a
        // thread scope for large levels measured 0.38x — 2.6x SLOWER (2048²:
        // serial 1.110, banded 2.937 ns/out-px, best-of-15). The kernel is
        // memory-bound at ~18 GB/s single-thread, so extra threads buy
        // bandwidth contention plus spawn cost. Same law as the rs_h264 SAD
        // record: a memory-bound kernel does not scale with added compute.
        // (The row-range signature the attempt introduced is kept — neutral.)
        if super::blocks::simd_avx2() {
            // SAFETY: bounds as below; AVX2 confirmed by the runtime check.
            unsafe { downsample_2d_rows_avx2(src, sw, 0, dh_us, out) };
            return Ok(());
        }
        // SAFETY: SSSE3 confirmed; `src` covers `sw * sh * 4` bytes (checked
        // above), `out` covers `(sw/2) * (sh/2) * 4` bytes by construction,
        // and the kernel's index arithmetic stays inside both (see its
        // `# Safety` contract).
        unsafe { downsample_2d_rows_ssse3(src, sw, 0, dh_us, out) };
        return Ok(());
    }

    // 2D with no degenerate axis: the clamps in the general loop below can
    // NEVER fire when both dimensions are >= 2 (for odd dimensions the last
    // source column/row is simply never a second tap), so every quad is a full
    // 2x2 and the rounding divide is the constant `(s + 2) >> 2`. Stated as
    // its own branch-free loop, LLVM can vectorize the body — the general
    // loop's runtime-clamped trip counts are exactly what blocked it.
    if sd == 1 && sw >= 2 && sh >= 2 {
        for y in 0..dh as usize {
            let r0 = (y * 2) * sw * 4;
            let r1 = r0 + sw * 4;
            let orow = y * dw as usize * 4;
            for x in 0..dw as usize {
                let i0 = r0 + x * 8;
                let i1 = r1 + x * 8;
                // SWAR: each row's two pixels as one u64; split even/odd bytes
                // into u16 lanes (R,B in `lo`, G,A in `hi`), row-sum, then
                // fold the cross-pixel halves. Row sums cap at 510 and totals
                // at 1020 — no lane can carry. Bit-exact with the byte form.
                let a = u64::from_le_bytes(src[i0..i0 + 8].try_into().unwrap());
                let b = u64::from_le_bytes(src[i1..i1 + 8].try_into().unwrap());
                const M: u64 = 0x00FF_00FF_00FF_00FF;
                let lo = (a & M) + (b & M);
                let hi = ((a >> 8) & M) + ((b >> 8) & M);
                let lo = (lo & 0xFFFF_FFFF) + (lo >> 32);
                let hi = (hi & 0xFFFF_FFFF) + (hi >> 32);
                let r = (((lo & 0xFFFF) as u32 + 2) >> 2) as u8;
                let bch = ((((lo >> 16) & 0xFFFF) as u32 + 2) >> 2) as u8;
                let g = (((hi & 0xFFFF) as u32 + 2) >> 2) as u8;
                let al = ((((hi >> 16) & 0xFFFF) as u32 + 2) >> 2) as u8;
                let o = orow + x * 4;
                let w32 = u32::from_le_bytes([r, g, bch, al]);
                out[o..o + 4].copy_from_slice(&w32.to_le_bytes());
            }
        }
        return Ok(());
    }

    // Exactly one degenerate axis in 2D — a 1-wide or 1-tall level, the chain
    // tail of every non-square texture: source pixels are contiguous either
    // way, every output pixel averages exactly two neighbours, and byte-wise
    // `(a + b + 1) >> 1` is the carry-safe identity
    // `(a | b) - (((a ^ b) >> 1) & 0x7F...)` — no clamps, no division, no
    // per-channel loop, and portable (no SIMD gate needed).
    if sd == 1 && (sw == 1) != (sh == 1) {
        let l = sw.max(sh); // source pixels; >= 2 because only one axis is 1
        let dl = l / 2;
        for i in 0..dl {
            let s8 = u64::from_le_bytes(src[i * 8..i * 8 + 8].try_into().unwrap());
            let a = (s8 & 0xFFFF_FFFF) as u32;
            let b = (s8 >> 32) as u32;
            let w32 = (a | b) - (((a ^ b) >> 1) & 0x7F7F_7F7F);
            out[i * 4..i * 4 + 4].copy_from_slice(&w32.to_le_bytes());
        }
        return Ok(());
    }

    // Volumes with no degenerate axis: the same clamp analysis holds in all
    // three axes (depth >= 2 means every output slice has both z taps), so
    // every output pixel is a full 2x2x2 — exactly `(sum8 + 4) >> 3`.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if sd >= 2
        && sw >= 2
        && sh >= 2
        && src.len() >= sw * sh * sd * 4
        && crate::swizzle::has_ssse3()
    {
        // SAFETY: SSSE3 confirmed; `src` covers the whole volume (checked
        // above) and `out` covers `dw * dh * dd * 4` bytes by construction.
        unsafe { downsample_3d_ssse3(src, sw, sh, sd, out) };
        return Ok(());
    }

    for z in 0..dd as usize {
        for y in 0..dh as usize {
            for x in 0..dw as usize {
                let x0 = (x * 2).min(sw - 1);
                let y0 = (y * 2).min(sh - 1);
                let z0 = (z * 2).min(sd - 1);
                let x1 = (x0 + 1).min(sw - 1);
                let y1 = (y0 + 1).min(sh - 1);
                let z1 = (z0 + 1).min(sd - 1);

                let mut acc = [0u32; 4];
                let mut n = 0u32;
                for zz in z0..=z1 {
                    for yy in y0..=y1 {
                        for xx in x0..=x1 {
                            let i = ((zz * sh + yy) * sw + xx) * 4;
                            for c in 0..4 {
                                acc[c] += src[i + c] as u32;
                            }
                            n += 1;
                        }
                    }
                }
                let o = ((z * dh as usize + y) * dw as usize + x) * 4;
                // `n` is a product of three axis factors that are each 1 or 2,
                // so it is ALWAYS a power of two (1, 2, 4 or 8) — the rounding
                // divide is a shift, which the compiler cannot see because `n`
                // is a runtime accumulator.
                let sh = n.trailing_zeros();
                for c in 0..4 {
                    out[o + c] = ((acc[c] + (n >> 1)) >> sh) as u8;
                }
            }
        }
    }
    Ok(())
}

/// The 2D box filter for any dimensions >= 2: two output pixels per iteration.
///
/// Per pair of output pixels, sixteen bytes (four source pixels) are loaded
/// from each of the two source rows. `pshufb` regroups each row so that the
/// two horizontally-adjacent samples of every channel sit in adjacent bytes,
/// `pmaddubsw` against ones folds those pairs into i16 horizontal sums, the
/// two rows are added, and `(sum + 2) >> 2` lands back in `0..=255` for an
/// exact `packus`.
///
/// The `dw % 2 == 1` remainder pixel of each row is finished scalar, with the
/// identical arithmetic.
///
/// # Safety
///
/// Caller guarantees SSSE3, `sw >= 2 && sh >= 2` (any parity — `dw = sw/2`
/// floors, and for odd dimensions the last source column/row is never read,
/// exactly as the scalar form never taps it), `src` at least `sw * sh * 4`
/// bytes and `out` at least `(sw/2) * (sh/2) * 4` bytes.
#[cfg(all(feature = "simd", target_arch = "x86_64"))]
#[target_feature(enable = "ssse3")]
unsafe fn downsample_2d_rows_ssse3(src: &[u8], sw: usize, y0: usize, y1: usize, out: &mut [u8]) {
    use std::arch::x86_64::*;
    let dw = sw / 2;
    let sp = src.as_ptr();
    let dp = out.as_mut_ptr();
    // Channel-pair regroup: [p0.r,p1.r, p0.g,p1.g, p0.b,p1.b, p0.a,p1.a,
    //                        p2.r,p3.r, p2.g,p3.g, p2.b,p3.b, p2.a,p3.a].
    let sel = _mm_setr_epi8(0, 4, 1, 5, 2, 6, 3, 7, 8, 12, 9, 13, 10, 14, 11, 15);
    let ones = _mm_set1_epi8(1);
    let two = _mm_set1_epi16(2);
    let pairs = dw / 2;
    for y in y0..y1 {
        let r0 = sp.add((y * 2) * sw * 4);
        let r1 = sp.add((y * 2 + 1) * sw * 4);
        let orow = dp.add((y - y0) * dw * 4);
        for p in 0..pairs {
            let a = _mm_loadu_si128(r0.add(p * 16) as *const __m128i);
            let b = _mm_loadu_si128(r1.add(p * 16) as *const __m128i);
            let ha = _mm_maddubs_epi16(_mm_shuffle_epi8(a, sel), ones);
            let hb = _mm_maddubs_epi16(_mm_shuffle_epi8(b, sel), ones);
            let s = _mm_srli_epi16(_mm_add_epi16(_mm_add_epi16(ha, hb), two), 2);
            let packed = _mm_packus_epi16(s, s);
            _mm_storel_epi64(orow.add(p * 8) as *mut __m128i, packed);
        }
        if dw % 2 == 1 {
            // One output pixel left on this row: the last 2x2 quad.
            let x0 = (dw - 1) * 2;
            let i0 = x0 * 4;
            let i1 = i0 + 4;
            for c in 0..4 {
                let sum = *r0.add(i0 + c) as u32
                    + *r0.add(i1 + c) as u32
                    + *r1.add(i0 + c) as u32
                    + *r1.add(i1 + c) as u32;
                *orow.add((dw - 1) * 4 + c) = ((sum + 2) >> 2) as u8;
            }
        }
    }
}

/// AVX2 twin of [`downsample_2d_rows_ssse3`]: FOUR output pixels per
/// iteration. Identical lane arithmetic at double width — `vpshufb`,
/// `vpmaddubsw` and `vpackuswb` all operate within 128-bit lanes, so each
/// half computes exactly what the SSSE3 form computes and one cross-lane
/// permute at the end restores output order. Byte-identical by construction
/// and by the `avx2_rows_match_ssse3_rows` oracle.
///
/// # Safety
///
/// As [`downsample_2d_rows_ssse3`], plus AVX2 confirmed by the caller.
#[cfg(all(feature = "simd", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn downsample_2d_rows_avx2(src: &[u8], sw: usize, y0: usize, y1: usize, out: &mut [u8]) {
    use std::arch::x86_64::*;
    let dw = sw / 2;
    let sp = src.as_ptr();
    let dp = out.as_mut_ptr();
    let sel128 = _mm_setr_epi8(0, 4, 1, 5, 2, 6, 3, 7, 8, 12, 9, 13, 10, 14, 11, 15);
    let sel = _mm256_broadcastsi128_si256(sel128);
    let ones = _mm256_set1_epi8(1);
    let two = _mm256_set1_epi16(2);
    let ones128 = _mm_set1_epi8(1);
    let two128 = _mm_set1_epi16(2);
    let quads = dw / 4;
    for y in y0..y1 {
        let r0 = sp.add((y * 2) * sw * 4);
        let r1 = sp.add((y * 2 + 1) * sw * 4);
        let orow = dp.add((y - y0) * dw * 4);
        for q in 0..quads {
            let a = _mm256_loadu_si256(r0.add(q * 32) as *const __m256i);
            let b = _mm256_loadu_si256(r1.add(q * 32) as *const __m256i);
            let ha = _mm256_maddubs_epi16(_mm256_shuffle_epi8(a, sel), ones);
            let hb = _mm256_maddubs_epi16(_mm256_shuffle_epi8(b, sel), ones);
            let s = _mm256_srli_epi16(_mm256_add_epi16(_mm256_add_epi16(ha, hb), two), 2);
            let packed = _mm256_packus_epi16(s, s);
            // Qword 0 = out px 0..1, qword 2 = out px 2..3; 0b0000_1000
            // gathers them into the low 128 bits in order.
            let ordered = _mm256_permute4x64_epi64::<0b0000_1000>(packed);
            _mm_storeu_si128(
                orow.add(q * 16) as *mut __m128i,
                _mm256_castsi256_si128(ordered),
            );
        }
        // 0..=3 output pixels remain: one SSSE3-shaped pair, then the odd
        // column, exactly as the narrow kernel.
        let rem = dw - quads * 4;
        if rem >= 2 {
            let p = quads * 2;
            let a = _mm_loadu_si128(r0.add(p * 16) as *const __m128i);
            let b = _mm_loadu_si128(r1.add(p * 16) as *const __m128i);
            let ha = _mm_maddubs_epi16(_mm_shuffle_epi8(a, sel128), ones128);
            let hb = _mm_maddubs_epi16(_mm_shuffle_epi8(b, sel128), ones128);
            let s = _mm_srli_epi16(_mm_add_epi16(_mm_add_epi16(ha, hb), two128), 2);
            let packed = _mm_packus_epi16(s, s);
            _mm_storel_epi64(orow.add(p * 8) as *mut __m128i, packed);
        }
        if dw % 2 == 1 {
            let i0 = (dw - 1) * 2 * 4;
            let i1 = i0 + 4;
            for c in 0..4 {
                let sum = *r0.add(i0 + c) as u32
                    + *r0.add(i1 + c) as u32
                    + *r1.add(i0 + c) as u32
                    + *r1.add(i1 + c) as u32;
                *orow.add((dw - 1) * 4 + c) = ((sum + 2) >> 2) as u8;
            }
        }
    }
}

/// The 3D box filter for volumes with all dimensions >= 2: two output pixels
/// per iteration, eight taps summed in i16 lanes (row sums cap at 510, the
/// eight-tap total at 2040 — no saturation), `(sum + 4) >> 3` exact.
///
/// # Safety
///
/// Caller guarantees SSSE3, all of `sw`, `sh`, `sd` >= 2 (any parity — for
/// odd dimensions the last source column/row/slice is never a tap, exactly as
/// the scalar form never reads it), `src` at least `sw * sh * sd * 4` bytes
/// and `out` at least `(sw/2) * (sh/2) * (sd/2) * 4` bytes.
#[cfg(all(feature = "simd", target_arch = "x86_64"))]
#[target_feature(enable = "ssse3")]
unsafe fn downsample_3d_ssse3(src: &[u8], sw: usize, sh: usize, sd: usize, out: &mut [u8]) {
    use std::arch::x86_64::*;
    let dw = sw / 2;
    let dh = sh / 2;
    let dd = sd / 2;
    let sp = src.as_ptr();
    let dp = out.as_mut_ptr();
    let sel = _mm_setr_epi8(0, 4, 1, 5, 2, 6, 3, 7, 8, 12, 9, 13, 10, 14, 11, 15);
    let ones = _mm_set1_epi8(1);
    let four = _mm_set1_epi16(4);
    let slice = sw * sh * 4;
    let pairs = dw / 2;
    for z in 0..dd {
        let sa = sp.add((z * 2) * slice);
        let sb = sp.add((z * 2 + 1) * slice);
        let dz = dp.add(z * dh * dw * 4);
        for y in 0..dh {
            let a0 = sa.add((y * 2) * sw * 4);
            let a1 = sa.add((y * 2 + 1) * sw * 4);
            let b0 = sb.add((y * 2) * sw * 4);
            let b1 = sb.add((y * 2 + 1) * sw * 4);
            let orow = dz.add(y * dw * 4);
            for p in 0..pairs {
                let m = |r: *const u8| {
                    _mm_maddubs_epi16(
                        _mm_shuffle_epi8(
                            _mm_loadu_si128(r.add(p * 16) as *const __m128i),
                            sel,
                        ),
                        ones,
                    )
                };
                let s = _mm_add_epi16(_mm_add_epi16(m(a0), m(a1)), _mm_add_epi16(m(b0), m(b1)));
                let s = _mm_srli_epi16(_mm_add_epi16(s, four), 3);
                let packed = _mm_packus_epi16(s, s);
                _mm_storel_epi64(orow.add(p * 8) as *mut __m128i, packed);
            }
            if dw % 2 == 1 {
                // One output pixel left on this row: the last 2x2x2 block.
                let i0 = (dw - 1) * 2 * 4;
                let i1 = i0 + 4;
                for c in 0..4 {
                    let sum = *a0.add(i0 + c) as u32
                        + *a0.add(i1 + c) as u32
                        + *a1.add(i0 + c) as u32
                        + *a1.add(i1 + c) as u32
                        + *b0.add(i0 + c) as u32
                        + *b0.add(i1 + c) as u32
                        + *b1.add(i0 + c) as u32
                        + *b1.add(i1 + c) as u32;
                    *orow.add((dw - 1) * 4 + c) = ((sum + 4) >> 3) as u8;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    /// The pre-kernel scalar form, kept verbatim as the reference.
    fn downsample_reference(src: &[u8], width: u32, height: u32, depth: u32) -> Vec<u8> {
        let dw = (width / 2).max(1) as usize;
        let dh = (height / 2).max(1) as usize;
        let dd = (depth / 2).max(1) as usize;
        let (sw, sh, sd) = (width as usize, height as usize, depth as usize);
        let mut out = vec![0u8; dw * dh * dd * 4];
        for z in 0..dd {
            for y in 0..dh {
                for x in 0..dw {
                    let x0 = (x * 2).min(sw - 1);
                    let y0 = (y * 2).min(sh - 1);
                    let z0 = (z * 2).min(sd - 1);
                    let x1 = (x0 + 1).min(sw - 1);
                    let y1 = (y0 + 1).min(sh - 1);
                    let z1 = (z0 + 1).min(sd - 1);
                    let mut acc = [0u32; 4];
                    let mut n = 0u32;
                    for zz in z0..=z1 {
                        for yy in y0..=y1 {
                            for xx in x0..=x1 {
                                let i = ((zz * sh + yy) * sw + xx) * 4;
                                for c in 0..4 {
                                    acc[c] += src[i + c] as u32;
                                }
                                n += 1;
                            }
                        }
                    }
                    let o = ((z * dh + y) * dw + x) * 4;
                    for c in 0..4 {
                        out[o + c] = ((acc[c] + n / 2) / n) as u8;
                    }
                }
            }
        }
        out
    }

    /// Byte-identical against the reference across every dimension-parity
    /// shape: even/odd widths and heights (kernel and scalar paths), volumes,
    /// and the degenerate 1-pixel axes.
    #[test]
    fn downsample_matches_reference() {
        let mut state = 0x5eed_0f_1e_a1_b0c5u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..4_000u32 {
            let (w, h, d) = match case % 8 {
                // Deliberate parity/degeneracy shapes, then random.
                0 => (2, 2, 1),
                1 => (1, 7, 1),
                2 => (16, 1, 1),
                3 => (5, 6, 1),
                4 => (6, 5, 1),
                5 => (4, 4, 4),
                6 => (3, 3, 2),
                _ => (
                    (next() % 40 + 1) as u32,
                    (next() % 40 + 1) as u32,
                    (next() % 3 + 1) as u32,
                ),
            };
            let mut src = vec![0u8; (w * h * d * 4) as usize];
            for b in src.iter_mut() {
                *b = next() as u8;
            }
            let got = super::downsample_rgba8(&src, w, h, d).unwrap();
            let want = downsample_reference(&src, w, h, d);
            assert_eq!(got, want, "case {case}: {w}x{h}x{d}");
        }
        // A case past PARALLEL_MIN_OUT_PX (and odd-width): the banded
        // multi-thread path must byte-match the reference too.
        {
            let (w, h) = (1101u32, 612u32);
            let mut src = vec![0u8; (w * h * 4) as usize];
            let mut s3 = 0x0bad_cafe_1234_5678u64;
            for b in src.iter_mut() {
                s3 ^= s3 << 13;
                s3 ^= s3 >> 7;
                s3 ^= s3 << 17;
                *b = s3 as u8;
            }
            assert_eq!(
                super::downsample_rgba8(&src, w, h, 1).unwrap(),
                downsample_reference(&src, w, h, 1),
                "parallel band path"
            );
        }
        // One surface-sized even case, exercising the kernel's full row loop.
        let (w, h) = (512u32, 384u32);
        let mut src = vec![0u8; (w * h * 4) as usize];
        let mut s2 = 0x9e37_79b9_7f4a_7c15u64;
        for b in src.iter_mut() {
            s2 ^= s2 << 13;
            s2 ^= s2 >> 7;
            s2 ^= s2 << 17;
            *b = s2 as u8;
        }
        assert_eq!(
            super::downsample_rgba8(&src, w, h, 1).unwrap(),
            downsample_reference(&src, w, h, 1)
        );
    }

    /// The ping-pong `_into` chain must produce byte-identical levels to the
    /// allocating chain, across parities and volumes.
    #[test]
    fn into_chain_matches_allocating_chain() {
        let mut state = 0xc4a1_9_0f_2b5du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..400u32 {
            let (mut w, mut h, mut d) = (
                (next() % 65 + 1) as u32,
                (next() % 65 + 1) as u32,
                (next() % 4 + 1) as u32,
            );
            let mut src = vec![0u8; (w * h * d * 4) as usize];
            for b in src.iter_mut() {
                *b = next() as u8;
            }
            let mut alloc_cur = src.clone();
            let mut cur_buf: Vec<u8> = Vec::new();
            let mut next_buf: Vec<u8> = Vec::new();
            for level in 0..6 {
                let cur: &[u8] = if level == 0 { &src } else { &cur_buf };
                assert_eq!(alloc_cur, cur, "case {case} level {level}");
                if w == 1 && h == 1 && d == 1 {
                    break;
                }
                alloc_cur = super::downsample_rgba8(&alloc_cur, w, h, d).unwrap();
                super::downsample_rgba8_into(cur, w, h, d, &mut next_buf).unwrap();
                std::mem::swap(&mut cur_buf, &mut next_buf);
                w = (w / 2).max(1);
                h = (h / 2).max(1);
                d = (d / 2).max(1);
            }
        }
    }

    /// Scalar-path A/B — the frozen pre-campaign scalar loop
    /// (`downsample_reference`) against the SHIPPING path on odd dimensions
    /// (1001² → 500², which takes the scalar route even on x86: the kernel
    /// requires even/even). Re-run after each scalar-path win; each run's
    /// ratio is an in-process pair, so dividing successive ratios attributes
    /// the increments while box-speed drift cancels.
    /// Run with: `cargo test --release probe_scalar_path -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn probe_scalar_path_ab() {
        const W: u32 = 1001;
        const H: u32 = 1001;
        let mut state = 0xdead_beef_cafe_f00du64;
        let mut src = vec![0u8; (W * H * 4) as usize];
        for b in src.iter_mut() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *b = state as u8;
        }
        use std::hint::black_box;
        let best = |f: &mut dyn FnMut() -> u64| {
            let mut best = u64::MAX;
            for _ in 0..15 {
                let t = std::time::Instant::now();
                let sink = black_box(f());
                let dt = t.elapsed().as_nanos() as u64;
                assert_ne!(sink, u64::MAX);
                best = best.min(dt);
            }
            best
        };
        let ref_ns = best(&mut || {
            downsample_reference(black_box(&src), W, H, 1)[123] as u64 + 1
        });
        let mut buf = Vec::new();
        let ship_ns = best(&mut || {
            super::downsample_rgba8_into(black_box(&src), W, H, 1, &mut buf).unwrap();
            buf[123] as u64 + 1
        });
        let px = ((W / 2) * (H / 2)) as f64;
        eprintln!(
            "scalar path 1001² -> 500²: frozen reference {:.3} ns/out-px, shipping {:.3} ns/out-px, ratio {:.2}x",
            ref_ns as f64 / px,
            ship_ns as f64 / px,
            ref_ns as f64 / ship_ns as f64,
        );
    }

    /// The AVX2 row kernel must byte-match the SSSE3 row kernel across every
    /// width residue its quad/pair/odd tail structure can see, and random
    /// heights.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    #[test]
    fn avx2_rows_match_ssse3_rows() {
        if !crate::encode::blocks::simd_avx2() || !crate::swizzle::has_ssse3() {
            eprintln!("AVX2/SSSE3 not available; skipping");
            return;
        }
        let mut state = 0xa2b2_c2d2_e2f2_0212u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..4_000u32 {
            // Widths 2..=37 cover every dw % 4 residue; heights 2..=17.
            let sw = (next() % 36 + 2) as usize;
            let sh = (next() % 16 + 2) as usize;
            let mut src = vec![0u8; sw * sh * 4];
            for b in src.iter_mut() {
                *b = next() as u8;
            }
            let (dw, dh) = (sw / 2, sh / 2);
            let mut out_a = vec![0u8; dw * dh * 4];
            let mut out_b = vec![0u8; dw * dh * 4];
            // SAFETY: features checked above; buffers sized exactly.
            unsafe {
                super::downsample_2d_rows_avx2(&src, sw, 0, dh, &mut out_a);
                super::downsample_2d_rows_ssse3(&src, sw, 0, dh, &mut out_b);
            }
            assert_eq!(out_a, out_b, "case {case}: {sw}x{sh}");
        }
    }

    /// W10 A/B — the SSSE3 row kernel against its AVX2 twin, one big level
    /// (2048² → 1024²), in-process.
    /// Run with: `cargo test --release probe_avx2_rows -- --ignored --nocapture`
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    #[test]
    #[ignore]
    fn probe_avx2_rows_ab() {
        assert!(crate::encode::blocks::simd_avx2());
        const W: usize = 2048;
        const H: usize = 2048;
        let mut state = 0xdead_beef_cafe_f00du64;
        let mut src = vec![0u8; W * H * 4];
        for b in src.iter_mut() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *b = state as u8;
        }
        let mut out = vec![0u8; (W / 2) * (H / 2) * 4];
        use std::hint::black_box;
        let best = |f: &mut dyn FnMut() -> u64| {
            let mut best = u64::MAX;
            for _ in 0..31 {
                let t = std::time::Instant::now();
                let sink = black_box(f());
                let dt = t.elapsed().as_nanos() as u64;
                assert_ne!(sink, u64::MAX);
                best = best.min(dt);
            }
            best
        };
        let sse_ns = best(&mut || {
            // SAFETY: SSSE3 implied by AVX2; buffers sized exactly.
            unsafe { super::downsample_2d_rows_ssse3(black_box(&src), W, 0, H / 2, &mut out) };
            out[123] as u64 + 1
        });
        let avx_ns = best(&mut || {
            // SAFETY: AVX2 asserted above; buffers sized exactly.
            unsafe { super::downsample_2d_rows_avx2(black_box(&src), W, 0, H / 2, &mut out) };
            out[123] as u64 + 1
        });
        let px = ((W / 2) * (H / 2)) as f64;
        eprintln!(
            "level 2048² -> 1024²: ssse3 {:.3} ns/out-px, avx2 {:.3} ns/out-px, ratio {:.2}x",
            sse_ns as f64 / px,
            avx_ns as f64 / px,
            sse_ns as f64 / avx_ns as f64,
        );
    }

    /// W9 A/B — the surface-range walk: `surface_mut` per level (each call
    /// re-walks the mip chain, O(levels²) per slice) against ONE
    /// `subresource_chain_ranges` walk, on a 4096² 13-level BC1 chain.
    /// Run with: `cargo test --release probe_chain_walk -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn probe_chain_walk_ab() {
        let dds = crate::Dds::new_dxgi(crate::NewDxgiParams {
            height: 4096,
            width: 4096,
            depth: None,
            format: crate::DxgiFormat::BC1_UNorm,
            mipmap_levels: Some(13),
            array_layers: None,
            caps2: None,
            is_cubemap: false,
            resource_dimension: crate::D3D10ResourceDimension::Texture2D,
            alpha_mode: crate::AlphaMode::Straight,
        })
        .unwrap();
        use std::hint::black_box;
        let best = |f: &mut dyn FnMut() -> u64| {
            let mut best = u64::MAX;
            for _ in 0..31 {
                let t = std::time::Instant::now();
                let sink = black_box(f());
                let dt = t.elapsed().as_nanos() as u64;
                assert_ne!(sink, u64::MAX);
                best = best.min(dt);
            }
            best
        };
        let per_level_ns = best(&mut || {
            let mut sink = 0u64;
            for _ in 0..100 {
                for mip in 0..13u32 {
                    let id = crate::SubresourceId::mip_layer(mip, 0);
                    let r = black_box(&dds).subresource_range(id).unwrap();
                    sink = sink.wrapping_add(r.start as u64);
                }
            }
            sink
        });
        let one_walk_ns = best(&mut || {
            let mut sink = 0u64;
            for _ in 0..100 {
                let ranges = black_box(&dds).subresource_chain_ranges(0, 0).unwrap();
                for r in &ranges {
                    sink = sink.wrapping_add(r.start as u64);
                }
            }
            sink
        });
        eprintln!(
            "chain ranges, 13 levels x100: per-level walks {:.1} ns/chain, one walk {:.1} ns/chain, ratio {:.2}x",
            per_level_ns as f64 / 100.0,
            one_walk_ns as f64 / 100.0,
            per_level_ns as f64 / one_walk_ns as f64,
        );
    }

    /// W-1D A/B — the frozen reference against the shipping degenerate-axis
    /// path (8192×1 → 4096×1, the chain tail of wide non-square textures).
    /// Run with: `cargo test --release probe_1d -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn probe_1d_ab() {
        const L: u32 = 8192;
        let mut state = 0xdead_beef_cafe_f00du64;
        let mut src = vec![0u8; (L * 4) as usize];
        for b in src.iter_mut() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *b = state as u8;
        }
        use std::hint::black_box;
        let best = |f: &mut dyn FnMut() -> u64| {
            let mut best = u64::MAX;
            for _ in 0..31 {
                let t = std::time::Instant::now();
                let sink = black_box(f());
                let dt = t.elapsed().as_nanos() as u64;
                assert_ne!(sink, u64::MAX);
                best = best.min(dt);
            }
            best
        };
        let ref_ns = best(&mut || {
            downsample_reference(black_box(&src), L, 1, 1)[3] as u64 + 1
        });
        let mut buf = Vec::new();
        let ship_ns = best(&mut || {
            super::downsample_rgba8_into(black_box(&src), L, 1, 1, &mut buf).unwrap();
            buf[3] as u64 + 1
        });
        let px = (L / 2) as f64;
        eprintln!(
            "1D tail 8192x1 -> 4096x1: reference {:.3} ns/out-px, shipping {:.3} ns/out-px, ratio {:.2}x",
            ref_ns as f64 / px,
            ship_ns as f64 / px,
            ref_ns as f64 / ship_ns as f64,
        );
    }

    /// W7 A/B — the frozen reference against the shipping VOLUME path
    /// (256×256×8 → 128×128×4, previously all-scalar with live z-clamps).
    /// Run with: `cargo test --release probe_volume -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn probe_volume_ab() {
        const W: u32 = 256;
        const H: u32 = 256;
        const D: u32 = 8;
        let mut state = 0xdead_beef_cafe_f00du64;
        let mut src = vec![0u8; (W * H * D * 4) as usize];
        for b in src.iter_mut() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *b = state as u8;
        }
        use std::hint::black_box;
        let best = |f: &mut dyn FnMut() -> u64| {
            let mut best = u64::MAX;
            for _ in 0..15 {
                let t = std::time::Instant::now();
                let sink = black_box(f());
                let dt = t.elapsed().as_nanos() as u64;
                assert_ne!(sink, u64::MAX);
                best = best.min(dt);
            }
            best
        };
        let ref_ns = best(&mut || {
            downsample_reference(black_box(&src), W, H, D)[123] as u64 + 1
        });
        let mut buf = Vec::new();
        let ship_ns = best(&mut || {
            super::downsample_rgba8_into(black_box(&src), W, H, D, &mut buf).unwrap();
            buf[123] as u64 + 1
        });
        let px = ((W / 2) * (H / 2) * (D / 2)) as f64;
        eprintln!(
            "volume 256x256x8 -> 128x128x4: reference {:.3} ns/out-px, shipping {:.3} ns/out-px, ratio {:.2}x",
            ref_ns as f64 / px,
            ship_ns as f64 / px,
            ref_ns as f64 / ship_ns as f64,
        );
    }

    /// W1 A/B — the removed work itself: per-level `clear + resize(0→n, 0)`
    /// (a full memset of every level) against the shipping `truncate` prep,
    /// over the ten level sizes of a 1024² chain.
    /// Run with: `cargo test --release probe_bufprep -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn probe_bufprep_ab() {
        let sizes: Vec<usize> = (1..=10u32)
            .map(|l| {
                let d = 1usize << l;
                ((1024 / d) * (1024 / d) * 4).max(4)
            })
            .collect();
        use std::hint::black_box;
        let best = |f: &mut dyn FnMut() -> u64| {
            let mut best = u64::MAX;
            for _ in 0..31 {
                let t = std::time::Instant::now();
                let sink = black_box(f());
                let dt = t.elapsed().as_nanos() as u64;
                assert_ne!(sink, u64::MAX);
                best = best.min(dt);
            }
            best
        };
        let mut buf_a: Vec<u8> = Vec::new();
        let zero_ns = best(&mut || {
            let mut sink = 0u64;
            for &s in black_box(&sizes) {
                buf_a.clear();
                buf_a.resize(s, 0);
                sink = sink.wrapping_add(buf_a[s / 2] as u64 + 1);
            }
            sink
        });
        let mut buf_b: Vec<u8> = Vec::new();
        let trunc_ns = best(&mut || {
            let mut sink = 0u64;
            for &s in black_box(&sizes) {
                if buf_b.len() >= s {
                    buf_b.truncate(s);
                } else {
                    buf_b.resize(s, 0);
                }
                sink = sink.wrapping_add(buf_b[s / 2] as u64 + 1);
            }
            sink
        });
        eprintln!(
            "buffer prep, 10-level 1024² chain: zeroing {:.1} us, truncate {:.1} us, ratio {:.2}x",
            zero_ns as f64 / 1000.0,
            trunc_ns as f64 / 1000.0,
            zero_ns as f64 / trunc_ns as f64,
        );
    }

    /// W2 A/B — six 256² face chains with a fresh buffer pair per face (the
    /// old driver shape) against one hoisted pair shared across faces (the
    /// shipping shape). Both arms run the identical shipping downsample chain;
    /// the delta is allocation + first-touch faulting + cold pages per face.
    /// Run with: `cargo test --release probe_face_chain -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn probe_face_chain_ab() {
        const W: u32 = 256;
        const H: u32 = 256;
        const FACES: usize = 6;
        const LEVELS: u32 = 9; // 256 -> 1
        let mut state = 0xdead_beef_cafe_f00du64;
        let mut faces = Vec::new();
        for _ in 0..FACES {
            let mut src = vec![0u8; (W * H * 4) as usize];
            for b in src.iter_mut() {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *b = state as u8;
            }
            faces.push(src);
        }
        use std::hint::black_box;
        let best = |f: &mut dyn FnMut() -> u64| {
            let mut best = u64::MAX;
            for _ in 0..15 {
                let t = std::time::Instant::now();
                let sink = black_box(f());
                let dt = t.elapsed().as_nanos() as u64;
                assert_ne!(sink, u64::MAX);
                best = best.min(dt);
            }
            best
        };
        let run_face = |src: &[u8], cur_buf: &mut Vec<u8>, next_buf: &mut Vec<u8>| -> u64 {
            let mut sink = 0u64;
            let (mut w, mut h) = (W, H);
            for level in 0..LEVELS {
                let cur: &[u8] = if level == 0 { src } else { cur_buf };
                sink = sink.wrapping_add(cur[0] as u64);
                if level + 1 < LEVELS {
                    super::downsample_rgba8_into(cur, w, h, 1, next_buf).unwrap();
                    std::mem::swap(cur_buf, next_buf);
                    w = (w / 2).max(1);
                    h = (h / 2).max(1);
                }
            }
            sink
        };
        let fresh_ns = best(&mut || {
            let mut sink = 0u64;
            for src in black_box(&faces) {
                let mut cur_buf: Vec<u8> = Vec::new();
                let mut next_buf: Vec<u8> = Vec::new();
                sink = sink.wrapping_add(run_face(src, &mut cur_buf, &mut next_buf));
            }
            sink
        });
        let mut cur_buf: Vec<u8> = Vec::new();
        let mut next_buf: Vec<u8> = Vec::new();
        let hoisted_ns = best(&mut || {
            let mut sink = 0u64;
            for src in black_box(&faces) {
                sink = sink.wrapping_add(run_face(src, &mut cur_buf, &mut next_buf));
            }
            sink
        });
        eprintln!(
            "6-face 256² chains: fresh pair/face {:.1} us, hoisted pair {:.1} us, ratio {:.2}x",
            fresh_ns as f64 / 1000.0,
            hoisted_ns as f64 / 1000.0,
            fresh_ns as f64 / hoisted_ns as f64,
        );
    }

    /// Best-of-N A/B of the driver plumbing: the old chain (unconditional
    /// per-layer `to_vec` + a fresh allocation per level) against the new
    /// (encode-from-source + two recycled ping-pong buffers), full 1024² mip
    /// chain. Both arms do the identical downsample kernel work; the delta is
    /// purely the copy + allocation + page-fault tax the restructure removes.
    /// Run with: `cargo test --release mips -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn probe_mip_chain_ab() {
        const W: u32 = 1024;
        const H: u32 = 1024;
        let mut state = 0xdead_beef_cafe_f00du64;
        let mut src = vec![0u8; (W * H * 4) as usize];
        for b in src.iter_mut() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *b = state as u8;
        }
        let levels = 11u32; // 1024 -> 1
        use std::hint::black_box;
        let best = |f: &mut dyn FnMut() -> u64| {
            let mut best = u64::MAX;
            for _ in 0..15 {
                let t = std::time::Instant::now();
                let sink = black_box(f());
                let dt = t.elapsed().as_nanos() as u64;
                assert_ne!(sink, u64::MAX);
                best = best.min(dt);
            }
            best
        };
        let old_ns = best(&mut || {
            let mut sink = 0u64;
            let mut mip = black_box(&src[..]).to_vec();
            let (mut w, mut h) = (W, H);
            for level in 0..levels {
                sink = sink.wrapping_add(mip[0] as u64);
                if level + 1 < levels {
                    mip = super::downsample_rgba8(&mip, w, h, 1).unwrap();
                    w = (w / 2).max(1);
                    h = (h / 2).max(1);
                }
            }
            sink
        });
        let new_ns = best(&mut || {
            let mut sink = 0u64;
            let mut cur_buf: Vec<u8> = Vec::new();
            let mut next_buf: Vec<u8> = Vec::new();
            let (mut w, mut h) = (W, H);
            for level in 0..levels {
                let cur: &[u8] = if level == 0 { black_box(&src[..]) } else { &cur_buf };
                sink = sink.wrapping_add(cur[0] as u64);
                if level + 1 < levels {
                    super::downsample_rgba8_into(cur, w, h, 1, &mut next_buf).unwrap();
                    std::mem::swap(&mut cur_buf, &mut next_buf);
                    w = (w / 2).max(1);
                    h = (h / 2).max(1);
                }
            }
            sink
        });
        eprintln!(
            "mip chain 1024x1024 x{levels} levels: old (to_vec + alloc/level) {:.1} us, new (ping-pong) {:.1} us, ratio {:.2}x",
            old_ns as f64 / 1000.0,
            new_ns as f64 / 1000.0,
            old_ns as f64 / new_ns as f64,
        );
    }

    /// Best-of-N micro A/B of the changed code: the shipping `downsample_rgba8`
    /// (kernel path on SSSE3) against the reference scalar loop, 1024x1024 → 512x512.
    /// Run with: `cargo test --release mips -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn probe_downsample_ab() {
        const W: u32 = 1024;
        const H: u32 = 1024;
        let mut state = 0xdead_beef_cafe_f00du64;
        let mut src = vec![0u8; (W * H * 4) as usize];
        for b in src.iter_mut() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *b = state as u8;
        }
        let best = |f: &mut dyn FnMut() -> u64| {
            let mut best = u64::MAX;
            for _ in 0..31 {
                let t = std::time::Instant::now();
                let sink = f();
                let dt = t.elapsed().as_nanos() as u64;
                assert_ne!(sink, 0);
                best = best.min(dt);
            }
            best
        };
        let scalar_ns = best(&mut || downsample_reference(&src, W, H, 1)[123] as u64 + 1);
        let fast_ns = best(&mut || super::downsample_rgba8(&src, W, H, 1).unwrap()[123] as u64 + 1);
        let px = (W * H / 4) as f64; // output pixels
        eprintln!(
            "downsample 1024x1024->512x512: scalar {:.3} ns/out-px, kernel path {:.3} ns/out-px, ratio {:.2}x",
            scalar_ns as f64 / px,
            fast_ns as f64 / px,
            scalar_ns as f64 / fast_ns as f64,
        );
    }
}
