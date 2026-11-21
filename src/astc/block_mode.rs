//! The 11-bit block mode field ("Block Mode"): weight grid size, weight range
//! and dual-plane flag, for the 2D and the 3D layouts, plus the
//! footprint-dependent legality rules.

use super::footprint::AstcFootprint;
use super::ise;

/// What an 11-bit block mode means, before footprint-dependent legality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeClass {
    /// `(mode & 0x1FF) == 0x1FC`.
    VoidExtent,
    /// A reserved encoding (always an illegal block).
    Reserved,
    /// A weight grid layout.
    Grid(GridMode),
}

/// A decoded (non-void, non-reserved) block mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridMode {
    pub w: u8,
    pub h: u8,
    pub d: u8,
    /// Weight range index `0..=11` (2 to 32 levels).
    pub range: u8,
    pub dual_plane: bool,
}

impl GridMode {
    /// Stored weights, both planes.
    pub const fn weight_count(&self) -> u32 {
        let n = self.w as u32 * self.h as u32 * self.d as u32;
        if self.dual_plane {
            2 * n
        } else {
            n
        }
    }
    /// Bits of the weight ISE sequence.
    pub const fn weight_bits(&self) -> u32 {
        ise::sequence_bits(self.weight_count(), self.range as usize)
    }
}

const fn bits(m: u32, lo: u32, n: u32) -> u32 {
    (m >> lo) & ((1 << n) - 1)
}

const fn grid(w: u32, h: u32, d: u32, rho: u32, p: u32, dp: u32) -> ModeClass {
    // rho is 2..=7 by construction of every caller.
    ModeClass::Grid(GridMode {
        w: w as u8,
        h: h as u8,
        d: d as u8,
        range: ((rho - 2) + 6 * p) as u8,
        dual_plane: dp != 0,
    })
}

/// Classify a 2D block mode.
pub(crate) const fn classify_2d(m: u32) -> ModeClass {
    let m = m & 0x7FF;
    if m & 0x1FF == 0x1FC {
        return ModeClass::VoidExtent;
    }
    let dp = bits(m, 10, 1);
    let p = bits(m, 9, 1);
    if bits(m, 0, 2) != 0 {
        let rho = bits(m, 4, 1) | (bits(m, 0, 1) << 1) | (bits(m, 1, 1) << 2);
        let a = bits(m, 7, 2);
        let b = bits(m, 5, 2);
        match bits(m, 2, 2) {
            0 => grid(a + 4, b + 2, 1, rho, p, dp),
            1 => grid(a + 8, b + 2, 1, rho, p, dp),
            2 => grid(b + 2, a + 8, 1, rho, p, dp),
            _ => {
                if bits(m, 8, 1) == 0 {
                    grid(b + 2, bits(m, 7, 1) + 6, 1, rho, p, dp)
                } else {
                    grid(bits(m, 7, 1) + 2, b + 2, 1, rho, p, dp)
                }
            }
        }
    } else {
        if bits(m, 2, 2) == 0 {
            return ModeClass::Reserved;
        }
        let rho = bits(m, 4, 1) | (bits(m, 2, 1) << 1) | (bits(m, 3, 1) << 2);
        let b = bits(m, 5, 2);
        match bits(m, 7, 2) {
            0 => grid(12, b + 2, 1, rho, p, dp),
            1 => grid(b + 2, 12, 1, rho, p, dp),
            2 => grid(b + 6, bits(m, 9, 2) + 6, 1, rho, 0, 0),
            _ => match b {
                0 => grid(6, 10, 1, rho, p, dp),
                1 => grid(10, 6, 1, rho, p, dp),
                _ => ModeClass::Reserved,
            },
        }
    }
}

/// Classify a 3D block mode.
pub(crate) const fn classify_3d(m: u32) -> ModeClass {
    let m = m & 0x7FF;
    if m & 0x1FF == 0x1FC {
        return ModeClass::VoidExtent;
    }
    let dp = bits(m, 10, 1);
    let p = bits(m, 9, 1);
    if bits(m, 0, 2) != 0 {
        let rho = bits(m, 4, 1) | (bits(m, 0, 1) << 1) | (bits(m, 1, 1) << 2);
        grid(
            bits(m, 5, 2) + 2,
            bits(m, 7, 2) + 2,
            bits(m, 2, 2) + 2,
            rho,
            p,
            dp,
        )
    } else {
        if bits(m, 2, 2) == 0 {
            return ModeClass::Reserved;
        }
        let rho = bits(m, 4, 1) | (bits(m, 2, 1) << 1) | (bits(m, 3, 1) << 2);
        let b = bits(m, 5, 2);
        let hi = bits(m, 9, 2);
        match bits(m, 7, 2) {
            0 => grid(6, hi + 2, b + 2, rho, 0, 0),
            1 => grid(b + 2, 6, hi + 2, rho, 0, 0),
            2 => grid(b + 2, hi + 2, 6, rho, 0, 0),
            _ => match b {
                0 => grid(6, 2, 2, rho, p, dp),
                1 => grid(2, 6, 2, rho, p, dp),
                2 => grid(2, 2, 6, rho, p, dp),
                _ => ModeClass::Reserved,
            },
        }
    }
}

/// Classify a block mode for a footprint (2D or 3D layout).
pub const fn classify(m: u32, fp: AstcFootprint) -> ModeClass {
    if fp.is_3d() {
        classify_3d(m)
    } else {
        classify_2d(m)
    }
}

/// Why a grid mode is illegal for a footprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridIllegal {
    TooManyWeights,
    WeightBitsTooMany,
    WeightBitsTooFew,
    GridLargerThanFootprint,
}

/// Footprint-dependent legality of a grid mode (spec "Illegal Encodings").
pub const fn check(g: &GridMode, fp: AstcFootprint) -> Result<(), GridIllegal> {
    if g.weight_count() > 64 {
        return Err(GridIllegal::TooManyWeights);
    }
    let wb = g.weight_bits();
    if wb > 96 {
        return Err(GridIllegal::WeightBitsTooMany);
    }
    if wb < 24 {
        return Err(GridIllegal::WeightBitsTooFew);
    }
    if g.w as u32 > fp.width() || g.h as u32 > fp.height() || g.d as u32 > fp.depth() {
        return Err(GridIllegal::GridLargerThanFootprint);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn void_extent_and_reserved() {
        assert_eq!(classify_2d(0x1FC), ModeClass::VoidExtent);
        assert_eq!(classify_2d(0x3FC), ModeClass::VoidExtent);
        assert_eq!(classify_2d(0), ModeClass::Reserved);
        // bits[8:6] = 111, bits[1:0] = 00, bits[5:2] not all ones.
        assert_eq!(classify_2d(0b001_1100_0100), ModeClass::Reserved);
        assert_eq!(classify_3d(0x1FC), ModeClass::VoidExtent);
    }

    #[test]
    fn every_2d_grid_is_reachable() {
        // Every (w, h) in 2..=12 x 2..=12 the layout can express appears.
        let mut seen = std::collections::BTreeSet::new();
        for m in 0..2048 {
            if let ModeClass::Grid(g) = classify_2d(m) {
                assert_eq!(g.d, 1);
                assert!(g.range < 12);
                seen.insert((g.w, g.h));
            }
        }
        assert!(seen.contains(&(12, 12)) == false);
        assert!(seen.contains(&(6, 10)) && seen.contains(&(10, 6)));
        assert!(seen.contains(&(12, 5)) && seen.contains(&(5, 12)));
    }
}
