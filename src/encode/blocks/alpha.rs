//! BC4 / BC5 single- and dual-channel blocks, and the alpha half of BC3.
//!
//! Unsigned and signed paths are separate all the way down: the signed encoder
//! scores its candidates in the UNORM reconstruction domain (the scoreboard's
//! domain), not in SNORM SSE, which is what took the signed formats to parity.

use super::*;

/// The alpha channel of a block as sixteen contiguous bytes.
///
/// Three call sites wrote `pixels.map(|p| p[3])`; see
/// `simd::alpha_channel_avx2`.
pub(super) fn alpha_channel(pixels: &[[u8; 4]; 16]) -> [u8; 16] {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        return simd::alpha_channel_avx2(pixels);
    }
    pixels.map(|p| p[3])
}

/// Min and max of the sixteen samples.
///
/// Four call sites ran this as a scalar sixteen-iteration `min`/`max` chain;
/// see `simd::alpha_minmax_avx2`, which is exact rather than approximate.
pub(super) fn sample_minmax(samples: &[u8; 16]) -> (u8, u8) {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        return simd::alpha_minmax_avx2(samples);
    }
    sample_minmax_scalar(samples)
}

/// Kept OUT of line: the fallback arm of an AVX2 dispatch.
#[cold]
#[inline(never)]
fn sample_minmax_scalar(samples: &[u8; 16]) -> (u8, u8) {
    let mut lo = 255u8;
    let mut hi = 0u8;
    for &s in samples {
        lo = lo.min(s);
        hi = hi.max(s);
    }
    (lo, hi)
}

/// Min/max endpoints only (BC3 alpha / surface-flat BC4).
pub(super) fn encode_alpha_block_fast_u(samples: [u8; 16]) -> [u8; 8] {
    let (lo, hi) = sample_minmax(&samples);
    if lo == hi {
        return pack_alpha_indices(hi, lo, &alpha_palette4_u(hi, lo), &samples);
    }
    pack_alpha_indices(hi, lo, &alpha_palette6_u(hi, lo), &samples)
}

// ---------------------------------------------------------------------------
// BC4 / BC5
// ---------------------------------------------------------------------------

pub fn encode_bc4(pixels: [[u8; 4]; 16], signed: bool, out: &mut [u8]) {
    out[..8].copy_from_slice(&encode_alpha_block(pixels.map(|p| p[0]), signed));
}

pub fn encode_bc5(pixels: [[u8; 4]; 16], signed: bool, out: &mut [u8]) {
    out[..8].copy_from_slice(&encode_alpha_block(pixels.map(|p| p[0]), signed));
    out[8..16].copy_from_slice(&encode_alpha_block(pixels.map(|p| p[1]), signed));
}

/// Surface-flat BC4/5: dual min/max only.
pub fn encode_bc4_flat(pixels: [[u8; 4]; 16], signed: bool, out: &mut [u8]) {
    out[..8].copy_from_slice(&encode_alpha_block_flat(pixels.map(|p| p[0]), signed));
}

pub fn encode_bc5_flat(pixels: [[u8; 4]; 16], signed: bool, out: &mut [u8]) {
    out[..8].copy_from_slice(&encode_alpha_block_flat(pixels.map(|p| p[0]), signed));
    out[8..16].copy_from_slice(&encode_alpha_block_flat(pixels.map(|p| p[1]), signed));
}

pub(super) fn encode_alpha_block(samples: [u8; 16], signed: bool) -> [u8; 8] {
    if signed {
        encode_alpha_block_signed(samples)
    } else {
        encode_alpha_block_unsigned(samples)
    }
}

pub(super) fn encode_alpha_block_flat(samples: [u8; 16], signed: bool) -> [u8; 8] {
    if signed {
        let mut vals = [0i32; 16];
        let mut lo = 127i32;
        let mut hi = -127i32;
        for (i, &s) in samples.iter().enumerate() {
            let v = unorm_u8_to_snorm_i32(s);
            vals[i] = v;
            lo = lo.min(v);
            hi = hi.max(v);
        }
        if lo == hi {
            return pack_alpha_indices_s(hi, lo, &alpha_palette4_s(hi, lo), &samples);
        }
        let mut best = pack_alpha_indices_s(hi, lo, &alpha_palette6_s(hi, lo), &samples);
        let mut best_err = alpha_sse_s(&samples, &best);
        consider_alpha_s(lo, hi, &samples, &mut best, &mut best_err);
        best
    } else {
        encode_alpha_block_fast_u(samples)
    }
}

pub(super) fn encode_alpha_block_unsigned(samples: [u8; 16]) -> [u8; 8] {
    let (lo, hi) = sample_minmax(&samples);
    if lo == hi {
        return pack_alpha_indices(hi, lo, &alpha_palette4_u(hi, lo), &samples);
    }

    let mut best = pack_alpha_indices(hi, lo, &alpha_palette6_u(hi, lo), &samples);
    let mut best_err = alpha_sse_u(&samples, &best);
    consider_alpha_u(lo, hi, &samples, &mut best, &mut best_err);
    let span = hi as i32 - lo as i32;

    let fast = quality_is_fast();
    let (n_unique, uniq) = unique_values_u_capped(&samples, 5);
    // Unique-pairs: skip in Fast; skip when dual already near-perfect; full exhaust only ≤4 uniques
    // (5-uniques only when residual still high after dual).
    if !fast && n_unique <= 5 {
        if best_err > 4 && (n_unique <= 4 || best_err > 16) {
            consider_unique_pairs_u(&samples, &uniq[..n_unique], &mut best, &mut best_err);
        }
    } else if !fast && n_unique > 5 && span > 4 {
        let a_hi = (hi as i32 - 1) as u8;
        let a_lo = (lo as i32 + 1) as u8;
        consider_alpha_u(a_hi, a_lo, &samples, &mut best, &mut best_err);
        consider_alpha_u(a_lo, a_hi, &samples, &mut best, &mut best_err);
    }
    refine_alpha_u(&samples, n_unique, span, &mut best, &mut best_err);
    // Unsigned twin of the signed windowed sweep (same gate + prune).
    // DEFAULT OFF: it costs ~3.2s corpus CPU for +0.15..0.45 dB on 14 cases
    // that already beat DirectXTex by 0.4-1.9 dB — the CPU budget instead
    // funds the BC1 lattice and BC7 mode 5, whose gains are 2-20x larger.
    // Opt in with RUSTY_DDS_BC45U_WINDOW=1. (err>16 tight-gate variant was
    // tried: keeps only 10-45% of the smooth-map gains, which live at err
    // 5..16.)
    if unsigned_window_enabled()
        && !quality_is_fast()
        && (16..=64).contains(&span)
        && best_err > 4
    {
        unsigned_window_sweep(&samples, &mut best, &mut best_err);
    }
    best
}

