//! Block-compressed decode via [`bcdec_rs`] (pure Rust, MIT).
//!
//! Hot-path wins vs naïve tiling:
//! - stack scratch (no per-call `Vec` for the 4×4)
//! - BC4/BC5 expand to RGBA while writing (no full-image RG buffer)
//! - power-of-two-aligned surfaces: decode straight into the output pitch
//! - BC7: `std::thread` strip parallelism when the block count is large enough

use crate::error::Error;

/// Parallelize BC7 only when enough work amortizes `thread::scope` spawn cost.
/// 32×32 (64 blocks) is too small — spawn overhead dominated (~10–20× slower).
/// 256×256 = 4096 blocks is a practical floor for strip workers.
/// Measured on a 24-core box, BC7, blocks vs serial throughput:
///
/// | blocks | serial | parallel | |
/// |---|---|---|---|
/// | 65 536 | 172.6 Mpx/s | 484.0 | par wins 2.8x |
/// | 16 384 | 172.3 Mpx/s | 265.4 | par wins 1.54x |
/// | 4 096  | 200.9 Mpx/s | 88.9  | **par loses 2.26x** |
///
/// The previous value was 4 096 — exactly the size where spawning threads is a
/// net loss. 16 384 is the smallest size where parallelism is *measured* to win;
/// the true break-even lies between the two and would need a dedicated sweep to
/// pin down, so this errs towards the proven side.
const BC7_PARALLEL_MIN_BLOCKS: usize = 16_384;

// A worker-scaling rule (`workers = blocks / 8192`) was tried here on the theory
// that 24 threads over-subscribe a small job. Measurement REFUTED it: it cost
// mip 0 31% (484 -> 332 Mpx/s) and mip 1 26% (265 -> 195). Above the threshold
// the spawn cost amortises fine and more workers is simply better. Do not
// re-add it without a measurement that disagrees.

/// Allocate an RGBA8 surface and decode into it.
///
/// Prefer the `_into` twin when you can reuse a buffer: a fresh one costs the
/// operating system faulting in and zeroing every page before the decoder
/// overwrites it, which measured at 41% of a 1024^2 BC7 decode.
fn alloc_and_decode(
    data: &[u8],
    width: u32,
    height: u32,
    block_bytes: usize,
    f: impl FnOnce(&mut [u8]) -> Result<(), Error>,
) -> Result<Vec<u8>, Error> {
    // Validate the payload BEFORE allocating. `width` and `height` are
    // header-derived and the output is 4 bytes a pixel, so a corrupt header can
    // name a surface needing hundreds of gigabytes. The decoders check this too,
    // but they run after the allocation would already have been attempted —
    // which is precisely the inversion `parser_robustness` caught, aborting the
    // process on a 256 GiB request.
    let (_, _, expected) = block_grid(width, height, block_bytes)?;
    if data.len() < expected {
        return Err(Error::TruncatedData);
    }
    let need = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(Error::OutOfBounds)?;
    let mut out = vec![0u8; need];
    f(&mut out)?;
    Ok(out)
}

pub fn decode_bc1(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>, Error> {
    alloc_and_decode(data, width, height, 8, |o| {
        decode_bc1_into(data, width, height, o)
    })
}

pub fn decode_bc1_into(data: &[u8], width: u32, height: u32, out: &mut [u8]) -> Result<(), Error> {
    // One runtime check per surface, not per block. A `#[target_feature]`
    // function cannot be inlined into a caller that lacks the feature, so
    // dispatching inside the block loop pays a real call — measured at 27% of
    // BC1 decode, against a gather worth less than that. Hoisting the boundary
    // above the loop removes the call and lets the palette build inline into
    // it. The validation below is the same as `decode_rgba_blocks_into`'s, and
    // anything it does not cover falls through to that shared path.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if width % 4 == 0 && height % 4 == 0 && crate::decode::simd::has_pshufb() {
        let (blocks_x, blocks_y, expected) = block_grid(width, height, 8)?;
        if data.len() < expected {
            return Err(Error::TruncatedData);
        }
        let out_w = width as usize;
        check_out_len(out, out_w, height as usize)?;
        // SAFETY: SSSE3 is checked above. `block_grid` bounds the block count to
        // `expected <= data.len()`, `check_out_len` bounds `out`, and the shape
        // is exactly `decode_rgba_blocks_into`'s aligned case.
        unsafe {
            crate::decode::simd::bc1_blocks_ssse3(data, blocks_x, blocks_y, out, out_w);
        }
        return Ok(());
    }
    decode_rgba_blocks_into(data, width, height, 8, out, |block, dst, pitch| {
        bc1_color_block(block, dst, pitch, false);
    })
}

pub fn decode_bc2(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>, Error> {
    alloc_and_decode(data, width, height, 16, |o| {
        decode_bc2_into(data, width, height, o)
    })
}

pub fn decode_bc2_into(data: &[u8], width: u32, height: u32, out: &mut [u8]) -> Result<(), Error> {
    decode_rgba_blocks_into(data, width, height, 16, out, |block, dst, pitch| {
        bc2_block_rgba(block, dst, pitch);
    })
}

pub fn decode_bc3(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>, Error> {
    alloc_and_decode(data, width, height, 16, |o| {
        decode_bc3_into(data, width, height, o)
    })
}

pub fn decode_bc3_into(data: &[u8], width: u32, height: u32, out: &mut [u8]) -> Result<(), Error> {
    decode_rgba_blocks_into(data, width, height, 16, out, |block, dst, pitch| {
        bc3_block_rgba(block, dst, pitch);
    })
}

pub fn decode_bc4(
    data: &[u8],
    width: u32,
    height: u32,
    is_signed: bool,
) -> Result<Vec<u8>, Error> {
    alloc_and_decode(data, width, height, 8, |o| {
        decode_bc4_into(data, width, height, is_signed, o)
    })
}

pub fn decode_bc4_into(
    data: &[u8],
    width: u32,
    height: u32,
    is_signed: bool,
    out: &mut [u8],
) -> Result<(), Error> {
    let (blocks_x, blocks_y, expected) = block_grid(width, height, 8)?;
    if data.len() < expected {
        return Err(Error::TruncatedData);
    }
    let out_w = width as usize;
    let out_h = height as usize;
    check_out_len(out, out_w, out_h)?;
    let aligned = width % 4 == 0 && height % 4 == 0;
    let pitch = out_w * 4;
    let mut scratch = [0u8; 64];

    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let bi = (by * blocks_x + bx) * 8;
            let blk = &data[bi..bi + 8];
            if aligned {
                // Straight into the destination: one pass, packed word stores.
                let offset = (by * 4 * out_w + bx * 4) * 4;
                bc4_block_rgba(blk, &mut out[offset..], pitch, is_signed);
            } else {
                bc4_block_rgba(blk, &mut scratch, 16, is_signed);
                blit_rgba4(&scratch, out, out_w, out_h, bx * 4, by * 4);
            }
        }
    }
    Ok(())
}

pub fn decode_bc5(
    data: &[u8],
    width: u32,
    height: u32,
    is_signed: bool,
) -> Result<Vec<u8>, Error> {
    alloc_and_decode(data, width, height, 16, |o| {
        decode_bc5_into(data, width, height, is_signed, o)
    })
}

pub fn decode_bc5_into(
    data: &[u8],
    width: u32,
    height: u32,
    is_signed: bool,
    out: &mut [u8],
) -> Result<(), Error> {
    let (blocks_x, blocks_y, expected) = block_grid(width, height, 16)?;
    if data.len() < expected {
        return Err(Error::TruncatedData);
    }
    let out_w = width as usize;
    let out_h = height as usize;
    check_out_len(out, out_w, out_h)?;
    let aligned = width % 4 == 0 && height % 4 == 0;
    let pitch = out_w * 4;
    let mut scratch = [0u8; 64];

    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let bi = (by * blocks_x + bx) * 16;
            let blk = &data[bi..bi + 16];
            if aligned {
                let offset = (by * 4 * out_w + bx * 4) * 4;
                bc5_block_rgba(blk, &mut out[offset..], pitch, is_signed);
            } else {
                bc5_block_rgba(blk, &mut scratch, 16, is_signed);
                blit_rgba4(&scratch, out, out_w, out_h, bx * 4, by * 4);
            }
        }
    }
    Ok(())
}

