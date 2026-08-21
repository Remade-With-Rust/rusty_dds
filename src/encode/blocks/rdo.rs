//! Rate-distortion-optimized BC1 (Oodle-Texture-class, v1).
//!
//! BCn payloads ship inside an LZ archive (Star Citizen's `.p4k` is
//! zip/deflate), so the real rate of a block is not its fixed 8 bytes but
//! how well those bytes MATCH earlier ones. This pass re-chooses blocks
//! among LZ-friendlier candidates under a Lagrangian:
//!
//! ```text
//! J = SSE - lambda * estimated_bytes_saved
//! ```
//!
//! Candidates per block, all legal BC1 by construction (conformance is
//! free; only the rate/quality point moves):
//!   - reuse the PREVIOUS block wholesale        (8-byte match)
//!   - reuse a recent block's INDEX bytes,
//!     endpoints re-fit optimally by LS          (4-byte match)
//!   - reuse a recent block's ENDPOINT bytes,
//!     indices re-fit exactly                    (4-byte match)
//!
//! `lambda = 0` (i.e. [`crate::Rdo::Off`]) disables the pass — byte-identical
//! to the normal path, gated by `tests/encode_determinism.rs`.
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

/// Split `blocks_y` into one row-strip per worker.
///
/// Pass 2 carries a sliding match window and a reference to the row above, so
/// strips cannot share state — each gets its own, starting cold. That is what
/// makes this the one change in the RDO campaign that moves output: a block at a
/// strip boundary sees an empty window where the serial encoder saw sixteen
/// candidates. The ladder is the gate, not a hash.
/// # Why strips start COLD
///
/// Seeding a strip's window from pass 1's baseline blocks for the row above was
/// tried and measured **worse**: deflated size at λ=200 went 76.45% (cold) to
/// 76.63% (seeded). Baseline blocks are candidates that never appear in the
/// output, so matching against them compresses nothing while displacing history
/// that would. An empty window costs one row; a wrong window costs the strip.
fn rdo_strips(blocks_y: usize) -> Vec<(usize, usize)> {
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, blocks_y.max(1));
    // Strips must be at least a few rows or the cold-window cost dominates the
    // parallel gain: every strip boundary is a row that starts with no history.
    let workers = workers.min(blocks_y.div_ceil(8).max(1));
    let mut v = Vec::with_capacity(workers);
    let base = blocks_y / workers;
    let extra = blocks_y % workers;
    let mut start = 0;
    for wi in 0..workers {
        let len = base + usize::from(wi < extra);
        v.push((start, start + len));
        start += len;
    }
    v
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
    // The dictionary is fixed for the whole image, so its LS terms are too.
    let dict_ls: Vec<Option<TableLs>> = dict.iter().map(|&t| table_ls(t)).collect();

    // Previous ROW of emitted blocks: vertical repetition is the dominant
    // long-range structure in textures, and deflate's 32KB window covers a
    // full block row at any sane width.
    let strips = rdo_strips(blocks_y);
    let q = super::QUALITY.with(|c| c.get());
    let dict = &dict;
    let dict_ls = &dict_ls;
    let base_blocks = &base_blocks;
    std::thread::scope(|scope| {
        let mut rest = out;
        for &(by0, by1) in &strips {
            let band_len = (by1 - by0) * blocks_x * 8;
            let (band, tail) = rest.split_at_mut(band_len);
            rest = tail;
            scope.spawn(move || {
                super::with_quality(q, || {
                // Per-strip row history: a strip never reads the row above its
                // own first row, so these cannot be shared.
                let mut prev_row: Vec<[u8; 8]> = vec![[0u8; 8]; blocks_x];
                let mut cur_row: Vec<[u8; 8]> = vec![[0u8; 8]; blocks_x];
                // Ring buffers of recently emitted structures.
                let mut recent_blocks: [[u8; 8]; WINDOW] = [[0u8; 8]; WINDOW];
                let mut recent_tables: [u32; WINDOW] = [0; WINDOW];
                // Computed once as a table enters the window, not once per block that tries it.
                let mut recent_ls: [Option<TableLs>; WINDOW] = [None; WINDOW];
                let mut recent_eps: [(u16, u16); WINDOW] = [(0, 0); WINDOW];
                // The donor's 4-colour palette, built once on entry rather than by every
                // block that tries it.
                let mut recent_pal: [[[u8; 3]; 4]; WINDOW] = [[[0u8; 3]; 4]; WINDOW];
                // The widened form the index fit broadcasts from, built once per
                // ring-buffer update rather than on each of the ~14 fits a block
                // that reuse it.
                let mut recent_pal16: [super::bc1::Pal16; WINDOW] =
                    [super::bc1::widen_pal(&[[0u8; 3]; 4]); WINDOW];
                let mut filled = 0usize;
                let mut prev_block = [0u8; 8];

                for by in by0..by1 {
                    for bx in 0..blocks_x {
                        let pixels = gather_block(rgba, w, h, bx, by);

                        // Baseline: the normal quality path (from pass 1).
                        let base = base_blocks[by * blocks_x + bx];
                        let base_err = bc1_block_sse(&pixels, &base);

                        if base_err == 0 {
                            let oi = ((by - by0) * blocks_x + bx) * 8;
                            band[oi..oi + 8].copy_from_slice(&base);
                            prev_block = base;
                            cur_row[bx] = base;
                            let slot = (by * blocks_x + bx) % WINDOW;
                            recent_blocks[slot] = base;
                            recent_tables[slot] = u32::from_le_bytes([base[4], base[5], base[6], base[7]]);
                            recent_ls[slot] = table_ls(recent_tables[slot]);
                            recent_eps[slot] = (
                                u16::from_le_bytes([base[0], base[1]]),
                                u16::from_le_bytes([base[2], base[3]]),
                            );
                            recent_pal[slot] = super::bc1::byte_pal_if_needed(
                                recent_eps[slot].0.max(recent_eps[slot].1),
                                recent_eps[slot].0.min(recent_eps[slot].1),
                            );
                            recent_pal16[slot] = super::bc1::pal16_from_565(
                                recent_eps[slot].0.max(recent_eps[slot].1),
                                recent_eps[slot].0.min(recent_eps[slot].1),
                            );
                            filled += 1;
                            continue;
                        }
                        let mut best = base;
                        // The baseline may ALREADY repeat naturally — credit it, or a
                        // substitution can book phantom savings while destroying real
                        // ones (computer_key: payload GREW 5% before this correction).
                        let n0 = filled.min(WINDOW);
                        let above: Option<&[u8; 8]> = if by > by0 { Some(&prev_row[bx]) } else { None };
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

                            // Block-invariant: the accumulator runs 25.4 times on these.
                            let pxv = ls_pixels(&pixels);
                            // Pixel-only SSE term: a property of the block, so
                            // it is computed once here rather than inside each
                            // windowed fit. See `bc1::psq_rgb`.
                            let psq = super::bc1::psq_rgb(&pixels);
                            let n = filled.min(WINDOW);
                            // The window can hold the SAME table more than once
                            // — repetitive content emits repeats constantly —
                            // and refitting a table twice yields the same
                            // endpoints, the same error, and a strict `<` that
                            // cannot fire. Skipping repeats is exact, and the
                            // refit is 40.9% of BC1 RDO.
                            let mut tried: [u32; WINDOW] = [0; WINDOW];
                            let mut ntried = 0usize;
                            let mut tried_bits = 0u64;
                            let mut tried_eps: [(u16, u16); WINDOW] = [(0, 0); WINDOW];
                            let mut neps = 0usize;
                            let mut eps_bits = 0u64;
                            for k in 0..n {
                                // 2. Reuse index table, LS-refit endpoints.
                                let table = recent_tables[k];
                                let tb = filter_bit(table);
                                let dup = (tried_bits & tb) != 0
                                    && tried[..ntried].contains(&table);
                                if !dup {
                                    tried[ntried] = table;
                                    ntried += 1;
                                    tried_bits |= tb;
                                }
                                // `lim` tracks `best_j`, which only moves when a
                                // candidate WINS. It was recomputed for the
                                // endpoint path a few lines below on every
                                // iteration; now it is recomputed only where it
                                // can have changed — same value, one `ceil` and
                                // one convert fewer per pass.
                                let mut lim = (best_j + lam * SAVE_PART).ceil() as i32;
                                if lim > 0 && !dup {
                                    if let Some(cand) = recent_ls[k]
                                        .as_ref()
                                        .and_then(|ls| refit_with_ls(&pixels, &pxv, ls, table))
                                    {
                                        if let Some(err) = bc1_block_sse_limited(&pixels, &cand, lim) {
                                            let j = err as f32 - lam * SAVE_PART;
                                            if j < best_j {
                                                best_j = j;
                                                best = cand;
                                                best_class = Class::Table;
                                                lim = (best_j + lam * SAVE_PART).ceil() as i32;
                                            }
                                        }
                                    }
                                }
                                // 3. Reuse endpoints, re-fit indices. Same
                                // duplicate story as the tables above: an
                                // endpoint pair repeated in the window refits to
                                // the same block and the same error.
                                let (c0, c1) = recent_eps[k];
                                let eb = filter_bit((c0 as u32) | ((c1 as u32) << 16));
                                let edup = (eps_bits & eb) != 0
                                    && tried_eps[..neps].contains(&(c0, c1));
                                if !edup {
                                    tried_eps[neps] = (c0, c1);
                                    neps += 1;
                                    eps_bits |= eb;
                                }
                                if c0 > c1 && lim > 0 && !edup {
                                    if let Some((blk, err)) = super::bc1::pack_bc1_scored_pre(
                                        &pixels, c0, c1, &recent_pal[k], &recent_pal16[k], psq,
                                        lim,
                                    ) {
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
                            for (di, &table) in dict.iter().enumerate() {
                                // `tried[..ntried]` is exactly the DISTINCT set
                                // of `recent_tables[..n]` -- the window loop
                                // above built it -- so membership is identical
                                // while the scan is shorter by however many
                                // repeats the window held (about 6.7 of 16 on
                                // this corpus). DICT_N of these run per block.
                                if (tried_bits & filter_bit(table)) != 0
                                    && tried[..ntried].contains(&table)
                                {
                                    continue; // already tried via the window
                                }
                                let lim = (best_j + lam * SAVE_PART).ceil() as i32;
                                if lim <= 0 {
                                    break;
                                }
                                if let Some(cand) = dict_ls[di]
                                    .as_ref()
                                    .and_then(|ls| refit_with_ls(&pixels, &pxv, ls, table))
                                {
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

                        let oi = ((by - by0) * blocks_x + bx) * 8;
                        band[oi..oi + 8].copy_from_slice(&best);
                        prev_block = best;
                        cur_row[bx] = best;
                        let slot = (by * blocks_x + bx) % WINDOW;
                        recent_blocks[slot] = best;
                        recent_tables[slot] =
                            u32::from_le_bytes([best[4], best[5], best[6], best[7]]);
                        recent_ls[slot] = table_ls(recent_tables[slot]);
                        recent_eps[slot] = (
                            u16::from_le_bytes([best[0], best[1]]),
                            u16::from_le_bytes([best[2], best[3]]),
                        );
                        recent_pal[slot] = super::bc1::byte_pal_if_needed(
                            recent_eps[slot].0.max(recent_eps[slot].1),
                            recent_eps[slot].0.min(recent_eps[slot].1),
                        );
                        recent_pal16[slot] = super::bc1::pal16_from_565(
                                recent_eps[slot].0.max(recent_eps[slot].1),
                                recent_eps[slot].0.min(recent_eps[slot].1),
                            );
                        filled += 1;
                    }
                    std::mem::swap(&mut prev_row, &mut cur_row);
                }

                });
            });
        }
        debug_assert!(rest.is_empty());
    });
    Ok(())
}

/// Like `bc1_block_sse` but aborts once the partial sum reaches `limit`
/// (a candidate at or past the limit can never be accepted).
/// The four RGB palette entries of a BC1 block, packed one per `u32` with a zero
/// alpha byte so the vector kernel can `pshufb` them directly.
#[inline]
/// # A refuted change, recorded so it is not retried
///
/// Returning only the packed form and having the scalar tail unpack from it —
/// so the `[[u8; 3]; 4]` need not be materialised — measured **worse**:
/// 103 -> 120 instructions for this function, with timing at +1.4%, z = +1.07,
/// i.e. nothing. Both representations are cheap to derive together from the two
/// 565 words, and splitting them costs more in the return than it saves in the
/// body.
fn bc1_colors_packed(block: &[u8; 8]) -> [u32; 4] {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let a = from_565_packed(c0);
    let b = from_565_packed(c1);
    // Vectorising this palette build has now been REFUTED TWICE, and the second
    // time the first reason had expired.
    //
    // The second attempt (2026-08-20): the executed path measured **91 -> 106**
    // instructions — 90 in the body plus a 16-instruction out-of-line kernel.
    // The six divisions by 3 were never the cost: LLVM strength-reduces `/3` to
    // a multiply-shift, so the scalar arithmetic was already about eighteen
    // instructions. What sank it is that a `#[target_feature]` kernel can never
    // inline into a caller without the feature, so every call pays a real call
    // boundary — and a 16-instruction kernel is far too small to amortise one.
    // **A kernel has to be big enough to pay for the boundary it sits behind.**
    //
    // The FIRST refutation, recorded below, is kept because it is still true of
    // the code it describes — but note that its premise expired when the palette
    // moved to packed `u32` words in §70 #9. It was re-tested for exactly that
    // reason. Doing
    // all six divisions by 3 in one `mulhi_epu16` -- exact, since the dividends
    // are at most 765 and `21846/65536` errs by 0.0082 there against a largest
    // fractional part of 2/3 -- measured **135 -> 178 instructions**. Building the
    // vectors from `[u8; 3]` costs six inserts and two stores back, more than the
    // six divisions it removes.
    //
    // The win the refutation walked past was one level up: this used to return
    // BOTH `[[u8; 3]; 4]` and `[u32; 4]`, building the byte arrays and then
    // re-reading twelve bytes out of them to assemble the words. On AVX2 -- the
    // path that actually runs -- only the words are ever read; the byte arrays
    // were consumed solely by the scalar fallback. Now the words are assembled
    // directly and the fallback unpacks them, which costs it three shifts.
    if c0 > c1 {
        [a, b, lerp_packed::<2, 1>(a, b), lerp_packed::<1, 2>(a, b)]
    } else {
        [a, b, lerp_packed::<1, 1>(a, b), 0]
    }
}

fn bc1_block_sse_limited(pixels: &[[u8; 4]; 16], block: &[u8; 8], limit: i32) -> Option<i32> {
    let table = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    // The abort moves from the running prefix to the completed total, which is
    // the same decision: squared errors are non-negative, so a prefix reaches
    // `limit` if and only if the total does.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        // The palette is built INSIDE the kernel from the 565 words. Only the
        // scalar fallback below still needs `bc1_colors_packed`, so its 91
        // instructions no longer run on this machine at all.
        let c0 = u16::from_le_bytes([block[0], block[1]]);
        let c1 = u16::from_le_bytes([block[2], block[3]]);
        let err = simd::bc1_fixed_sse_565_avx2(pixels, c0, c1, table);
        return (err < limit).then_some(err);
    }
    let packed = bc1_colors_packed(block);
    let mut err = 0i32;
    for (i, p) in pixels.iter().enumerate() {
        let idx = ((table >> (2 * i)) & 3) as usize;
        err += sqr_rgb([p[0], p[1], p[2]], unpack_rgb(packed[idx]));
        if err >= limit {
            return None;
        }
    }
    Some(err)
}

/// Decode-true SSE of an arbitrary BC1 block against source pixels
/// (both 4-color and punch modes, matching the decoder's mode rule).
fn bc1_block_sse(pixels: &[[u8; 4]; 16], block: &[u8; 8]) -> i32 {
    let table = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        let c0 = u16::from_le_bytes([block[0], block[1]]);
        let c1 = u16::from_le_bytes([block[2], block[3]]);
        return simd::bc1_fixed_sse_565_avx2(pixels, c0, c1, table);
    }
    let packed = bc1_colors_packed(block);
    let mut err = 0i32;
    for (i, p) in pixels.iter().enumerate() {
        let idx = ((table >> (2 * i)) & 3) as usize;
        err += sqr_rgb([p[0], p[1], p[2]], unpack_rgb(packed[idx]));
    }
    err
}

/// Given a FIXED index table, solve LS endpoints in RGB, quantize to 565,
/// and emit a 4-color block carrying exactly that table. Returns None for
/// degenerate weight layouts or when quantized endpoints collapse into the
/// punch-mode ordering (which would reinterpret the table).
/// The half of the normal equations that depends **only on the index table**.
///
/// `a00`, `a01`, `a11` and therefore `det` are sums over per-pixel weights, and
/// those weights come from the table alone — no pixel value enters them. Yet
/// `refit_endpoints_for_table` ran **17 times per block** (measured), rebuilding
/// them every time, for tables drawn from a 16-entry sliding window and a
/// 24-entry dictionary. Computing them once as a table enters either structure
/// removes a 16-iteration accumulation, a determinant and a degeneracy test from
/// every one of those calls.
///
/// The raw terms are stored rather than `a11/det` etc. because the caller's
/// expression is `(a11 * b0 - a01 * b1) / det`, and pre-dividing would change
/// the floating-point result.
/// Per-block pixels in the shape the LS accumulator wants. Empty without SIMD,
/// where `ls_accum_scalar` reads the bytes directly.
#[cfg(all(feature = "simd", target_arch = "x86_64"))]
type LsPixels = [[f32; 8]; 16];
#[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
type LsPixels = ();

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
fn ls_pixels(pixels: &[[u8; 4]; 16]) -> LsPixels {
    if simd::has_avx2() {
        simd::ls_pixels(pixels)
    } else {
        [[0f32; 8]; 16]
    }
}
#[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
fn ls_pixels(_pixels: &[[u8; 4]; 16]) -> LsPixels {}

/// As [`ls_pixels`], in the channel-interleaved layout the mode-6 accumulator
/// wants. BC1 and BC7 use different layouts because their weight vectors differ.
#[cfg(all(feature = "simd", target_arch = "x86_64"))]
fn ls_pixels_mode6(pixels: &[[u8; 4]; 16]) -> LsPixels {
    if simd::has_avx2() {
        simd::ls_pixels_mode6(pixels)
    } else {
        [[0f32; 8]; 16]
    }
}
#[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
fn ls_pixels_mode6(_pixels: &[[u8; 4]; 16]) -> LsPixels {}

/// Mode-6 LS on pre-converted pixels where the target supports it, falling back
/// to the byte-taking scalar path otherwise.
#[cfg(all(feature = "simd", target_arch = "x86_64"))]
fn ls_endpoints_mode6_hot(
    pixels: &[[u8; 4]; 16],
    pxv: &LsPixels,
    indices: &[u8; 16],
) -> Option<([u8; 4], [u8; 4])> {
    if simd::has_avx2() {
        super::bc7::ls_endpoints_mode6_pxv(pxv, indices)
    } else {
        super::bc7::ls_endpoints_mode6_scalar(pixels, indices)
    }
}
#[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
fn ls_endpoints_mode6_hot(
    pixels: &[[u8; 4]; 16],
    _pxv: &LsPixels,
    indices: &[u8; 16],
) -> Option<([u8; 4], [u8; 4])> {
    super::bc7::ls_endpoints_mode6_scalar(pixels, indices)
}

#[derive(Clone, Copy)]
struct TableLs {
    a00: f32,
    a01: f32,
    a11: f32,
    det: f32,
    /// `(1-w, 1-w, 1-w, 1-w, w, w, w, w)` per pixel position — table-only, and
    /// pre-spread to the shape [`simd::ls_accum_sse`] multiplies by.
    ///
    /// The kernel used to rebuild that spread every pixel with a
    /// `vbroadcastsd` plus a `vpermps`. Storing it costs 512 bytes a table
    /// rather than 128; the same trick FAILED for the mode-6 accumulator
    /// (160 -> 177) because there the row index is `indices[i]`, data-dependent,
    /// so a 32-byte stride needs real address arithmetic. Here the index is the
    /// unrolled loop counter, so every offset is a compile-time constant and
    /// there is no address arithmetic to pay.
    uw: [[f32; 8]; 16],
}

fn table_ls(table: u32) -> Option<TableLs> {
    // 4-color weights toward c1 by index: 0 -> 0, 1 -> 1, 2 -> 1/3, 3 -> 2/3.
    const W: [f32; 4] = [0.0, 1.0, 1.0 / 3.0, 2.0 / 3.0];
    let mut a00 = 0f32;
    let mut a01 = 0f32;
    let mut a11 = 0f32;
    let mut uw = [[0f32; 8]; 16];
    for (i, slot) in uw.iter_mut().enumerate() {
        let wgt = W[((table >> (2 * i)) & 3) as usize];
        let u = 1.0 - wgt;
        a00 += u * u;
        a01 += u * wgt;
        a11 += wgt * wgt;
        *slot = [u, u, u, u, wgt, wgt, wgt, wgt];
    }
    let det = a00 * a11 - a01 * a01;
    if det.abs() < 1e-4 {
        return None;
    }
    Some(TableLs { a00, a01, a11, det, uw })
}

/// The pixel-dependent half: accumulate `b0`/`b1` and solve.
///
/// The accumulation order is the original's — ascending `i`, `b0` then `b1` —
/// so the float sums are bit-identical, and the solve is the original
/// expression unchanged.
/// Scalar twin of [`simd::ls_accum_sse`], and its oracle.
#[inline]
fn ls_accum_scalar(pixels: &[[u8; 4]; 16], uw: &[[f32; 8]; 16]) -> ([f32; 3], [f32; 3]) {
    let mut b0 = [0f32; 3];
    let mut b1 = [0f32; 3];
    for (i, p) in pixels.iter().enumerate() {
        let (u, wgt) = (uw[i][0], uw[i][4]);
        for c in 0..3 {
            let x = p[c] as f32;
            b0[c] += u * x;
            b1[c] += wgt * x;
        }
    }
    (b0, b1)
}

/// A 64-bit presence filter over the window's dedup scratch arrays.
///
/// Two membership tests run per window entry and the dictionary scan runs more,
/// 55.3 calls a block in total, and they walk 361 elements between them because
/// the answer is usually NO. One bit per hashed value answers "definitely
/// absent" in four instructions, and only a set bit costs a scan.
///
/// This is EXACT: a clear bit proves absence, and a set bit falls through to the
/// same linear scan as before, so the result never changes — only the work does.
///
/// A vectorised `contains` was tried instead and REFUTED: at 55 calls a block a
/// `#[target_feature]` kernel pays about 553 instructions of call boundary
/// alone, against the roughly 1,083 of scalar scanning it replaces.
#[inline]
fn filter_bit(v: u32) -> u64 {
    // Multiply-shift rather than `v & 63`: index tables share low bits far too
    // often for the bottom six to spread well.
    1u64 << ((v.wrapping_mul(0x9E37_79B1) >> 26) & 63)
}

fn refit_with_ls(
    pixels: &[[u8; 4]; 16],
    pxv: &LsPixels,
    ls: &TableLs,
    table: u32,
) -> Option<[u8; 8]> {
    // 59% of BC1 RDO's instruction cost by the deterministic model. The kernel
    // keeps one pixel per iteration and separate mul/add, so every lane's
    // accumulation order and rounding match this loop exactly.
    let (a00, a01, a11, det) = (ls.a00, ls.a01, ls.a11, ls.det);
    let mut e0 = [0u8; 3];
    let mut e1 = [0u8; 3];
    // Set by the vector path, which packs to 565 inside the kernel.
    let mut qq: Option<(u16, u16)> = None;
    // The solve is six float divisions scalar; vectorised it is two `divps`,
    // and bit-identical for free because IEEE defines these lane-wise. Rounding
    // stays scalar — Rust's `round` is half-away-from-zero and no SSE rounding
    // mode matches it.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    let done = if simd::has_avx2() {
        // Accumulate, solve and pack to 565 in ONE call: the accumulator's two
        // [f32; 4] results used to be stored, returned across a
        // `#[target_feature]` boundary and immediately loaded back as the
        // solve's arguments.
        qq = Some(simd::ls_accum_solve_565(pxv, &ls.uw, a00, a01, a11, det));
        true
    } else {
        false
    };
    #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
    let done = false;
    if !done {
        let (b0, b1) = ls_accum_scalar(pixels, &ls.uw);
        for c in 0..3 {
            e0[c] = round_clamp_u8((a11 * b0[c] - a01 * b1[c]) / det);
            e1[c] = round_clamp_u8((a00 * b1[c] - a01 * b0[c]) / det);
        }
    }
    let (q0, q1) = match qq {
        Some(v) => v,
        None => (to_565(e0), to_565(e1)),
    };
    if q0 <= q1 {
        return None; // would flip to punch mode and reinterpret the table
    }
    // NOTE: the caller re-derives this block's palette from the packed 565 words
    // immediately afterwards. It cannot simply reuse `e0`/`e1` — `to_565`
    // quantises, so the palette must come from the ROUND-TRIPPED values — but
    // see `bc1_colors_packed`, which is where that cost lives.
    let mut out = [0u8; 8];
    out[0..2].copy_from_slice(&q0.to_le_bytes());
    out[2..4].copy_from_slice(&q1.to_le_bytes());
    out[4..8].copy_from_slice(&table.to_le_bytes());
    Some(out)
}

const DICT_N: usize = 24;

/// Pass 1: histogram the baseline encoder index tables, return the most
/// popular DICT_N (the global match dictionary for pass 2).
/// A trivial hasher for the `u32` index tables the dictionary counts.
///
/// The histogram takes one `HashMap::entry` per block, and Rust's default
/// SipHash-1-3 costs far more to hash four bytes than the probe that follows it.
/// A multiply-shift is enough here, and it changes nothing observable: the
/// dictionary is chosen by a TOTAL order — count descending, then table value
/// ascending — so iteration order cannot reach the output.
#[derive(Default)]
struct U32Hasher(u64);

impl std::hash::Hasher for U32Hasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        // Not used by `write_u32`, but `Hasher` requires it.
        for &b in bytes {
            self.0 = (self.0 ^ b as u64).wrapping_mul(0x0100_0000_01b3);
        }
    }
    #[inline]
    fn write_u32(&mut self, v: u32) {
        // Fibonacci hashing: the high bits of a golden-ratio multiply are well
        // spread, which is what the table's bucket index wants.
        self.0 = (v as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

type TableCounts = std::collections::HashMap<u32, u32, std::hash::BuildHasherDefault<U32Hasher>>;

fn build_table_dict(
    rgba: &[u8],
    w: usize,
    h: usize,
    blocks_x: usize,
    blocks_y: usize,
) -> (Vec<u32>, Vec<[u8; 8]>) {
    // Pass 1 is embarrassingly parallel and **byte-identical**, unlike pass 2.
    // Its only shared state is the count histogram, and integer addition is
    // order-independent, so per-strip histograms merge to the same map whatever
    // order the strips finish in. The block vector is index-addressed by strip.
    // The final ranking is a total order — count descending, then table value
    // ascending — so the dictionary is identical too.
    //
    // This is `encode_bc1_bytes` (1,173 instructions) plus a `gather_block` for
    // every block in the image, and it ran on one thread.
    let nblocks = blocks_x * blocks_y;
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, blocks_y.max(1));
    let mut strips: Vec<(usize, usize)> = Vec::with_capacity(workers);
    let base = blocks_y / workers;
    let extra = blocks_y % workers;
    let mut start = 0;
    for wi in 0..workers {
        let len = base + usize::from(wi < extra);
        strips.push((start, start + len));
        start += len;
    }
    let q = super::QUALITY.with(|c| c.get());
    let mut parts: Vec<(Vec<[u8; 8]>, TableCounts)> = Vec::with_capacity(workers);
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for &(by0, by1) in &strips {
            handles.push(scope.spawn(move || {
                super::with_quality(q, || {
                    let mut local = Vec::with_capacity((by1 - by0) * blocks_x);
                    let mut lc = TableCounts::default();
                    for by in by0..by1 {
                        for bx in 0..blocks_x {
                            let pixels = gather_block(rgba, w, h, bx, by);
                            let blk = encode_bc1_bytes(pixels);
                            let c0 = u16::from_le_bytes([blk[0], blk[1]]);
                            let c1 = u16::from_le_bytes([blk[2], blk[3]]);
                            if c0 > c1 {
                                let t = u32::from_le_bytes([blk[4], blk[5], blk[6], blk[7]]);
                                *lc.entry(t).or_insert(0) += 1;
                            }
                            local.push(blk);
                        }
                    }
                    (local, lc)
                })
            }));
        }
        for hnd in handles {
            parts.push(hnd.join().expect("rdo pass-1 worker panicked"));
        }
    });
    let mut counts = TableCounts::default();
    let mut blocks = Vec::with_capacity(nblocks);
    for (local, lc) in parts {
        blocks.extend_from_slice(&local);
        for (t, n) in lc {
            *counts.entry(t).or_insert(0) += n;
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

/// One 565 field expanded to 8 bits — [`from_565`], one channel of it.
#[inline]
fn from_565_chan(c: u16, ch: usize) -> u8 {
    match ch {
        0 => {
            let r = ((c >> 11) & 31) as u8;
            (r << 3) | (r >> 2)
        }
        1 => {
            let g = ((c >> 5) & 63) as u8;
            (g << 2) | (g >> 4)
        }
        _ => {
            let b = (c & 31) as u8;
            (b << 3) | (b >> 2)
        }
    }
}

/// SSE of ONE channel of a 4-colour BC1 block against the source.
///
/// `polish_endpoints_fixed_table` perturbs a single 565 field, which can only
/// move that channel's palette column and therefore only that channel's error
/// term. Scoring one channel instead of three is two-thirds less work in both
/// the palette build and the pixel loop.
///
/// Four-colour mode only, which is this function's contract — it returns before
/// the sweep when `c0 <= c1`. The interpolants are `lerp_rgb`'s, one column.
#[inline]
fn bc1_chan_sse(planar: &[[u8; 16]; 3], ch: usize, c0: u16, c1: u16, table: u32) -> i32 {
    let a = from_565_chan(c0, ch) as u32;
    let b = from_565_chan(c1, ch) as u32;
    let cols = [
        a as u8,
        b as u8,
        ((2 * a + b) / 3) as u8,
        ((a + 2 * b) / 3) as u8,
    ];
    // One `pshufb` expands all sixteen indices from the four-byte palette; the
    // channel arrives planar because the caller holds the pixels fixed for the
    // whole sweep. 182 instructions as the scalar loop below.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        return simd::bc1_chan_sse_avx2(&planar[ch], cols, table);
    }
    let mut e = 0i32;
    for (i, &x) in planar[ch].iter().enumerate() {
        let idx = ((table >> (2 * i)) & 3) as usize;
        let d = cols[idx] as i32 - x as i32;
        e += d * d;
    }
    e
}

/// +-1 contract moves on the 565 endpoints with the index table HELD FIXED
/// (the table bytes are the LZ match; endpoints are literals either way).
fn polish_endpoints_fixed_table(pixels: &[[u8; 4]; 16], block: &mut [u8; 8]) {
    let table = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    // Pixels transposed once for the whole sweep — the channel scorer wants a
    // contiguous run, and the pixels do not change across candidates.
    let mut planar = [[0u8; 16]; 3];
    for (i, px) in pixels.iter().enumerate() {
        planar[0][i] = px[0];
        planar[1][i] = px[1];
        planar[2][i] = px[2];
    }
    // Per-channel error, carried across the sweep: a candidate moves one 565
    // field, so only one term can change and the total is a two-add fixup.
    let mut ce = [0i32; 3];
    {
        let (c0, c1) = (
            u16::from_le_bytes([block[0], block[1]]),
            u16::from_le_bytes([block[2], block[3]]),
        );
        for (ch, e) in ce.iter_mut().enumerate() {
            *e = bc1_chan_sse(&planar, ch, c0, c1, table);
        }
    }
    let mut err = ce[0] + ce[1] + ce[2];
    debug_assert_eq!(err, bc1_block_sse(pixels, block));
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
                // 11 -> red, 5 -> green, 0 -> blue.
                let ch = match shift {
                    11 => 0usize,
                    5 => 1,
                    _ => 2,
                };
                let cand = bc1_chan_sse(&planar, ch, n0, n1, table);
                let total = err - ce[ch] + cand;
                if total < err {
                    err = total;
                    ce[ch] = cand;
                    block[0..2].copy_from_slice(&n0.to_le_bytes());
                    block[2..4].copy_from_slice(&n1.to_le_bytes());
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

    let strips = rdo_strips(blocks_y);
    let q = super::QUALITY.with(|c| c.get());
    std::thread::scope(|scope| {
        let mut rest = out;
        for &(by0, by1) in &strips {
            let band_len = (by1 - by0) * blocks_x * 16;
            let (band, tail) = rest.split_at_mut(band_len);
            rest = tail;
            scope.spawn(move || {
                super::with_quality(q, || {
                // Per-strip row history, as in the BC1 driver.

                let mut recent: [([u8; 16], bool); BC7_WINDOW] = [([0u8; 16], false); BC7_WINDOW];
                // Parsed once when a block enters the window, not once per block that later
                // examines it. `parse_mode6` measured 8.27 calls per block, every one of them
                // re-extracting bitfields from a block this driver had already emitted.
                type Mode6Parts = ([u8; 4], u8, [u8; 4], u8, [u8; 16]);
                let mut recent_m6: [Option<Mode6Parts>; BC7_WINDOW] = [None; BC7_WINDOW];
                let mut prev_row: Vec<[u8; 16]> = vec![[0u8; 16]; blocks_x];
                let mut cur_row: Vec<[u8; 16]> = vec![[0u8; 16]; blocks_x];
                let mut filled = 0usize;
                let mut prev_block = [0u8; 16];

                for by in by0..by1 {
                    for bx in 0..blocks_x {
                        let pixels = gather_block(rgba, w, h, bx, by);
                        let mut base = [0u8; 16];
                        // The encoder returns the SSE it computed internally;
                        // `bc7_block_sse` used to re-derive it here.
                        let base_err = super::bc7::encode_bc7_mode6_scored(pixels, &mut base);

                        // Exact blocks are untouchable: preservation is structural,
                        // not an emergent property of the acceptance math.
                        if base_err == 0 {
                            let oi = ((by - by0) * blocks_x + bx) * 16;
                            band[oi..oi + 16].copy_from_slice(&base);
                            prev_block = base;
                            cur_row[bx] = base;
                            let slot = (by * blocks_x + bx) % BC7_WINDOW;
                            recent[slot] = (base, base[0] & 0x7F == 0x40);
                            recent_m6[slot] = parse_mode6(&base);
                            filled += 1;
                            continue;
                        }
                        // Built only past the exact-block early-out above, which
                        // returns before either is read.
                        // Transposed once per block; every candidate borrows it.
                        let planar = Mode6Planar::new(&pixels);
                        // Likewise block-invariant: the mode-6 LS accumulator
                        // runs 9.14 times a block on these.
                        let pxv = ls_pixels_mode6(&pixels);

                        let mut best = base;
                        let n0 = filled.min(BC7_WINDOW);
                        let above: Option<&[u8; 16]> = if by > by0 { Some(&prev_row[bx]) } else { None };
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
                            // An exact early-out was tried here, the analogue of
                            // BC1's `lim > 0` guard which this loop never had: a
                            // candidate wins only if `err - lam * SAVE_HALF8 <
                            // best_j`, and `err` is a sum of squares, so once
                            // `best_j + lam * SAVE_HALF8 <= 0` nothing can win
                            // and the loop may `break`. It is correct and it was
                            // byte-identical — and it FIRES ALMOST NEVER:
                            // polish calls went 8.178 -> 8.176 a block. `best_j`
                            // simply never gets good enough to close the bound
                            // here, unlike BC1's, so the guard costs two float
                            // operations an iteration to save nothing. Removed.
                            // Deduplicating donors was tried and REVERTED: a
                            // counter found **0.000 duplicate donors per block**
                            // on representative content, so the check is pure
                            // cost. BC1's window does repeat (6.8 tables and 1.7
                            // endpoint pairs a block) because many blocks encode
                            // to the same 4-byte table; a whole 16-byte BC7 block
                            // repeating is far rarer.
                            for k in 0..n {
                                let (donor, is_m6) = recent[k];
                                if !is_m6 {
                                    continue;
                                }
                                let _ = &donor;
                                let Some((dq0, dp0, dq1, dp1, didx)) = recent_m6[k] else {
                                    continue;
                                };
                                // 2. Tail reuse: donor p1 + indices, our endpoints by LS.
                                if let Some((e0, e1)) = ls_endpoints_mode6_hot(&pixels, &pxv, &didx)
                                {
                                    let q0 = quantize_7p_best(e0);
                                    let q1 = quantize_7p_fixed(e1, dp1);
                                    // p0 is ours (head byte); try both cheaply via helper.
                                    let (mut q0a, p0a) = q0;
                                    let mut q1a = q1.0;
                                    // Endpoint polish with indices FIXED: the tail bytes
                                    // are the LZ match, head endpoint bytes are literals
                                    // either way — ±1 moves recover quality for free.
                                    let dfixed = Mode6Fixed::new(&planar, &didx);
                                    let mut ce =
                                        mode6_chan_errs(&dfixed, q0a, p0a, q1a, dp1);
                                    let mut err: i64 = ce.iter().sum();
                                    polish_mode6_endpoints(
                                        &dfixed,
                                        &mut ce, &mut q0a, p0a, &mut q1a, dp1, &mut err,
                                    );
                                    // Packed only if it wins, as above.
                                    let j = err as f32 - lam * SAVE_HALF8;
                                    if j < best_j {
                                        best_j = j;
                                        best = pack_bc7_mode6(q0a, p0a, q1a, dp1, didx);
                                        debug_assert_eq!(&best[8..16], &donor[8..16]);
                                    }
                                }
                                // 3. Head reuse: donor endpoints + p0, our p1 + indices.
                                // The donor endpoint does not vary with p1, so it is
                                // unquantized once rather than twice.
                                let du0 = unquantize_7p(dq0, dp0);
                                // `base` depends only on the donor endpoint, so
                                // it is shared by both p-bits.
                                let dbase = super::bc7::palette_mode6_base(du0);
                                for p1 in 0..2u8 {
                                    let (idx, errv) = super::bc7::palette_and_fit_mode6(
                                        &pixels,
                                        dbase,
                                        du0,
                                        unquantize_7p(dq1, p1),
                                    );
                                    if idx[0] > 7 {
                                        continue; // swap would rewrite the head bytes
                                    }
                                    // Packed only if it wins. Most candidates do not, and
                                    // packing costs 99 instructions — it ran 19 times a
                                    // block to produce a block usually thrown away.
                                    let j = errv as f32 - lam * SAVE_HALF8;
                                    if j < best_j {
                                        best_j = j;
                                        best = pack_bc7_mode6(dq0, dp0, dq1, p1, idx);
                                        debug_assert_eq!(&best[0..8], &donor[0..8]);
                                    }
                                }
                            }
                        }

                        let oi = ((by - by0) * blocks_x + bx) * 16;
                        band[oi..oi + 16].copy_from_slice(&best);
                        prev_block = best;
                        cur_row[bx] = best;
                        let slot = (by * blocks_x + bx) % BC7_WINDOW;
                        recent[slot] = (best, best[0] & 0x7F == 0x40);
                        recent_m6[slot] = parse_mode6(&best);
                        filled += 1;
                    }
                    std::mem::swap(&mut prev_row, &mut cur_row);
                }

                });
            });
        }
        debug_assert!(rest.is_empty());
    });
    Ok(())
}

/// Any-mode BC7 block SSE via the decode oracle (RGBA).
#[cfg(feature = "decode")]
fn bc7_block_sse(pixels: &[[u8; 4]; 16], block: &[u8; 16]) -> i64 {
    let mut dec = [0u8; 64];
    // This crate's own BC7 decoder, not the reference one. It is measured 10.4x
    // faster than `bcdec_rs::bc7` and oracle-tested against it, and it declines
    // exactly one input — the reserved encoding — which falls through here just
    // as it does in `decode_bc7`. Byte-identical by construction.
    if !crate::decode::bcn::bc7_fast_block(block, &mut dec, 16) {
        bcdec_rs::bc7(block, &mut dec, 16);
    }
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
/// The block's invariants for a mode-6 endpoint sweep: pixels transposed to
/// planar, and the sixteen weights already looked up.
///
/// Both are fixed for the whole sweep — the indices are, so `W6M[indices[i]]`
/// is, and the pixels never change. Computing them once turns each of the 259
/// per-block channel scores from a strided gather plus sixteen table lookups
/// into one contiguous load.
struct Mode6Fixed<'a> {
    planar: &'a Mode6Planar,
    w: [i16; 16],
}

/// The block's pixels, transposed. Constant for every candidate the block will
/// ever try, so it is built once and borrowed.
///
/// It used to live inside `Mode6Fixed` and be rebuilt on every construction —
/// 64 stores, 8.18 times a block for the polish alone, plus once per
/// `mode6_sse`. Only the weights actually vary between those calls.
pub(super) struct Mode6Planar {
    planar: [[u8; 16]; 4],
}

impl Mode6Planar {
    #[inline]
    fn new(pixels: &[[u8; 4]; 16]) -> Self {
        #[cfg(all(feature = "simd", target_arch = "x86_64"))]
        if simd::has_avx2() {
            return Self { planar: simd::planar_avx2(pixels) };
        }
        let mut planar = [[0u8; 16]; 4];
        for (i, px) in pixels.iter().enumerate() {
            planar[0][i] = px[0];
            planar[1][i] = px[1];
            planar[2][i] = px[2];
            planar[3][i] = px[3];
        }
        Self { planar }
    }
}

impl<'a> Mode6Fixed<'a> {
    #[inline]
    fn new(planar: &'a Mode6Planar, indices: &[u8; 16]) -> Self {
        let mut w = [0i16; 16];
        for (i, slot) in w.iter_mut().enumerate() {
            *slot = W6M[indices[i] as usize] as i16;
        }
        Self { planar, w }
    }
}

/// Mode-6 SSE for **one channel**, from that channel's two unquantized
/// endpoints and the block's fixed weights.
///
/// The whole point of splitting it out: mode-6 SSE is a sum over channels, and
/// each channel's palette depends only on that channel's endpoints. So the ±1
/// endpoint sweep in [`polish_mode6_endpoints`] — which moves exactly one
/// channel of one endpoint — can rescore **one** channel instead of four.
///
/// The arithmetic is [`palette_mode6`]'s, one column of it, including the `as
/// u8` truncation, so the sum over channels is bit-identical to the whole-
/// palette form.
#[inline]
fn mode6_chan_sse(fixed: &Mode6Fixed, c: usize, v0: u8, v1: u8) -> i64 {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        return simd::mode6_chan_sse_avx2(&fixed.planar.planar[c], &fixed.w, v0, v1);
    }
    let base = v0 as i32 * 64 + 32;
    let delta = v1 as i32 - v0 as i32;
    let mut err = 0i64;
    for (i, &x) in fixed.planar.planar[c].iter().enumerate() {
        let v = ((base + fixed.w[i] as i32 * delta) >> 6) as u8;
        let d = v as i64 - x as i64;
        err += d * d;
    }
    err
}