/// ±4 exhaustive window around the unsigned winner; 6-lerp pairs whose
/// palette range provably can't beat `best_err` are skipped (4-lerp pairs
/// carry 0/255 sentinels and are never pruned); strict `<` keeps it
/// quality-monotone.
pub(super) fn unsigned_window_sweep(samples: &[u8; 16], best: &mut [u8; 8], best_err: &mut i32) {
    let b0 = best[0] as i32;
    let b1 = best[1] as i32;
    let (smin, smax) = sample_minmax(samples);
    for d0 in -4i32..=4 {
        for d1 in -4i32..=4 {
            if d0 == 0 && d1 == 0 {
                continue;
            }
            let a0 = (b0 + d0).clamp(0, 255) as u8;
            let a1 = (b1 + d1).clamp(0, 255) as u8;
            if a0 > a1 {
                let over = (smax as i32 - a0 as i32).max(0);
                let under = (a1 as i32 - smin as i32).max(0);
                if over * over + under * under >= *best_err {
                    continue;
                }
            }
            consider_alpha_u(a0, a1, samples, best, best_err);
            if *best_err == 0 {
                return;
            }
        }
    }
}

/// Collect up to `cap` uniques; returns `cap+1` if busier (no full sort).
pub(super) fn unique_values_u_capped(samples: &[u8; 16], cap: usize) -> (usize, [u8; 16]) {
    let mut uniq = [0u8; 16];
    let mut n = 0usize;
    'outer: for &s in samples {
        for i in 0..n {
            if uniq[i] == s {
                continue 'outer;
            }
        }
        if n >= cap {
            return (cap + 1, uniq);
        }
        uniq[n] = s;
        n += 1;
    }
    (n, uniq)
}

pub(super) fn consider_unique_pairs_u(
    samples: &[u8; 16],
    uniq: &[u8],
    best: &mut [u8; 8],
    best_err: &mut i32,
) {
    for i in 0..uniq.len() {
        for j in 0..uniq.len() {
            if i == j {
                continue;
            }
            consider_alpha_u(uniq[i], uniq[j], samples, best, best_err);
            if *best_err == 0 {
                return;
            }
        }
    }
}

/// LS, then content-adaptive local refine.
/// When `RUSTY_DDS_BC45_REFINE_HARVEST` is set, always runs neighborhood and logs
/// null vs post-search SSE (observe-only; decisions match the non-gated path).
pub(super) fn refine_alpha_u(
    samples: &[u8; 16],
    n_unique: usize,
    span: i32,
    best: &mut [u8; 8],
    best_err: &mut i32,
) {
    if *best_err == 0 {
        return;
    }
    // LS skip: after good seeds / unique-pairs, float LS rarely moves endpoints.
    let full = crate::encode::harvest::full_refine();
    let fast = quality_is_fast();
    let do_ls = full || (!fast && n_unique > 2 && *best_err > 8) || (fast && *best_err > 8);
    if do_ls {
        // Iterate LS -> index refit while SSE keeps falling (BC1-refine shape).
        for _ in 0..4 {
            let Some((r0, r1)) = ls_alpha_endpoints_u(samples, best) else {
                break;
            };
            let prev = *best_err;
            consider_alpha_u(r0, r1, samples, best, best_err);
            consider_alpha_u(r1, r0, samples, best, best_err);
            if *best_err == 0 {
                return;
            }
            if *best_err >= prev {
                break;
            }
        }
    }
    // Fast: no neighborhood. Simple low-err: no neighborhood.
    if fast || (!full && n_unique <= 5 && *best_err <= 24) {
        return;
    }
    let d = if *best_err > 64 {
        2
    } else if *best_err > 8 {
        1
    } else {
        0
    };
    if d == 0 {
        return;
    }

    let harvesting = crate::encode::harvest::enabled();
    let score = neighborhood_score(*best_err, span);
    if !full && !harvesting && skip_neighborhood(*best_err, score, n_unique) {
        return;
    }

    let axis = n_unique > 5;
    let null_err = *best_err;
    refine_alpha_neighborhood_u(samples, best, best_err, d, axis);
    if harvesting {
        crate::encode::harvest::record(false, n_unique, span, null_err, *best_err, axis);
    }
}

/// `null_err * 16 / span` — span-normalized LS residual (corpus harvest feature).
#[inline]
pub(super) fn neighborhood_score(null_err: i32, span: i32) -> i32 {
    (null_err * 16) / span.max(1)
}

/// Skip ±N when LS residual is span-small.
/// Ceiling (ambientCG busy blocks reaching neighborhood, 512k rows):
/// - score<=8 → 26.7% skip / 96.13% gain-kept (prior ship)
/// - score<=9 → 35.5% skip / 93.76% gain-kept
/// - score<=10 → 43.7% skip / 90.95% gain-kept  ← busy ship (holdout maps)
#[inline]
pub(super) fn skip_neighborhood(null_err: i32, score: i32, _n_unique: usize) -> bool {
    null_err <= 4 || score <= 10
}

