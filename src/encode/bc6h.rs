//! BC6H_UF16 encoder, mode 11 only (single subset, 10-bit endpoints, 4-bit
//! indices) — the BC7-mode-6 analog for HDR.
//!
//! Everything runs in the HALF-BITS integer domain: for non-negative
//! half-floats the bit pattern is monotone in value, decode's output IS half
//! bits (`(interp*31)>>6`), and so palette entries and targets compare with
//! plain integer SSE. No float math inside the kernel.
//!
//! Decode math mirrored from `bcdec_rs` (the round-trip oracle):
//!   unquantize10(q) = 0 | 0xFFFF | ((q<<16)+0x8000)>>10   (0 / 1023 pinned)
//!   palette[w]      = (uq_a*(64-w) + uq_b*w + 32) >> 6,  w in W4
//!   half_bits       = (palette*31) >> 6

use crate::error::Error;

const W4: [i32; 16] = [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64];

/// f32 -> half bits, clamped for UF16: negatives/NaN -> 0, +inf/overflow ->
/// 65504 (max finite half).
pub(crate) fn f32_to_half_uf16(v: f32) -> u16 {
    if !(v > 0.0) {
        return 0; // negatives and NaN clamp to zero in an unsigned format
    }
    let v = v.min(65504.0);
    let bits = v.to_bits();
    let exp = ((bits >> 23) & 0xFF) as i32 - 127;
    if exp < -24 {
        return 0;
    }
    if exp < -14 {
        // subnormal half
        let mant = (bits & 0x7F_FFFF) | 0x80_0000;
        let shift = (-14 - exp) as u32;
        let half_mant = mant >> (13 + shift);
        let round = (mant >> (12 + shift)) & 1;
        return (half_mant + round) as u16;
    }
    let half = (((exp + 15) as u32) << 10) | ((bits >> 13) & 0x3FF);
    let round = (bits >> 12) & 1;
    (half as u16).saturating_add(round as u16).min(0x7BFF)
}

#[inline]
fn unquantize10(q: i32) -> i32 {
    if q == 0 {
        0
    } else if q == 1023 {
        0xFFFF
    } else {
        ((q << 16) + 0x8000) >> 10
    }
}

#[inline]
fn half_from_interp(v: i32) -> i32 {
    (v * 31) >> 6
}

/// Best 10-bit quantized endpoint whose DECODED half value approximates the
/// target half bits (searched ±2 around the analytic estimate).
fn quantize10_for_half(target_half: i32) -> i32 {
    // decode(q) at w=0: half = (unquantize10(q)*31)>>6 with unq ~= q*64,
    // so half ~= q*31 and the estimate is simply half/31.
    let est = target_half / 31;
    let mut best_q = 0;
    let mut best_e = i64::MAX;
    for q in (est - 2).max(0)..=(est + 2).min(1023) {
        let h = half_from_interp(unquantize10(q));
        let e = ((h - target_half) as i64).pow(2);
        if e < best_e {
            best_e = e;
            best_q = q;
        }
    }
    best_q
}

/// 16-entry joint RGB palette in half-bits for a quantized endpoint pair.
fn palette(qw: [i32; 3], qx: [i32; 3]) -> [[i32; 3]; 16] {
    let uw = [
        unquantize10(qw[0]),
        unquantize10(qw[1]),
        unquantize10(qw[2]),
    ];
    let ux = [
        unquantize10(qx[0]),
        unquantize10(qx[1]),
        unquantize10(qx[2]),
    ];
    let mut pal = [[0i32; 3]; 16];
    for (k, &w) in W4.iter().enumerate() {
        for c in 0..3 {
            pal[k][c] = half_from_interp((uw[c] * (64 - w) + ux[c] * w + 32) >> 6);
        }
    }
    pal
}

/// Fit indices for one palette; returns (indices, SSE in half-bits domain).
fn fit(halves: &[[i32; 3]; 16], pal: &[[i32; 3]; 16]) -> ([u8; 16], i64) {
    let mut idx = [0u8; 16];
    let mut err = 0i64;
    for (i, px) in halves.iter().enumerate() {
        let mut bi = 0u8;
        let mut be = i64::MAX;
        for (k, p) in pal.iter().enumerate() {
            let mut e = 0i64;
            for c in 0..3 {
                let d = (p[c] - px[c]) as i64;
                e += d * d;
            }
            if e < be {
                be = e;
                bi = k as u8;
            }
        }
        idx[i] = bi;
        err += be;
    }
    (idx, err)
}

