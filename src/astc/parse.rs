//! Physical block (128 bits) to its parsed, unquantised symbolic form, with
//! the illegal-encoding reasons of the specification's "Illegal Encodings".

use super::block_mode::{self, GridIllegal, ModeClass};
use super::footprint::AstcFootprint;
use super::ise::{self, read_bits};
use super::quant::{self, COLOR_UNQUANT, WEIGHT_UNQUANT};

/// Why a block is illegal (decodes to the error colour).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum IllegalReason {
    /// A block mode listed as reserved.
    ReservedBlockMode,
    /// A 2D void-extent block whose reserved bits 10 and 11 are not both 1.
    VoidExtentReservedBits,
    /// A void-extent block with a low coordinate not below its high one
    /// (and not the all-ones "no extent" pattern).
    VoidExtentBadCoords,
    /// More than 64 weights (both planes counted).
    TooManyWeights,
    /// More than 96 bits of weight data.
    WeightBitsTooMany,
    /// Fewer than 24 bits of weight data.
    WeightBitsTooFew,
    /// The weight grid is larger than the footprint in some axis.
    GridLargerThanFootprint,
    /// More than 18 colour endpoint integers.
    TooManyColorIntegers,
    /// Too few colour bits for even the 6-level range.
    ColorBitsTooFew,
    /// Dual-plane weights with four partitions.
    DualPlaneFourPartitions,
}

/// The kind of a parsed block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Normal,
    /// LDR void-extent: UNORM16 colour.
    VoidLdr,
    /// HDR void-extent: FP16 colour bits.
    VoidHdr,
    Illegal(IllegalReason),
}

/// A parsed block: every field the decoder uses, plus the bit-budget fields so
/// tests can diff the layout and not only the values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolicBlock {
    pub kind: BlockKind,
    /// The raw 11-bit block mode.
    pub mode: u16,
    pub grid: [u8; 3],
    /// Weight range index 0..=11.
    pub weight_range: u8,
    pub dual_plane: bool,
    /// 0..=3; meaningful when `dual_plane`.
    pub plane2_component: u8,
    /// 1..=4.
    pub partitions: u8,
    /// 10-bit partition pattern index.
    pub partition_seed: u16,
    pub cem: [u8; 4],
    /// The multi-partition CEM selector was `00` (one CEM for all partitions).
    pub cem_matched: bool,
    /// Colour range index 4..=20.
    pub color_range: u8,
    /// Number of colour endpoint integers.
    pub color_count: u8,
    /// Unquantised colour values (0..=255), in stream order.
    pub color_values: [u8; 18],
    /// Unquantised weights (0..=64), in stream order (planes interleaved).
    pub weights: [u8; 64],
    /// Void-extent colour (UNORM16 or FP16 bits).
    pub void_color: [u16; 4],
    // Bit-budget fields.
    pub weight_bits: u8,
    pub color_bits: u8,
    pub color_start: u8,
    pub below_weights_pos: u8,
}

impl SymbolicBlock {
    pub(crate) const fn empty(kind: BlockKind, mode: u16) -> Self {
        Self {
            kind,
            mode,
            grid: [0; 3],
            weight_range: 0,
            dual_plane: false,
            plane2_component: 0,
            partitions: 0,
            partition_seed: 0,
            cem: [0; 4],
            cem_matched: false,
            color_range: 0,
            color_count: 0,
            color_values: [0; 18],
            weights: [0; 64],
            void_color: [0; 4],
            weight_bits: 0,
            color_bits: 0,
            color_start: 0,
            below_weights_pos: 0,
        }
    }

    fn illegal(mode: u16, r: IllegalReason) -> Self {
        Self::empty(BlockKind::Illegal(r), mode)
    }

    /// Stored weights per plane.
    pub fn plane_weight_count(&self) -> usize {
        self.grid[0] as usize * self.grid[1] as usize * self.grid[2] as usize
    }
}

/// Parse one block. Total: every 128-bit pattern yields a `SymbolicBlock`.
pub fn parse(block: &[u8; 16], fp: AstcFootprint) -> SymbolicBlock {
    let b = u128::from_le_bytes(*block);
    let mode = (b & 0x7FF) as u16;
    parse_with(b, mode, fp, mode_info(mode, fp))
}

/// A block mode classified for a footprint (footprint-dependent legality
/// included). The fast decoder tabulates this per footprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModeInfo {
    Void,
    Illegal(IllegalReason),
    Grid(block_mode::GridMode),
}

pub(crate) fn mode_info(mode: u16, fp: AstcFootprint) -> ModeInfo {
    match block_mode::classify(mode as u32, fp) {
        ModeClass::VoidExtent => ModeInfo::Void,
        ModeClass::Reserved => ModeInfo::Illegal(IllegalReason::ReservedBlockMode),
        ModeClass::Grid(g) => match block_mode::check(&g, fp) {
            Ok(()) => ModeInfo::Grid(g),
            Err(e) => ModeInfo::Illegal(match e {
                GridIllegal::TooManyWeights => IllegalReason::TooManyWeights,
                GridIllegal::WeightBitsTooMany => IllegalReason::WeightBitsTooMany,
                GridIllegal::WeightBitsTooFew => IllegalReason::WeightBitsTooFew,
                GridIllegal::GridLargerThanFootprint => IllegalReason::GridLargerThanFootprint,
            }),
        },
    }
}