pub(super) fn refine_alpha_neighborhood_u(
    samples: &[u8; 16],
    best: &mut [u8; 8],
    best_err: &mut i32,
    d: i32,
    axis_aligned: bool,
) {
    let b0 = best[0];
    let b1 = best[1];
    if axis_aligned {
        let six = b0 > b1;
        for d0 in -d..=d {
            if d0 == 0 {
                continue;
            }
            let a0 = (b0 as i32 + d0).clamp(0, 255) as u8;
            if (a0 > b1) != six {
                continue;
            }
            consider_alpha_u(a0, b1, samples, best, best_err);
            if *best_err == 0 {
                return;
            }
        }
        let b0 = best[0];
        let b1 = best[1];
        let six = b0 > b1;
        for d1 in -d..=d {
            if d1 == 0 {
                continue;
            }
            let a1 = (b1 as i32 + d1).clamp(0, 255) as u8;
            if (b0 > a1) != six {
                continue;
            }
            consider_alpha_u(b0, a1, samples, best, best_err);
            if *best_err == 0 {
                return;
            }
        }
    } else {
        for d0 in -d..=d {
            for d1 in -d..=d {
                if d0 == 0 && d1 == 0 {
                    continue;
                }
                let a0 = (b0 as i32 + d0).clamp(0, 255) as u8;
                let a1 = (b1 as i32 + d1).clamp(0, 255) as u8;
                consider_alpha_u(a0, a1, samples, best, best_err);
                if *best_err == 0 {
                    return;
                }
            }
        }
    }
}

pub(super) fn consider_alpha_u(a0: u8, a1: u8, samples: &[u8; 16], best: &mut [u8; 8], best_err: &mut i32) {
    if a0 == a1 {
        return;
    }
    let palette = if a0 > a1 {
        alpha_palette6_u(a0, a1)
    } else {
        alpha_palette4_u(a0, a1)
    };
    if let Some((packed, err)) = pack_alpha_indices_err(a0, a1, &palette, samples, *best_err) {
        *best_err = err;
        *best = packed;
    }
}

/// Ascending value order of the 6-lerp alpha palette (a0 > a1): a1 first,
/// interpolants descend from idx2 (near a0) — and of the 4-lerp palette
/// (a0 <= a1): sentinel 0, a0, interpolants ascend, a1, sentinel 255.
pub(super) const ALPHA_ORDER6: [u8; 8] = [1, 7, 6, 5, 4, 3, 2, 0];
pub(super) const ALPHA_ORDER4: [u8; 8] = [6, 0, 2, 3, 4, 5, 1, 7];

/// Threshold selector for nearest-palette-entry: 7 boundaries between the
/// (deduped) ascending values, tie rules baked to reproduce the linear
/// scan's strict-`<` lowest-index behaviour EXACTLY. Proven byte-identical
/// by full enumeration (`alpha_select_matches_linear_exhaustive`).
pub(super) struct AlphaSelect {
    thr: [i32; 7],
    lut: [u8; 8],
    n: usize,
}

impl AlphaSelect {
    pub(super) fn build(palette: &[u8; 8], order: &[u8; 8]) -> Self {
        // Dedupe equal values keeping the LOWEST original index (the linear
        // scan's tie winner among equal values).
        let mut vals = [0i32; 8];
        let mut idxs = [0u8; 8];
        let mut n = 0usize;
        for &o in order {
            let v = palette[o as usize] as i32;
            if n > 0 && vals[n - 1] == v {
                if o < idxs[n - 1] {
                    idxs[n - 1] = o;
                }
                continue;
            }
            vals[n] = v;
            idxs[n] = o;
            n += 1;
        }
        let mut thr = [i32::MAX; 7];
        for k in 0..n - 1 {
            let (vl, vh) = (vals[k], vals[k + 1]);
            let sum = vl + vh;
            let m = sum >> 1;
            // s > thr[k] selects the HIGH side. Odd sum: no exact tie.
            // Even sum: tie at m goes to the lower ORIGINAL index.
            thr[k] = if sum & 1 == 1 {
                m
            } else if idxs[k] < idxs[k + 1] {
                m
            } else {
                m - 1
            };
        }
        Self { thr, lut: idxs, n }
    }

    #[inline]
    pub(super) fn select(&self, s: u8) -> u8 {
        let s = s as i32;
        let mut rank = 0usize;
        for k in 0..self.n.saturating_sub(1) {
            rank += (s > self.thr[k]) as usize;
        }
        self.lut[rank]
    }
}

/// Pack indices + SSE; returns `None` if SSE cannot beat `err_limit` (early abort).
/// Pack endpoints plus sixteen 3-bit indices into the eight-byte alpha block.
#[inline]
/// Sixteen 3-bit indices as one 48-bit word.
///
/// This loop existed in FOUR places; see `simd::alpha_pack_indices_avx2` for
/// the vector form and why its folds cannot saturate.
fn pack_index_bits(indices: &[u8; 16]) -> u64 {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        return simd::alpha_pack_indices_avx2(indices);
    }
    pack_index_bits_scalar(indices)
}

/// Kept OUT of line: the fallback arm of an AVX2 dispatch.
#[cold]
#[inline(never)]
fn pack_index_bits_scalar(indices: &[u8; 16]) -> u64 {
    let mut acc = 0u64;
    for (i, idx) in indices.iter().enumerate() {
        acc |= (*idx as u64) << (3 * i);
    }
    acc
}

/// The six index bytes of an alpha block, as one little-endian word.
///
/// A BC4/BC5 block is two endpoint bytes then six bytes of packed 3-bit
/// indices, little-endian and contiguous — so this is a load, not a
/// six-iteration shift-and-or. The loop it replaces existed in THREE places.
#[inline]
fn alpha_index_bits(block: &[u8; 8]) -> u64 {
    u64::from_le_bytes([
        block[2], block[3], block[4], block[5], block[6], block[7], 0, 0,
    ])
}

