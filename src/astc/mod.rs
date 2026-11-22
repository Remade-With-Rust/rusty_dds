//! ASTC (Adaptive Scalable Texture Compression): decode, in pure Rust.
//!
//! Every block of the Khronos specification decodes: LDR and HDR endpoint
//! modes, all 14 2D and 10 3D footprints, sRGB and linear LDR operation,
//! 8-bit and half-float output, void-extent blocks, and every illegal encoding
//! mapped to the reference error colour. The output is bit-exact with ARM
//! astcenc 5.7.1 in each of the six (profile x output type) cells; see
//! [`reference`] for the rules.
//!
//! The output type selects the decode mode, as in astcenc: 8-bit output is
//! `decode_unorm8` (top 8 bits of the interpolated value), half-float output
//! is `decode_float16`.
//!
//! This module depends on the rest of the crate only through [`crate::Error`]
//! and the plain image structs, so it can move into its own crate later
//! without a rewrite.
//!
//! ```
//! use rusty_dds::astc::{self, AstcFootprint, AstcProfile};
//!
//! // One 6x6 void-extent block: constant opaque mid-grey.
//! let mut block = [0u8; 16];
//! block[0] = 0xFC;
//! block[1] = 0xFD; // void-extent, LDR, reserved bits set
//! for b in &mut block[2..8] { *b = 0xFF; } // no extent
//! for c in 0..4 { block[8 + 2 * c] = 0x80; block[9 + 2 * c] = 0x80; }
//! block[14] = 0xFF; block[15] = 0xFF; // alpha = 1.0
//! let img = astc::decode_rgba8(&block, 6, 6, AstcFootprint::B6X6, AstcProfile::Ldr)?;
//! assert_eq!(&img.pixels[..4], &[0x80, 0x80, 0x80, 0xFF]);
//! # Ok::<(), rusty_dds::Error>(())
//! ```

mod block_mode;
mod cem;
pub mod container;
pub(crate) mod decode;
mod fast;
#[cfg(feature = "encode")]
#[cfg_attr(docsrs, doc(cfg(feature = "encode")))]
pub mod encode;
mod footprint;
mod infill;
mod ise;
#[cfg(feature = "encode")]
pub(crate) mod pack;
mod parse;
mod partition;
mod quant;
pub mod reference;
#[cfg(feature = "simd")]
mod simd;
mod tables;
mod texel;

#[cfg(any(test, feature = "__astc_internals"))]
#[doc(hidden)]
pub mod testgen;
#[cfg(test)]
mod tests;

pub use container::ktx::{self, Ktx2View, KtxView};
pub use container::{AstcFile, AstcSurface};
pub use footprint::{block_rows, payload_len, AstcFootprint};
#[cfg(feature = "encode")]
pub use encode::{
    encode_block_rows_into, encode_rgba8, encode_rgba8_into, encode_rgba_f32, encode_volume_rgba8, AstcEncodeOptions,
    AstcPreset,
};
pub use parse::IllegalReason;
pub use reference::{decode_block_f16, decode_block_rgba8, illegal_reason};

use crate::{Error, ImageRgba8, ImageRgbaF32};

/// The four decode profiles, mirroring astcenc's.
///
/// - `LdrSrgb`: sRGB operation (8-bit endpoint expansion with `0x80`); HDR
///   content decodes to the error colour.
/// - `Ldr`: linear LDR operation; HDR content decodes to the error colour.
/// - `HdrRgbLdrA`: HDR operation; HDR modes without coded alpha get an LDR
///   alpha of 1.0.
/// - `Hdr`: HDR operation; HDR modes without coded alpha get an HDR alpha of
///   1.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AstcProfile {
    LdrSrgb,
    Ldr,
    HdrRgbLdrA,
    Hdr,
}

/// Outcome of decoding one block. Illegal blocks are not errors: they decode
/// to the reference error colour, as the specification requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AstcBlockStatus {
    /// A legal block, decoded.
    Ok,
    /// An illegal encoding; every texel is the error colour.
    Illegal(IllegalReason),
    /// HDR content (an HDR endpoint mode in some partition, or an HDR
    /// void-extent block) under an LDR profile; those texels are the error
    /// colour.
    HdrInLdr,
}

/// What a surface decode saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct AstcDecodeReport {
    pub blocks: u64,
    pub illegal_blocks: u64,
    /// Blocks with an HDR endpoint mode or HDR void-extent decoded under an
    /// LDR profile.
    pub hdr_in_ldr_blocks: u64,
}

decode::surface_api!(decode::Fast);

/// Internal stages, exposed only for the differential harness in
/// `tools/astc-harness`. Not a stable API: the feature is dev-only.
#[cfg(feature = "__astc_internals")]
#[doc(hidden)]
pub mod internals {
    pub use super::block_mode::{check, classify, GridIllegal, GridMode, ModeClass};
    pub use super::cem::{decode as cem_decode, expand as cem_expand, value_count, Endpoints, RawEndpoints};
    pub use super::infill::{infill, GRID_PAD};
    pub use super::ise::{decode as ise_decode, sequence_bits, LEVELS};
    pub use super::parse::{parse, BlockKind, SymbolicBlock};
    pub use super::partition::select_partition;
    pub use super::quant::{COLOR_UNQUANT, WEIGHT_UNQUANT};
    pub use super::testgen;
}