/// [`parse`] with the block mode already classified (`info` must be
/// `mode_info(mode, fp)`).
pub(crate) fn parse_with(b: u128, mode: u16, fp: AstcFootprint, info: ModeInfo) -> SymbolicBlock {
    let g = match info {
        ModeInfo::Void => return parse_void(b, mode, fp),
        ModeInfo::Illegal(r) => return SymbolicBlock::illegal(mode, r),
        ModeInfo::Grid(g) => g,
    };
    let partitions = read_bits(b, 11, 2) + 1;
    if g.dual_plane && partitions == 4 {
        return SymbolicBlock::illegal(mode, IllegalReason::DualPlaneFourPartitions);
    }

    let weight_bits = g.weight_bits();
    let mut below = 128 - weight_bits;
    let mut cem = [0u8; 4];
    let mut cem_matched = false;
    let mut seed = 0u32;
    let mut config_bits = 17;
    if partitions == 1 {
        cem[0] = read_bits(b, 13, 4) as u8;
    } else {
        seed = (b >> 13) as u32 & 0x3FF;
        let sel = read_bits(b, 23, 2);
        if sel == 0 {
            let c = read_bits(b, 25, 4) as u8;
            cem = [c; 4];
            cem_matched = true;
            config_bits = 29;
        } else {
            let high = 3 * partitions - 4;
            below -= high;
            // CEM field: 6 bits at 23..=28, then `high` bits just below the weights.
            let field = read_bits(b, 23, 6) | (read_bits(b, below, high) << 6);
            let base = sel - 1;
            let p = partitions as usize;
            for (i, c) in cem.iter_mut().enumerate().take(p) {
                let class = base + ((field >> (2 + i)) & 1);
                let low = (field >> (2 + p + 2 * i)) & 3;
                *c = ((class << 2) | low) as u8;
            }
            config_bits = 25 + 3 * partitions;
        }
    }
    let color_count: u32 = cem
        .iter()
        .take(partitions as usize)
        .map(|&c| super::cem::value_count(c) as u32)
        .sum();
    if color_count > 18 {
        return SymbolicBlock::illegal(mode, IllegalReason::TooManyColorIntegers);
    }
    let dual_bits = if g.dual_plane { 2 } else { 0 };
    let color_bits = 128u32.saturating_sub(config_bits + weight_bits + dual_bits);
    let color_range = match quant::color_range_lut(color_count, color_bits) {
        Some(r) => r,
        None => return SymbolicBlock::illegal(mode, IllegalReason::ColorBitsTooFew),
    };

    let mut s = SymbolicBlock::empty(BlockKind::Normal, mode);
    s.grid = [g.w, g.h, g.d];
    s.weight_range = g.range;
    s.dual_plane = g.dual_plane;
    s.partitions = partitions as u8;
    s.partition_seed = seed as u16;
    s.cem = cem;
    s.cem_matched = cem_matched;
    s.color_range = color_range as u8;
    s.color_count = color_count as u8;
    s.weight_bits = weight_bits as u8;
    s.color_bits = color_bits as u8;
    s.color_start = if partitions == 1 { 17 } else { 29 };
    s.below_weights_pos = below as u8;
    if g.dual_plane {
        s.plane2_component = read_bits(b, below - 2, 2) as u8;
    }

    // Colour values, LSB first from bit 17 or 29, unquantised as they are
    // decoded (every value is below the range's level count, at most 256).
    let n = color_count as usize;
    let cu = &COLOR_UNQUANT[color_range];
    ise::decode_map(
        b,
        s.color_start as u32,
        color_range,
        &mut s.color_values[..n],
        |v| cu[v as u8 as usize],
    );

    // Weights: the 128 bits reversed, read LSB first, unquantised as they are
    // decoded (every value is below the range's level count, at most 32, so
    // the mask never changes it).
    let rev = b.reverse_bits();
    let wn = g.weight_count() as usize;
    let wu = &WEIGHT_UNQUANT[g.range as usize];
    ise::decode_map(rev, 0, g.range as usize, &mut s.weights[..wn], |v| {
        wu[(v & 31) as usize]
    });
    s
}

fn parse_void(b: u128, mode: u16, fp: AstcFootprint) -> SymbolicBlock {
    let hdr = mode & 0x200 != 0;
    let field = |lo: u32, n: u32| ((b >> lo) as u32) & ((1u32 << n) - 1);
    if !fp.is_3d() {
        if field(10, 2) != 3 {
            return SymbolicBlock::illegal(mode, IllegalReason::VoidExtentReservedBits);
        }
        let (s0, s1, t0, t1) = (field(12, 13), field(25, 13), field(38, 13), field(51, 13));
        let all_ones = s0 == 0x1FFF && s1 == 0x1FFF && t0 == 0x1FFF && t1 == 0x1FFF;
        if (s0 >= s1 || t0 >= t1) && !all_ones {
            return SymbolicBlock::illegal(mode, IllegalReason::VoidExtentBadCoords);
        }
    } else {
        let (s0, s1) = (field(10, 9), field(19, 9));
        let (t0, t1) = (field(28, 9), field(37, 9));
        let (r0, r1) = (field(46, 9), field(55, 9));
        let all_ones = [s0, s1, t0, t1, r0, r1].iter().all(|&v| v == 0x1FF);
        if (s0 >= s1 || t0 >= t1 || r0 >= r1) && !all_ones {
            return SymbolicBlock::illegal(mode, IllegalReason::VoidExtentBadCoords);
        }
    }
    let kind = if hdr {
        BlockKind::VoidHdr
    } else {
        BlockKind::VoidLdr
    };
    let mut s = SymbolicBlock::empty(kind, mode);
    for (i, c) in s.void_color.iter_mut().enumerate() {
        *c = field(64 + 16 * i as u32, 16) as u16;
    }
    s
}