/// The inverse: write the low six bytes of `bits` into the block's index area.
///
/// Existed in FOUR places as a six-iteration shift-mask-store.
#[inline]
fn write_index_bits(out: &mut [u8; 8], bits: u64) {
    out[2..8].copy_from_slice(&bits.to_le_bytes()[..6]);
}

fn pack_alpha_out(a0: u8, a1: u8, indices: &[u8; 16], err: i32) -> ([u8; 8], i32) {
    let mut out = [0u8; 8];
    out[0] = a0;
    out[1] = a1;
    let bits = pack_index_bits(indices);
    write_index_bits(&mut out, bits);
    (out, err)
}

pub(super) fn pack_alpha_indices_err(
    a0: u8,
    a1: u8,
    palette: &[u8; 8],
    samples: &[u8; 16],
    err_limit: i32,
) -> Option<([u8; 8], i32)> {
    // Vectorised nearest-palette scan: sixteen samples against eight entries in
    // registers, no per-candidate selector to build. Byte-identical to the
    // scalar scan, which is its oracle.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if super::simd::has_avx2() {
        let (indices, err) = super::simd::alpha_fit_avx2(palette, samples);
        if err >= err_limit {
            return None;
        }
        return Some(pack_alpha_out(a0, a1, &indices, err));
    }
    let (indices, err) = pack_alpha_indices_err_scalar(a0, a1, palette, samples, err_limit)?;
    Some(pack_alpha_out(a0, a1, &indices, err))
}

/// Kept OUT of line: the fallback arm of an AVX2 dispatch, never executed on a
/// machine with AVX2.
///
/// It carries BOTH scalar selectors — the `AlphaSelect` table build and the
/// plain eight-entry scan — and inlined at the dispatch the pair landed whole
/// inside every caller: `consider_alpha_u` measured 1191 instructions and
/// `encode_alpha_block_unsigned` 1217, with sixteen vector instructions between
/// them.
#[cold]
#[inline(never)]
fn pack_alpha_indices_err_scalar(
    a0: u8,
    a1: u8,
    palette: &[u8; 8],
    samples: &[u8; 16],
    err_limit: i32,
) -> Option<([u8; 16], i32)> {
    let _ = (a0, a1);
    let mut indices = [0u8; 16];
    let mut err = 0i32;
    if alpha_sel_enabled() {
        let order = if a0 > a1 { &ALPHA_ORDER6 } else { &ALPHA_ORDER4 };
        let sel = AlphaSelect::build(palette, order);
        for (i, &s) in samples.iter().enumerate() {
            let best = sel.select(s);
            indices[i] = best;
            let diff = palette[best as usize] as i32 - s as i32;
            err += diff * diff;
            if err >= err_limit {
                return None;
            }
        }
    } else {
        for (i, &s) in samples.iter().enumerate() {
            let mut best = 0u8;
            let mut best_d = i32::MAX;
            for (j, &p) in palette.iter().enumerate() {
                let d = (p as i32 - s as i32).abs();
                if d < best_d {
                    best_d = d;
                    best = j as u8;
                }
            }
            indices[i] = best;
            let diff = palette[best as usize] as i32 - s as i32;
            err += diff * diff;
            if err >= err_limit {
                return None;
            }
        }
    }
    Some((indices, err))
}

/// Least-squares endpoints from current indices (6-lerp weights). Falls back to None on 4-lerp.
pub(super) fn ls_alpha_endpoints_u(samples: &[u8; 16], block: &[u8; 8]) -> Option<(u8, u8)> {
    let a0 = block[0];
    let a1 = block[1];
    if a0 <= a1 {
        return None; // 4-lerp uses 0/255 sentinels — skip LS.
    }
    let bits = alpha_index_bits(block);
    // BC4 6-lerp: idx 0→e0, 1→e1, 2..7 → (6..1)*e0 + (1..6)*e1 over 7.
    const W: [f32; 8] = [0.0, 1.0, 1.0 / 7.0, 2.0 / 7.0, 3.0 / 7.0, 4.0 / 7.0, 5.0 / 7.0, 6.0 / 7.0];
    let mut sw = 0.0f32;
    let mut sw2 = 0.0f32;
    let mut sx = 0.0f32;
    let mut sxw = 0.0f32;
    let mut n = 0.0f32;
    for i in 0..16 {
        let idx = ((bits >> (3 * i)) & 7) as usize;
        let w = W[idx]; // weight toward e1
        let x = samples[i] as f32;
        sw += w;
        sw2 += w * w;
        sx += x;
        sxw += x * w;
        n += 1.0;
    }
    let det = n * sw2 - sw * sw;
    if det.abs() < 1e-3 {
        return None;
    }
    let e0 = (sx * sw2 - sxw * sw) / det;
    let e1 = (n * sxw - sw * sx) / det;
    Some((
        e0.round().clamp(0.0, 255.0) as u8,
        e1.round().clamp(0.0, 255.0) as u8,
    ))
}

/// Integer UNORM→SNORM matching `round(((u/255)*2-1)*127)` clamped to [-127,127].
#[inline]
pub(super) fn unorm_u8_to_snorm_i32(u: u8) -> i32 {
    (((u as i32) * 253 + 127) / 254 - 127).clamp(-127, 127)
}

/// SNORM i32 → UNORM u8 LUT (index = s + 127 for s in [-127, 127]).
pub(super) const SNORM_TO_UNORM: [u8; 255] = {
    let mut t = [0u8; 255];
    let mut i = 0;
    while i < 255 {
        let s = i as i32 - 127;
        t[i] = ((((s + 127) * 255) + 127) / 254) as u8;
        i += 1;
    }
    t
};

