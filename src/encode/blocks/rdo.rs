//! Rate-distortion-optimized BC1 (Oodle-Texture-class, v1).
//!
//! BCn payloads ship inside an LZ archive (Star Citizen's `.p4k` is
//! zip/deflate), so the real rate of a block is not its fixed 8 bytes but
//! how well those bytes MATCH earlier ones. This pass re-chooses blocks
//! among LZ-friendlier candidates under a Lagrangian:
//!
//!     J = SSE  -  lambda * estimated_bytes_saved
//!
//! Candidates per block, all legal BC1 by construction (conformance is
//! free; only the rate/quality point moves):
//!   - reuse the PREVIOUS block wholesale        (8-byte match)
//!   - reuse a recent block's INDEX bytes,
//!     endpoints re-fit optimally by LS          (4-byte match)
//!   - reuse a recent block's ENDPOINT bytes,
//!     indices re-fit exactly                    (4-byte match)
//!
//! lambda = 0 disables the pass (byte-identical to the normal path).
//! The window runs in scan order; RDO encodes serially (cook-for-
//! distribution is a batch job — determinism over parallelism here).

use super::*;

const WINDOW: usize = 16;

/// Estimated deflate bytes saved by each substitution class. Coarse by
/// design: deflate emits a (len, dist) pair for a match; an 8-byte match
/// saves roughly 6-7 literal bytes, a 4-byte region roughly 2-3. The
/// lambda sweep absorbs the constant.
const SAVE_WHOLE: f32 = 7.0;
const SAVE_PART: f32 = 2.5;

#[derive(PartialEq, Clone, Copy)]
enum Class {
    Base,
    Whole,
    Table,
    Endpoints,
}

pub(crate) fn encode_image_bc1_rdo(
    rgba: &[u8],
    width: u32,
    height: u32,
    lambda: f32,
    out: &mut [u8],
) -> Result<(), Error> {
    let w = width as usize;
    let h = height as usize;
    if rgba.len() < w * h * 4 {
        return Err(Error::TruncatedData);
    }
    let blocks_x = (w + 3) / 4;
    let blocks_y = (h + 3) / 4;
    let need = blocks_x
        .checked_mul(blocks_y)
        .and_then(|n| n.checked_mul(8))
        .ok_or(Error::OutOfBounds)?;
    if out.len() < need {
        return Err(Error::TruncatedData);
    }

    // Pass 1 - global dictionary: encode every block normally, histogram
    // the index tables, keep the most popular DICT_N as global candidates.
    // The baseline blocks are kept and reused by pass 2 (no re-encode).
    let (dict, base_blocks) = build_table_dict(rgba, w, h, blocks_x, blocks_y);

    // Ring buffers of recently emitted structures.
    let mut recent_tables: [u32; WINDOW] = [0; WINDOW];
    let mut recent_eps: [(u16, u16); WINDOW] = [(0, 0); WINDOW];
    let mut filled = 0usize;
    let mut prev_block = [0u8; 8];

    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let pixels = gather_block(rgba, w, h, bx, by);

            // Baseline: the normal quality path (from pass 1).
            let base = base_blocks[by * blocks_x + bx];
            let base_err = bc1_block_sse(&pixels, &base);

            let mut best = base;
            let mut best_j = base_err as f32;
            let mut best_class = Class::Base;

            if filled > 0 {
                // 1. Whole previous block.
                let lim = (best_j + lambda * SAVE_WHOLE).ceil() as i32;
                if lim > 0 {
                    if let Some(err) = bc1_block_sse_limited(&pixels, &prev_block, lim) {
                        let j = err as f32 - lambda * SAVE_WHOLE;
                        if j < best_j {
                            best_j = j;
                            best = prev_block;
                            best_class = Class::Whole;
                        }
                    }
                }

                let n = filled.min(WINDOW);
                for k in 0..n {
                    // 2. Reuse index table, LS-refit endpoints.
                    let table = recent_tables[k];
                    let lim = (best_j + lambda * SAVE_PART).ceil() as i32;
                    if lim > 0 {
                        if let Some(cand) = refit_endpoints_for_table(&pixels, table) {
                            if let Some(err) = bc1_block_sse_limited(&pixels, &cand, lim) {
                                let j = err as f32 - lambda * SAVE_PART;
                                if j < best_j {
                                    best_j = j;
                                    best = cand;
                                    best_class = Class::Table;
                                }
                            }
                        }
                    }
                    // 3. Reuse endpoints, re-fit indices.
                    let (c0, c1) = recent_eps[k];
                    let lim = (best_j + lambda * SAVE_PART).ceil() as i32;
                    if c0 > c1 && lim > 0 {
                        if let Some((blk, err)) =
                            pack_bc1_scored_565(&pixels, c0, c1, lim)
                        {
                            let j = err as f32 - lambda * SAVE_PART;
                            if j < best_j {
                                best_j = j;
                                best = blk;
                                best_class = Class::Endpoints;
                            }
                        }
                    }
                }
                // 4. Global popular tables (two-pass dictionary): the whole
                // image converges on the same few 4-byte index strings.
                for &table in dict.iter() {
                    if recent_tables[..n].contains(&table) {
                        continue; // already tried via the window
                    }
                    let lim = (best_j + lambda * SAVE_PART).ceil() as i32;
                    if lim <= 0 {
                        break;
                    }
                    if let Some(cand) = refit_endpoints_for_table(&pixels, table) {
                        if let Some(err) = bc1_block_sse_limited(&pixels, &cand, lim) {
                            let j = err as f32 - lambda * SAVE_PART;
                            if j < best_j {
                                best_j = j;
                                best = cand;
                                best_class = Class::Table;
                            }
                        }
                    }
                }
            }

            // Endpoint polish for table-reuse winners: the 4 index bytes must
            // stay matched, but the endpoint bytes are literals anyway - the
            // 565 contract lattice recovers quality at ZERO rate cost.
            if best_class == Class::Table {
                polish_endpoints_fixed_table(&pixels, &mut best);
            }
            let _ = best_class;

            let oi = (by * blocks_x + bx) * 8;
            out[oi..oi + 8].copy_from_slice(&best);
            prev_block = best;
            let slot = (by * blocks_x + bx) % WINDOW;
            recent_tables[slot] =
                u32::from_le_bytes([best[4], best[5], best[6], best[7]]);
            recent_eps[slot] = (
                u16::from_le_bytes([best[0], best[1]]),
                u16::from_le_bytes([best[2], best[3]]),
            );
            filled += 1;
        }
    }
    Ok(())
}