pub fn decode_bc7(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>, Error> {
    alloc_and_decode(data, width, height, 16, |o| {
        decode_bc7_into(data, width, height, o)
    })
}

pub fn decode_bc7_into(data: &[u8], width: u32, height: u32, out: &mut [u8]) -> Result<(), Error> {
    let (blocks_x, blocks_y, expected) = block_grid(width, height, 16)?;
    if data.len() < expected {
        return Err(Error::TruncatedData);
    }
    let out_w = width as usize;
    let out_h = height as usize;
    check_out_len(out, out_w, out_h)?;

    let aligned = width % 4 == 0 && height % 4 == 0;
    let parallel = aligned
        && blocks_y >= 2
        && blocks_x.saturating_mul(blocks_y) >= BC7_PARALLEL_MIN_BLOCKS;

    if parallel {
        decode_bc7_parallel(data, out, out_w, blocks_x, blocks_y);
    } else if aligned {
        decode_bc7_direct(data, out, out_w, blocks_x, blocks_y);
    } else {
        decode_bc7_scratch(data, out, out_w, out_h, blocks_x, blocks_y);
    }
    Ok(())
}

/// The destination must be exactly one RGBA8 surface. Too small would write out
/// of range; too large would leave a stale tail the caller could mistake for
/// decoded pixels.
fn check_out_len(out: &[u8], out_w: usize, out_h: usize) -> Result<(), Error> {
    let need = out_w
        .checked_mul(out_h)
        .and_then(|n| n.checked_mul(4))
        .ok_or(Error::OutOfBounds)?;
    if out.len() != need {
        return Err(Error::OutOfBounds);
    }
    Ok(())
}

/// The four RGBA palette entries of a BC1 colour block.
///
/// Split out of [`bc1_color_block`] so the SSSE3 block loop can share it. It is
/// a plain `#[inline]` function, not a `#[target_feature]` one, which is
/// deliberate: a plain function inlines into a `#[target_feature]` caller, so
/// the palette is built in registers there and never round-trips through the
/// stack. Building it element-wise on the far side of an ABI boundary cost
/// +14% against the scalar loop it was meant to beat.
#[inline]
pub(super) fn bc1_palette(blk: &[u8], opaque: bool) -> [u32; 4] {
    let c0 = u16::from_le_bytes([blk[0], blk[1]]) as u32;
    let c1 = u16::from_le_bytes([blk[2], blk[3]]) as u32;
    let (r0, g0, b0) = ((c0 >> 11) & 0x1f, (c0 >> 5) & 0x3f, c0 & 0x1f);
    let (r1, g1, b1) = ((c1 >> 11) & 0x1f, (c1 >> 5) & 0x3f, c1 & 0x1f);

    // RGBA in memory order, packed little-endian so a store is one word.
    let px = |r: u32, g: u32, b: u32, a: u32| r | (g << 8) | (b << 16) | (a << 24);

    let mut pal = [0u32; 4];
    pal[0] = px(
        (r0 * 527 + 23) >> 6,
        (g0 * 259 + 33) >> 6,
        (b0 * 527 + 23) >> 6,
        255,
    );
    pal[1] = px(
        (r1 * 527 + 23) >> 6,
        (g1 * 259 + 33) >> 6,
        (b1 * 527 + 23) >> 6,
        255,
    );
    if c0 > c1 || opaque {
        // Four-colour block: two interpolants at 1/3 and 2/3.
        pal[2] = px(
            ((2 * r0 + r1) * 351 + 61) >> 7,
            ((2 * g0 + g1) * 2763 + 1039) >> 11,
            ((2 * b0 + b1) * 351 + 61) >> 7,
            255,
        );
        pal[3] = px(
            ((r0 + r1 * 2) * 351 + 61) >> 7,
            ((g0 + g1 * 2) * 2763 + 1039) >> 11,
            ((b0 + b1 * 2) * 351 + 61) >> 7,
            255,
        );
    } else {
        // Three-colour block: one midpoint, and index 3 is transparent black.
        pal[2] = px(
            ((r0 + r1) * 1053 + 125) >> 8,
            ((g0 + g1) * 4145 + 1019) >> 11,
            ((b0 + b1) * 1053 + 125) >> 8,
            255,
        );
        pal[3] = 0;
    }

    pal
}

#[cfg(test)]
pub(super) fn bc1_color_block_for_test(blk: &[u8], out: &mut [u8], pitch: usize, opaque: bool) {
    bc1_color_block(blk, out, pitch, opaque)
}

/// Decode one BC1 colour block to RGBA8.
///
/// `opaque` forces the four-colour interpretation regardless of endpoint order,
/// which is what BC2 and BC3 colour blocks require.
///
/// Two things the general decoder does that this does not. It walks the index
/// word with `indices >>= 2` after every pixel, which is a **sixteen-deep serial
/// dependency chain** — the same shape that dominated BC7 before 0.3.6 — and it
/// copies each pixel with a bounds-checked four-byte `copy_from_slice`. Reading
/// each index by computed offset from an immutable `u32` makes all sixteen
/// independent, and building the palette as four `u32`s turns the copy into a
/// single word store.
///
/// The 565-to-888 expansion constants are the reference implementation's, and
/// the oracle test asserts bit-identical output across random blocks and both
/// endpoint orderings.
#[inline]
fn bc1_color_block(blk: &[u8], out: &mut [u8], pitch: usize, opaque: bool) {
    let pal = bc1_palette(blk, opaque);
    let idx = u32::from_le_bytes([blk[4], blk[5], blk[6], blk[7]]);
    // A whole block row per store: one slice range-check instead of four. See
    // `bc5_block_rgba` — this was worth +32% there.
    for row in 0..4usize {
        let mut px = [0u8; 16];
        for col in 0..4usize {
            let e = pal[((idx >> (2 * (row * 4 + col))) & 0x3) as usize];
            px[col * 4..col * 4 + 4].copy_from_slice(&e.to_le_bytes());
        }
        let o = row * pitch;
        out[o..o + 16].copy_from_slice(&px);
    }
}

#[cfg(test)]
mod bc1_tests {
    use super::bc1_color_block;

    /// Bit-identical to the reference across random blocks, both endpoint
    /// orderings, and the degenerate `c0 == c1` case where the three-colour
    /// branch is taken and index 3 must come out fully transparent.
    #[test]
    fn bc1_color_block_matches_the_general_decoder() {
        let mut state = 0x5eed_1234_9876_fedcu64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..40_000 {
            let mut blk = [0u8; 8];
            match case {
                0 => {}
                1 => blk.iter_mut().for_each(|x| *x = 0xff),
                2 => blk[..4].copy_from_slice(&[0x34, 0x12, 0x34, 0x12]), // c0 == c1
                _ => blk.copy_from_slice(&next().to_le_bytes()),
            }

            // BC1 proper: the endpoint order selects the mode.
            let mut ours = [0u8; 64];
            bc1_color_block(&blk, &mut ours, 16, false);
            let mut theirs = [0u8; 64];
            bcdec_rs::bc1(&blk, &mut theirs, 16);
            assert_eq!(ours, theirs, "case {case}: BC1 diverged, block {blk:02x?}");

            // Opaque mode, as BC2 and BC3 colour blocks use. bcdec reaches it
            // through bc2 with a zeroed alpha block, so compare colour only.
            let mut opaque = [0u8; 64];
            bc1_color_block(&blk, &mut opaque, 16, true);
            let mut wrapped = [0u8; 16];
            wrapped[8..].copy_from_slice(&blk);
            let mut theirs2 = [0u8; 64];
            bcdec_rs::bc2(&wrapped, &mut theirs2, 16);
            for p in 0..16 {
                assert_eq!(
                    opaque[p * 4..p * 4 + 3],
                    theirs2[p * 4..p * 4 + 3],
                    "case {case}: opaque-mode colour diverged at pixel {p}"
                );
            }
        }
    }
}

/// Build the eight-entry palette of a BC4 block, as the reference does.
///
/// The endpoint order selects six interpolants or four plus the two extremes.
/// Signed blocks clamp `-128` to `-127` and use `-127`/`127` as the extremes.
#[inline(always)]
fn bc4_palette_packed(a0: u8, a1: u8, is_signed: bool) -> u64 {
    const W4: [i32; 4] = [13107, 26215, 39321, 52429];
    const W6: [i32; 6] = [9363, 18724, 28086, 37450, 46812, 56173];

    let (e0, e1) = if is_signed {
        ((a0 as i8 as i32).max(-127), (a1 as i8 as i32).max(-127))
    } else {
        (a0 as i32, a1 as i32)
    };

    // The weight pairs sum to exactly 65536 — `W6[5-k] + W6[k] == 65536`, and
    // likewise `W4` — so
    //
    //     (W[n-k]*e0 + W[k]*e1 + 32768) >> 16
    //       ==  e0 + ((W[k]*(e1 - e0) + 32768) >> 16)
    //
    // because `65536 * e0` has sixteen zero low bits and an arithmetic shift
    // right is a floor division. One multiply per entry, and `delta` is shared.
    let delta = e1 - e0;
    let b = |v: i32| (v as u8) as u64;

    // Written as a balanced OR tree over independent terms, NOT accumulated in a
    // loop. `packed |= x << (8*k)` around a loop is an eight-deep serial
    // dependency chain, and the `[i32; 8]` it read from was a stack round trip
    // on top of that — the same two defects fixed in the index unpack and the
    // palette handoff. The palette build measured at ~34% of the BC5 block once
    // the gather and index unpack were fast.
    let pack = |v: [i32; 8]| {
        let lo = b(v[0]) | (b(v[1]) << 8) | (b(v[2]) << 16) | (b(v[3]) << 24);
        let hi = b(v[4]) | (b(v[5]) << 8) | (b(v[6]) << 16) | (b(v[7]) << 24);
        lo | (hi << 32)
    };
    let interp = |w: i32| e0 + ((w * delta + 32768) >> 16);

    // The branch on `e0 > e1` was rewritten branchless (compute both weight
    // sets, select with a mask) and measured NEUTRAL: BC5 625.6 vs 609.3 Mpx/s,
    // BC4 680.5 vs 685.3, eight samples per arm. Whatever the palette costs, it
    // is not this mispredict. Reverted; do not re-try without a number.
    if e0 > e1 {
        pack([
            e0,
            e1,
            interp(W6[0]),
            interp(W6[1]),
            interp(W6[2]),
            interp(W6[3]),
            interp(W6[4]),
            interp(W6[5]),
        ])
    } else {
        pack([
            e0,
            e1,
            interp(W4[0]),
            interp(W4[1]),
            interp(W4[2]),
            interp(W4[3]),
            if is_signed { -127 } else { 0 },
            if is_signed { 127 } else { 255 },
        ])
    }
}

/// The sixteen 3-bit indices of a BC4 block, as one immutable word.
///
/// The reference walks these with `indices >>= 3` after every pixel, a
/// sixteen-deep serial dependency chain. Returning the word lets each index be
/// read by computed offset instead, so all sixteen are independent.
#[inline(always)]
fn bc4_indices(blk: &[u8]) -> u64 {
    u64::from_le_bytes([blk[0], blk[1], blk[2], blk[3], blk[4], blk[5], blk[6], blk[7]]) >> 16
}

/// Decode a BC4 block straight to RGBA8: value in red, zero in green and blue,
/// opaque alpha.
///
/// The general path decodes sixteen single-channel bytes and then expands them
/// to RGBA in a second pass over the block. Writing packed words directly fuses
/// the two.
#[inline]
fn bc4_block_rgba(blk: &[u8], out: &mut [u8], pitch: usize, is_signed: bool) {
    let pal_packed = bc4_palette_packed(blk[0], blk[1], is_signed);
    let pal = pal_packed.to_le_bytes();
    let idx = bc4_indices(blk);

    // BC4 is BC5 with a zero second channel: the same gather, with an all-zero
    // green palette and a zero index word, yields (v, 0, 0, 255) per pixel.
    // Reuses the kernel and its oracle rather than duplicating them.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if out.len() >= 3 * pitch + 16
        && crate::decode::simd::bc5_gather(pal_packed, 0, idx, 0, out, pitch)
    {
        return;
    }
    // A whole block row per store: one slice range-check instead of four. See
    // `bc5_block_rgba` — this was worth +32% there.
    for row in 0..4usize {
        let mut px = [0u8; 16];
        for col in 0..4usize {
            let v = pal[((idx >> (3 * (row * 4 + col))) & 0x7) as usize] as u32;
            let word = v | (255 << 24);
            px[col * 4..col * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        let o = row * pitch;
        out[o..o + 16].copy_from_slice(&px);
    }
}

/// Decode a BC5 block pair straight to RGBA8: first channel in red, second in
/// green, zero in blue, opaque alpha.
#[inline]
// The BC5 block is LATENCY bound, not throughput bound. Measured: doubling the
// palette work is FREE (563.7 vs 521.5 Mpx/s, three samples each). The block is
// one serial chain — load endpoints, delta, multiply, shift, add, pack, `movq`,
// `pshufb`, unpack, store — roughly 25 cycles deep, and the palette build is
// ~32% of it (full ~572 Mpx/s against ~847 with a trivial block-dependent
// palette).
//
// This explains three refutations that each looked promising and each measured
// neutral, because all three were THROUGHPUT edits against a LATENCY wall:
//
//   * halving the multiplies via the 65536-sum identity   (0.3.15)
//   * branchless endpoint selection, killing a mispredict (0.3.16)
//   * pairing two blocks per iteration for ILP            — 592.0 vs 579.7
//     Mpx/s, eight samples per arm. The out-of-order engine was already
//     overlapping adjacent blocks; saying so explicitly added nothing.
//
// The only remaining lever is SHORTENING THE CHAIN, not doing less work in it
// or doing more of it at once. The untried candidate is building the palette
// directly in an `__m128i` — skipping the scalar pack tree and the `movq` that
// follows it, worth maybe six cycles of the twenty-five. Measure the chain, not
// the operation count.
fn bc5_block_rgba(blk: &[u8], out: &mut [u8], pitch: usize, is_signed: bool) {
    let pr_packed = bc4_palette_packed(blk[0], blk[1], is_signed);
    let pg_packed = bc4_palette_packed(blk[8], blk[9], is_signed);
    let ir = bc4_indices(&blk[..8]);
    let ig = bc4_indices(&blk[8..16]);

    // One `pshufb` per channel replaces thirty-two dependent byte loads. Needs
    // SSSE3, so it is detected and the scalar path below stays as the twin.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if out.len() >= 3 * pitch + 16
        && crate::decode::simd::bc5_gather(pr_packed, pg_packed, ir, ig, out, pitch)
    {
        return;
    }
    let (pr, pg) = (pr_packed.to_le_bytes(), pg_packed.to_le_bytes());
    // A whole block row per store. Four separate four-byte `copy_from_slice`
    // calls carry four slice range-checks; building the row and writing it once
    // carries one, and the row is contiguous in the destination by construction.
    for row in 0..4usize {
        let mut px = [0u8; 16];
        for col in 0..4usize {
            let sh = 3 * (row * 4 + col);
            let r = pr[((ir >> sh) & 0x7) as usize] as u32;
            let g = pg[((ig >> sh) & 0x7) as usize] as u32;
            let word = r | (g << 8) | (255 << 24);
            px[col * 4..col * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        let o = row * pitch;
        out[o..o + 16].copy_from_slice(&px);
    }
}

#[cfg(test)]
mod bc45_tests {
    use super::{bc4_block_rgba, bc5_block_rgba};

    fn rng(seed: u64) -> impl FnMut() -> u64 {
        let mut state = seed;
        move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        }
    }

    /// Both endpoint orderings and both signednesses, against the reference.
    /// The `a0 == a1` and `-128` cases are pinned because they select the
    /// four-interpolant branch and the signed clamp respectively.
    #[test]
    fn bc4_matches_the_general_decoder() {
        for &is_signed in &[false, true] {
            let mut next = rng(0xabcd_1234_5678_9012 ^ is_signed as u64);
            for case in 0..30_000 {
                let mut blk = [0u8; 8];
                match case {
                    0 => {}
                    1 => blk.iter_mut().for_each(|x| *x = 0xff),
                    2 => blk[..2].copy_from_slice(&[0x80, 0x80]), // -128 clamp, a0 == a1
                    3 => blk[..2].copy_from_slice(&[0x10, 0x90]), // a0 < a1
                    _ => blk.copy_from_slice(&next().to_le_bytes()),
                }
                let mut ours = [0u8; 64];
                bc4_block_rgba(&blk, &mut ours, 16, is_signed);

                // The reference writes one channel; expand it the same way the
                // old two-pass path did.
                let mut single = [0u8; 16];
                bcdec_rs::bc4(&blk, &mut single, 4, is_signed);
                for p in 0..16 {
                    let want = [single[p], 0, 0, 255];
                    assert_eq!(
                        ours[p * 4..p * 4 + 4],
                        want,
                        "signed={is_signed} case {case} pixel {p}, block {blk:02x?}"
                    );
                }
            }
        }
    }

    #[test]
    fn bc5_matches_the_general_decoder() {
        for &is_signed in &[false, true] {
            let mut next = rng(0x1357_9bdf_2468_ace0 ^ is_signed as u64);
            for case in 0..30_000 {
                let mut blk = [0u8; 16];
                match case {
                    0 => {}
                    1 => blk.iter_mut().for_each(|x| *x = 0xff),
                    _ => {
                        blk[..8].copy_from_slice(&next().to_le_bytes());
                        blk[8..].copy_from_slice(&next().to_le_bytes());
                    }
                }
                let mut ours = [0u8; 64];
                bc5_block_rgba(&blk, &mut ours, 16, is_signed);

                let mut pair = [0u8; 32];
                bcdec_rs::bc5(&blk, &mut pair, 8, is_signed);
                for p in 0..16 {
                    let want = [pair[p * 2], pair[p * 2 + 1], 0, 255];
                    assert_eq!(
                        ours[p * 4..p * 4 + 4],
                        want,
                        "signed={is_signed} case {case} pixel {p}"
                    );
                }
            }
        }
    }
}

/// BC3's alpha palette, which is **not** BC4's.
///
/// BC4 interpolates with fixed-point weights (`a_weights6` / `a_weights4`,
/// `>> 16`); BC3's alpha block uses integer division by 7 and 5. Those disagree:
/// for `a0 = 60, a1 = 133` the four-interpolant entry is 74 by division and 75
/// by weights. The reference implementation makes the same distinction, so
/// matching it bit-for-bit means keeping both forms. The oracle test caught this
/// immediately when BC3 was first wired to the BC4 palette.
#[inline(always)]
fn bc3_alpha_palette(a0: u8, a1: u8) -> [u8; 8] {
    let (a0, a1) = (a0 as u32, a1 as u32);
    let mut p = [0u32; 8];
    p[0] = a0;
    p[1] = a1;
    if a0 > a1 {
        for k in 0..6u32 {
            p[2 + k as usize] = ((6 - k) * a0 + (k + 1) * a1 + 1) / 7;
        }
    } else {
        for k in 0..4u32 {
            p[2 + k as usize] = ((4 - k) * a0 + (k + 1) * a1 + 1) / 5;
        }
        p[6] = 0;
        p[7] = 255;
    }
    [
        p[0] as u8, p[1] as u8, p[2] as u8, p[3] as u8, p[4] as u8, p[5] as u8, p[6] as u8,
        p[7] as u8,
    ]
}

/// Decode one BC2 block to RGBA8: explicit 4-bit alpha, then an opaque-mode
/// colour block.
#[inline]
fn bc2_block_rgba(blk: &[u8], out: &mut [u8], pitch: usize) {
    // Colour first — it writes an opaque alpha that the loop below replaces.
    bc1_color_block(&blk[8..16], out, pitch, true);
    for row in 0..4usize {
        let a = u16::from_le_bytes([blk[row * 2], blk[row * 2 + 1]]);
        for col in 0..4usize {
            // 4-bit alpha scaled to 8 bits by the reference's factor of 17,
            // which is exactly 0x0F -> 0xFF.
            out[row * pitch + col * 4 + 3] = (((a >> (4 * col)) & 0x0f) as u8) * 17;
        }
    }
}

/// Decode one BC3 block to RGBA8: a BC4-style interpolated alpha block, then an
/// opaque-mode colour block.
///
/// The alpha half is literally a BC4 block, so it reuses [`bc4_indices`] —
/// including its independent index reads. The palette is BC3's own division
/// form, not BC4's weight form; see [`bc3_alpha_palette`] for why they differ.
#[inline]
fn bc3_block_rgba(blk: &[u8], out: &mut [u8], pitch: usize) {
    bc1_color_block(&blk[8..16], out, pitch, true);
    let pal = bc3_alpha_palette(blk[0], blk[1]);
    let idx = bc4_indices(&blk[..8]);
    for p in 0..16usize {
        let o = (p / 4) * pitch + (p % 4) * 4;
        out[o + 3] = pal[((idx >> (3 * p)) & 0x7) as usize];
    }
}

#[cfg(test)]
mod bc23_tests {
    use super::{bc2_block_rgba, bc3_block_rgba};

    fn rng(seed: u64) -> impl FnMut() -> u64 {
        let mut state = seed;
        move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        }
    }

    #[test]
    fn bc2_matches_the_general_decoder() {
        let mut next = rng(0x2222_3333_4444_5555);
        for case in 0..30_000 {
            let mut blk = [0u8; 16];
            match case {
                0 => {}
                1 => blk.iter_mut().for_each(|x| *x = 0xff),
                _ => {
                    blk[..8].copy_from_slice(&next().to_le_bytes());
                    blk[8..].copy_from_slice(&next().to_le_bytes());
                }
            }
            let mut ours = [0u8; 64];
            bc2_block_rgba(&blk, &mut ours, 16);
            let mut theirs = [0u8; 64];
            bcdec_rs::bc2(&blk, &mut theirs, 16);
            assert_eq!(ours, theirs, "case {case}: BC2 diverged, block {blk:02x?}");
        }
    }

    #[test]
    fn bc3_matches_the_general_decoder() {
        let mut next = rng(0x6666_7777_8888_9999);
        for case in 0..30_000 {
            let mut blk = [0u8; 16];
            match case {
                0 => {}
                1 => blk.iter_mut().for_each(|x| *x = 0xff),
                // a0 == a1 selects the four-interpolant alpha branch.
                2 => blk[..2].copy_from_slice(&[0x40, 0x40]),
                _ => {
                    blk[..8].copy_from_slice(&next().to_le_bytes());
                    blk[8..].copy_from_slice(&next().to_le_bytes());
                }
            }
            let mut ours = [0u8; 64];
            bc3_block_rgba(&blk, &mut ours, 16);
            let mut theirs = [0u8; 64];
            bcdec_rs::bc3(&blk, &mut theirs, 16);
            assert_eq!(ours, theirs, "case {case}: BC3 diverged, block {blk:02x?}");
        }
    }
}

fn decode_rgba_blocks_into(
    data: &[u8],
    width: u32,
    height: u32,
    block_bytes: usize,
    out: &mut [u8],
    decode_block: impl Fn(&[u8], &mut [u8], usize),
) -> Result<(), Error> {
    let (blocks_x, blocks_y, expected) = block_grid(width, height, block_bytes)?;
    if data.len() < expected {
        return Err(Error::TruncatedData);
    }
    let out_w = width as usize;
    let out_h = height as usize;
    check_out_len(out, out_w, out_h)?;
    let pitch = out_w * 4;

    if width % 4 == 0 && height % 4 == 0 {
        for by in 0..blocks_y {
            for bx in 0..blocks_x {
                let bi = (by * blocks_x + bx) * block_bytes;
                let offset = (by * 4 * out_w + bx * 4) * 4;
                decode_block(&data[bi..bi + block_bytes], &mut out[offset..], pitch);
            }
        }
    } else {
        let mut scratch = [0u8; 64];
        for by in 0..blocks_y {
            for bx in 0..blocks_x {
                let bi = (by * blocks_x + bx) * block_bytes;
                decode_block(&data[bi..bi + block_bytes], &mut scratch, 16);
                blit_rgba4(&scratch, out, out_w, out_h, bx * 4, by * 4);
            }
        }
    }
    Ok(())
}

fn decode_bc7_direct(
    data: &[u8],
    out: &mut [u8],
    out_w: usize,
    blocks_x: usize,
    blocks_y: usize,
) {
    let pitch = out_w * 4;
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let bi = (by * blocks_x + bx) * 16;
            let offset = (by * 4 * out_w + bx * 4) * 4;
            let (blk, dst) = (&data[bi..bi + 16], &mut out[offset..]);
            if !bc7_fast_block(blk, dst, pitch) {
                bcdec_rs::bc7(blk, dst, pitch);
            }
        }
    }
}

fn decode_bc7_scratch(
    data: &[u8],
    out: &mut [u8],
    out_w: usize,
    out_h: usize,
    blocks_x: usize,
    blocks_y: usize,
) {
    let mut scratch = [0u8; 64];
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let bi = (by * blocks_x + bx) * 16;
            let blk = &data[bi..bi + 16];
            if !bc7_fast_block(blk, &mut scratch, 16) {
                bcdec_rs::bc7(blk, &mut scratch, 16);
            }
            blit_rgba4(&scratch, out, out_w, out_h, bx * 4, by * 4);
        }
    }
}