/// SNORM i32 → UNORM u8, matching the corpus scoreboard (`snorm_bits_to_unorm`).
pub(super) fn snorm_i32_to_unorm_u8(s: i32) -> u8 {
    SNORM_TO_UNORM[(s.clamp(-127, 127) + 127) as usize]
}

pub(super) fn encode_alpha_block_signed(samples: [u8; 16]) -> [u8; 8] {
    let (mut best, mut best_err, lo, hi, span, _n_unique) =
        encode_alpha_block_signed_presweep(samples);
    // Windowed endpoint sweep: the LS/±2 search leaves ~0.5 dB on smooth
    // signed content (Wood normals) — the UNORM-scored optimum sits beyond
    // its reach but NEAR the current best (harvest: ±4 window keeps 93% of
    // Wood's ceiling gain at ~80 pairs/block; a full-span sweep costs 6-25x
    // more for gains only on maps already ahead of DirectXTex).
    let _ = (lo, hi);
    // DEFAULT OFF, matching the unsigned twin — see `BC45_SIGNED_WINDOW`.
    // Measured serial on the corpus: this sweep cost 3-5x the encode time for
    // 0.05-0.61 dB on maps we already led.
    if signed_window_enabled() && !quality_is_fast() && signed_sweep_gate(span, best_err) {
        signed_window_sweep(&samples, &mut best, &mut best_err);
    }
    best
}

/// ±4 exhaustive window around the pre-sweep winner (both palette modes are
/// reachable near the b0==b1 boundary); strict `<` keeps it quality-monotone.
pub(super) fn signed_window_sweep(samples: &[u8; 16], best: &mut [u8; 8], best_err: &mut i32) {
    let b0 = best[0] as i8 as i32;
    let b1 = best[1] as i8 as i32;
    // Range lower bound (busy-block pruning): a sample above the pair's
    // UNORM palette ceiling contributes at least (smax-phi)^2, and one below
    // the floor at least (plo-smin)^2 — 6-lerp palettes lie inside their
    // endpoints, so a pair whose bound reaches best_err provably cannot win
    // and skipping it is byte-identical. (4-lerp pairs carry 0/255 sentinels
    // and are never pruned.)
    let (smin, smax) = sample_minmax(samples);
    for d0 in -4i32..=4 {
        for d1 in -4i32..=4 {
            if d0 == 0 && d1 == 0 {
                continue;
            }
            let a0 = (b0 + d0).clamp(-127, 127);
            let a1 = (b1 + d1).clamp(-127, 127);
            if a0 > a1 {
                let phi = snorm_i32_to_unorm_u8(a0) as i32;
                let plo = snorm_i32_to_unorm_u8(a1) as i32;
                let over = (smax as i32 - phi).max(0);
                let under = (plo - smin as i32).max(0);
                if over * over + under * under >= *best_err {
                    continue;
                }
            }
            consider_alpha_s(a0, a1, samples, best, best_err);
            if *best_err == 0 {
                return;
            }
        }
    }
}

/// The full signed search pipeline BEFORE the exhaustive sweep.
/// Returns (block, err, lo, hi, span, n_unique) — split out so the harvest
/// test can measure the sweep's null arm.
pub(super) fn encode_alpha_block_signed_presweep(samples: [u8; 16]) -> ([u8; 8], i32, i32, i32, i32, usize) {
    // Search endpoints in SNORM, but score (and assign indices) by UNORM recon —
    // the bake-off PSNR is UNORM after snorm→unorm, not SNORM-domain SSE.
    let mut vals = [0i32; 16];
    let mut lo = 127i32;
    let mut hi = -127i32;
    for (i, &s) in samples.iter().enumerate() {
        let v = unorm_u8_to_snorm_i32(s);
        vals[i] = v;
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if lo == hi {
        let b = pack_alpha_indices_s(hi, lo, &alpha_palette4_s(hi, lo), &samples);
        let err = alpha_sse_s(&samples, &b);
        return (b, err, lo, hi, 0, 1);
    }

    let mut best = pack_alpha_indices_s(hi, lo, &alpha_palette6_s(hi, lo), &samples);
    let mut best_err = alpha_sse_s(&samples, &best);
    consider_alpha_s(lo, hi, &samples, &mut best, &mut best_err);
    let span = hi - lo;

    let fast = quality_is_fast();
    let (n_unique, uniq) = unique_values_s_capped(&vals, 5);
    if !fast && n_unique <= 5 {
        if best_err > 4 && (n_unique <= 4 || best_err > 16) {
            consider_unique_pairs_s(&samples, &uniq[..n_unique], &mut best, &mut best_err);
        }
    } else if !fast && n_unique > 5 && span > 4 {
        consider_alpha_s(hi - 1, lo + 1, &samples, &mut best, &mut best_err);
        consider_alpha_s(lo + 1, hi - 1, &samples, &mut best, &mut best_err);
    }
    refine_alpha_s(&samples, &vals, n_unique, span, &mut best, &mut best_err);
    (best, best_err, lo, hi, span, n_unique)
}

/// Harvest-tuned gate (target/signed_sweep_harvest.csv, 643k blocks):
/// span < 8 gained ZERO across 169k blocks, and `gain <= best_err` means a
/// tiny residual can never pay for the window.
#[inline]
pub(super) fn signed_sweep_gate(span: i32, best_err: i32) -> bool {
    // Harvest-tuned (target/signed_sweep_harvest.csv, 643k blocks): span < 8
    // gained ZERO across 169k blocks; spans > 32 only gain on maps already
    // ahead of DirectXTex; `gain <= best_err` means err <= 4 can't pay.
    (8..=32).contains(&span) && best_err > 4
}

/// Bounded near-exhaustive signed endpoint sweep (both orders => both
/// palette modes), candidates added under strict `<` (quality-monotone).
#[cfg(test)]
pub(super) fn signed_sweep(lo: i32, hi: i32, samples: &[u8; 16], best: &mut [u8; 8], best_err: &mut i32) {
    let a_lo = (lo - 8).max(-127);
    let a_hi = (hi + 8).min(127);
    for e0 in a_lo..=a_hi {
        for e1 in a_lo..=a_hi {
            if e0 == e1 {
                continue;
            }
            consider_alpha_s(e0, e1, samples, best, best_err);
            if *best_err == 0 {
                return;
            }
        }
    }
}

pub(super) fn unique_values_s_capped(vals: &[i32; 16], cap: usize) -> (usize, [i32; 16]) {
    let mut uniq = [0i32; 16];
    let mut n = 0usize;
    'outer: for &s in vals {
        for i in 0..n {
            if uniq[i] == s {
                continue 'outer;
            }
        }
        if n >= cap {
            return (cap + 1, uniq);
        }
        uniq[n] = s;
        n += 1;
    }
    (n, uniq)
}