/// Like `bc1_block_sse` but aborts once the partial sum reaches `limit`
/// (a candidate at or past the limit can never be accepted).
fn bc1_block_sse_limited(pixels: &[[u8; 4]; 16], block: &[u8; 8], limit: i32) -> Option<i32> {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let a = from_565(c0);
    let b = from_565(c1);
    let colors = if c0 > c1 {
        [a, b, lerp_rgb(a, b, 2, 1), lerp_rgb(a, b, 1, 2)]
    } else {
        [a, b, lerp_rgb(a, b, 1, 1), [0, 0, 0]]
    };
    let table = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    let mut err = 0i32;
    for (i, p) in pixels.iter().enumerate() {
        let idx = ((table >> (2 * i)) & 3) as usize;
        err += sqr_rgb([p[0], p[1], p[2]], colors[idx]);
        if err >= limit {
            return None;
        }
    }
    Some(err)
}

/// Decode-true SSE of an arbitrary BC1 block against source pixels
/// (both 4-color and punch modes, matching the decoder's mode rule).
fn bc1_block_sse(pixels: &[[u8; 4]; 16], block: &[u8; 8]) -> i32 {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let a = from_565(c0);
    let b = from_565(c1);
    let colors = if c0 > c1 {
        [a, b, lerp_rgb(a, b, 2, 1), lerp_rgb(a, b, 1, 2)]
    } else {
        [a, b, lerp_rgb(a, b, 1, 1), [0, 0, 0]]
    };
    let table = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    let mut err = 0i32;
    for (i, p) in pixels.iter().enumerate() {
        let idx = ((table >> (2 * i)) & 3) as usize;
        err += sqr_rgb([p[0], p[1], p[2]], colors[idx]);
    }
    err
}

