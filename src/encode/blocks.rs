//! Per-block BCn encoders (pure Rust).
//!
//! Quality / speed profile (vs DirectXTex):
//! 1. BC4/5 — decoder-matched palettes; unique/axis dispatch; LS + neighborhood search-skip;
//!    signed path scores UNORM recon (scoreboard domain), not SNORM SSE
//! 2. BC1–3 — luminance seed; chroma second seed only when colorful
//! 3. BC7 mode 6 — variance-gated seed menu; LS refine only the winner
//! 4. Strip-parallel encode when block count ≥ 4096 (same threshold as BC7 decode)

use std::cell::Cell;

use crate::error::Error;
use super::tuning::{
    alpha_sel_enabled, bc1_lattice_min_err, bc1_lattice_rounds, bc7_m1_min_err,
    unsigned_window_enabled,
};

mod m1;
mod rdo;
#[cfg(feature = "simd")]
mod simd;

pub(crate) use rdo::encode_image_bc1_rdo;
#[cfg(feature = "decode")]
pub(crate) use rdo::encode_image_bc7_rdo;

/// Encode effort vs speed. Default [`EncodeQuality::Quality`] is the corpus bake-off path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum EncodeQuality {
    /// Full adaptive search (unique-pairs, LS, neighborhood with search-skip).
    #[default]
    Quality,
    /// Dual min/max + LS only — no unique-pairs / neighborhood (cook-fast).
    Fast,
}

thread_local! {
    static QUALITY: Cell<EncodeQuality> = Cell::new(EncodeQuality::Quality);
}

pub(crate) fn with_quality<R>(q: EncodeQuality, f: impl FnOnce() -> R) -> R {
    QUALITY.with(|c| {
        let prev = c.replace(q);
        let out = f();
        c.set(prev);
        out
    })
}

#[inline]
fn quality_is_fast() -> bool {
    QUALITY.with(|c| c.get() == EncodeQuality::Fast)
}

/// Spawn strips only when the work is large enough to pay for them.
///
/// 4096 was inherited from BC7 decode and never validated for encode. Measured:
/// the crossover is **~512 blocks**, eight times lower, and the old value was
/// forcing every surface between the two onto one thread. Paired wall-clock,
/// BC7, pinned, threshold 512 against 4096:
///
/// | blocks | verdict |
/// |---|---|
/// | 256 | both serial, unchanged |
/// | 529 | flat — this is the crossover |
/// | 1024 | **+23.6%**, 10/10, z = +3.16 |
/// | 2116 | **+44.9%**, 10/10, z = +3.16 |
/// | 4096 | both parallel, unchanged |
///
/// Below ~256 blocks threading measurably *loses* (0.54x at 144), so this is set
/// above the break-even rather than at it. It applies per surface, so it governs
/// each mip level independently — a 512^2 mip chain has three levels in the band
/// this fixes.
const ENCODE_PARALLEL_MIN_BLOCKS: usize = 512;

pub fn encode_image(
    rgba: &[u8],
    width: u32,
    height: u32,
    block_bytes: usize,
    encode_block: impl Fn([[u8; 4]; 16], &mut [u8]) + Sync,
    out: &mut [u8],
) -> Result<(), Error> {
    let blocks_x = (width as usize + 3) / 4;
    let blocks_y = (height as usize + 3) / 4;
    let expected = blocks_x
        .checked_mul(blocks_y)
        .and_then(|n| n.checked_mul(block_bytes))
        .ok_or(Error::OutOfBounds)?;
    if out.len() < expected {
        return Err(Error::TruncatedData);
    }
    let w = width as usize;
    let h = height as usize;
    if rgba.len() < w * h * 4 {
        return Err(Error::TruncatedData);
    }
    debug_assert!(block_bytes <= 16);

    let nblocks = blocks_x.saturating_mul(blocks_y);
    if blocks_y >= 2 && nblocks >= ENCODE_PARALLEL_MIN_BLOCKS {
        encode_image_parallel(rgba, w, h, blocks_x, blocks_y, block_bytes, encode_block, out);
    } else {
        encode_image_serial(rgba, w, h, blocks_x, blocks_y, block_bytes, encode_block, out);
    }
    Ok(())
}

fn encode_image_serial(
    rgba: &[u8],
    w: usize,
    h: usize,
    blocks_x: usize,
    blocks_y: usize,
    block_bytes: usize,
    encode_block: impl Fn([[u8; 4]; 16], &mut [u8]),
    out: &mut [u8],
) {
    let mut scratch = [0u8; 16];
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let pixels = gather_block(rgba, w, h, bx, by);
            let slot = &mut scratch[..block_bytes];
            encode_block(pixels, slot);
            let oi = (by * blocks_x + bx) * block_bytes;
            out[oi..oi + block_bytes].copy_from_slice(slot);
        }
    }
}

fn encode_image_parallel(
    rgba: &[u8],
    w: usize,
    h: usize,
    blocks_x: usize,
    blocks_y: usize,
    block_bytes: usize,
    encode_block: impl Fn([[u8; 4]; 16], &mut [u8]) + Sync,
    out: &mut [u8],
) {
    let row_bytes = blocks_x * block_bytes;
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, blocks_y);

    let mut ranges: Vec<(usize, usize)> = Vec::with_capacity(workers);
    let base = blocks_y / workers;
    let extra = blocks_y % workers;
    let mut start = 0;
    for wi in 0..workers {
        let len = base + usize::from(wi < extra);
        ranges.push((start, start + len));
        start += len;
    }

    // Propagate encode quality into worker threads (thread-local is per-thread).
    let q = QUALITY.with(|c| c.get());

    std::thread::scope(|scope| {
        let mut rest = out;
        for &(by0, by1) in &ranges {
            let band_len = (by1 - by0) * row_bytes;
            let (band, tail) = rest.split_at_mut(band_len);
            rest = tail;
            let encode_block = &encode_block;
            scope.spawn(move || {
                with_quality(q, || {
                    let mut scratch = [0u8; 16];
                    for by in by0..by1 {
                        let local = by - by0;
                        for bx in 0..blocks_x {
                            let pixels = gather_block(rgba, w, h, bx, by);
                            let slot = &mut scratch[..block_bytes];
                            encode_block(pixels, slot);
                            let oi = local * row_bytes + bx * block_bytes;
                            band[oi..oi + block_bytes].copy_from_slice(slot);
                        }
                    }
                });
            });
        }
        debug_assert!(rest.is_empty());
    });
}

