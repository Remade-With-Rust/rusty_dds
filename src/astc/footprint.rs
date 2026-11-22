//! Block footprints: the 14 legal 2D sizes and the 10 legal 3D sizes, and the
//! block-grid arithmetic every surface driver and container shares.

/// A legal ASTC block footprint. Only the 14 2D and 10 3D sizes the
/// specification defines can be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AstcFootprint {
    w: u8,
    h: u8,
    d: u8,
}

const LEGAL_2D: [(u8, u8); 14] = [
    (4, 4),
    (5, 4),
    (5, 5),
    (6, 5),
    (6, 6),
    (8, 5),
    (8, 6),
    (8, 8),
    (10, 5),
    (10, 6),
    (10, 8),
    (10, 10),
    (12, 10),
    (12, 12),
];

const LEGAL_3D: [(u8, u8, u8); 10] = [
    (3, 3, 3),
    (4, 3, 3),
    (4, 4, 3),
    (4, 4, 4),
    (5, 4, 4),
    (5, 5, 4),
    (5, 5, 5),
    (6, 5, 5),
    (6, 6, 5),
    (6, 6, 6),
];

impl AstcFootprint {
    pub const B4X4: Self = Self::raw(4, 4, 1);
    pub const B5X4: Self = Self::raw(5, 4, 1);
    pub const B5X5: Self = Self::raw(5, 5, 1);
    pub const B6X5: Self = Self::raw(6, 5, 1);
    pub const B6X6: Self = Self::raw(6, 6, 1);
    pub const B8X5: Self = Self::raw(8, 5, 1);
    pub const B8X6: Self = Self::raw(8, 6, 1);
    pub const B8X8: Self = Self::raw(8, 8, 1);
    pub const B10X5: Self = Self::raw(10, 5, 1);
    pub const B10X6: Self = Self::raw(10, 6, 1);
    pub const B10X8: Self = Self::raw(10, 8, 1);
    pub const B10X10: Self = Self::raw(10, 10, 1);
    pub const B12X10: Self = Self::raw(12, 10, 1);
    pub const B12X12: Self = Self::raw(12, 12, 1);
    pub const B3X3X3: Self = Self::raw(3, 3, 3);
    pub const B4X3X3: Self = Self::raw(4, 3, 3);
    pub const B4X4X3: Self = Self::raw(4, 4, 3);
    pub const B4X4X4: Self = Self::raw(4, 4, 4);
    pub const B5X4X4: Self = Self::raw(5, 4, 4);
    pub const B5X5X4: Self = Self::raw(5, 5, 4);
    pub const B5X5X5: Self = Self::raw(5, 5, 5);
    pub const B6X5X5: Self = Self::raw(6, 5, 5);
    pub const B6X6X5: Self = Self::raw(6, 6, 5);
    pub const B6X6X6: Self = Self::raw(6, 6, 6);