pub(super) fn consider_unique_pairs_s(
    samples: &[u8; 16],
    uniq: &[i32],
    best: &mut [u8; 8],
    best_err: &mut i32,
) {
    for i in 0..uniq.len() {
        for j in 0..uniq.len() {
            if i == j {
                continue;
            }
            consider_alpha_s(uniq[i], uniq[j], samples, best, best_err);
            if *best_err == 0 {
                return;
            }
        }
    }
}

pub(super) fn refine_alpha_s(
    samples: &[u8; 16],
    vals: &[i32; 16],
    n_unique: usize,
    span: i32,
    best: &mut [u8; 8],
    best_err: &mut i32,
) {
    if *best_err == 0 {
        return;
    }
    let full = crate::encode::harvest::full_refine();
    let fast = quality_is_fast();
    let do_ls = full || (!fast && n_unique > 2 && *best_err > 8) || (fast && *best_err > 8);
    if do_ls {
        // Iterate LS -> index refit while the UNORM-scored SSE keeps falling
        // (same shape as the BC1 refine loop; each pass is ~2 pack evals).
        for _ in 0..4 {
            let Some((r0, r1)) = ls_alpha_endpoints_s(vals, best) else {
                break;
            };
            let prev = *best_err;
            consider_alpha_s(r0, r1, samples, best, best_err);
            consider_alpha_s(r1, r0, samples, best, best_err);
            if *best_err == 0 {
                return;
            }
            if *best_err >= prev {
                break;
            }
        }
    }
    if fast || (!full && n_unique <= 5 && *best_err <= 24) {
        return;
    }
    let d = if *best_err > 64 {
        2
    } else if *best_err > 8 {
        1
    } else {
        0
    };
    if d == 0 {
        return;
    }

    let harvesting = crate::encode::harvest::enabled();
    let score = neighborhood_score(*best_err, span);
    if !full && !harvesting && skip_neighborhood(*best_err, score, n_unique) {
        return;
    }

    let axis = n_unique > 5;
    let null_err = *best_err;
    refine_alpha_neighborhood_s(samples, best, best_err, d, axis);
    if harvesting {
        crate::encode::harvest::record(true, n_unique, span, null_err, *best_err, axis);
    }
}

pub(super) fn refine_alpha_neighborhood_s(
    samples: &[u8; 16],
    best: &mut [u8; 8],
    best_err: &mut i32,
    d: i32,
    axis_aligned: bool,
) {
    let b0 = best[0] as i8 as i32;
    let b1 = best[1] as i8 as i32;
    if axis_aligned {
        let six = b0 > b1;
        for d0 in -d..=d {
            if d0 == 0 {
                continue;
            }
            let a0 = (b0 + d0).clamp(-127, 127);
            if (a0 > b1) != six {
                continue;
            }
            consider_alpha_s(a0, b1, samples, best, best_err);
            if *best_err == 0 {
                return;
            }
        }
        let b0 = best[0] as i8 as i32;
        let b1 = best[1] as i8 as i32;
        let six = b0 > b1;
        for d1 in -d..=d {
            if d1 == 0 {
                continue;
            }
            let a1 = (b1 + d1).clamp(-127, 127);
            if (b0 > a1) != six {
                continue;
            }
            consider_alpha_s(b0, a1, samples, best, best_err);
            if *best_err == 0 {
                return;
            }
        }
    } else {
        for d0 in -d..=d {
            for d1 in -d..=d {
                if d0 == 0 && d1 == 0 {
                    continue;
                }
                consider_alpha_s(
                    (b0 + d0).clamp(-127, 127),
                    (b1 + d1).clamp(-127, 127),
                    samples,
                    best,
                    best_err,
                );
                if *best_err == 0 {
                    return;
                }
            }
        }
    }
}

pub(super) fn consider_alpha_s(
    a0: i32,
    a1: i32,
    samples: &[u8; 16],
    best: &mut [u8; 8],
    best_err: &mut i32,
) {
    if a0 == a1 {
        return;
    }
    let a0 = a0.clamp(-127, 127);
    let a1 = a1.clamp(-127, 127);
    if a0 == a1 {
        return;
    }
    let palette = if a0 > a1 {
        alpha_palette6_s(a0, a1)
    } else {
        alpha_palette4_s(a0, a1)
    };
    if let Some((packed, err)) = pack_alpha_indices_s_err(a0, a1, &palette, samples, *best_err) {
        *best_err = err;
        *best = packed;
    }
}

pub(super) fn pack_alpha_indices_s_err(
    a0: i32,
    a1: i32,
    palette: &[i32; 8],
    samples: &[u8; 16],
    err_limit: i32,
) -> Option<([u8; 8], i32)> {
    let mut pal_u = [0u8; 8];
    for (i, &p) in palette.iter().enumerate() {
        pal_u[i] = snorm_i32_to_unorm_u8(p);
    }
    // One vector pass for all sixteen samples. The scalar form aborted on the
    // running prefix; every term is a squared difference and so non-negative,
    // which makes the running sum monotone — "some prefix reaches the limit"
    // and "the total reaches the limit" are the same statement, so one
    // comparison decides it.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    let (indices, err) = if simd::has_avx2() {
        simd::alpha_select_avx2(&pal_u, samples)
    } else {
        alpha_select_scalar(&pal_u, samples)
    };
    #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
    let (indices, err) = alpha_select_scalar(&pal_u, samples);
    if err >= err_limit {
        return None;
    }
    let mut out = [0u8; 8];
    out[0] = a0 as i8 as u8;
    out[1] = a1 as i8 as u8;
    let bits = pack_index_bits(&indices);
    write_index_bits(&mut out, bits);
    Some((out, err))
}