/// Given a FIXED index table, solve LS endpoints in RGB, quantize to 565,
/// and emit a 4-color block carrying exactly that table. Returns None for
/// degenerate weight layouts or when quantized endpoints collapse into the
/// punch-mode ordering (which would reinterpret the table).
fn refit_endpoints_for_table(pixels: &[[u8; 4]; 16], table: u32) -> Option<[u8; 8]> {
    // 4-color weights toward c1 by index: 0 -> 0, 1 -> 1, 2 -> 1/3, 3 -> 2/3.
    const W: [f32; 4] = [0.0, 1.0, 1.0 / 3.0, 2.0 / 3.0];
    let mut a00 = 0f32;
    let mut a01 = 0f32;
    let mut a11 = 0f32;
    let mut b0 = [0f32; 3];
    let mut b1 = [0f32; 3];
    for (i, p) in pixels.iter().enumerate() {
        let wgt = W[((table >> (2 * i)) & 3) as usize];
        let u = 1.0 - wgt;
        a00 += u * u;
        a01 += u * wgt;
        a11 += wgt * wgt;
        for c in 0..3 {
            let x = p[c] as f32;
            b0[c] += u * x;
            b1[c] += wgt * x;
        }
    }
    let det = a00 * a11 - a01 * a01;
    if det.abs() < 1e-4 {
        return None;
    }
    let mut e0 = [0u8; 3];
    let mut e1 = [0u8; 3];
    for c in 0..3 {
        e0[c] = ((a11 * b0[c] - a01 * b1[c]) / det).round().clamp(0.0, 255.0) as u8;
        e1[c] = ((a00 * b1[c] - a01 * b0[c]) / det).round().clamp(0.0, 255.0) as u8;
    }
    let q0 = to_565(e0);
    let q1 = to_565(e1);
    if q0 <= q1 {
        return None; // would flip to punch mode and reinterpret the table
    }
    let mut out = [0u8; 8];
    out[0..2].copy_from_slice(&q0.to_le_bytes());
    out[2..4].copy_from_slice(&q1.to_le_bytes());
    out[4..8].copy_from_slice(&table.to_le_bytes());
    Some(out)
}

const DICT_N: usize = 24;

/// Pass 1: histogram the baseline encoder index tables, return the most
/// popular DICT_N (the global match dictionary for pass 2).
fn build_table_dict(
    rgba: &[u8],
    w: usize,
    h: usize,
    blocks_x: usize,
    blocks_y: usize,
) -> (Vec<u32>, Vec<[u8; 8]>) {
    use std::collections::HashMap;
    let mut counts: HashMap<u32, u32> = HashMap::new();
    let mut blocks = Vec::with_capacity(blocks_x * blocks_y);
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let pixels = gather_block(rgba, w, h, bx, by);
            let blk = encode_bc1_bytes(pixels);
            let c0 = u16::from_le_bytes([blk[0], blk[1]]);
            let c1 = u16::from_le_bytes([blk[2], blk[3]]);
            if c0 > c1 {
                let t = u32::from_le_bytes([blk[4], blk[5], blk[6], blk[7]]);
                *counts.entry(t).or_insert(0) += 1;
            }
            blocks.push(blk);
        }
    }
    let mut v: Vec<(u32, u32)> = counts.into_iter().collect();
    v.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let dict = v
        .into_iter()
        .take(DICT_N)
        .filter(|&(_, n)| n >= 2)
        .map(|(t, _)| t)
        .collect();
    (dict, blocks)
}

/// +-1 contract moves on the 565 endpoints with the index table HELD FIXED
/// (the table bytes are the LZ match; endpoints are literals either way).
fn polish_endpoints_fixed_table(pixels: &[[u8; 4]; 16], block: &mut [u8; 8]) {
    let mut err = bc1_block_sse(pixels, block);
    for _round in 0..2 {
        let c0 = u16::from_le_bytes([block[0], block[1]]);
        let c1 = u16::from_le_bytes([block[2], block[3]]);
        if c0 <= c1 {
            return;
        }
        let prev = err;
        for (base_is_c0, d) in [(true, -1i32), (false, 1i32)] {
            for (shift, maxv) in [(11u16, 31u16), (5, 63), (0, 31)] {
                let c0n = u16::from_le_bytes([block[0], block[1]]);
                let c1n = u16::from_le_bytes([block[2], block[3]]);
                let base = if base_is_c0 { c0n } else { c1n };
                let cur = (base >> shift) & maxv;
                let nv = cur as i32 + d;
                if nv < 0 || nv > maxv as i32 {
                    continue;
                }
                let cand = (base & !(maxv << shift)) | ((nv as u16) << shift);
                let (n0, n1) = if base_is_c0 { (cand, c1n) } else { (c0n, cand) };
                if n0 <= n1 {
                    continue; // must stay 4-color or the table reinterprets
                }
                let mut trial = *block;
                trial[0..2].copy_from_slice(&n0.to_le_bytes());
                trial[2..4].copy_from_slice(&n1.to_le_bytes());
                let e = bc1_block_sse(pixels, &trial);
                if e < err {
                    err = e;
                    *block = trial;
                }
            }
        }
        if err >= prev {
            break;
        }
    }
}

