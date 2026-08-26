//! Compile-time selector tables shared across the SIMD kernels.
//!
//! `decode` and `encode` are independently optional features, and kernels on
//! BOTH sides read `BC1_SEL` (the decode surface kernels, and the encoder's
//! fixed-table BC1 SSE). A table homed under either feature would leave the
//! other side dangling when built alone — `encode,simd` without `decode` failed
//! to compile for exactly that reason — so the tables live here, gated only on
//! `simd` + x86_64.

/// `pshufb` selectors for four BC1 pixels, indexed by the byte holding their
/// four 2-bit indices.
///
/// A BC1 palette is four RGBA entries — exactly sixteen bytes, exactly one
/// register — so one `pshufb` produces four whole pixels. All that is needed is
/// the byte selector, and there are only 256 of them: `SEL[b][4k + c]` is
/// `4 * ((b >> 2k) & 3) + c`. 4 KiB, L1-resident, built at compile time.
const fn build_bc1_sel() -> [[u8; 16]; 256] {
    let mut t = [[0u8; 16]; 256];
    let mut b = 0usize;
    while b < 256 {
        let mut k = 0usize;
        while k < 4 {
            let e = ((b >> (2 * k)) & 3) as u8;
            let mut c = 0usize;
            while c < 4 {
                t[b][k * 4 + c] = e * 4 + c as u8;
                c += 1;
            }
            k += 1;
        }
        b += 1;
    }
    t
}

pub(crate) static BC1_SEL: [[u8; 16]; 256] = build_bc1_sel();
