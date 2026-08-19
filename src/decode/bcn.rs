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
    decode_rgba_blocks_into(data, width, height, 8, out, |block, dst, pitch| {
        bcdec_rs::bc1(block, dst, pitch);
    })
}

pub fn decode_bc2(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>, Error> {
    alloc_and_decode(data, width, height, 16, |o| {
        decode_bc2_into(data, width, height, o)
    })
}

pub fn decode_bc2_into(data: &[u8], width: u32, height: u32, out: &mut [u8]) -> Result<(), Error> {
    decode_rgba_blocks_into(data, width, height, 16, out, |block, dst, pitch| {
        bcdec_rs::bc2(block, dst, pitch);
    })
}

pub fn decode_bc3(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>, Error> {
    alloc_and_decode(data, width, height, 16, |o| {
        decode_bc3_into(data, width, height, o)
    })
}

pub fn decode_bc3_into(data: &[u8], width: u32, height: u32, out: &mut [u8]) -> Result<(), Error> {
    decode_rgba_blocks_into(data, width, height, 16, out, |block, dst, pitch| {
        bcdec_rs::bc3(block, dst, pitch);
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
    let mut block_r = [0u8; 16];

    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let bi = (by * blocks_x + bx) * 8;
            bcdec_rs::bc4(&data[bi..bi + 8], &mut block_r, 4, is_signed);
            blit_r_to_rgba(&block_r, out, out_w, out_h, bx * 4, by * 4);
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
    let mut block_rg = [0u8; 32];

    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let bi = (by * blocks_x + bx) * 16;
            bcdec_rs::bc5(&data[bi..bi + 16], &mut block_rg, 8, is_signed);
            blit_rg_to_rgba(&block_rg, out, out_w, out_h, bx * 4, by * 4);
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

#[inline]
fn blit_r_to_rgba(block_r: &[u8; 16], out: &mut [u8], out_w: usize, out_h: usize, px0: usize, py0: usize) {
    let copy_w = 4.min(out_w - px0);
    let copy_h = 4.min(out_h - py0);
    for row in 0..copy_h {
        for col in 0..copy_w {
            let v = block_r[row * 4 + col];
            let dst = ((py0 + row) * out_w + px0 + col) * 4;
            out[dst] = v;
            out[dst + 1] = 0;
            out[dst + 2] = 0;
            out[dst + 3] = 255;
        }
    }
}

#[inline]
fn blit_rg_to_rgba(
    block_rg: &[u8; 32],
    out: &mut [u8],
    out_w: usize,
    out_h: usize,
    px0: usize,
    py0: usize,
) {
    let copy_w = 4.min(out_w - px0);
    let copy_h = 4.min(out_h - py0);
    for row in 0..copy_h {
        for col in 0..copy_w {
            let src = row * 8 + col * 2;
            let dst = ((py0 + row) * out_w + px0 + col) * 4;
            out[dst] = block_rg[src];
            out[dst + 1] = block_rg[src + 1];
            out[dst + 2] = 0;
            out[dst + 3] = 255;
        }
    }
}

// --------------------------------------------------------------- BC7 mode 6

// A mode-5 fast path was written, verified bit-identical to the general decoder
// across all four rotations x 10 000 randomised blocks, and REFUTED on speed:
// neutral at every size, even on a synthetic surface where 100% of blocks are
// mode 5 (128^2: 157.2 vs 158.9 Mpx/s; 256^2: 158.9 vs 164.3, four ABBA samples
// each). Mode 5 is only ~9% of real blocks, so it could never have shown in a
// whole-surface measurement either. Whatever mode 6 gains below, it is not
// "specialisation" in general — do not assume modes 1/3 will pay without
// measuring them the same way.

/// Dispatch a block to whichever specialised decoder claims it.
///
/// Ordered by measured share: mode 6 is ~88% of blocks, mode 1 the next largest
/// two-subset mode. Returns `false` when no fast path applies, so the caller
/// falls back to the general decoder.
#[inline]
fn bc7_fast_block(blk: &[u8], out: &mut [u8], pitch: usize) -> bool {
    bc7_mode6_block(blk, out, pitch)
        || bc7_mode1_block(blk, out, pitch)
        || bc7_mode3_block(blk, out, pitch)
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
#[inline]
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
    let idx = b >> 82;

    for p in 0..16usize {
        let (off, w) = bc7_p2_index_at(p, fixup, 3);
        let weight = BC7_WEIGHTS3[((idx >> off) & ((1u128 << w) - 1)) as usize];
        let s = (((subsets >> p) & 1) as usize) * 2;
        let (a, c) = (&e[s], &e[s + 1]);
        let o = (p / 4) * pitch + (p % 4) * 4;
        out[o] = ((a[0] * (64 - weight) + c[0] * weight + 32) >> 6) as u8;
        out[o + 1] = ((a[1] * (64 - weight) + c[1] * weight + 32) >> 6) as u8;
        out[o + 2] = ((a[2] * (64 - weight) + c[2] * weight + 32) >> 6) as u8;
        out[o + 3] = 0xff;
    }
    true
}

/// Decode one mode 3 BC7 block straight to RGBA8.
///
/// Two subsets, 7-bit RGB endpoints with a unique p-bit each, 2-bit indices,
/// opaque alpha. Same argument as [`bc7_mode1_block`].
///
/// Returns `false` if the block is not mode 3.
#[inline]
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
    let idx = b >> 98;

    for p in 0..16usize {
        let (off, w) = bc7_p2_index_at(p, fixup, 2);
        let weight = BC7_WEIGHTS2[((idx >> off) & ((1u128 << w) - 1)) as usize];
        let s = (((subsets >> p) & 1) as usize) * 2;
        let (a, c) = (&e[s], &e[s + 1]);
        let o = (p / 4) * pitch + (p % 4) * 4;
        out[o] = ((a[0] * (64 - weight) + c[0] * weight + 32) >> 6) as u8;
        out[o + 1] = ((a[1] * (64 - weight) + c[1] * weight + 32) >> 6) as u8;
        out[o + 2] = ((a[2] * (64 - weight) + c[2] * weight + 32) >> 6) as u8;
        out[o + 3] = 0xff;
    }
    true
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
#[inline]
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

    // Indices occupy bits 65..128. The first is the fix-up index and carries one
    // less bit, its high bit being implicitly zero.
    let idx = b >> 65;
    for i in 0..16usize {
        let w = if i == 0 {
            BC7_WEIGHTS4[(idx & 0x7) as usize]
        } else {
            BC7_WEIGHTS4[((idx >> (3 + (i - 1) * 4)) & 0xf) as usize]
        };
        let iw = 64 - w;
        let o = (i / 4) * pitch + (i % 4) * 4;
        out[o] = ((e0[0] * iw + e1[0] * w + 32) >> 6) as u8;
        out[o + 1] = ((e0[1] * iw + e1[1] * w + 32) >> 6) as u8;
        out[o + 2] = ((e0[2] * iw + e1[2] * w + 32) >> 6) as u8;
        out[o + 3] = ((e0[3] * iw + e1[3] * w + 32) >> 6) as u8;
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