fn decode_bc7_parallel(
    data: &[u8],
    out: &mut [u8],
    out_w: usize,
    blocks_x: usize,
    blocks_y: usize,
) {
    let pitch = out_w * 4;
    let strip_bytes = 4 * pitch;
    // `available_parallelism` is a syscall; it cannot usefully change within a
    // process, and this used to run on every decode call.
    static CORES: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let cores = *CORES.get_or_init(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    });
    let workers = cores.clamp(1, blocks_y);

    // Split block-rows across workers; each owns a disjoint 4-scanline strip band.
    let mut ranges: Vec<(usize, usize)> = Vec::with_capacity(workers);
    let base = blocks_y / workers;
    let extra = blocks_y % workers;
    let mut start = 0;
    for w in 0..workers {
        let len = base + usize::from(w < extra);
        ranges.push((start, start + len));
        start += len;
    }

    std::thread::scope(|s| {
        let mut rest = out;
        let mut consumed_rows = 0usize;
        for &(by0, by1) in &ranges {
            let row0 = by0 * 4;
            debug_assert_eq!(row0, consumed_rows);
            let strip_len = (by1 - by0) * strip_bytes;
            let (band, tail) = rest.split_at_mut(strip_len);
            rest = tail;
            consumed_rows = by1 * 4;
            s.spawn(move || {
                for by in by0..by1 {
                    let local_y = by - by0;
                    for bx in 0..blocks_x {
                        let bi = (by * blocks_x + bx) * 16;
                        let offset = (local_y * 4 * out_w + bx * 4) * 4;
                        let (blk, dst) = (&data[bi..bi + 16], &mut band[offset..]);
                        if !bc7_fast_block(blk, dst, pitch) {
                            bcdec_rs::bc7(blk, dst, pitch);
                        }
                    }
                }
            });
        }
        debug_assert!(rest.is_empty());
    });
}

fn block_grid(width: u32, height: u32, block_bytes: usize) -> Result<(usize, usize, usize), Error> {
    if width == 0 || height == 0 {
        return Err(Error::InvalidField("zero image dimension".into()));
    }
    let blocks_x = (width as usize + 3) / 4;
    let blocks_y = (height as usize + 3) / 4;
    let expected = blocks_x
        .checked_mul(blocks_y)
        .and_then(|n| n.checked_mul(block_bytes))
        .ok_or(Error::OutOfBounds)?;
    Ok((blocks_x, blocks_y, expected))
}