/// Two candidates for one channel in one call — see
/// [`simd::mode6_chan_sse_pair_avx2`]. Falls back to two scalar evaluations.
#[inline]
/// Score the `-1` and `+1` candidates for one channel.
///
/// The second candidate is passed as DELTAS from the first, not as absolute
/// endpoints: the polish only ever moves one quantized endpoint by `+/-1`, and
/// `unquantize` is `(q << 1) | p`, so the two unquantized values differ by
/// exactly 4. That makes `base` differ by `4 * 64 = 256` and `delta` by 4, both
/// compile-time constants at the call site, so the kernel derives the second
/// candidate with two adds instead of two scalar computations and two
/// broadcasts.
fn mode6_chan_sse_pair(
    fixed: &Mode6Fixed,
    c: usize,
    a: u8,
    b: u8,
    dbase: i16,
    ddelta: i16,
) -> (i64, i64) {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        return simd::mode6_chan_sse_pair_avx2(
            &fixed.planar.planar[c],
            &fixed.w,
            a,
            b,
            dbase,
            ddelta,
        );
    }
    // Scalar fallback rebuilds the second candidate from the same deltas.
    let (a1, b1) = if dbase != 0 {
        ((a as i16 + dbase / 64) as u8, b)
    } else {
        (a, (b as i16 + ddelta) as u8)
    };
    (
        mode6_chan_sse(fixed, c, a, b),
        mode6_chan_sse(fixed, c, a1, b1),
    )
}