#[allow(clippy::type_complexity)]
fn try_pair(
    halves: &[[i32; 3]; 16],
    e0: [i32; 3],
    e1: [i32; 3],
) -> ([i32; 3], [i32; 3], [u8; 16], i64) {
    let mut qw = [0i32; 3];
    let mut qx = [0i32; 3];
    for c in 0..3 {
        qw[c] = quantize10_for_half(e0[c]);
        qx[c] = quantize10_for_half(e1[c]);
    }
    let pal = palette(qw, qx);
    let (mut idx, err) = fit(halves, &pal);
    // Anchor: pixel 0's index has 3 bits (MSB implicitly 0). W4 symmetry:
    // W4[15-i] == 64 - W4[i], so swap + invert reconstructs identically.
    if idx[0] > 7 {
        std::mem::swap(&mut qw, &mut qx);
        for v in idx.iter_mut() {
            *v = 15 - *v;
        }
    }
    (qw, qx, idx, err)
}

/// Encode one 4x4 block of RGBA f32 (alpha ignored) to a mode-11 BC6H_UF16
/// block.
pub(crate) fn encode_block_uf16(pixels: &[[f32; 4]; 16], out: &mut [u8]) {
    let mut halves = [[0i32; 3]; 16];
    let mut lo = [i32::MAX; 3];
    let mut hi = [0i32; 3];
    for (i, p) in pixels.iter().enumerate() {
        for c in 0..3 {
            let h = f32_to_half_uf16(p[c]) as i32;
            halves[i][c] = h;
            lo[c] = lo[c].min(h);
            hi[c] = hi[c].max(h);
        }
    }

    // Seed: per-channel min/max, then one LS refit round in the half domain.
    let (mut qw, mut qx, mut idx, err) = try_pair(&halves, hi, lo);
    if err > 0 {
        if let Some((r0, r1)) = ls_endpoints(&halves, &idx) {
            let cand = try_pair(&halves, r0, r1);
            if cand.3 < err {
                (qw, qx, idx, _) = cand;
            }
        }
    }

    // Pack: 5 mode bits (0b00011 LSB-first), 6x10-bit endpoints, 63 index
    // bits (pixel 0 gets 3).
    let mut bits = Bits::default();
    bits.push(0b00011, 5);
    bits.push(qw[0] as u64, 10);
    bits.push(qw[1] as u64, 10);
    bits.push(qw[2] as u64, 10);
    bits.push(qx[0] as u64, 10);
    bits.push(qx[1] as u64, 10);
    bits.push(qx[2] as u64, 10);
    bits.push(idx[0] as u64, 3);
    for &v in &idx[1..] {
        bits.push(v as u64, 4);
    }
    out[..16].copy_from_slice(&bits.into_array());
}

/// LS endpoints from current indices (half-bits domain, W4 weights).
fn ls_endpoints(halves: &[[i32; 3]; 16], indices: &[u8; 16]) -> Option<([i32; 3], [i32; 3])> {
    let mut a00 = 0f64;
    let mut a01 = 0f64;
    let mut a11 = 0f64;
    let mut b0 = [0f64; 3];
    let mut b1 = [0f64; 3];
    for (i, px) in halves.iter().enumerate() {
        let w = W4[indices[i] as usize] as f64 / 64.0;
        let u = 1.0 - w;
        a00 += u * u;
        a01 += u * w;
        a11 += w * w;
        for c in 0..3 {
            let x = px[c] as f64;
            b0[c] += u * x;
            b1[c] += w * x;
        }
    }
    let det = a00 * a11 - a01 * a01;
    if det.abs() < 1e-6 {
        return None;
    }
    let mut e0 = [0i32; 3];
    let mut e1 = [0i32; 3];
    for c in 0..3 {
        e0[c] = ((a11 * b0[c] - a01 * b1[c]) / det).round().clamp(0.0, 65504.0) as i32;
        e1[c] = ((a00 * b1[c] - a01 * b0[c]) / det).round().clamp(0.0, 65504.0) as i32;
    }
    Some((e0, e1))
}

#[derive(Default)]
struct Bits {
    low: u64,
    high: u64,
    pos: u32,
}

impl Bits {
    fn push(&mut self, value: u64, n: u32) {
        let mask = if n == 64 { u64::MAX } else { (1u64 << n) - 1 };
        let v = value & mask;
        if self.pos < 64 {
            self.low |= v << self.pos;
            if self.pos + n > 64 {
                self.high |= v >> (64 - self.pos);
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

/// Encode a full RGBA f32 slice to BC6H_UF16 mode-11 blocks.
pub(crate) fn encode_slice_uf16(
    rgba: &[f32],
    width: u32,
    height: u32,
    out: &mut [u8],
) -> Result<(), Error> {
    if width == 0 || height == 0 {
        return Err(Error::InvalidField("zero image dimension".into()));
    }
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
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let mut px = [[0f32; 4]; 16];
            for row in 0..4 {
                for col in 0..4 {
                    let x = (bx * 4 + col).min(w - 1);
                    let y = (by * 4 + row).min(h - 1);
                    let i = (y * w + x) * 4;
                    px[row * 4 + col] = [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]];
                }
            }
            let oi = (by * blocks_x + bx) * 16;
            encode_block_uf16(&px, &mut out[oi..oi + 16]);
        }
    }
    Ok(())
}