pub(super) fn ls_alpha_endpoints_s(vals: &[i32; 16], block: &[u8; 8]) -> Option<(i32, i32)> {
    let a0 = block[0] as i8 as i32;
    let a1 = block[1] as i8 as i32;
    if a0 <= a1 {
        return None;
    }
    let bits = alpha_index_bits(block);
    const W: [f32; 8] = [0.0, 1.0, 1.0 / 7.0, 2.0 / 7.0, 3.0 / 7.0, 4.0 / 7.0, 5.0 / 7.0, 6.0 / 7.0];
    let mut sw = 0.0f32;
    let mut sw2 = 0.0f32;
    let mut sx = 0.0f32;
    let mut sxw = 0.0f32;
    let mut n = 0.0f32;
    for i in 0..16 {
        let idx = ((bits >> (3 * i)) & 7) as usize;
        let w = W[idx];
        let x = vals[i] as f32;
        sw += w;
        sw2 += w * w;
        sx += x;
        sxw += x * w;
        n += 1.0;
    }
    let det = n * sw2 - sw * sw;
    if det.abs() < 1e-3 {
        return None;
    }
    let e0 = (sx * sw2 - sxw * sw) / det;
    let e1 = (n * sxw - sw * sx) / det;
    Some((
        e0.round().clamp(-127.0, 127.0) as i32,
        e1.round().clamp(-127.0, 127.0) as i32,
    ))
}

pub(super) fn alpha_sse_u(samples: &[u8; 16], block: &[u8; 8]) -> i32 {
    let a0 = block[0];
    let a1 = block[1];
    let palette = if a0 > a1 {
        alpha_palette6_u(a0, a1)
    } else {
        alpha_palette4_u(a0, a1)
    };
    let bits = alpha_index_bits(block);
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        return simd::alpha_fixed_sse_avx2(samples, &palette, bits);
    }
    alpha_fixed_sse_scalar(samples, &palette, bits)
}


/// Kept OUT of line: the fallback arm of an AVX2 dispatch.
#[cold]
#[inline(never)]
fn alpha_fixed_sse_scalar(samples: &[u8; 16], palette: &[u8; 8], bits: u64) -> i32 {
    let mut err = 0i32;
    for i in 0..16 {
        let idx = ((bits >> (3 * i)) & 7) as usize;
        let d = palette[idx] as i32 - samples[i] as i32;
        err += d * d;
    }
    err
}

pub(super) fn alpha_sse_s(samples: &[u8; 16], block: &[u8; 8]) -> i32 {
    let a0 = block[0] as i8 as i32;
    let a1 = block[1] as i8 as i32;
    let palette = if a0 > a1 {
        alpha_palette6_s(a0, a1)
    } else {
        alpha_palette4_s(a0, a1)
    };
    // The signed palette is converted to the unorm bytes the comparison
    // actually uses ONCE, ahead of the scan, instead of inside all sixteen
    // iterations — after which this is the same kernel the unsigned twin runs.
    let mut pal_u = [0u8; 8];
    for (i, &p) in palette.iter().enumerate() {
        pal_u[i] = snorm_i32_to_unorm_u8(p);
    }
    let bits = alpha_index_bits(block);
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if simd::has_avx2() {
        return simd::alpha_fixed_sse_avx2(samples, &pal_u, bits);
    }
    alpha_fixed_sse_scalar(samples, &pal_u, bits)
}

pub(super) fn pack_alpha_indices(a0: u8, a1: u8, palette: &[u8; 8], samples: &[u8; 16]) -> [u8; 8] {
    // Sixteen samples against eight entries by |p - s|, first minimum wins.
    //
    // That is character-for-character what `alpha_select_avx2` does, and what
    // the signed twin `pack_alpha_indices_s` has routed to since the presweep
    // work — this one was simply never wired up and stayed a 677-instruction
    // scalar double loop. Same comparison, same strict `<`, so the same
    // lowest-index tie-break: byte-identical, and the shared oracle already
    // guards the kernel.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    let indices = if simd::has_avx2() {
        simd::alpha_select_avx2(palette, samples).0
    } else {
        alpha_select_scalar(palette, samples).0
    };
    #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
    let indices = alpha_select_scalar(palette, samples).0;
    let mut out = [0u8; 8];
    out[0] = a0;
    out[1] = a1;
    let bits = pack_index_bits(&indices);
    write_index_bits(&mut out, bits);
    out
}

/// Nearest-palette index per sample, and the resulting SSE.
///
/// Scalar twin of [`simd::alpha_select_avx2`]; the vector form must match it
/// exactly, including the FIRST-minimum tie-break of the strict `<`.
/// Kept OUT of line: this is the fallback arm of an AVX2 dispatch, so on
/// any machine that has AVX2 it is never executed — but inlined at the
/// dispatch it lands in the hot body and interleaves with the code that
/// does run.
#[cold]
#[inline(never)]
pub(super) fn alpha_select_scalar(pal_u: &[u8; 8], samples: &[u8; 16]) -> ([u8; 16], i32) {
    let mut indices = [0u8; 16];
    let mut err = 0i32;
    for (i, &s) in samples.iter().enumerate() {
        let mut best = 0u8;
        let mut best_d = i32::MAX;
        for (j, &pu) in pal_u.iter().enumerate() {
            let d = (pu as i32 - s as i32).abs();
            if d < best_d {
                best_d = d;
                best = j as u8;
            }
        }
        indices[i] = best;
        let diff = pal_u[best as usize] as i32 - s as i32;
        err += diff * diff;
    }
    (indices, err)
}

