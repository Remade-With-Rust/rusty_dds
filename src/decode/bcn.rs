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
const BC7_PARALLEL_MIN_BLOCKS: usize = 4096;

pub fn decode_bc1(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>, Error> {
    decode_rgba_blocks(data, width, height, 8, |block, dst, pitch| {
        bcdec_rs::bc1(block, dst, pitch);
    })
}

pub fn decode_bc2(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>, Error> {
    decode_rgba_blocks(data, width, height, 16, |block, dst, pitch| {
        bcdec_rs::bc2(block, dst, pitch);
    })
}

pub fn decode_bc3(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>, Error> {
    decode_rgba_blocks(data, width, height, 16, |block, dst, pitch| {
        bcdec_rs::bc3(block, dst, pitch);
    })
}

pub fn decode_bc4(
    data: &[u8],
    width: u32,
    height: u32,
    is_signed: bool,
) -> Result<Vec<u8>, Error> {
    let (blocks_x, blocks_y, expected) = block_grid(width, height, 8)?;
    if data.len() < expected {
        return Err(Error::TruncatedData);
    }
    let out_w = width as usize;
    let out_h = height as usize;
    let mut out = vec![0u8; out_w * out_h * 4];
    let mut block_r = [0u8; 16];

    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let bi = (by * blocks_x + bx) * 8;
            bcdec_rs::bc4(&data[bi..bi + 8], &mut block_r, 4, is_signed);
            blit_r_to_rgba(&block_r, &mut out, out_w, out_h, bx * 4, by * 4);
        }
    }
    Ok(out)
}

pub fn decode_bc5(
    data: &[u8],
    width: u32,
    height: u32,
    is_signed: bool,
) -> Result<Vec<u8>, Error> {
    let (blocks_x, blocks_y, expected) = block_grid(width, height, 16)?;
    if data.len() < expected {
        return Err(Error::TruncatedData);
    }
    let out_w = width as usize;
    let out_h = height as usize;
    let mut out = vec![0u8; out_w * out_h * 4];
    let mut block_rg = [0u8; 32];

    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let bi = (by * blocks_x + bx) * 16;
            bcdec_rs::bc5(&data[bi..bi + 16], &mut block_rg, 8, is_signed);
            blit_rg_to_rgba(&block_rg, &mut out, out_w, out_h, bx * 4, by * 4);
        }
    }
    Ok(out)
}

pub fn decode_bc7(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>, Error> {
    let (blocks_x, blocks_y, expected) = block_grid(width, height, 16)?;
    if data.len() < expected {
        return Err(Error::TruncatedData);
    }
    let out_w = width as usize;
    let out_h = height as usize;
    let mut out = vec![0u8; out_w * out_h * 4];

    let aligned = width.is_multiple_of(4) && height.is_multiple_of(4);
    let parallel = aligned
        && blocks_y >= 2
        && blocks_x.saturating_mul(blocks_y) >= BC7_PARALLEL_MIN_BLOCKS;

    if parallel {
        decode_bc7_parallel(data, &mut out, out_w, blocks_x, blocks_y);
    } else if aligned {
        decode_bc7_direct(data, &mut out, out_w, blocks_x, blocks_y);
    } else {
        decode_bc7_scratch(data, &mut out, out_w, out_h, blocks_x, blocks_y);
    }
    Ok(out)
}

fn decode_rgba_blocks(
    data: &[u8],
    width: u32,
    height: u32,
    block_bytes: usize,
    decode_block: impl Fn(&[u8], &mut [u8], usize),
) -> Result<Vec<u8>, Error> {
    let (blocks_x, blocks_y, expected) = block_grid(width, height, block_bytes)?;
    if data.len() < expected {
        return Err(Error::TruncatedData);
    }
    let out_w = width as usize;
    let out_h = height as usize;
    let mut out = vec![0u8; out_w * out_h * 4];
    let pitch = out_w * 4;

    if width.is_multiple_of(4) && height.is_multiple_of(4) {
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
                blit_rgba4(&scratch, &mut out, out_w, out_h, bx * 4, by * 4);
            }
        }
    }
    Ok(out)
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
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, blocks_y);

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