    /// The 14 2D footprints, smallest block (highest bit rate) first.
    pub const ALL_2D: &'static [AstcFootprint] = &[
        Self::B4X4,
        Self::B5X4,
        Self::B5X5,
        Self::B6X5,
        Self::B6X6,
        Self::B8X5,
        Self::B8X6,
        Self::B8X8,
        Self::B10X5,
        Self::B10X6,
        Self::B10X8,
        Self::B10X10,
        Self::B12X10,
        Self::B12X12,
    ];

    /// The 10 3D footprints, smallest block first.
    pub const ALL_3D: &'static [AstcFootprint] = &[
        Self::B3X3X3,
        Self::B4X3X3,
        Self::B4X4X3,
        Self::B4X4X4,
        Self::B5X4X4,
        Self::B5X5X4,
        Self::B5X5X5,
        Self::B6X5X5,
        Self::B6X6X5,
        Self::B6X6X6,
    ];

    const fn raw(w: u8, h: u8, d: u8) -> Self {
        Self { w, h, d }
    }

    /// A 2D footprint, or `None` if `w x h` is not one of the 14 legal sizes.
    pub const fn new_2d(w: u32, h: u32) -> Option<Self> {
        let mut i = 0;
        while i < LEGAL_2D.len() {
            if LEGAL_2D[i].0 as u32 == w && LEGAL_2D[i].1 as u32 == h {
                return Some(Self::raw(w as u8, h as u8, 1));
            }
            i += 1;
        }
        None
    }

    /// A 3D footprint, or `None` if `w x h x d` is not one of the 10 legal sizes.
    /// `d == 1` is accepted and means the 2D footprint.
    pub const fn new_3d(w: u32, h: u32, d: u32) -> Option<Self> {
        if d == 1 {
            return Self::new_2d(w, h);
        }
        let mut i = 0;
        while i < LEGAL_3D.len() {
            let (a, b, c) = LEGAL_3D[i];
            if a as u32 == w && b as u32 == h && c as u32 == d {
                return Some(Self::raw(w as u8, h as u8, d as u8));
            }
            i += 1;
        }
        None
    }

    /// Block width in texels.
    pub const fn width(self) -> u32 {
        self.w as u32
    }
    /// Block height in texels.
    pub const fn height(self) -> u32 {
        self.h as u32
    }
    /// Block depth in texels; 1 for a 2D footprint.
    pub const fn depth(self) -> u32 {
        self.d as u32
    }
    /// Texels per block.
    pub const fn texels(self) -> u32 {
        self.w as u32 * self.h as u32 * self.d as u32
    }
    /// `true` for the 3D footprints.
    pub const fn is_3d(self) -> bool {
        self.d > 1
    }

    /// Blocks needed along x, y and z to cover a `width x height x depth`
    /// surface (edge blocks are cropped on decode).
    pub const fn block_counts(self, width: u32, height: u32, depth: u32) -> (u32, u32, u32) {
        (
            ceil_div(width, self.w as u32),
            ceil_div(height, self.h as u32),
            ceil_div(depth, self.d as u32),
        )
    }
}

impl core::fmt::Display for AstcFootprint {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.d > 1 {
            write!(f, "{}x{}x{}", self.w, self.h, self.d)
        } else {
            write!(f, "{}x{}", self.w, self.h)
        }
    }
}

const fn ceil_div(a: u32, b: u32) -> u32 {
    // `b` is a footprint dimension (3..=12), never zero.
    a / b + (a % b != 0) as u32
}

/// Bytes needed for a `width x height x depth` surface at footprint `fp`
/// (16 bytes per block). `None` on overflow.
pub fn payload_len(width: u32, height: u32, depth: u32, fp: AstcFootprint) -> Option<usize> {
    let (bx, by, bz) = fp.block_counts(width, height, depth);
    (bx as usize)
        .checked_mul(by as usize)?
        .checked_mul(bz as usize)?
        .checked_mul(16)
}

/// Block rows covering `height` texel rows (one 2D slice).
pub fn block_rows(height: u32, fp: AstcFootprint) -> u32 {
    ceil_div(height, fp.height())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_legal_sizes_construct() {
        let mut n2 = 0;
        let mut n3 = 0;
        for w in 0..16 {
            for h in 0..16 {
                if AstcFootprint::new_2d(w, h).is_some() {
                    n2 += 1;
                }
                for d in 2..8 {
                    if AstcFootprint::new_3d(w, h, d).is_some() {
                        n3 += 1;
                    }
                }
            }
        }
        assert_eq!(n2, 14);
        assert_eq!(n3, 10);
        assert_eq!(AstcFootprint::ALL_2D.len(), 14);
        assert_eq!(AstcFootprint::ALL_3D.len(), 10);
        assert_eq!(AstcFootprint::new_3d(6, 6, 1), Some(AstcFootprint::B6X6));
    }

    #[test]
    fn payload_arithmetic() {
        assert_eq!(payload_len(127, 128, 1, AstcFootprint::B6X5), Some(22 * 26 * 16));
        assert_eq!(payload_len(1, 1, 1, AstcFootprint::B12X12), Some(16));
        assert_eq!(payload_len(0, 1, 1, AstcFootprint::B4X4), Some(0));
        assert_eq!(block_rows(10, AstcFootprint::B12X10), 1);
        assert_eq!(block_rows(11, AstcFootprint::B12X10), 2);
    }
}