#[inline]
fn blit_rgba4(scratch: &[u8; 64], out: &mut [u8], out_w: usize, out_h: usize, px0: usize, py0: usize) {
    let copy_w = 4.min(out_w - px0);
    let copy_h = 4.min(out_h - py0);
    for row in 0..copy_h {
        let src = row * 16;
        let dst = ((py0 + row) * out_w + px0) * 4;
        out[dst..dst + copy_w * 4].copy_from_slice(&scratch[src..src + copy_w * 4]);
    }
}



// --------------------------------------------------------------- BC7 mode 6

// Every BC7 mode now has a specialised decoder. An earlier mode-5 attempt
// measured neutral and was recorded here as refuting the whole approach; that
// was wrong. The approach is sound and the code was not — it resolved the
// rotation with a conditional swap inside the per-pixel loop. See
// `bc7_mode5_block` for the correction and the numbers.

/// Dispatch a block to its specialised decoder.
///
/// BC7 encodes the mode in unary in the low bits of byte 0, so the mode number
/// is one `trailing_zeros`. Branching on it directly gives the compiler a jump
/// table; an `||` chain of eight per-mode probes does not, and costs a
/// mode-5 block seven failed checks before it is claimed.
///
/// The per-mode decoders are deliberately **not** `#[inline]`. Inlining all
/// eight into this one function was measured at 8-10% *slower* on real content
/// than calling them out of line: the block loop is hot and small, and eight
/// inlined decoders blow its instruction footprint. The isolated per-mode
/// benchmarks could not see this, because each one only ever exercises a single
/// decoder and never pays for the other seven being resident.
///
/// Returns `false` for the reserved encoding, which falls through to the
/// general decoder to be zero-filled per spec.
#[inline]
fn bc7_fast_block(blk: &[u8], out: &mut [u8], pitch: usize) -> bool {
    if blk[0] == 0 {
        // Reserved: no mode bit set in the low byte.
        return false;
    }
    match blk[0].trailing_zeros() {
        0 => bc7_mode0_block(blk, out, pitch),
        1 => bc7_mode1_block(blk, out, pitch),
        2 => bc7_mode2_block(blk, out, pitch),
        3 => bc7_mode3_block(blk, out, pitch),
        4 => bc7_mode4_block(blk, out, pitch),
        5 => bc7_mode5_block(blk, out, pitch),
        6 => bc7_mode6_block(blk, out, pitch),
        7 => bc7_mode7_block(blk, out, pitch),
        _ => false,
    }
}

/// Subset assignment for BC7's 64 two-subset partitions: bit `p` set means
/// pixel `p` (raster order) belongs to subset 1. Derived from the spec table.
const BC7_P2_SUBSET: [u16; 64] = [
    0xcccc, 0x8888, 0xeeee, 0xecc8, 0xc880, 0xfeec, 0xfec8, 0xec80,
    0xc800, 0xffec, 0xfe80, 0xe800, 0xffe8, 0xff00, 0xfff0, 0xf000,
    0xf710, 0x008e, 0x7100, 0x08ce, 0x008c, 0x7310, 0x3100, 0x8cce,
    0x088c, 0x3110, 0x6666, 0x366c, 0x17e8, 0x0ff0, 0x718e, 0x399c,
    0xaaaa, 0xf0f0, 0x5a5a, 0x33cc, 0x3c3c, 0x55aa, 0x9696, 0xa55a,
    0x73ce, 0x13c8, 0x324c, 0x3bdc, 0x6996, 0xc33c, 0x9966, 0x0660,
    0x0272, 0x04e4, 0x4e40, 0x2720, 0xc936, 0x936c, 0x39c6, 0x639c,
    0x9336, 0x9cc6, 0x817e, 0xe718, 0xccf0, 0x0fcc, 0x7744, 0xee22,
];

/// Pixel index carrying subset 1's fix-up index, per partition. Subset 0's
/// fix-up is always pixel 0. Both are stored with one bit fewer.
const BC7_P2_FIXUP: [u8; 64] = [
    15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15,
    15,  2,  8,  2,  2,  8,  8, 15,  2,  8,  2,  2,  8,  8,  2,  2,
    15, 15,  6,  8,  2,  8, 15, 15,  2,  8,  2,  2,  2, 15, 15,  6,
     6,  2,  6,  8, 15, 15,  2,  2, 15, 15, 15, 15, 15,  2,  2, 15,
];

/// BC7 interpolation weights for 3-bit indices.
const BC7_WEIGHTS3: [u32; 8] = [0, 9, 18, 27, 37, 46, 55, 64];

/// BC7 interpolation weights for 2-bit indices.
const BC7_WEIGHTS2: [u32; 4] = [0, 21, 43, 64];

/// Base and delta per endpoint pair, so interpolation costs one multiply per
/// channel instead of two.
///
/// The spec form `(e0 * (64 - w) + e1 * w + 32) >> 6` equals
/// `(e0 * 64 + 32 + w * (e1 - e0)) >> 6`, and in that form only one operand
/// varies with the weight. The endpoint pairs are constant for a whole block, so
/// this runs once and the sixteen-pixel loop keeps just a multiply-add.
/// Identical arithmetic, identical bytes out.
#[inline(always)]
fn bc7_bd3(e: &[[u32; 3]], pairs: usize) -> [([i32; 3], [i32; 3]); 3] {
    let mut out = [([0i32; 3], [0i32; 3]); 3];
    for (k, slot) in out.iter_mut().enumerate().take(pairs) {
        let (a, c) = (&e[k * 2], &e[k * 2 + 1]);
        for i in 0..3 {
            slot.0[i] = a[i] as i32 * 64 + 32;
            slot.1[i] = c[i] as i32 - a[i] as i32;
        }
    }
    out
}

/// [`bc7_bd3`] for the modes that carry alpha.
#[inline(always)]
fn bc7_bd4(e: &[[u32; 4]], pairs: usize) -> [([i32; 4], [i32; 4]); 2] {
    let mut out = [([0i32; 4], [0i32; 4]); 2];
    for (k, slot) in out.iter_mut().enumerate().take(pairs) {
        let (a, c) = (&e[k * 2], &e[k * 2 + 1]);
        for i in 0..4 {
            slot.0[i] = a[i] as i32 * 64 + 32;
            slot.1[i] = c[i] as i32 - a[i] as i32;
        }
    }
    out
}

/// Bit offset and width of pixel `p`s index, for a two-subset mode with
/// `bits`-wide indices and subset 1 anchored at `fixup`.
///
/// The two fix-up pixels store one bit fewer, which is why the general decoder
/// reads indices through a stateful cursor: each reads position depends on
/// every read before it. Computing the offset arithmetically makes all sixteen
/// extractions independent instead.
#[inline(always)]
fn bc7_p2_index_at(p: usize, fixup: usize, bits: u32) -> (u32, u32) {
    let short = usize::from(p > 0) + usize::from(p > fixup);
    let off = bits as usize * p - short;
    let w = bits - u32::from(p == 0 || p == fixup);
    (off as u32, w)
}

/// Decode one mode 1 BC7 block straight to RGBA8.
///
/// Two subsets, 6-bit RGB endpoints with a p-bit shared per subset, 3-bit
/// indices, opaque alpha.
///
/// The win is not the partition lookup, which the format requires and which
/// stays. It is the index reads. `bcdec_rs` pulls indices through a stateful
/// bitstream whose every read mutates the cursor, so sixteen of them form a
/// sixteen-deep serial dependency chain: read `n + 1` cannot start until read
/// `n` retires. Reading each index by computed offset from an immutable `u128`
/// makes all sixteen independent, and the out-of-order engine runs them in
/// parallel.
///
/// Returns `false` if the block is not mode 1.
fn bc7_mode1_block(blk: &[u8], out: &mut [u8], pitch: usize) -> bool {
    if blk[0] & 0x3 != 0x2 {
        return false;
    }
    let Ok(bytes) = <[u8; 16]>::try_from(&blk[..16]) else {
        return false;
    };
    let b = u128::from_le_bytes(bytes);

    let partition = ((b >> 2) & 0x3f) as usize;
    let p0 = ((b >> 80) & 1) as u32;
    let p1 = ((b >> 81) & 1) as u32;

    // 6 bits, then the subsets shared p-bit appended as the low bit, then the
    // 7-bit value shifted up with its MSB replicated into the vacated bit.
    let ep = |shift: u32, pbit: u32| {
        let v = ((((b >> shift) & 0x3f) as u32) << 1) | pbit;
        let t = v << 1;
        t | (t >> 7)
    };
    // Component-major in the block: R0..R3, then G0..G3, then B0..B3.
    let e = [
        [ep(8, p0), ep(32, p0), ep(56, p0)],
        [ep(14, p0), ep(38, p0), ep(62, p0)],
        [ep(20, p1), ep(44, p1), ep(68, p1)],
        [ep(26, p1), ep(50, p1), ep(74, p1)],
    ];

    let subsets = BC7_P2_SUBSET[partition];
    let fixup = BC7_P2_FIXUP[partition] as usize;
    // The index region is at most 47 bits, so it fits a `u64`. Doing the wide
    // shift once and the sixteen per-pixel shifts narrow matters: a `u128` shift
    // on x86_64 is a multi-instruction sequence, a `u64` shift is one.
    let idx = (b >> 82) as u64;

    // One multiply per channel, not two: see `bc7_mode6_block`. Base and delta
    // are per endpoint pair, so both subsets are prepared up front.
    let bd = bc7_bd3(&e, 2);

    let weight_of = |p: usize| {
        let (off, w) = bc7_p2_index_at(p, fixup, 3);
        BC7_WEIGHTS3[((idx >> off) & ((1u64 << w) - 1)) as usize]
    };

    // Two pixels per store; `p` is even, so both lie in one block row.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    {
        let bdp = crate::decode::simd::pack_bd3(&bd, 2);
        for p in (0..16usize).step_by(2) {
            let (b0, d0) = bdp[((subsets >> p) & 1) as usize];
            let q = p + 1;
            let (b1, d1) = bdp[((subsets >> q) & 1) as usize];
            let o = (p / 4) * pitch + (p % 4) * 4;
            crate::decode::simd::write2(
                b0,
                d0,
                b1,
                d1,
                weight_of(p) as i16,
                weight_of(q) as i16,
                &mut out[o..o + 8],
            );
        }
    }

    #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
    {
        for p in 0..16usize {
            let weight = weight_of(p) as i32;
            let (base, delta) = &bd[((subsets >> p) & 1) as usize];
            let o = (p / 4) * pitch + (p % 4) * 4;
            out[o] = ((base[0] + weight * delta[0]) >> 6) as u8;
            out[o + 1] = ((base[1] + weight * delta[1]) >> 6) as u8;
            out[o + 2] = ((base[2] + weight * delta[2]) >> 6) as u8;
            out[o + 3] = 0xff;
        }
    }
    true
}

