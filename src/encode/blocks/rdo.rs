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

    // Previous ROW of emitted blocks: vertical repetition is the dominant
    // long-range structure in textures, and deflate's 32KB window covers a
    // full block row at any sane width.
    let mut prev_row: Vec<[u8; 8]> = vec![[0u8; 8]; blocks_x];
    let mut cur_row: Vec<[u8; 8]> = vec![[0u8; 8]; blocks_x];
    // Ring buffers of recently emitted structures.
    let mut recent_blocks: [[u8; 8]; WINDOW] = [[0u8; 8]; WINDOW];
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

            if base_err == 0 {
                let oi = (by * blocks_x + bx) * 8;
                out[oi..oi + 8].copy_from_slice(&base);
                prev_block = base;
                cur_row[bx] = base;
                let slot = (by * blocks_x + bx) % WINDOW;
                recent_blocks[slot] = base;
                recent_tables[slot] = u32::from_le_bytes([base[4], base[5], base[6], base[7]]);
                recent_eps[slot] = (
                    u16::from_le_bytes([base[0], base[1]]),
                    u16::from_le_bytes([base[2], base[3]]),
                );
                filled += 1;
                continue;
            }
            let mut best = base;
            // The baseline may ALREADY repeat naturally — credit it, or a
            // substitution can book phantom savings while destroying real
            // ones (computer_key: payload GREW 5% before this correction).
            let n0 = filled.min(WINDOW);
            let above: Option<&[u8; 8]> = if by > 0 { Some(&prev_row[bx]) } else { None };
            let mut base_score = score_bc1(&base, &recent_blocks[..n0]);
            if let Some(ab) = above {
                if ab == &base {
                    base_score = SAVE_WHOLE;
                } else if (ab[4..8] == base[4..8] || ab[0..4] == base[0..4])
                    && base_score < SAVE_PART
                {
                    base_score = SAVE_PART;
                }
            }
            // Activity masking via allowance scaling (see the BC7 note).
            let lam = lambda * (base_err as f32 / 192.0).min(1.0);
            let mut best_j = base_err as f32 - lam * base_score;
            let mut best_class = Class::Base;

            if filled > 0 {
                // 1. Whole previous block.
                let lim = (best_j + lam * SAVE_WHOLE).ceil() as i32;
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
                    let lim = (best_j + lam * SAVE_PART).ceil() as i32;
                    if lim > 0 {
                        if let Some(cand) = refit_endpoints_for_table(&pixels, table) {
                            if let Some(err) = bc1_block_sse_limited(&pixels, &cand, lim) {
                                let j = err as f32 - lam * SAVE_PART;
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
                    let lim = (best_j + lam * SAVE_PART).ceil() as i32;
                    if c0 > c1 && lim > 0 {
                        if let Some((blk, err)) =
                            pack_bc1_scored_565(&pixels, c0, c1, lim)
                        {
                            let j = err as f32 - lam * SAVE_PART;
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
                    let lim = (best_j + lam * SAVE_PART).ceil() as i32;
                    if lim <= 0 {
                        break;
                    }
                    if let Some(cand) = refit_endpoints_for_table(&pixels, table) {
                        if let Some(err) = bc1_block_sse_limited(&pixels, &cand, lim) {
                            let j = err as f32 - lam * SAVE_PART;
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
            cur_row[bx] = best;
            let slot = (by * blocks_x + bx) % WINDOW;
            recent_blocks[slot] = best;
            recent_tables[slot] =
                u32::from_le_bytes([best[4], best[5], best[6], best[7]]);
            recent_eps[slot] = (
                u16::from_le_bytes([best[0], best[1]]),
                u16::from_le_bytes([best[2], best[3]]),
            );
            filled += 1;
        }
        std::mem::swap(&mut prev_row, &mut cur_row);
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

// ---------------------------------------------------------------------------
// BC7 RDO (mode-6 structured reuse + any-mode whole-block reuse).
//
// Mode-6 bit layout (from pack_bc7_mode6): bits 0..6 mode, 7..62 endpoints
// (r0 r1 g0 g1 b0 b1 a0 a1, 7 bits each), 63 p0, 64 p1, 65..127 indices
// (anchor 3 bits + 15x4). Byte halves therefore split cleanly:
//   head = bytes 0..8  = mode + endpoints + p0
//   tail = bytes 8..16 = p1 + all index bits
// Reusing a donor's tail keeps an 8-byte LZ match while our endpoints are
// LS-refit under the donor's indices; reusing a head keeps the donor's
// endpoints while our indices are refit (rejected if the anchor would force
// an endpoint swap, which would rewrite the head).
// ---------------------------------------------------------------------------

const SAVE_WHOLE16: f32 = 14.0;
const SAVE_HALF8: f32 = 6.0;
const BC7_WINDOW: usize = 16;

#[cfg(feature = "decode")]
pub(crate) fn encode_image_bc7_rdo(
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
        .and_then(|n| n.checked_mul(16))
        .ok_or(Error::OutOfBounds)?;
    if out.len() < need {
        return Err(Error::TruncatedData);
    }

    let mut recent: [([u8; 16], bool); BC7_WINDOW] = [([0u8; 16], false); BC7_WINDOW];
    let mut prev_row: Vec<[u8; 16]> = vec![[0u8; 16]; blocks_x];
    let mut cur_row: Vec<[u8; 16]> = vec![[0u8; 16]; blocks_x];
    let mut filled = 0usize;
    let mut prev_block = [0u8; 16];

    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let pixels = gather_block(rgba, w, h, bx, by);

            let mut base = [0u8; 16];
            encode_bc7_mode6(pixels, &mut base);
            let base_err = bc7_block_sse(&pixels, &base);

            // Exact blocks are untouchable: preservation is structural,
            // not an emergent property of the acceptance math.
            if base_err == 0 {
                let oi = (by * blocks_x + bx) * 16;
                out[oi..oi + 16].copy_from_slice(&base);
                prev_block = base;
                cur_row[bx] = base;
                let slot = (by * blocks_x + bx) % BC7_WINDOW;
                recent[slot] = (base, base[0] & 0x7F == 0x40);
                filled += 1;
                continue;
            }
            let mut best = base;
            let n0 = filled.min(BC7_WINDOW);
            let above: Option<&[u8; 16]> = if by > 0 { Some(&prev_row[bx]) } else { None };
            let mut base_score = score_bc7(&base, &recent[..n0]);
            if let Some(ab) = above {
                if ab == &base {
                    base_score = SAVE_WHOLE16;
                } else if (ab[8..16] == base[8..16] || ab[0..8] == base[0..8])
                    && base_score < SAVE_HALF8
                {
                    base_score = SAVE_HALF8;
                }
            }
            // Activity masking done as ALLOWANCE SCALING: the Lagrangian
            // budget a block may spend scales with the error it already
            // carries (lambda_eff = lambda * min(1, base_err/T)). Pristine
            // blocks get ~zero budget, so per-block nicks cannot compound
            // into map-level dB loss on smooth content; busy blocks (where
            // error hides) trade at full lambda.
            let lam = lambda * (base_err as f32 / 256.0).min(1.0);
            let mut best_j = base_err as f32 - lam * base_score;

            if filled > 0 {
                // 1. Whole previous block + the block one ROW above.
                let mut wholes: [Option<[u8; 16]>; 2] = [Some(prev_block), None];
                if let Some(ab) = above {
                    wholes[1] = Some(*ab);
                }
                for cand in wholes.into_iter().flatten() {
                    let err = bc7_block_sse(&pixels, &cand);
                    let j = err as f32 - lam * SAVE_WHOLE16;
                    if j < best_j {
                        best_j = j;
                        best = cand;
                    }
                }

                let n = filled.min(BC7_WINDOW);
                for k in 0..n {
                    let (donor, is_m6) = recent[k];
                    if !is_m6 {
                        continue;
                    }
                    let Some((dq0, dp0, dq1, dp1, didx)) = parse_mode6(&donor) else {
                        continue;
                    };
                    // 2. Tail reuse: donor p1 + indices, our endpoints by LS.
                    if let Some((e0, e1)) = ls_endpoints_mode6(&pixels, &didx) {
                        let q0 = quantize_7p_fixed(e0, dp0_choice(&pixels, e0));
                        let q1 = quantize_7p_fixed(e1, dp1);
                        // p0 is ours (head byte); try both cheaply via helper.
                        let (mut q0a, p0a) = q0;
                        let mut q1a = q1.0;
                        // Endpoint polish with indices FIXED: the tail bytes
                        // are the LZ match, head endpoint bytes are literals
                        // either way — ±1 moves recover quality for free.
                        let mut err = mode6_sse(&pixels, q0a, p0a, q1a, dp1, &didx);
                        polish_mode6_endpoints(
                            &pixels, &mut q0a, p0a, &mut q1a, dp1, &didx, &mut err,
                        );
                        let cand = pack_bc7_mode6(q0a, p0a, q1a, dp1, didx);
                        debug_assert_eq!(&cand[8..16], &donor[8..16]);
                        let j = err as f32 - lam * SAVE_HALF8;
                        if j < best_j {
                            best_j = j;
                            best = cand;
                        }
                    }
                    // 3. Head reuse: donor endpoints + p0, our p1 + indices.
                    for p1 in 0..2u8 {
                        let pal = palette_mode6(
                            unquantize_7p(dq0, dp0),
                            unquantize_7p(dq1, p1),
                        );
                        let (idx, errv) = fit_indices_mode6(&pixels, &pal);
                        if idx[0] > 7 {
                            continue; // swap would rewrite the head bytes
                        }
                        let cand = pack_bc7_mode6(dq0, dp0, dq1, p1, idx);
                        debug_assert_eq!(&cand[0..8], &donor[0..8]);
                        let j = errv as f32 - lam * SAVE_HALF8;
                        if j < best_j {
                            best_j = j;
                            best = cand;
                        }
                    }
                }
            }

            let oi = (by * blocks_x + bx) * 16;
            out[oi..oi + 16].copy_from_slice(&best);
            prev_block = best;
            cur_row[bx] = best;
            let slot = (by * blocks_x + bx) % BC7_WINDOW;
            recent[slot] = (best, best[0] & 0x7F == 0x40);
            filled += 1;
        }
        std::mem::swap(&mut prev_row, &mut cur_row);
    }
    Ok(())
}

/// Any-mode BC7 block SSE via the decode oracle (RGBA).
#[cfg(feature = "decode")]
fn bc7_block_sse(pixels: &[[u8; 4]; 16], block: &[u8; 16]) -> i64 {
    let mut dec = [0u8; 64];
    bcdec_rs::bc7(block, &mut dec, 16);
    let mut err = 0i64;
    for i in 0..16 {
        for c in 0..4 {
            let d = dec[i * 4 + c] as i64 - pixels[i][c] as i64;
            err += d * d;
        }
    }
    err
}

/// Mode-6 SSE from quantized endpoints + fixed indices (native math).
fn mode6_sse(
    pixels: &[[u8; 4]; 16],
    q0: [u8; 4],
    p0: u8,
    q1: [u8; 4],
    p1: u8,
    indices: &[u8; 16],
) -> i64 {
    let pal = palette_mode6(unquantize_7p(q0, p0), unquantize_7p(q1, p1));
    let mut err = 0i64;
    for (i, px) in pixels.iter().enumerate() {
        let p = pal[indices[i] as usize];
        for c in 0..4 {
            let d = p[c] as i64 - px[c] as i64;
            err += d * d;
        }
    }
    err
}

/// Per-channel best 7-bit quantization under a FIXED p-bit.
fn quantize_7p_fixed(c: [u8; 4], p: u8) -> ([u8; 4], u8) {
    let mut q = [0u8; 4];
    for i in 0..4 {
        let base = c[i] >> 1;
        let mut bq = base.min(127);
        let mut be = i32::MAX;
        for cand in base.saturating_sub(1)..=(base + 1).min(127) {
            let recon = unquantize_7p_chan(cand, p);
            let e = (recon as i32 - c[i] as i32).pow(2);
            if e < be {
                be = e;
                bq = cand;
            }
        }
        q[i] = bq;
    }
    (q, p)
}

/// Pick our own p0 for the tail-reuse candidate (endpoint 0 is free).
fn dp0_choice(pixels: &[[u8; 4]; 16], e0: [u8; 4]) -> u8 {
    let _ = pixels;
    // Cheap: pick the p that reconstructs e0 best on average.
    let mut errs = [0i32; 2];
    for (p, e) in errs.iter_mut().enumerate() {
        let (q, _) = quantize_7p_fixed(e0, p as u8);
        let r = unquantize_7p(q, p as u8);
        for c in 0..4 {
            *e += (r[c] as i32 - e0[c] as i32).pow(2);
        }
    }
    (errs[1] < errs[0]) as u8
}

/// Parse a mode-6 block back to (q0, p0, q1, p1, indices).
fn parse_mode6(block: &[u8; 16]) -> Option<([u8; 4], u8, [u8; 4], u8, [u8; 16])> {
    if block[0] & 0x7F != 0x40 {
        return None;
    }
    let low = u64::from_le_bytes(block[0..8].try_into().unwrap());
    let high = u64::from_le_bytes(block[8..16].try_into().unwrap());
    let bit = |i: u32| -> u64 {
        if i < 64 {
            (low >> i) & 1
        } else {
            (high >> (i - 64)) & 1
        }
    };
    let bits = |start: u32, n: u32| -> u64 {
        let mut v = 0u64;
        for k in 0..n {
            v |= bit(start + k) << k;
        }
        v
    };
    let mut q0 = [0u8; 4];
    let mut q1 = [0u8; 4];
    let mut pos = 7u32;
    for c in 0..4 {
        q0[c] = bits(pos, 7) as u8;
        pos += 7;
        q1[c] = bits(pos, 7) as u8;
        pos += 7;
    }
    let p0 = bit(63) as u8;
    let p1 = bit(64) as u8;
    let mut indices = [0u8; 16];
    indices[0] = bits(65, 3) as u8;
    let mut ip = 68u32;
    for v in indices.iter_mut().skip(1) {
        *v = bits(ip, 4) as u8;
        ip += 4;
    }
    Some((q0, p0, q1, p1, indices))
}

/// ±1 moves on the 7-bit mode-6 endpoint channels with p-bits and indices
/// held fixed (the polish never touches the matched tail bytes).
fn polish_mode6_endpoints(
    pixels: &[[u8; 4]; 16],
    q0: &mut [u8; 4],
    p0: u8,
    q1: &mut [u8; 4],
    p1: u8,
    indices: &[u8; 16],
    err: &mut i64,
) {
    for _round in 0..2 {
        let prev = *err;
        for which in 0..2 {
            for c in 0..4 {
                for d in [-1i32, 1] {
                    let mut t0 = *q0;
                    let mut t1 = *q1;
                    let target = if which == 0 { &mut t0 } else { &mut t1 };
                    let nv = target[c] as i32 + d;
                    if nv < 0 || nv > 127 {
                        continue;
                    }
                    target[c] = nv as u8;
                    let e = mode6_sse(pixels, t0, p0, t1, p1, indices);
                    if e < *err {
                        *err = e;
                        *q0 = t0;
                        *q1 = t1;
                    }
                }
            }
        }
        if *err >= prev {
            break;
        }
    }
}

/// LZ-match value the block ALREADY carries against the recent window:
/// whole-block repeat, or a repeated 4-byte half (table / endpoints).
fn score_bc1(block: &[u8; 8], recent: &[[u8; 8]]) -> f32 {
    let mut best = 0f32;
    for r in recent {
        if r == block {
            return SAVE_WHOLE;
        }
        if r[4..8] == block[4..8] || r[0..4] == block[0..4] {
            best = SAVE_PART;
        }
    }
    best
}


/// LZ-match value a BC7 block already carries vs the recent window.
fn score_bc7(block: &[u8; 16], recent: &[([u8; 16], bool)]) -> f32 {
    let mut best = 0f32;
    for (r, _) in recent {
        if r == block {
            return SAVE_WHOLE16;
        }
        if r[8..16] == block[8..16] || r[0..8] == block[0..8] {
            best = SAVE_HALF8;
        }
    }
    best
}

