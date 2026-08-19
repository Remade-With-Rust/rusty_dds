//! Deterministic hashing and seeded randomness.
//!
//! Every number the harness uses to decide *what work to do* comes from here,
//! never from the clock. FNV-1a 64 matches the payload-hash convention the
//! encoder campaign harnesses already use.

pub const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a over `bytes`, continuing from `seed`.
pub fn fnv1a_seed(seed: u64, bytes: &[u8]) -> u64 {
    let mut h = seed;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// FNV-1a over `bytes`.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    fnv1a_seed(FNV_OFFSET, bytes)
}

/// Fold a `u64` into a running hash. Order-dependent by construction — callers
/// that combine per-item hashes across threads must sort first (see
/// [`combine_sorted`]).
pub fn mix(h: u64, v: u64) -> u64 {
    fnv1a_seed(h, &v.to_le_bytes())
}

/// Order-independent combination: sort, then fold.
///
/// This is what makes a multi-threaded frame's `upload_hash` deterministic —
/// worker completion order varies run to run, the hash must not.
pub fn combine_sorted(values: &mut [u64]) -> u64 {
    values.sort_unstable();
    let mut h = FNV_OFFSET;
    for &v in values.iter() {
        h = mix(h, v);
    }
    h
}

/// Bulk content hash for payload bytes — the work-count parity gate.
///
/// **Not FNV.** `fnv1a_seed` is byte-at-a-time and each multiply depends on the
/// previous one, so it runs at ~1.1 GB/s. A `traverse`/high run hashes 822 MiB,
/// which measured at **758 ms — 97% of the harness's own "staging copy" row and
/// ~60% of its streaming total.** The instrument was most of what it measured:
/// both arms paid it equally so comparisons stayed fair, but it diluted every
/// real difference by a factor of three.
///
/// This keeps FNV-1a's mixing but runs **four independent lanes over 8-byte
/// words**, so the CPU can pipeline instead of stalling on one dependency chain.
/// It is a divergence detector, not a cryptographic digest: it must catch two
/// stacks handing the GPU different bytes, and nothing here is adversarial.
///
/// Changing it changes every `trace_hash`, so boards recorded with the old hash
/// cannot be compared against boards recorded with this one.
pub fn bulk_hash(seed: u64, bytes: &[u8]) -> u64 {
    let mut lanes = [
        seed ^ FNV_OFFSET,
        seed.rotate_left(16) ^ 0x9e37_79b9_7f4a_7c15,
        seed.rotate_left(32) ^ 0xbf58_476d_1ce4_e5b9,
        seed.rotate_left(48) ^ 0x94d0_49bb_1331_11eb,
    ];
    let mut chunks = bytes.chunks_exact(32);
    for c in &mut chunks {
        for (i, lane) in lanes.iter_mut().enumerate() {
            let w = u64::from_le_bytes(c[i * 8..i * 8 + 8].try_into().expect("8 bytes"));
            *lane = (*lane ^ w).wrapping_mul(FNV_PRIME);
        }
    }
    // Fold the lanes, then absorb the tail one byte at a time. The length goes
    // in too, so a payload that is a prefix of another cannot collide with it.
    let mut h = lanes[0]
        ^ lanes[1].rotate_left(17)
        ^ lanes[2].rotate_left(34)
        ^ lanes[3].rotate_left(51);
    h = h.wrapping_mul(FNV_PRIME) ^ bytes.len() as u64;
    fnv1a_seed(h, chunks.remainder())
}

/// SplitMix64 — deterministic, seedable, no state shared with the OS.
#[derive(Clone, Copy)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_add(0x9e37_79b9_7f4a_7c15))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    pub fn next_f32(&mut self) -> f32 {
        ((self.next_u64() >> 40) as f32) / (1u32 << 24) as f32
    }

    /// Uniform in `[lo, hi)`.
    pub fn range_f32(&mut self, lo: f32, hi: f32) -> f32 {
        lo + self.next_f32() * (hi - lo)
    }
}

/// Stable scalar hash of a lattice cell — the basis of the procedural sources.
pub fn hash_2d(seed: u64, x: i32, y: i32) -> f32 {
    let mut h = fnv1a_seed(seed, &x.to_le_bytes());
    h = fnv1a_seed(h, &y.to_le_bytes());
    ((h >> 40) as f32) / (1u32 << 24) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combine_is_order_independent() {
        let mut a = vec![7u64, 3, 99, 1];
        let mut b = vec![1u64, 99, 7, 3];
        assert_eq!(combine_sorted(&mut a), combine_sorted(&mut b));
    }

    #[test]
    fn bulk_hash_detects_every_single_bit_flip() {
        // The gate exists to catch two stacks emitting different bytes, so a
        // one-bit difference anywhere — including in the unaligned tail — must
        // change the hash.
        for len in [0usize, 1, 7, 8, 31, 32, 33, 1000, 4096, 4097] {
            let base = vec![0xA5u8; len];
            let h = bulk_hash(1, &base);
            for bit in 0..(len * 8).min(512) {
                let mut m = base.clone();
                m[bit / 8] ^= 1 << (bit % 8);
                assert_ne!(bulk_hash(1, &m), h, "len {len} bit {bit} not detected");
            }
            // Truncation must not collide with the full payload either.
            if len > 0 {
                assert_ne!(bulk_hash(1, &base[..len - 1]), h, "len {len} truncation");
            }
        }
    }

    #[test]
    fn bulk_hash_is_stable_and_seed_sensitive() {
        let data = vec![7u8; 1234];
        assert_eq!(bulk_hash(3, &data), bulk_hash(3, &data));
        assert_ne!(bulk_hash(3, &data), bulk_hash(4, &data));
    }

    #[test]
    fn rng_is_reproducible() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..64 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }
}