/// The four channel errors, which sum to the block's mode-6 SSE.
#[inline]
fn mode6_chan_errs(fixed: &Mode6Fixed, q0: [u8; 4], p0: u8, q1: [u8; 4], p1: u8) -> [i64; 4] {
    let c0 = unquantize_7p(q0, p0);
    let c1 = unquantize_7p(q1, p1);
    // All four channels share the weight vector and all four are always needed,
    // so they go in one call. (Contrast the four-CANDIDATE fusion in polish,
    // which was refuted: there, some of the four are discarded by the range
    // guards, and scoring them anyway gave back what the saved boundaries won.)
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        let v = [
            (c0[0], c1[0]),
            (c0[1], c1[1]),
            (c0[2], c1[2]),
            (c0[3], c1[3]),
        ];
        return simd::mode6_chan_errs_avx2(&fixed.planar.planar, &fixed.w, &v);
    }
    let mut e = [0i64; 4];
    for (c, ec) in e.iter_mut().enumerate() {
        *ec = mode6_chan_sse(fixed, c, c0[c], c1[c]);
    }
    e
}


/// `(quantized value, squared reconstruction error)` for every channel value and
/// both p-bits.
///
/// The search this replaces is a pure function of `(value, p)` over a **256 x 2**
/// domain — 512 answers total — yet it ran 33 times per block, each time scanning
/// three candidates and calling `unquantize_7p_chan` on each. The table is 2 KiB
/// and built at compile time.
///
/// The error is carried alongside because [`dp0_choice`] needs exactly that sum
/// and was recomputing it through a second quantize plus an unquantize.
///
/// Tie-break is the scalar's: strict `<`, so the lowest candidate wins, and the
/// scan order is `base-1, base, base+1` clamped to `0..=127`.
const fn build_q7_table() -> [[(u8, u16); 256]; 2] {
    let mut t = [[(0u8, 0u16); 256]; 2];
    let mut p = 0usize;
    while p < 2 {
        let mut v = 0usize;
        while v < 256 {
            let base = (v >> 1) as u8;
            let lo = if base == 0 { 0 } else { base - 1 };
            let hi = if base + 1 > 127 { 127 } else { base + 1 };
            let mut bq = if base < 127 { base } else { 127 };
            let mut be = i32::MAX;
            let mut cand = lo;
            while cand <= hi {
                let recon = (((cand as u32) << 1) | p as u32) as u8;
                let d = recon as i32 - v as i32;
                let e = d * d;
                if e < be {
                    be = e;
                    bq = cand;
                }
                cand += 1;
            }
            t[p][v] = (bq, be as u16);
            v += 1;
        }
        p += 1;
    }
    t
}

