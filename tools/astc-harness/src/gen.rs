//! Block sources: random, mutated corpus, file, and a legal-biased mix.

use rusty_dds::astc::internals as ai;
use rusty_dds::astc::AstcFootprint;

/// splitmix64.
pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    pub fn fill(&mut self, buf: &mut [u8]) {
        for c in buf.chunks_mut(8) {
            let v = self.next().to_le_bytes();
            c.copy_from_slice(&v[..c.len()]);
        }
    }
    pub fn block(&mut self) -> [u8; 16] {
        let mut b = [0u8; 16];
        self.fill(&mut b);
        b
    }
}

pub fn read_astc_blocks(path: &str) -> Vec<[u8; 16]> {
    let d = std::fs::read(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let body = if d.len() >= 16 && d[..4] == [0x13, 0xAB, 0xA1, 0x5C] { &d[16..] } else { &d[..] };
    body.chunks_exact(16)
        .map(|c| {
            let mut b = [0u8; 16];
            b.copy_from_slice(c);
            b
        })
        .collect()
}

fn set_bits(b: &mut u128, pos: u32, n: u32, v: u128) {
    let m = ((1u128 << n) - 1) << pos;
    *b = (*b & !m) | ((v << pos) & m);
}

/// A random block biased toward legal layouts: a legal block mode for `fp`,
/// a partition count the mode allows, and random remaining bits; 1 in 16 is a
/// void-extent block (LDR or HDR, sometimes with a valid extent).
pub fn mixed(rng: &mut Rng, fp: AstcFootprint, legal_modes: &[(u16, bool)]) -> [u8; 16] {
    let mut b = u128::from_le_bytes(rng.block());
    if rng.below(16) == 0 {
        let hdr = rng.below(2) as u128;
        set_bits(&mut b, 0, 9, 0x1FC);
        set_bits(&mut b, 9, 1, hdr);
        if !fp.is_3d() {
            set_bits(&mut b, 10, 2, if rng.below(8) == 0 { rng.below(4) as u128 } else { 3 });
            match rng.below(3) {
                0 => set_bits(&mut b, 12, 52, (1u128 << 52) - 1),
                1 => {
                    let (s0, t0) = (rng.below(4096) as u128, rng.below(4096) as u128);
                    set_bits(&mut b, 12, 13, s0);
                    set_bits(&mut b, 25, 13, s0 + 1 + rng.below(4000) as u128);
                    set_bits(&mut b, 38, 13, t0);
                    set_bits(&mut b, 51, 13, t0 + 1 + rng.below(4000) as u128);
                }
                _ => {}
            }
        } else if rng.below(2) == 0 {
            set_bits(&mut b, 10, 54, (1u128 << 54) - 1);
        }
        if hdr == 1 && rng.below(2) == 0 {
            // Mostly finite halves; Inf/NaN constants are left to random bits.
            for c in 0..4 {
                set_bits(&mut b, 64 + 16 * c, 16, (rng.below(0x7C00) | (rng.below(2) << 15)) as u128);
            }
        }
        return b.to_le_bytes();
    }
    let (mode, dual) = legal_modes[rng.below(legal_modes.len() as u64) as usize];
    set_bits(&mut b, 0, 11, mode as u128);
    let parts = if dual { rng.below(3) } else { rng.below(4) };
    set_bits(&mut b, 11, 2, parts as u128);
    b.to_le_bytes()
}

pub fn legal_modes(fp: AstcFootprint) -> Vec<(u16, bool)> {
    (0..2048u32)
        .filter_map(|m| match ai::classify(m, fp) {
            ai::ModeClass::Grid(g) if ai::check(&g, fp).is_ok() => Some((m as u16, g.dual_plane)),
            _ => None,
        })
        .collect()
}

pub fn source(spec: &str, fp: AstcFootprint) -> Vec<[u8; 16]> {
    let parts: Vec<&str> = spec.splitn(3, ':').collect();
    let seed = 0xA57C ^ ((fp.width() as u64) << 16 | (fp.height() as u64) << 8 | fp.depth() as u64);
    let mut rng = Rng::new(seed ^ spec.len() as u64);
    match parts[0] {
        "random" => {
            let n: usize = parts[1].parse().unwrap();
            (0..n).map(|_| rng.block()).collect()
        }
        "mix" => {
            let n: usize = parts[1].parse().unwrap();
            let lm = legal_modes(fp);
            (0..n).map(|_| mixed(&mut rng, fp, &lm)).collect()
        }
        "mutate" => {
            let n: usize = parts[1].parse().unwrap();
            let base = read_astc_blocks(parts[2]);
            (0..n)
                .map(|_| {
                    let mut b = base[rng.below(base.len() as u64) as usize];
                    let flips = 1 + rng.below(8);
                    for _ in 0..flips {
                        let bit = rng.below(128) as usize;
                        b[bit / 8] ^= 1 << (bit % 8);
                    }
                    b
                })
                .collect()
        }
        "file" => read_astc_blocks(parts[1]),
        _ => panic!("unknown source {spec}"),
    }
}