/// Decode one mode 3 BC7 block straight to RGBA8.
///
/// Two subsets, 7-bit RGB endpoints with a unique p-bit each, 2-bit indices,
/// opaque alpha. Same argument as [`bc7_mode1_block`].
///
/// Returns `false` if the block is not mode 3.
fn bc7_mode3_block(blk: &[u8], out: &mut [u8], pitch: usize) -> bool {
    if blk[0] & 0xf != 0x8 {
        return false;
    }
    let Ok(bytes) = <[u8; 16]>::try_from(&blk[..16]) else {
        return false;
    };
    let b = u128::from_le_bytes(bytes);

    let partition = ((b >> 4) & 0x3f) as usize;
    // 7 bits plus a unique p-bit is already 8, so no MSB replication is needed.
    let ep = |shift: u32, pbit_shift: u32| {
        ((((b >> shift) & 0x7f) as u32) << 1) | (((b >> pbit_shift) & 1) as u32)
    };
    let e = [
        [ep(10, 94), ep(38, 94), ep(66, 94)],
        [ep(17, 95), ep(45, 95), ep(73, 95)],
        [ep(24, 96), ep(52, 96), ep(80, 96)],
        [ep(31, 97), ep(59, 97), ep(87, 97)],
    ];

    let subsets = BC7_P2_SUBSET[partition];
    let fixup = BC7_P2_FIXUP[partition] as usize;
    // The index region is at most 47 bits, so it fits a `u64`. Doing the wide
    // shift once and the sixteen per-pixel shifts narrow matters: a `u128` shift
    // on x86_64 is a multi-instruction sequence, a `u64` shift is one.
    let idx = (b >> 98) as u64;

    // One multiply per channel, not two: see `bc7_mode6_block`.
    let bd = bc7_bd3(&e, 2);

    let weight_of = |p: usize| {
        let (off, w) = bc7_p2_index_at(p, fixup, 2);
        BC7_WEIGHTS2[((idx >> off) & ((1u64 << w) - 1)) as usize]
    };

    // Two pixels per store; `p` is even, so both lie in one block row.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    {
        let bdp = crate::decode::simd::pack_bd3(&bd, 2);
        for p in (0..16usize).step_by(2) {
            let (b0, d0) = bdp[((subsets >> p) & 1) as usize];
            let q = p + 1;
            let (b1, d1) = bdp[((subsets >> q) & 1) as usize];
            let o = (p / 4) * pitch + (p % 4) * 4;
            crate::decode::simd::write2(
                b0,
                d0,
                b1,
                d1,
                weight_of(p) as i16,
                weight_of(q) as i16,
                &mut out[o..o + 8],
            );
        }
    }

    #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
    {
        for p in 0..16usize {
            let weight = weight_of(p) as i32;
            let (base, delta) = &bd[((subsets >> p) & 1) as usize];
            let o = (p / 4) * pitch + (p % 4) * 4;
            out[o] = ((base[0] + weight * delta[0]) >> 6) as u8;
            out[o + 1] = ((base[1] + weight * delta[1]) >> 6) as u8;
            out[o + 2] = ((base[2] + weight * delta[2]) >> 6) as u8;
            out[o + 3] = 0xff;
        }
    }
    true
}

/// Decode one mode 7 BC7 block straight to RGBA8.
///
/// Two subsets, RGBA 5.5.5.5 endpoints with a unique p-bit per endpoint, 2-bit
/// indices. Structurally mode 3 with a real alpha channel: one index set drives
/// all four components, so the same sixteen-deep index-read chain is what the
/// general decoder is paying, and the same fix removes it.
///
/// Unlike modes 1 and 3 this mode carries alpha, so nothing can be assumed
/// opaque.
///
/// Returns `false` if the block is not mode 7.
fn bc7_mode7_block(blk: &[u8], out: &mut [u8], pitch: usize) -> bool {
    if blk[0] != 0x80 {
        return false;
    }
    let Ok(bytes) = <[u8; 16]>::try_from(&blk[..16]) else {
        return false;
    };
    let b = u128::from_le_bytes(bytes);

    let partition = ((b >> 8) & 0x3f) as usize;
    // 5 bits plus a unique p-bit is 6; shift the MSB to bit 7 and replicate the
    // top two bits down into the vacated ones.
    let ep = |shift: u32, pbit_shift: u32| {
        let v = ((((b >> shift) & 0x1f) as u32) << 1) | (((b >> pbit_shift) & 1) as u32);
        let t = v << 2;
        t | (t >> 6)
    };
    // Component-major: R0..R3, G0..G3, B0..B3, then A0..A3. The p-bit is per
    // endpoint and applies to all four components of that endpoint.
    let e = [
        [ep(14, 94), ep(34, 94), ep(54, 94), ep(74, 94)],
        [ep(19, 95), ep(39, 95), ep(59, 95), ep(79, 95)],
        [ep(24, 96), ep(44, 96), ep(64, 96), ep(84, 96)],
        [ep(29, 97), ep(49, 97), ep(69, 97), ep(89, 97)],
    ];

    let subsets = BC7_P2_SUBSET[partition];
    let fixup = BC7_P2_FIXUP[partition] as usize;
    // The index region is at most 47 bits, so it fits a `u64`. Doing the wide
    // shift once and the sixteen per-pixel shifts narrow matters: a `u128` shift
    // on x86_64 is a multi-instruction sequence, a `u64` shift is one.
    let idx = (b >> 98) as u64;

    // One multiply per channel, not two: see `bc7_mode6_block`.
    let bd = bc7_bd4(&e, 2);

    let weight_of = |p: usize| {
        let (off, w) = bc7_p2_index_at(p, fixup, 2);
        BC7_WEIGHTS2[((idx >> off) & ((1u64 << w) - 1)) as usize]
    };

    // Two pixels per store; `p` is even, so both lie in one block row.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    {
        let bdp = crate::decode::simd::pack_bd4(&bd, 2);
        for p in (0..16usize).step_by(2) {
            let (b0, d0) = bdp[((subsets >> p) & 1) as usize];
            let q = p + 1;
            let (b1, d1) = bdp[((subsets >> q) & 1) as usize];
            let o = (p / 4) * pitch + (p % 4) * 4;
            crate::decode::simd::write2(
                b0,
                d0,
                b1,
                d1,
                weight_of(p) as i16,
                weight_of(q) as i16,
                &mut out[o..o + 8],
            );
        }
    }

    #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
    {
        for p in 0..16usize {
            let weight = weight_of(p) as i32;
            let (base, delta) = &bd[((subsets >> p) & 1) as usize];
            let o = (p / 4) * pitch + (p % 4) * 4;
            out[o] = ((base[0] + weight * delta[0]) >> 6) as u8;
            out[o + 1] = ((base[1] + weight * delta[1]) >> 6) as u8;
            out[o + 2] = ((base[2] + weight * delta[2]) >> 6) as u8;
            out[o + 3] = ((base[3] + weight * delta[3]) >> 6) as u8;
        }
    }
    true
}

#[cfg(test)]
mod bc7_mode7_tests {
    use super::bc7_mode7_block;

    /// Every partition, against the general decoder. Mode 7 is the only
    /// two-subset fast path carrying alpha, so a wrong p-bit or component offset
    /// would show up in the alpha channel alone and nowhere else.
    #[test]
    fn mode7_matches_the_general_decoder() {
        let mut state = 0x0ddc_0ffe_e0dd_f00du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for partition in 0..64u8 {
            for case in 0..200 {
                let mut raw = [0u8; 16];
                match case {
                    0 => {}
                    1 => raw.iter_mut().for_each(|x| *x = 0xff),
                    _ => {
                        let (x, y) = (next(), next());
                        raw[..8].copy_from_slice(&x.to_le_bytes());
                        raw[8..].copy_from_slice(&y.to_le_bytes());
                    }
                }
                // Mode 7 occupies all eight bits of byte 0; the partition sits
                // immediately above it.
                let v = u128::from_le_bytes(raw);
                let v = (v & !(0x3fu128 << 8)) | ((partition as u128) << 8);
                let mut blk = v.to_le_bytes();
                blk[0] = 0x80;

                let mut ours = [0u8; 64];
                assert!(bc7_mode7_block(&blk, &mut ours, 16), "partition {partition}");
                let mut theirs = [0u8; 64];
                bcdec_rs::bc7(&blk, &mut theirs, 16);
                assert_eq!(
                    ours, theirs,
                    "mode 7, partition {partition}, case {case} diverged"
                );
            }
        }
    }

    /// Mode 7 must not claim any other encoding. Byte 0 is entirely the mode
    /// field here, so a stray high bit elsewhere must not be mistaken for it.
    #[test]
    fn other_modes_are_declined() {
        for mode in 0..8u32 {
            let mut blk = [0u8; 16];
            blk[0] = 1u8 << mode;
            let mut px = [0u8; 64];
            assert_eq!(bc7_mode7_block(&blk, &mut px, 16), mode == 7, "vs mode {mode}");
        }
        // The reserved encoding must be declined too.
        let mut px = [0u8; 64];
        assert!(!bc7_mode7_block(&[0u8; 16], &mut px, 16));
    }
}

/// Bit offset and width of pixel `p`s index in a single-subset mode with
/// `bits`-wide indices. Only pixel 0 is a fix-up, so this is simpler than the
/// two-subset form but serves the same purpose: independent extractions.
#[inline(always)]
fn bc7_p1_index_at(p: usize, bits: u32) -> (u32, u32) {
    let off = bits as usize * p - usize::from(p > 0);
    let w = bits - u32::from(p == 0);
    (off as u32, w)
}

/// Decode one mode 4 BC7 block straight to RGBA8.
///
/// One subset, RGB 5.5.5 with 6-bit alpha, no p-bits, a rotation, and **two**
/// index sets — a 2-bit set and a 3-bit set — with an index-selection bit
/// choosing which drives colour and which drives alpha.
///
/// This is the family my first mode 5 attempt handled badly. The lesson applied
/// here: the rotation is resolved **once**, into a channel map, rather than as a
/// conditional swap inside the per-pixel loop. A dynamic `swap` on every pixel
/// costs a branch and a pair of bounds-checked indexed accesses sixteen times
/// over, which was enough to eat the gain the independent index reads produced.
///
/// Returns `false` if the block is not mode 4.
fn bc7_mode4_block(blk: &[u8], out: &mut [u8], pitch: usize) -> bool {
    if blk[0] & 0x1f != 0x10 {
        return false;
    }
    let Ok(bytes) = <[u8; 16]>::try_from(&blk[..16]) else {
        return false;
    };
    let b = u128::from_le_bytes(bytes);

    let rotation = ((b >> 5) & 0x3) as usize;
    let isb = ((b >> 7) & 1) != 0;

    // Colour is 5 bits with no p-bit: shift the MSB to bit 7 and replicate the
    // top three bits down. Alpha is 6 bits, likewise with two.
    let c = |shift: u32| {
        let t = (((b >> shift) & 0x1f) as u32) << 3;
        t | (t >> 5)
    };
    let a = |shift: u32| {
        let t = (((b >> shift) & 0x3f) as u32) << 2;
        t | (t >> 6)
    };
    let e0 = [c(8), c(18), c(28), a(38)];
    let e1 = [c(13), c(23), c(33), a(44)];

    // Two index regions: the 2-bit set at bit 50, the 3-bit set at bit 81.
    // The index region is at most 47 bits, so it fits a `u64`. Doing the wide
    // shift once and the sixteen per-pixel shifts narrow matters: a `u128` shift
    // on x86_64 is a multi-instruction sequence, a `u64` shift is one.
    let i2 = (b >> 50) as u64;
    let i3 = (b >> 81) as u64;

    // Resolve the rotation once. `map[k]` is the output byte that computed
    // channel `k` belongs in; the rotation swaps alpha with one colour channel.
    let mut map = [0usize, 1, 2, 3];
    if rotation != 0 {
        map.swap(3, rotation - 1);
    }

    // One multiply per channel, not two: see `bc7_mode6_block`.
    let base = [
        e0[0] as i32 * 64 + 32,
        e0[1] as i32 * 64 + 32,
        e0[2] as i32 * 64 + 32,
        e0[3] as i32 * 64 + 32,
    ];
    let delta = [
        e1[0] as i32 - e0[0] as i32,
        e1[1] as i32 - e0[1] as i32,
        e1[2] as i32 - e0[2] as i32,
        e1[3] as i32 - e0[3] as i32,
    ];

    let weights = |p: usize| {
        let (o2, w2) = bc7_p1_index_at(p, 2);
        let (o3, w3) = bc7_p1_index_at(p, 3);
        let wa = BC7_WEIGHTS2[((i2 >> o2) & ((1u64 << w2) - 1)) as usize];
        let wb = BC7_WEIGHTS3[((i3 >> o3) & ((1u64 << w3) - 1)) as usize];
        // The index-selection bit decides which set drives colour and which
        // drives alpha; it does not change what the sets are.
        if isb {
            (wb, wa)
        } else {
            (wa, wb)
        }
    };

    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    {
        let (mut bo, mut dobj) = ([0i32; 4], [0i32; 4]);
        for k in 0..4 {
            bo[map[k]] = base[k];
            dobj[map[k]] = delta[k];
        }
        let (bp, dp) = (
            crate::decode::simd::pack4(bo),
            crate::decode::simd::pack4(dobj),
        );
        for p in (0..16usize).step_by(2) {
            let (c0, a0) = weights(p);
            let (c1, a1) = weights(p + 1);
            let o = (p / 4) * pitch + (p % 4) * 4;
            crate::decode::simd::write2_split(
                bp,
                dp,
                bp,
                dp,
                (c0 as i16, c1 as i16),
                (a0 as i16, a1 as i16),
                map[3],
                &mut out[o..o + 8],
            );
        }
    }

    #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
    {
        for p in 0..16usize {
            let (wc, walpha) = weights(p);
            let (wc, walpha) = (wc as i32, walpha as i32);
            let o = (p / 4) * pitch + (p % 4) * 4;
            out[o + map[0]] = ((base[0] + wc * delta[0]) >> 6) as u8;
            out[o + map[1]] = ((base[1] + wc * delta[1]) >> 6) as u8;
            out[o + map[2]] = ((base[2] + wc * delta[2]) >> 6) as u8;
            out[o + map[3]] = ((base[3] + walpha * delta[3]) >> 6) as u8;
        }
    }
    true
}

