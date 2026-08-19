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
            bcdec_rs::bc7(&data[bi..bi + 16], &mut out[offset..], pitch);
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
            bcdec_rs::bc7(&data[bi..bi + 16], &mut scratch, 16);
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
                        bcdec_rs::bc7(&data[bi..bi + 16], &mut band[offset..], pitch);
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