static Q7: [[(u8, u16); 256]; 2] = build_q7_table();

/// Quantize to 7 bits **and** pick the p-bit, in one pass over the table.
///
/// `dp0_choice` read `Q7` eight times to compare the two p-bits' reconstruction
/// error, then `quantize_7p_fixed` read it four more times on the same value to
/// get the quantized channels the winning row already held. Twelve lookups for
/// eight lookups' worth of information.
///
/// Same tie-break as the pair it replaces: `sum(1) < sum(0)`.
#[inline]
fn quantize_7p_best(c: [u8; 4]) -> ([u8; 4], u8) {
    let r0 = &Q7[0];
    let r1 = &Q7[1];
    let e0 = r0[c[0] as usize].1 as i32
        + r0[c[1] as usize].1 as i32
        + r0[c[2] as usize].1 as i32
        + r0[c[3] as usize].1 as i32;
    let e1 = r1[c[0] as usize].1 as i32
        + r1[c[1] as usize].1 as i32
        + r1[c[2] as usize].1 as i32
        + r1[c[3] as usize].1 as i32;
    let (row, p) = if e1 < e0 { (r1, 1u8) } else { (r0, 0u8) };
    (
        [
            row[c[0] as usize].0,
            row[c[1] as usize].0,
            row[c[2] as usize].0,
            row[c[3] as usize].0,
        ],
        p,
    )
}