#[cfg(test)]
mod bc7_mode4_tests {
    use super::bc7_mode4_block;

    /// Every rotation crossed with both index-selection values. Those two fields
    /// interact — the selection bit decides which weight a channel takes, the
    /// rotation decides where that channel lands — so neither can be verified
    /// alone.
    #[test]
    fn mode4_matches_the_general_decoder() {
        let mut state = 0xfeed_face_dead_10ccu64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for rotation in 0..4u8 {
            for isb in 0..2u8 {
                for case in 0..3_000 {
                    let mut raw = [0u8; 16];
                    match case {
                        0 => {}
                        1 => raw.iter_mut().for_each(|x| *x = 0xff),
                        _ => {
                            let (x, y) = (next(), next());
                            raw[..8].copy_from_slice(&x.to_le_bytes());
                            raw[8..].copy_from_slice(&y.to_le_bytes());
                        }
                    }
                    // Mode 4 in the low five bits, rotation at 5..7, selection at 7.
                    raw[0] = 0x10 | (rotation << 5) | (isb << 7);

                    let mut ours = [0u8; 64];
                    assert!(
                        bc7_mode4_block(&raw, &mut ours, 16),
                        "rot {rotation} isb {isb} not recognised"
                    );
                    let mut theirs = [0u8; 64];
                    bcdec_rs::bc7(&raw, &mut theirs, 16);
                    assert_eq!(
                        ours, theirs,
                        "mode 4, rotation {rotation}, isb {isb}, case {case} diverged"
                    );
                }
            }
        }
    }

    #[test]
    fn other_modes_are_declined() {
        for mode in 0..8u32 {
            for rot in 0..4u8 {
                for isb in 0..2u8 {
                    let mut blk = [0u8; 16];
                    blk[0] = (1u8 << mode) | (rot << 5) | (isb << 7);
                    // Mode 4 occupies bits 0..5; rotation and selection live
                    // above it and must never be read as part of the mode.
                    let mut px = [0u8; 64];
                    let expect = mode == 4;
                    assert_eq!(
                        bc7_mode4_block(&blk, &mut px, 16),
                        expect,
                        "mode {mode} rot {rot} isb {isb}"
                    );
                }
            }
        }
    }
}

/// Subset assignment for BC7s 64 three-subset partitions, two bits per
/// pixel in raster order. Derived from the spec table.
const BC7_P3_SUBSET: [u32; 64] = [
    0xaa685050, 0x6a5a5040, 0x5a5a4200, 0x5450a0a8,
    0xa5a50000, 0xa0a05050, 0x5555a0a0, 0x5a5a5050,
    0xaa550000, 0xaa555500, 0xaaaa5500, 0x90909090,
    0x94949494, 0xa4a4a4a4, 0xa9a59450, 0x2a0a4250,
    0xa5945040, 0x0a425054, 0xa5a5a500, 0x55a0a0a0,
    0xa8a85454, 0x6a6a4040, 0xa4a45000, 0x1a1a0500,
    0x0050a4a4, 0xaaa59090, 0x14696914, 0x69691400,
    0xa08585a0, 0xaa821414, 0x50a4a450, 0x6a5a0200,
    0xa9a58000, 0x5090a0a8, 0xa8a09050, 0x24242424,
    0x00aa5500, 0x24924924, 0x24499224, 0x50a50a50,
    0x500aa550, 0xaaaa4444, 0x66660000, 0xa5a0a5a0,
    0x50a050a0, 0x69286928, 0x44aaaa44, 0x66666600,
    0xaa444444, 0x54a854a8, 0x95809580, 0x96969600,
    0xa85454a8, 0x80959580, 0xaa141414, 0x96960000,
    0xaaaa1414, 0xa05050a0, 0xa0a5a5a0, 0x96000000,
    0x40804080, 0xa9a8a9a8, 0xaaaaaa44, 0x2a4a5254,
];

/// Pixel indices carrying the fix-up index for subsets 1 and 2, per
/// partition. Subset 0s fix-up is always pixel 0. All three store one bit
/// fewer, so all three shorten the index stream.
const BC7_P3_ANCHOR: [[u8; 2]; 64] = [
    [ 3,15], [ 3, 8], [15, 8], [15, 3], [ 8,15], [ 3,15], [15, 3], [15, 8],
    [ 8,15], [ 8,15], [ 6,15], [ 6,15], [ 6,15], [ 5,15], [ 3,15], [ 3, 8],
    [ 3,15], [ 3, 8], [ 8,15], [15, 3], [ 3,15], [ 3, 8], [ 6,15], [10, 8],
    [ 5, 3], [ 8,15], [ 8, 6], [ 6,10], [ 8,15], [ 5,15], [15,10], [15, 8],
    [ 8,15], [15, 3], [ 3,15], [ 5,10], [ 6,10], [10, 8], [ 8, 9], [15,10],
    [15, 6], [ 3,15], [15, 8], [ 5,15], [15, 3], [15, 6], [15, 6], [15, 8],
    [ 3,15], [15, 3], [ 5,15], [ 5,15], [ 5,15], [ 8,15], [ 5,15], [10,15],
    [ 5,15], [10,15], [ 8,15], [13,15], [15, 3], [12,15], [ 3,15], [ 3, 8],
];

/// Bit offset and width of pixel `p`s index in a three-subset mode. Fix-ups sit
/// at pixel 0 and at the two anchors, and each stores one bit fewer.
#[inline(always)]
fn bc7_p3_index_at(p: usize, anchors: [u8; 2], bits: u32) -> (u32, u32) {
    let (a1, a2) = (anchors[0] as usize, anchors[1] as usize);
    let short = usize::from(p > 0) + usize::from(p > a1) + usize::from(p > a2);
    let off = bits as usize * p - short;
    let w = bits - u32::from(p == 0 || p == a1 || p == a2);
    (off as u32, w)
}

/// Decode one mode 0 BC7 block straight to RGBA8.
///
/// Three subsets, a 4-bit partition (so only the first 16 partitions are
/// reachable), RGB 4.4.4 endpoints with a unique p-bit each, 3-bit indices,
/// opaque alpha.
///
/// Three subsets means three fix-up indices rather than two, so the index stream
/// is three bits shorter than a naive layout and the general decoder is even
/// more dependent on its stateful cursor to walk it. The offsets are still
/// arithmetic, so the sixteen reads still become independent.
///
/// Returns `false` if the block is not mode 0.
fn bc7_mode0_block(blk: &[u8], out: &mut [u8], pitch: usize) -> bool {
    if blk[0] & 0x1 != 0x1 {
        return false;
    }
    let Ok(bytes) = <[u8; 16]>::try_from(&blk[..16]) else {
        return false;
    };
    let b = u128::from_le_bytes(bytes);

    // Mode 0 carries only four partition bits.
    let partition = ((b >> 1) & 0xf) as usize;
    // 4 bits plus a unique p-bit is 5; shift the MSB to bit 7 and replicate the
    // top three bits into the vacated ones.
    let ep = |shift: u32, pbit_shift: u32| {
        let v = ((((b >> shift) & 0xf) as u32) << 1) | (((b >> pbit_shift) & 1) as u32);
        let t = v << 3;
        t | (t >> 5)
    };
    // Component-major: R0..R5, G0..G5, B0..B5, then six p-bits.
    let e = [
        [ep(5, 77), ep(29, 77), ep(53, 77)],
        [ep(9, 78), ep(33, 78), ep(57, 78)],
        [ep(13, 79), ep(37, 79), ep(61, 79)],
        [ep(17, 80), ep(41, 80), ep(65, 80)],
        [ep(21, 81), ep(45, 81), ep(69, 81)],
        [ep(25, 82), ep(49, 82), ep(73, 82)],
    ];

    let subsets = BC7_P3_SUBSET[partition];
    let anchors = BC7_P3_ANCHOR[partition];
    // The index region is at most 47 bits, so it fits a `u64`. Doing the wide
    // shift once and the sixteen per-pixel shifts narrow matters: a `u128` shift
    // on x86_64 is a multi-instruction sequence, a `u64` shift is one.
    let idx = (b >> 83) as u64;

    // One multiply per channel, not two: see `bc7_mode6_block`.
    let bd = bc7_bd3(&e, 3);

    let weight_of = |p: usize| {
        let (off, w) = bc7_p3_index_at(p, anchors, 3);
        BC7_WEIGHTS3[((idx >> off) & ((1u64 << w) - 1)) as usize]
    };

    // Two pixels per store; `p` is even, so both lie in one block row.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    {
        let bdp = crate::decode::simd::pack_bd3(&bd, 3);
        for p in (0..16usize).step_by(2) {
            let (b0, d0) = bdp[((subsets >> (2 * p)) & 0x3) as usize];
            let q = p + 1;
            let (b1, d1) = bdp[((subsets >> (2 * q)) & 0x3) as usize];
            let o = (p / 4) * pitch + (p % 4) * 4;
            crate::decode::simd::write2(
                b0,
                d0,
                b1,
                d1,
                weight_of(p) as i16,
                weight_of(q) as i16,
                &mut out[o..o + 8],
            );
        }
    }

    #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
    {
        for p in 0..16usize {
            let weight = weight_of(p) as i32;
            let (base, delta) = &bd[((subsets >> (2 * p)) & 0x3) as usize];
            let o = (p / 4) * pitch + (p % 4) * 4;
            out[o] = ((base[0] + weight * delta[0]) >> 6) as u8;
            out[o + 1] = ((base[1] + weight * delta[1]) >> 6) as u8;
            out[o + 2] = ((base[2] + weight * delta[2]) >> 6) as u8;
            out[o + 3] = 0xff;
        }
    }
    true
}