pub(super) fn pack_alpha_indices_s(a0: i32, a1: i32, palette: &[i32; 8], samples: &[u8; 16]) -> [u8; 8] {
    let mut pal_u = [0u8; 8];
    for (i, &p) in palette.iter().enumerate() {
        pal_u[i] = snorm_i32_to_unorm_u8(p);
    }
    // 16 samples x 8 entries of |pu - s| with a running argmin — one vector
    // pass, same first-minimum tie-break. See `simd::alpha_select_avx2`.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    let indices = if simd::has_avx2() {
        simd::alpha_select_avx2(&pal_u, samples).0
    } else {
        alpha_select_scalar(&pal_u, samples).0
    };
    #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
    let indices = alpha_select_scalar(&pal_u, samples).0;
    let mut out = [0u8; 8];
    out[0] = a0 as i8 as u8;
    out[1] = a1 as i8 as u8;
    let bits = pack_index_bits(&indices);
    write_index_bits(&mut out, bits);
    out
}

pub(super) fn alpha_palette6_u(max_v: u8, min_v: u8) -> [u8; 8] {
    // Match bcdec_rs / DirectXTex: fixed-point weights with +32768 rounding.
    const W6: [i32; 6] = [9363, 18724, 28086, 37450, 46812, 56173];
    // The two weights of every entry sum to 65536 — W6[k] + W6[5-k] is exactly
    // 1.0 in this fixed point — so
    //
    //     W_hi*max + W_lo*min + 32768
    //   = 65536*min + W_hi*(max - min) + 32768
    //
    // and 65536*min is an exact multiple of the shift, so it separates out
    // cleanly: the entry is `min + ((W_hi*d + 32768) >> 16)`. One multiply per
    // entry instead of two, and identical bit for bit — including for a
    // negative `min` or a negative `d`, because an arithmetic shift floors and
    // the separated term contributes no fraction to floor over.
    let min = min_v as i32;
    let d = max_v as i32 - min;
    let lerp = |w: i32| (min + ((w * d + 32768) >> 16)) as u8;
    [
        max_v,
        min_v,
        lerp(W6[5]),
        lerp(W6[4]),
        lerp(W6[3]),
        lerp(W6[2]),
        lerp(W6[1]),
        lerp(W6[0]),
    ]
}

pub(super) fn alpha_palette4_u(max_v: u8, min_v: u8) -> [u8; 8] {
    const W4: [i32; 4] = [13107, 26215, 39321, 52429];
    // The two weights of every entry sum to 65536 — W4[k] + W4[3-k] is exactly
    // 1.0 in this fixed point — so
    //
    //     W_hi*max + W_lo*min + 32768
    //   = 65536*min + W_hi*(max - min) + 32768
    //
    // and 65536*min is an exact multiple of the shift, so it separates out
    // cleanly: the entry is `min + ((W_hi*d + 32768) >> 16)`. One multiply per
    // entry instead of two, and identical bit for bit — including for a
    // negative `min` or a negative `d`, because an arithmetic shift floors and
    // the separated term contributes no fraction to floor over.
    let min = min_v as i32;
    let d = max_v as i32 - min;
    let lerp = |w: i32| (min + ((w * d + 32768) >> 16)) as u8;
    [
        max_v,
        min_v,
        lerp(W4[3]),
        lerp(W4[2]),
        lerp(W4[1]),
        lerp(W4[0]),
        0,
        255,
    ]
}

pub(super) fn alpha_palette6_s(max_v: i32, min_v: i32) -> [i32; 8] {
    const W6: [i32; 6] = [9363, 18724, 28086, 37450, 46812, 56173];
    // The two weights of every entry sum to 65536 — W6[k] + W6[5-k] is exactly
    // 1.0 in this fixed point — so
    //
    //     W_hi*max + W_lo*min + 32768
    //   = 65536*min + W_hi*(max - min) + 32768
    //
    // and 65536*min is an exact multiple of the shift, so it separates out
    // cleanly: the entry is `min + ((W_hi*d + 32768) >> 16)`. One multiply per
    // entry instead of two, and identical bit for bit — including for a
    // negative `min` or a negative `d`, because an arithmetic shift floors and
    // the separated term contributes no fraction to floor over.
    let d = max_v - min_v;
    let lerp = |w: i32| min_v + ((w * d + 32768) >> 16);
    [
        max_v,
        min_v,
        lerp(W6[5]),
        lerp(W6[4]),
        lerp(W6[3]),
        lerp(W6[2]),
        lerp(W6[1]),
        lerp(W6[0]),
    ]
}

pub(super) fn alpha_palette4_s(max_v: i32, min_v: i32) -> [i32; 8] {
    const W4: [i32; 4] = [13107, 26215, 39321, 52429];
    // The two weights of every entry sum to 65536 — W4[k] + W4[3-k] is exactly
    // 1.0 in this fixed point — so
    //
    //     W_hi*max + W_lo*min + 32768
    //   = 65536*min + W_hi*(max - min) + 32768
    //
    // and 65536*min is an exact multiple of the shift, so it separates out
    // cleanly: the entry is `min + ((W_hi*d + 32768) >> 16)`. One multiply per
    // entry instead of two, and identical bit for bit — including for a
    // negative `min` or a negative `d`, because an arithmetic shift floors and
    // the separated term contributes no fraction to floor over.
    let d = max_v - min_v;
    let lerp = |w: i32| min_v + ((w * d + 32768) >> 16);
    [
        max_v,
        min_v,
        lerp(W4[3]),
        lerp(W4[2]),
        lerp(W4[1]),
        lerp(W4[0]),
        -127,
        127,
    ]
}