/// Per-channel best 7-bit quantization under a FIXED p-bit.
fn quantize_7p_fixed(c: [u8; 4], p: u8) -> ([u8; 4], u8) {
    let row = &Q7[p as usize];
    (
        [
            row[c[0] as usize].0,
            row[c[1] as usize].0,
            row[c[2] as usize].0,
            row[c[3] as usize].0,
        ],
        p,
    )
}


/// Parse a mode-6 block back to (q0, p0, q1, p1, indices).
fn parse_mode6(block: &[u8; 16]) -> Option<([u8; 4], u8, [u8; 4], u8, [u8; 16])> {
    if block[0] & 0x7F != 0x40 {
        return None;
    }
    // Infallible: `block` is a `&[u8; 16]`, so both halves are exactly 8 bytes.
    // Spelled without `unwrap` so no user-reachable path can panic here.
    let mut lo = [0u8; 8];
    let mut hi = [0u8; 8];
    lo.copy_from_slice(&block[0..8]);
    hi.copy_from_slice(&block[8..16]);
    let low = u64::from_le_bytes(lo);
    let high = u64::from_le_bytes(hi);
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
    fixed: &Mode6Fixed,
    ce: &mut [i64; 4],
    q0: &mut [u8; 4],
    p0: u8,
    q1: &mut [u8; 4],
    p1: u8,
    err: &mut i64,
) {
    // Per-channel error, carried across the sweep. A candidate moves one channel
    // of one endpoint by ±1, which can only change that channel's term — so a
    // candidate costs ONE channel rescore, not four, and the block's total is a
    // three-add fixup. Measured at 201 `mode6_sse` calls per block before this,
    // ~51% of BC7 RDO encode.
    // `fixed` arrives from the caller rather than being rebuilt here — but do
    // NOT read that as a win. It was measured, and the second build was worth
    // **624 -> 618 instructions, -6**, against the ~50 that sixteen table
    // lookups and stores imply in the source. Whatever the second build looked
    // like at this level, the compiler had already made it nearly free. The
    // parameter is kept for the cleaner boundary and the -49/block, and the
    // number is recorded so nobody re-derives the idea expecting more.
    //
    // `ce` arrives from the caller, which just computed exactly these four
    // values to produce `err` — recomputing them here was one full
    // `mode6_chan_errs` (four kernel calls) per donor, 8.18 times a block.
    debug_assert_eq!(ce.iter().sum::<i64>(), *err);
    // A channel that did not move in a round CANNOT move in the next one, and
    // the proof is short. `mode6_chan_sse` reads only channel `c`'s endpoints,
    // so the four channels are fully independent given the fixed indices. If
    // neither `q0[c]` nor `q1[c]` moved, the next round builds bit-identical
    // candidates and gets bit-identical `cand` values. The accept test is
    // `total < *err` with `total = *err - ce[c] + cand`, so the `*err` terms
    // cancel and it reduces to `cand < ce[c]` — and `ce[c]` did not move
    // either. Every candidate that was rejected is rejected again, for the same
    // reason, at the same cost. Round 2 was rescoring them anyway.
    let mut active = [true; 4];
    for _round in 0..2 {
        let prev = *err;
        let mut moved = [false; 4];
        for which in 0..2 {
            for c in 0..4 {
                if !active[c] {
                    continue;
                }
                // A channel already at zero error cannot be improved: every
                // candidate's error is a sum of squares, so `total < err`
                // requires `cand < ce[c] = 0`, which is impossible. Skipping is
                // exact, and it is common — alpha is constant across a great
                // many blocks, and this loop runs 259 `mode6_chan_sse` calls a
                // block, 80.5% of BC7 RDO.
                if ce[c] == 0 {
                    continue;
                }
                // The endpoint we are NOT moving is fixed for both directions,
                // so its unquantized value is loop-invariant — it was being
                // re-derived on every candidate. `cur` is NOT invariant and
                // stays inside: if `d = -1` is accepted, `d = +1` must see the
                // moved value.
                let other = if which == 0 {
                    unquantize_7p_chan((*q1)[c], p1)
                } else {
                    unquantize_7p_chan((*q0)[c], p0)
                };
                let pbit = if which == 0 { p0 } else { p1 };
                let start = if which == 0 { (*q0)[c] } else { (*q1)[c] };
                // Fusing all FOUR of a channel's candidates -- both endpoints,
                // both directions -- into one call was tried and REFUTED.
                // Swapping the channel and endpoint loops to make it possible is
                // exact, and the speculation is valid 94% of the time (the
                // `which = 0` step accepts on only 6.0% of channel visits), so
                // the idea was sound. It just does not pay:
                //
                //   kernel calls   104.277 -> 78.347/blk  (-24.9%)
                //   kernel instrs    3,679 ->  3,901/blk  (+222)
                //   polish body        293 ->    287
                //   net                            about -87/blk, ~1%
                //
                // The quad kernel is cheaper per candidate (82/4 = 20.5 against
                // the pair's 44/2 = 22), but it must score all four whether or
                // not the +/-1 range guards will discard them, and that gives
                // back everything the saved call boundaries earn. Not worth a
                // speculative quad, staleness checks and a loop swap.
                //
                // Both directions read the same sixteen pixels and weights, so
                // they are scored in ONE call that loads them once. The two are
                // sequentially dependent in principle — if `-1` is accepted,
                // `+1` must see the moved value — but `-1` is accepted on 0.5%
                // of pairs, so the speculative pair is right 99.5% of the time
                // and the rare case simply rescores `+1`.
                let lo_ok = start >= 1;
                let hi_ok = start <= 126;
                let mk = |nv: u8| {
                    let mv = unquantize_7p_chan(nv, pbit);
                    if which == 0 { (mv, other) } else { (other, mv) }
                };
                let (mut cand_lo, mut cand_hi) = match (lo_ok, hi_ok) {
                    (true, true) => {
                        let (a0, b0) = mk(start - 1);
                        // `+1` differs from `-1` by 4 in the unquantized domain,
                        // on whichever endpoint is moving.
                        let (dbase, ddelta) =
                            if which == 0 { (4 * 64, -4) } else { (0, 4) };
                        let (l, h) = mode6_chan_sse_pair(fixed, c, a0, b0, dbase, ddelta);
                        (Some(l), Some(h))
                    }
                    (true, false) => {
                        let (a, b) = mk(start - 1);
                        (Some(mode6_chan_sse(fixed, c, a, b)), None)
                    }
                    (false, true) => {
                        let (a, b) = mk(start + 1);
                        (None, Some(mode6_chan_sse(fixed, c, a, b)))
                    }
                    (false, false) => (None, None),
                };
                for d in [-1i32, 1] {
                    let cur = if which == 0 { (*q0)[c] } else { (*q1)[c] };
                    let nv = cur as i32 + d;
                    if nv < 0 || nv > 127 {
                        continue;
                    }
                    let cand = match (d, if d < 0 { cand_lo } else { cand_hi }) {
                        // The speculative value is valid only while `cur` still
                        // equals the value it was computed from.
                        (_, Some(v)) if cur == start => v,
                        _ => {
                            let (a, b) = mk(nv as u8);
                            mode6_chan_sse(fixed, c, a, b)
                        }
                    };
                    if d < 0 {
                        cand_lo = None;
                    } else {
                        cand_hi = None;
                    }
                    let total = *err - ce[c] + cand;
                    if total < *err {
                        *err = total;
                        ce[c] = cand;
                        moved[c] = true;
                        if which == 0 {
                            (*q0)[c] = nv as u8;
                        } else {
                            (*q1)[c] = nv as u8;
                        }
                    }
                }
            }
        }
        active = moved;
        if *err >= prev {
            break;
        }
    }
}