/// Decode one mode 2 BC7 block straight to RGBA8.
///
/// Three subsets, a 6-bit partition, RGB 5.5.5 endpoints with no p-bits, 2-bit
/// indices, opaque alpha.
///
/// Returns `false` if the block is not mode 2.
fn bc7_mode2_block(blk: &[u8], out: &mut [u8], pitch: usize) -> bool {
    if blk[0] & 0x7 != 0x4 {
        return false;
    }
    let Ok(bytes) = <[u8; 16]>::try_from(&blk[..16]) else {
        return false;
    };
    let b = u128::from_le_bytes(bytes);

    let partition = ((b >> 3) & 0x3f) as usize;
    // 5 bits, no p-bit.
    let ep = |shift: u32| {
        let t = (((b >> shift) & 0x1f) as u32) << 3;
        t | (t >> 5)
    };
    let e = [
        [ep(9), ep(39), ep(69)],
        [ep(14), ep(44), ep(74)],
        [ep(19), ep(49), ep(79)],
        [ep(24), ep(54), ep(84)],
        [ep(29), ep(59), ep(89)],
        [ep(34), ep(64), ep(94)],
    ];

    let subsets = BC7_P3_SUBSET[partition];
    let anchors = BC7_P3_ANCHOR[partition];
    // The index region is at most 47 bits, so it fits a `u64`. Doing the wide
    // shift once and the sixteen per-pixel shifts narrow matters: a `u128` shift
    // on x86_64 is a multi-instruction sequence, a `u64` shift is one.
    let idx = (b >> 99) as u64;

    // One multiply per channel, not two: see `bc7_mode6_block`.
    let bd = bc7_bd3(&e, 3);

    let weight_of = |p: usize| {
        let (off, w) = bc7_p3_index_at(p, anchors, 2);
        BC7_WEIGHTS2[((idx >> off) & ((1u64 << w) - 1)) as usize]
    };

    // Two pixels per store; `p` is even, so both lie in one block row.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    {
        let bdp = crate::decode::simd::pack_bd3(&bd, 3);
        for p in (0..16usize).step_by(2) {
            let (b0, d0) = bdp[((subsets >> (2 * p)) & 0x3) as usize];
            let q = p + 1;
            let (b1, d1) = bdp[((subsets >> (2 * q)) & 0x3) as usize];
            let o = (p / 4) * pitch + (p % 4) * 4;
            crate::decode::simd::write2(
                b0,
                d0,
                b1,
                d1,
                weight_of(p) as i16,
                weight_of(q) as i16,
                &mut out[o..o + 8],
            );
        }
    }

    #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
    {
        for p in 0..16usize {
            let weight = weight_of(p) as i32;
            let (base, delta) = &bd[((subsets >> (2 * p)) & 0x3) as usize];
            let o = (p / 4) * pitch + (p % 4) * 4;
            out[o] = ((base[0] + weight * delta[0]) >> 6) as u8;
            out[o + 1] = ((base[1] + weight * delta[1]) >> 6) as u8;
            out[o + 2] = ((base[2] + weight * delta[2]) >> 6) as u8;
            out[o + 3] = 0xff;
        }
    }
    true
}

#[cfg(test)]
mod bc7_p3_tests {
    use super::{bc7_mode0_block, bc7_mode2_block, BC7_P3_ANCHOR, BC7_P3_SUBSET};

    /// Both three-subset fast paths, against the general decoder, over every
    /// partition each mode can address. Mode 0 has a 4-bit partition field and
    /// so reaches only the first 16; mode 2 reaches all 64.
    #[test]
    fn three_subset_modes_match_the_general_decoder() {
        for &(mode, mask, set, pshift, pbits) in
            &[(0u32, 0x1u8, 0x1u8, 1u32, 4u32), (2, 0x7, 0x4, 3, 6)]
        {
            let mut state = 0x1234_9876_abcd_5678u64 ^ mode as u64;
            let mut next = move || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state
            };
            let partitions = 1u32 << pbits;
            for partition in 0..partitions {
                for case in 0..200 {
                    let mut raw = [0u8; 16];
                    match case {
                        0 => {}
                        1 => raw.iter_mut().for_each(|x| *x = 0xff),
                        _ => {
                            let (x, y) = (next(), next());
                            raw[..8].copy_from_slice(&x.to_le_bytes());
                            raw[8..].copy_from_slice(&y.to_le_bytes());
                        }
                    }
                    let pmask = ((1u128 << pbits) - 1) << pshift;
                    let v = u128::from_le_bytes(raw);
                    let v = (v & !pmask) | ((partition as u128) << pshift);
                    let mut blk = v.to_le_bytes();
                    blk[0] = (blk[0] & !mask) | set;

                    let mut ours = [0u8; 64];
                    let claimed = if mode == 0 {
                        bc7_mode0_block(&blk, &mut ours, 16)
                    } else {
                        bc7_mode2_block(&blk, &mut ours, 16)
                    };
                    assert!(claimed, "mode {mode} partition {partition} not recognised");

                    let mut theirs = [0u8; 64];
                    bcdec_rs::bc7(&blk, &mut theirs, 16);
                    assert_eq!(
                        ours, theirs,
                        "mode {mode}, partition {partition}, case {case} diverged"
                    );
                }
            }
        }
    }

    #[test]
    fn other_modes_are_declined() {
        for mode in 0..8u32 {
            let mut blk = [0u8; 16];
            blk[0] = 1u8 << mode;
            let mut px = [0u8; 64];
            assert_eq!(bc7_mode0_block(&blk, &mut px, 16), mode == 0, "m0 vs {mode}");
            assert_eq!(bc7_mode2_block(&blk, &mut px, 16), mode == 2, "m2 vs {mode}");
        }
    }

    /// The three-subset tables must be the specs, not merely self-consistent.
    #[test]
    fn partition_tables_are_the_spec_tables() {
        assert_eq!(BC7_P3_SUBSET[0], 0xaa68_5050);
        assert_eq!(BC7_P3_SUBSET[1], 0x6a5a_5040);
        // Subset 0 always owns pixel 0.
        assert!(BC7_P3_SUBSET.iter().all(|m| m & 0x3 == 0));
        for (i, m) in BC7_P3_SUBSET.iter().enumerate() {
            // All three subsets must actually be used, and none may be 3.
            let mut seen = [false; 4];
            for p in 0..16 {
                seen[((m >> (2 * p)) & 0x3) as usize] = true;
            }
            assert!(seen[0] && seen[1] && seen[2], "partition {i} misses a subset");
            assert!(!seen[3], "partition {i} uses subset 3");
            // Each anchor must belong to the subset it anchors.
            let [a1, a2] = BC7_P3_ANCHOR[i];
            assert_eq!((m >> (2 * a1 as u32)) & 0x3, 1, "partition {i} anchor 1");
            assert_eq!((m >> (2 * a2 as u32)) & 0x3, 2, "partition {i} anchor 2");
        }
    }
}

/// Decode one mode 5 BC7 block straight to RGBA8.
///
/// One subset, RGB 7.7.7 with a separate 8-bit alpha, no p-bits, a rotation, and
/// two independent 2-bit index sets — one driving colour, one driving alpha.
///
/// An earlier attempt at this mode measured neutral and was reverted as a
/// refutation of the whole approach. That was wrong twice over. The approach is
/// sound — every other mode gained 18-73% from it — and the earlier code was
/// slow for a specific reason: it resolved the rotation with a conditional
/// `swap` **inside** the per-pixel loop, paying a branch and two bounds-checked
/// indexed accesses sixteen times per block. Mode 4, the same family, gained 73%
/// once the rotation was hoisted into a channel map computed once. That is what
/// this does.
///
/// Returns `false` if the block is not mode 5.
fn bc7_mode5_block(blk: &[u8], out: &mut [u8], pitch: usize) -> bool {
    // Five zero bits then a one; bits 6-7 are the rotation, so only the low six
    // bits identify the mode.
    if blk[0] & 0x3f != 0x20 {
        return false;
    }
    let Ok(bytes) = <[u8; 16]>::try_from(&blk[..16]) else {
        return false;
    };
    let b = u128::from_le_bytes(bytes);

    let rotation = ((b >> 6) & 0x3) as usize;
    // Colour is 7 bits: shift the MSB to bit 7 and replicate it into the vacated
    // low bit. Alpha is already 8 bits.
    let c = |shift: u32| {
        let t = (((b >> shift) & 0x7f) as u32) << 1;
        t | (t >> 7)
    };
    let a = |shift: u32| ((b >> shift) & 0xff) as u32;
    let e0 = [c(8), c(22), c(36), a(50)];
    let e1 = [c(15), c(29), c(43), a(58)];

    // Two 31-bit index regions: colour at bit 66, alpha at bit 97.
    // The index region is at most 47 bits, so it fits a `u64`. Doing the wide
    // shift once and the sixteen per-pixel shifts narrow matters: a `u128` shift
    // on x86_64 is a multi-instruction sequence, a `u64` shift is one.
    let ci = (b >> 66) as u64;
    let ai = (b >> 97) as u64;

    // Resolve the rotation once, into an output-byte map.
    let mut map = [0usize, 1, 2, 3];
    if rotation != 0 {
        map.swap(3, rotation - 1);
    }

    // One multiply per channel, not two: see `bc7_mode6_block`.
    let base = [
        e0[0] as i32 * 64 + 32,
        e0[1] as i32 * 64 + 32,
        e0[2] as i32 * 64 + 32,
        e0[3] as i32 * 64 + 32,
    ];
    let delta = [
        e1[0] as i32 - e0[0] as i32,
        e1[1] as i32 - e0[1] as i32,
        e1[2] as i32 - e0[2] as i32,
        e1[3] as i32 - e0[3] as i32,
    ];

    let weights = |p: usize| {
        let (off, w) = bc7_p1_index_at(p, 2);
        let mask = (1u64 << w) - 1;
        (
            BC7_WEIGHTS2[((ci >> off) & mask) as usize],
            BC7_WEIGHTS2[((ai >> off) & mask) as usize],
        )
    };

    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    {
        // Permute base/delta into output order so the rotation is resolved
        // before the vector op, leaving only the alpha lane to name.
        let (mut bo, mut dobj) = ([0i32; 4], [0i32; 4]);
        for k in 0..4 {
            bo[map[k]] = base[k];
            dobj[map[k]] = delta[k];
        }
        let (bp, dp) = (
            crate::decode::simd::pack4(bo),
            crate::decode::simd::pack4(dobj),
        );
        for p in (0..16usize).step_by(2) {
            let (c0, a0) = weights(p);
            let (c1, a1) = weights(p + 1);
            let o = (p / 4) * pitch + (p % 4) * 4;
            crate::decode::simd::write2_split(
                bp,
                dp,
                bp,
                dp,
                (c0 as i16, c1 as i16),
                (a0 as i16, a1 as i16),
                map[3],
                &mut out[o..o + 8],
            );
        }
    }

    #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
    {
        for p in 0..16usize {
            let (wc, wa) = weights(p);
            let (wc, wa) = (wc as i32, wa as i32);
            let o = (p / 4) * pitch + (p % 4) * 4;
            out[o + map[0]] = ((base[0] + wc * delta[0]) >> 6) as u8;
            out[o + map[1]] = ((base[1] + wc * delta[1]) >> 6) as u8;
            out[o + map[2]] = ((base[2] + wc * delta[2]) >> 6) as u8;
            out[o + map[3]] = ((base[3] + wa * delta[3]) >> 6) as u8;
        }
    }
    true
}

#[cfg(test)]
mod bc7_mode5_tests {
    use super::bc7_mode5_block;

    /// All four rotations. The rotation permutes channels on the way out, so it
    /// is exactly where a hoisted channel map could silently disagree with a
    /// per-pixel swap.
    #[test]
    fn mode5_matches_the_general_decoder() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for rotation in 0..4u8 {
            for case in 0..10_000 {
                let mut raw = [0u8; 16];
                match case {
                    0 => {}
                    1 => raw.iter_mut().for_each(|x| *x = 0xff),
                    _ => {
                        let (x, y) = (next(), next());
                        raw[..8].copy_from_slice(&x.to_le_bytes());
                        raw[8..].copy_from_slice(&y.to_le_bytes());
                    }
                }
                raw[0] = 0x20 | (rotation << 6);

                let mut ours = [0u8; 64];
                assert!(bc7_mode5_block(&raw, &mut ours, 16), "rot {rotation}");
                let mut theirs = [0u8; 64];
                bcdec_rs::bc7(&raw, &mut theirs, 16);
                assert_eq!(
                    ours, theirs,
                    "mode 5, rotation {rotation}, case {case} diverged"
                );
            }
        }
    }

    #[test]
    fn other_modes_are_declined() {
        for mode in 0..8u32 {
            for rot in 0..4u8 {
                let mut blk = [0u8; 16];
                blk[0] = (1u8 << mode) | (rot << 6);
                let mut px = [0u8; 64];
                assert_eq!(
                    bc7_mode5_block(&blk, &mut px, 16),
                    mode == 5,
                    "mode {mode} rot {rot}"
                );
            }
        }
    }
}

