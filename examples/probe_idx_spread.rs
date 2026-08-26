//! `pdep` vs `pshufb` for the BC4/BC5 3-bit index unpack (inline-exe §5.2k.2).
//!
//! The k.2 brick changes exactly one instruction sequence: sixteen 3-bit
//! indices to sixteen bytes. Everything else on that path is byte-for-byte
//! the same code, so the neutrality question reduces to "is the portable
//! sequence slower than `pdep` on hardware where `pdep` is fast?".
//!
//! Both arms here are REPLICAS of the shipping code (an example cannot reach
//! a private fn), compiled identically and differing only in the instructions
//! under test. Correctness of the shipping copy is proved separately, by
//! `idx_spread_matches_scalar`.
//!
//! # THIS INSTRUMENT GOT THE SIGN WRONG — read before trusting it
//!
//! It reports `pshufb` at **2.19x slower than `pdep`**, which predicts a
//! clear end-to-end regression. In the real kernel the opposite happened:
//! BC4 decode got **~1.17x FASTER** (0.369 -> 0.316 ns/px, no overlap across
//! six interleaved rounds) and BC5 came out neutral.
//!
//! The reason is what an isolated loop cannot see. Here the sequence is the
//! only work in flight, so this measures its own throughput on a dependent
//! chain — and `pdep` is a 3-cycle, one-per-cycle instruction that wins that
//! contest easily. In `bc5_gather_ssse3` the sequence competes for ISSUE
//! PORTS against a palette build, two shuffles and four stores, and there
//! `pdep` is single-ported while `pshufb`/`pmullw`/`psrlw` spread across
//! ports that had slack. Marginal cost in a busy kernel is not proportional
//! to standalone cost, and can invert.
//!
//! Keep this file as the record of that inversion, not as a gate. The
//! admissible number for a kernel change is measured IN the kernel — see the
//! in-situ ABBA described at §5.2k.2.
#![allow(unsafe_op_in_unsafe_fn)]
use std::arch::x86_64::*;
use std::time::Instant;

/// The retired form: two `pdep`, mask `0x0707..07`.
#[target_feature(enable = "bmi2,sse2")]
unsafe fn spread_pdep(w: u64) -> __m128i {
    const SPREAD: u64 = 0x0707_0707_0707_0707;
    _mm_set_epi64x(_pdep_u64(w >> 24, SPREAD) as i64, _pdep_u64(w, SPREAD) as i64)
}

/// The shipping form: `pshufb` window gather + multiply-shift extract.
#[target_feature(enable = "ssse3")]
unsafe fn spread_pshufb(w: u64) -> __m128i {
    let sel_lo = _mm_setr_epi8(0, -1, 0, -1, 0, 1, 1, -1, 1, -1, 1, 2, 2, -1, 2, -1);
    let sel_hi = _mm_setr_epi8(3, -1, 3, -1, 3, 4, 4, -1, 4, -1, 4, 5, 5, -1, 5, -1);
    let mult = _mm_setr_epi16(8192, 1024, 128, 4096, 512, 64, 2048, 256);
    let src = _mm_cvtsi64_si128(w as i64);
    let lo = _mm_srli_epi16(_mm_mullo_epi16(_mm_shuffle_epi8(src, sel_lo), mult), 13);
    let hi = _mm_srli_epi16(_mm_mullo_epi16(_mm_shuffle_epi8(src, sel_hi), mult), 13);
    _mm_packus_epi16(lo, hi)
}

fn main() {
    if !is_x86_feature_detected!("bmi2") || !is_x86_feature_detected!("ssse3") {
        println!("need bmi2+ssse3 to compare both arms; skipping");
        return;
    }
    // A fixed word stream, so both arms consume identical work.
    let n = 1 << 22;
    let mut words = Vec::with_capacity(n);
    let mut s = 0x9e37_79b9_7f4a_7c15u64;
    for _ in 0..n {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        words.push(s & 0x0000_FFFF_FFFF_FFFF);
    }

    // Correctness of the two replicas against each other, before timing them.
    for &w in words.iter().take(10_000) {
        let (a, b) = unsafe {
            let (mut x, mut y) = ([0u8; 16], [0u8; 16]);
            _mm_storeu_si128(x.as_mut_ptr() as *mut __m128i, spread_pdep(w));
            _mm_storeu_si128(y.as_mut_ptr() as *mut __m128i, spread_pshufb(w));
            (x, y)
        };
        assert_eq!(a, b, "replicas disagree on {w:#014x}");
    }

    // Accumulate into a checksum so neither arm can be eliminated as dead.
    let run = |f: unsafe fn(u64) -> __m128i| -> (u128, u64) {
        let mut best = u128::MAX;
        let mut sink = 0u64;
        for _ in 0..15 {
            let t = Instant::now();
            let mut acc = unsafe { _mm_setzero_si128() };
            for &w in &words {
                acc = unsafe { _mm_add_epi8(acc, f(w)) };
            }
            best = best.min(t.elapsed().as_nanos());
            let mut o = [0u8; 16];
            unsafe { _mm_storeu_si128(o.as_mut_ptr() as *mut __m128i, acc) };
            sink = sink.wrapping_add(u64::from_le_bytes(o[..8].try_into().unwrap()));
        }
        (best, sink)
    };

    // ABBA, so drift cannot favour whichever ran first.
    let (a1, s1) = run(spread_pdep);
    let (b1, s2) = run(spread_pshufb);
    let (b2, s3) = run(spread_pshufb);
    let (a2, s4) = run(spread_pdep);
    let pdep = a1.min(a2) as f64 / n as f64;
    let shuf = b1.min(b2) as f64 / n as f64;
    println!("pdep   {pdep:.4} ns/word  (best {} / {})", a1, a2);
    println!("pshufb {shuf:.4} ns/word  (best {} / {})", b1, b2);
    println!("ratio  {:.3}x  (>1 means pshufb slower)", shuf / pdep);
    println!("sink {}", s1 ^ s2 ^ s3 ^ s4);
}