/// LZ-match value the block ALREADY carries against the recent window:
/// whole-block repeat, or a repeated 4-byte half (table / endpoints).
fn score_bc1(block: &[u8; 8], recent: &[[u8; 8]]) -> f32 {
    // One `u64` per block and one `u32` per half: the whole-block test and both
    // half tests become integer compares instead of slice compares, and the
    // halves fall out of the same load. Same short-circuit, same result.
    let key = u64::from_le_bytes(*block);
    let (lo, hi) = (key as u32, (key >> 32) as u32);
    let mut best = 0f32;
    for r in recent {
        let rk = u64::from_le_bytes(*r);
        if rk == key {
            return SAVE_WHOLE;
        }
        if (rk >> 32) as u32 == hi || rk as u32 == lo {
            best = SAVE_PART;
        }
    }
    best
}


/// LZ-match value a BC7 block already carries vs the recent window.
fn score_bc7(block: &[u8; 16], recent: &[([u8; 16], bool)]) -> f32 {
    // As `score_bc1`: one `u128` for the block, two `u64` halves out of the same
    // value. Integer compares rather than slice compares.
    let key = u128::from_le_bytes(*block);
    let (lo, hi) = (key as u64, (key >> 64) as u64);
    let mut best = 0f32;
    for (r, _) in recent {
        let rk = u128::from_le_bytes(*r);
        if rk == key {
            return SAVE_WHOLE16;
        }
        if (rk >> 64) as u64 == hi || rk as u64 == lo {
            best = SAVE_HALF8;
        }
    }
    best
}