#[cfg(test)]
mod bc7_p2_tests {
    use super::{bc7_mode1_block, bc7_mode3_block, BC7_P2_FIXUP, BC7_P2_SUBSET};

    /// Both two-subset fast paths must match the general decoder bit for bit on
    /// every one of the 64 partitions, which is where a wrong subset mask or a
    /// wrong fix-up anchor would hide.
    #[test]
    fn two_subset_modes_match_the_general_decoder() {
        for &(mode, mask, set) in &[(1u32, 0x3u8, 0x2u8), (3, 0xf, 0x8)] {
            let mut state = 0xdead_beef_cafe_f00du64 ^ mode as u64;
            let mut next = move || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state
            };
            for partition in 0..64u8 {
                for case in 0..200 {
                    let mut raw = [0u8; 16];
                    match case {
                        0 => {}
                        1 => raw.iter_mut().for_each(|x| *x = 0xff),
                        _ => {
                            let (x, y) = (next(), next());
                            raw[..8].copy_from_slice(&x.to_le_bytes());
                            raw[8..].copy_from_slice(&y.to_le_bytes());
                        }
                    }
                    // Mode in the low bits, then the 6-bit partition field.
                    let pshift = mode + 1;
                    let v = u128::from_le_bytes(raw);
                    let v = (v & !(0x3fu128 << pshift)) | ((partition as u128) << pshift);
                    let mut blk = v.to_le_bytes();
                    blk[0] = (blk[0] & !mask) | set;

                    let mut ours = [0u8; 64];
                    let claimed = if mode == 1 {
                        bc7_mode1_block(&blk, &mut ours, 16)
                    } else {
                        bc7_mode3_block(&blk, &mut ours, 16)
                    };
                    assert!(claimed, "mode {mode} partition {partition} not recognised");

                    let mut theirs = [0u8; 64];
                    bcdec_rs::bc7(&blk, &mut theirs, 16);
                    assert_eq!(
                        ours, theirs,
                        "mode {mode}, partition {partition}, case {case} diverged"
                    );
                }
            }
        }
    }

    #[test]
    fn other_modes_are_declined() {
        for mode in 0..8u32 {
            let mut blk = [0u8; 16];
            blk[0] = 1u8 << mode;
            let mut px = [0u8; 64];
            assert_eq!(bc7_mode1_block(&blk, &mut px, 16), mode == 1, "m1 vs {mode}");
            assert_eq!(bc7_mode3_block(&blk, &mut px, 16), mode == 3, "m3 vs {mode}");
        }
    }

    /// The generated tables must be the specs, not merely self-consistent.
    #[test]
    fn partition_tables_are_the_spec_tables() {
        assert_eq!(BC7_P2_SUBSET[0], 0xcccc);
        assert_eq!(BC7_P2_SUBSET[1], 0x8888);
        assert_eq!(BC7_P2_SUBSET[2], 0xeeee);
        // Subset 0 always owns pixel 0, so bit 0 is never set.
        assert!(BC7_P2_SUBSET.iter().all(|m| m & 1 == 0));
        // Every partition must actually use both subsets.
        assert!(BC7_P2_SUBSET.iter().all(|&m| m != 0));
        // The anchor must belong to subset 1.
        for (i, &f) in BC7_P2_FIXUP.iter().enumerate() {
            assert!((1..=15).contains(&f), "partition {i} anchor {f}");
            assert_eq!(
                (BC7_P2_SUBSET[i] >> f) & 1,
                1,
                "partition {i} anchor not in subset 1"
            );
        }
    }
}

/// BC7 interpolation weights for 4-bit indices.
const BC7_WEIGHTS4: [u32; 16] = [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64];

/// Decode one **mode 6** BC7 block straight to RGBA8.
///
/// Mode 6 is 87% of the blocks a real encoder emits, and it is by far the
/// simplest shape BC7 has: one subset, so no partition-table lookup and no
/// per-pixel subset branch; RGBA 7.7.7.7 endpoints with one p-bit each; and
/// sixteen 4-bit indices packed contiguously. The generic decoder pays a
/// bitstream reader, a partition lookup and an index-width branch **per pixel**
/// to handle the other seven modes. Here all of that is loop-invariant.
///
/// Returns `false` if the block is not mode 6, leaving `out` untouched, so the
/// caller falls back to the general decoder.
///
/// # Where this shows up, and where it does not
///
/// Measured ABAB against the general decoder, serial, into a recycled buffer:
///
/// | surface | general | mode-6 path | |
/// |---|---:|---:|---|
/// | 1024^2 | 707-771 Mpx/s | 727-811 Mpx/s | no change |
/// | 256^2 | 201-206 | 235-242 | **+17%** |
/// | 128^2 | 200-203 | 242-258 | **+24%** |
/// | 64^2 | 196-220 | 254-261 | **+23%** |
///
/// At 1024^2 BC7 decode is **memory-bandwidth bound** — it scales only 3.7x on
/// 24 cores — so saving ALU work cannot show up there whatever it saves. The
/// gain is real only once the surface fits in cache. That is not a small case:
/// a full mip chain is mostly small surfaces, so a streamer decoding chains
/// spends most of its decode time in exactly this range.
fn bc7_mode6_block(blk: &[u8], out: &mut [u8], pitch: usize) -> bool {
    // Mode is unary: `mode` zero bits then a one. Mode 6 is 0x40 exactly.
    if blk[0] != 0x40 {
        return false;
    }
    let Ok(bytes) = <[u8; 16]>::try_from(&blk[..16]) else {
        return false;
    };
    let b = u128::from_le_bytes(bytes);

    // Eight 7-bit components at bit 7, then two p-bits at 63 and 64. Each
    // endpoint component is its 7 bits with the subset's p-bit appended as the
    // low bit, giving the 8-bit value the spec interpolates on.
    let f = |shift: u32| ((b >> shift) & 0x7f) as u32;
    let p0 = ((b >> 63) & 1) as u32;
    let p1 = ((b >> 64) & 1) as u32;
    let e0 = [
        (f(7) << 1) | p0,
        (f(21) << 1) | p0,
        (f(35) << 1) | p0,
        (f(49) << 1) | p0,
    ];
    let e1 = [
        (f(14) << 1) | p1,
        (f(28) << 1) | p1,
        (f(42) << 1) | p1,
        (f(56) << 1) | p1,
    ];

    // The spec interpolation is `(e0 * (64 - w) + e1 * w + 32) >> 6`, which is
    // two multiplies per channel. Rearranged:
    //
    //     e0*64 + 32 + w*(e1 - e0)
    //
    // it is one multiply against a base and a delta that are constant for the
    // whole block. Sixteen pixels x four channels means 128 multiplies become
    // 64, and the base/delta pair is computed once. Identical arithmetic, so
    // identical output — the oracle test covers that.
    let base = [
        (e0[0] as i32) * 64 + 32,
        (e0[1] as i32) * 64 + 32,
        (e0[2] as i32) * 64 + 32,
        (e0[3] as i32) * 64 + 32,
    ];
    let delta = [
        e1[0] as i32 - e0[0] as i32,
        e1[1] as i32 - e0[1] as i32,
        e1[2] as i32 - e0[2] as i32,
        e1[3] as i32 - e0[3] as i32,
    ];

    // Indices occupy bits 65..128. The first is the fix-up index and carries one
    // less bit, its high bit being implicitly zero.
    // Narrowed to `u64` for consistency with the other modes. Unlike them it is
    // NOT a speedup here: mode 6's shift amounts are compile-time constants in an
    // unrolled loop, so LLVM had already folded them.
    //
    // Two further attempts on this mode were REFUTED, both by measurement:
    //
    // * Normalising the fix-up index away, so all sixteen indices are uniformly
    //   four bits and the `i == 0` branch disappears: eight samples per arm,
    //   321.9 vs 326.8 Mpx/s. The branch was constant-folded too, so this cost
    //   three real operations to remove one that did not exist.
    // * Vectorising the weight lookup at all. Replacing the entire per-pixel
    //   lookup with a constant — the absolute ceiling — measured 345.7 Mpx/s
    //   against 336.9 with it. **The whole weight extraction is worth ~2.5%.**
    //   There is nothing here to win.
    let idx = (b >> 65) as u64;
    let weight = |i: usize| {
        if i == 0 {
            BC7_WEIGHTS4[(idx & 0x7) as usize]
        } else {
            BC7_WEIGHTS4[((idx >> (3 + (i - 1) * 4)) & 0xf) as usize]
        }
    };

    // Two pixels per store: sixteen-bit lanes hold eight channels, and `i` is
    // even so pixels `i` and `i + 1` are always adjacent within one block row.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    {
        let (bp, dp) = (crate::decode::simd::pack4(base), crate::decode::simd::pack4(delta));
        for i in (0..16usize).step_by(2) {
            let o = (i / 4) * pitch + (i % 4) * 4;
            crate::decode::simd::write2(
                bp,
                dp,
                bp,
                dp,
                weight(i) as i16,
                weight(i + 1) as i16,
                &mut out[o..o + 8],
            );
        }
    }

    #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
    {
        for i in 0..16usize {
            let w = weight(i) as i32;
            let o = (i / 4) * pitch + (i % 4) * 4;
            out[o] = ((base[0] + w * delta[0]) >> 6) as u8;
            out[o + 1] = ((base[1] + w * delta[1]) >> 6) as u8;
            out[o + 2] = ((base[2] + w * delta[2]) >> 6) as u8;
            out[o + 3] = ((base[3] + w * delta[3]) >> 6) as u8;
        }
    }
    true
}

#[cfg(test)]
mod bc7_mode6_tests {
    use super::{bc7_mode6_block, BC7_WEIGHTS4};

    /// The fast path must agree with the general decoder **bit for bit** on
    /// every mode-6 block, including the endpoint and index extremes where an
    /// off-by-one in a bit offset would otherwise hide.
    #[test]
    fn mode6_matches_the_general_decoder() {
        let mut state = 0x243f_6a88_85a3_08d3u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        for case in 0..20_000 {
            let mut blk = [0u8; 16];
            match case {
                // Pin the corners: all-zero payload, all-ones payload.
                0 => {}
                1 => blk.iter_mut().for_each(|b| *b = 0xff),
                _ => {
                    let (a, b) = (next(), next());
                    blk[..8].copy_from_slice(&a.to_le_bytes());
                    blk[8..].copy_from_slice(&b.to_le_bytes());
                }
            }
            blk[0] = 0x40; // force mode 6

            let mut ours = [0u8; 64];
            assert!(bc7_mode6_block(&blk, &mut ours, 16), "case {case}: not recognised");

            let mut theirs = [0u8; 64];
            bcdec_rs::bc7(&blk, &mut theirs, 16);

            assert_eq!(
                ours, theirs,
                "case {case}: mode-6 fast path diverged
  block {blk:02x?}"
            );
        }
    }

    /// Anything that is not mode 6 must be declined, not mis-decoded.
    #[test]
    fn other_modes_are_declined() {
        for mode in 0..8u32 {
            let mut blk = [0u8; 16];
            blk[0] = 1 << mode;
            let mut px = [0u8; 64];
            assert_eq!(
                bc7_mode6_block(&blk, &mut px, 16),
                mode == 6,
                "mode {mode} handled incorrectly"
            );
        }
        // The reserved encoding (no set bit in byte 0) must also be declined.
        let mut px = [0u8; 64];
        assert!(!bc7_mode6_block(&[0u8; 16], &mut px, 16));
    }

    #[test]
    fn weights_are_the_spec_table() {
        assert_eq!(BC7_WEIGHTS4[0], 0);
        assert_eq!(BC7_WEIGHTS4[15], 64);
        assert!(BC7_WEIGHTS4.windows(2).all(|w| w[0] < w[1]));
    }
}
