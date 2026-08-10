//! Bit-exact reference tiling using `bcdec_rs` directly (tests + benches).
//!
//! Mirrors [`super::decode_surface_pixels`] so A/B compares the same work.

use super::super::content::{slice_payload_bytes, DecodeContent};
use crate::error::Error;

/// Decode with the same expansion rules as `rusty_dds`, calling `bcdec_rs` per block.
pub fn reference_rgba8(
    kind: DecodeContent,
    data: &[u8],
    width: u32,
    height: u32,
    depth: u32,
) -> Result<Vec<u8>, Error> {
    if depth == 0 {
        return Err(Error::InvalidField("zero depth".into()));
    }
    if depth == 1 {
        return reference_slice(kind, data, width, height);
    }
    let slice_bytes = slice_payload_bytes(kind, width, height)?;
    let need = slice_bytes
        .checked_mul(depth as usize)
        .ok_or(Error::OutOfBounds)?;
    if data.len() < need {
        return Err(Error::TruncatedData);
    }
    let mut out = Vec::new();
    for z in 0..depth as usize {
        let start = z * slice_bytes;
        out.extend(reference_slice(
            kind,
            &data[start..start + slice_bytes],
            width,
            height,
        )?);
    }
    Ok(out)
}

fn reference_slice(
    kind: DecodeContent,
    data: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<u8>, Error> {
    match kind {
        DecodeContent::Bc1 => tile_rgba(data, width, height, 8, |b, o, p| {
            bcdec_rs::bc1(b, o, p)
        }),
        DecodeContent::Bc2 => tile_rgba(data, width, height, 16, |b, o, p| {
            bcdec_rs::bc2(b, o, p)
        }),
        DecodeContent::Bc3 => tile_rgba(data, width, height, 16, |b, o, p| {
            bcdec_rs::bc3(b, o, p)
        }),
        DecodeContent::Bc7 => tile_rgba(data, width, height, 16, |b, o, p| {
            bcdec_rs::bc7(b, o, p)
        }),
        DecodeContent::Bc4UNorm => tile_r_to_rgba(data, width, height, false),
        DecodeContent::Bc4SNorm => tile_r_to_rgba(data, width, height, true),
        DecodeContent::Bc5UNorm => tile_rg_to_rgba(data, width, height, false),
        DecodeContent::Bc5SNorm => tile_rg_to_rgba(data, width, height, true),
        DecodeContent::Rgba8 => {
            let n = slice_payload_bytes(kind, width, height)?;
            if data.len() < n {
                return Err(Error::TruncatedData);
            }
            Ok(data[..n].to_vec())
        }
        DecodeContent::Bgra8 => {
            let n = slice_payload_bytes(kind, width, height)?;
            if data.len() < n {
                return Err(Error::TruncatedData);
            }
            let mut out = Vec::with_capacity(n);
            for px in data[..n].chunks_exact(4) {
                out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
            Ok(out)
        }
    }
}

fn tile_rgba(
    data: &[u8],
    width: u32,
    height: u32,
    block_bytes: usize,
    decode_block: impl Fn(&[u8], &mut [u8], usize),
) -> Result<Vec<u8>, Error> {
    let blocks_x = (width as usize + 3) / 4;
    let blocks_y = (height as usize + 3) / 4;
    let expected = blocks_x * blocks_y * block_bytes;
    if data.len() < expected {
        return Err(Error::TruncatedData);
    }
    let mut out = vec![0u8; width as usize * height as usize * 4];
    let mut block_out = [0u8; 64];
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let bi = (by * blocks_x + bx) * block_bytes;
            decode_block(&data[bi..bi + block_bytes], &mut block_out, 16);
            let px0 = bx * 4;
            let py0 = by * 4;
            let copy_w = 4.min(width as usize - px0);
            let copy_h = 4.min(height as usize - py0);
            for row in 0..copy_h {
                let src = row * 16;
                let dst = ((py0 + row) * width as usize + px0) * 4;
                out[dst..dst + copy_w * 4].copy_from_slice(&block_out[src..src + copy_w * 4]);
            }
        }
    }
    Ok(out)
}

fn tile_r_to_rgba(
    data: &[u8],
    width: u32,
    height: u32,
    is_signed: bool,
) -> Result<Vec<u8>, Error> {
    let blocks_x = (width as usize + 3) / 4;
    let blocks_y = (height as usize + 3) / 4;
    let expected = blocks_x * blocks_y * 8;
    if data.len() < expected {
        return Err(Error::TruncatedData);
    }
    let mut r = vec![0u8; width as usize * height as usize];
    let mut block_out = [0u8; 16];
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let bi = (by * blocks_x + bx) * 8;
            bcdec_rs::bc4(&data[bi..bi + 8], &mut block_out, 4, is_signed);
            let px0 = bx * 4;
            let py0 = by * 4;
            let copy_w = 4.min(width as usize - px0);
            let copy_h = 4.min(height as usize - py0);
            for row in 0..copy_h {
                let src = row * 4;
                let dst = (py0 + row) * width as usize + px0;
                r[dst..dst + copy_w].copy_from_slice(&block_out[src..src + copy_w]);
            }
        }
    }
    let mut out = Vec::with_capacity(r.len() * 4);
    for v in r {
        out.extend_from_slice(&[v, 0, 0, 255]);
    }
    Ok(out)
}

fn tile_rg_to_rgba(
    data: &[u8],
    width: u32,
    height: u32,
    is_signed: bool,
) -> Result<Vec<u8>, Error> {
    let blocks_x = (width as usize + 3) / 4;
    let blocks_y = (height as usize + 3) / 4;
    let expected = blocks_x * blocks_y * 16;
    if data.len() < expected {
        return Err(Error::TruncatedData);
    }
    let mut rg = vec![0u8; width as usize * height as usize * 2];
    let mut block_out = [0u8; 32];
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let bi = (by * blocks_x + bx) * 16;
            bcdec_rs::bc5(&data[bi..bi + 16], &mut block_out, 8, is_signed);
            let px0 = bx * 4;
            let py0 = by * 4;
            let copy_w = 4.min(width as usize - px0);
            let copy_h = 4.min(height as usize - py0);
            for row in 0..copy_h {
                let src = row * 8;
                let dst = ((py0 + row) * width as usize + px0) * 2;
                rg[dst..dst + copy_w * 2].copy_from_slice(&block_out[src..src + copy_w * 2]);
            }
        }
    }
    let n = width as usize * height as usize;
    let mut out = Vec::with_capacity(n * 4);
    for i in 0..n {
        out.extend_from_slice(&[rg[i * 2], rg[i * 2 + 1], 0, 255]);
    }
    Ok(out)
}