#[inline]
fn gather_block(rgba: &[u8], w: usize, h: usize, bx: usize, by: usize) -> [[u8; 4]; 16] {
    let x0 = bx * 4;
    let y0 = by * 4;
    if x0 + 4 <= w && y0 + 4 <= h {
        // Interior block: four contiguous 16-byte row copies, no per-pixel clamps.
        let mut pixels = [[0u8; 4]; 16];
        for row in 0..4 {
            let src = ((y0 + row) * w + x0) * 4;
            for col in 0..4 {
                let i = src + col * 4;
                pixels[row * 4 + col] = [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]];
            }
        }
        return pixels;
    }
    let mut pixels = [[0u8, 0, 0, 255]; 16];
    for row in 0..4 {
        for col in 0..4 {
            let x = x0 + col;
            let y = y0 + row;
            let sx = x.min(w.saturating_sub(1));
            let sy = y.min(h.saturating_sub(1));
            let i = (sy * w + sx) * 4;
            pixels[row * 4 + col] = [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]];
        }
    }
    pixels
}

/// Global span of one RGBA channel (for surface-level BC4/5 fast path).
pub fn channel_span(rgba: &[u8], width: u32, height: u32, channel: usize) -> u8 {
    let w = width as usize;
    let h = height as usize;
    let mut lo = 255u8;
    let mut hi = 0u8;
    for y in 0..h {
        let row = y * w * 4;
        for x in 0..w {
            let v = rgba[row + x * 4 + channel];
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    hi.saturating_sub(lo)
}

// ---------------------------------------------------------------------------
// BC1 / BC2 / BC3

// ---------------------------------------------------------------------------
// Format encoders. Split by format so the hot paths stay readable; every item
// is crate-internal, and `blocks` re-exports them so the submodules can reach
// each other through `use super::*` exactly as they did when this was one file.
// ---------------------------------------------------------------------------

mod alpha;
mod bc1;
mod bc7;
#[cfg(test)]
mod oracles;

pub(crate) use alpha::*;
pub(crate) use bc1::*;
pub(crate) use bc7::*;


fn to_565(c: [u8; 3]) -> u16 {
    let r = (c[0] as u16 >> 3) & 31;
    let g = (c[1] as u16 >> 2) & 63;
    let b = (c[2] as u16 >> 3) & 31;
    (r << 11) | (g << 5) | b
}

fn from_565(c: u16) -> [u8; 3] {
    let r = ((c >> 11) & 31) as u8;
    let g = ((c >> 5) & 63) as u8;
    let b = (c & 31) as u8;
    [(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2)]
}

fn lerp_rgb(a: [u8; 3], b: [u8; 3], aw: u32, bw: u32) -> [u8; 3] {
    [
        ((aw * a[0] as u32 + bw * b[0] as u32) / (aw + bw)) as u8,
        ((aw * a[1] as u32 + bw * b[1] as u32) / (aw + bw)) as u8,
        ((aw * a[2] as u32 + bw * b[2] as u32) / (aw + bw)) as u8,
    ]
}

#[cfg(test)]
fn pack_indices_2bit(pixels: &[[u8; 4]; 16], colors: &[[u8; 3]; 4], alpha_punch: bool) -> u32 {
    let mut table = 0u32;
    for (i, p) in pixels.iter().enumerate() {
        let idx = if alpha_punch && p[3] < 128 {
            3
        } else {
            let mut best = 0usize;
            let mut best_d = i32::MAX;
            for (j, c) in colors.iter().enumerate() {
                let d = sqr_rgb([p[0], p[1], p[2]], *c);
                if d < best_d {
                    best_d = d;
                    best = j;
                }
            }
            best
        };
        table |= (idx as u32) << (2 * i);
    }
    table
}

fn sqr_rgb(a: [u8; 3], b: [u8; 3]) -> i32 {
    let mut s = 0i32;
    for i in 0..3 {
        let d = a[i] as i32 - b[i] as i32;
        s += d * d;
    }
    s
}

#[derive(Default)]
struct BitWriter {
    low: u64,
    high: u64,
    pos: u32,
}

impl BitWriter {
    fn write_bits(&mut self, value: u32, n: u32) {
        debug_assert!(n <= 32);
        let mask = if n == 32 {
            u64::MAX
        } else {
            (1u64 << n) - 1
        };
        let v = (value as u64) & mask;
        if self.pos < 64 {
            self.low |= v << self.pos;
            if self.pos + n > 64 {
                let overflow = self.pos + n - 64;
                self.high |= v >> (n - overflow);
            }
        } else {
            self.high |= v << (self.pos - 64);
        }
        self.pos += n;
    }

    fn into_array(self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0..8].copy_from_slice(&self.low.to_le_bytes());
        out[8..16].copy_from_slice(&self.high.to_le_bytes());
        out
    }
}

/// Runtime AVX2 check, re-exported for encoders outside this module.
#[cfg(all(feature = "simd", target_arch = "x86_64"))]
pub(crate) fn simd_avx2() -> bool {
    simd::has_avx2()
}
