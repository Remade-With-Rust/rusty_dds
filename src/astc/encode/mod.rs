//! ASTC encode (LDR), in pure Rust.
//!
//! Presets trade speed for quality; every emitted block is legal (it parses
//! in [`super::reference`], and the gates in tools/astc-harness check that
//! astcenc decodes it identically). Output is deterministic: no threads, no
//! hash maps, no time or randomness, fixed floating-point operation order and
//! no libm; it is identical however the caller splits block rows.

mod block;
mod context;

use alloc::vec::Vec;

use self::block::{encode_block, Params, Target};
use self::context::Ctx;
use super::footprint::{block_rows, payload_len, AstcFootprint};
use super::parse::BlockKind;
use super::AstcProfile;
use crate::Error;

/// Encoder speed/quality presets. Names follow astcenc's; the measured gap to
/// astcenc at each preset is in docs/artifacts/astc-encode-quality.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AstcPreset {
    Fastest,
    Fast,
    Medium,
    Thorough,
    Exhaustive,
}

/// Encoder options.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct AstcEncodeOptions {
    pub footprint: AstcFootprint,
    /// `LdrSrgb` or `Ldr`. HDR profiles return [`Error::UnsupportedFormat`].
    pub profile: AstcProfile,
    pub preset: AstcPreset,
    /// Optimise for the `decode_unorm8` rounding rule (astcenc's
    /// `-decode_unorm8`). Default false: optimise the 16-bit interpolated
    /// value. sRGB always uses the 8-bit rule.
    pub decode_unorm8: bool,
    /// Per-channel error weights (astcenc's `-cw`). Default `[1.0; 4]`.
    pub channel_weights: [f32; 4],
}

impl AstcEncodeOptions {
    /// Defaults: `Medium`, `decode_unorm8 = false`, unit channel weights.
    pub const fn new(footprint: AstcFootprint, profile: AstcProfile) -> Self {
        Self {
            footprint,
            profile,
            preset: AstcPreset::Medium,
            decode_unorm8: false,
            channel_weights: [1.0; 4],
        }
    }
    pub const fn with_preset(mut self, preset: AstcPreset) -> Self {
        self.preset = preset;
        self
    }
    pub const fn with_decode_unorm8(mut self, on: bool) -> Self {
        self.decode_unorm8 = on;
        self
    }
    pub const fn with_channel_weights(mut self, w: [f32; 4]) -> Self {
        self.channel_weights = w;
        self
    }
}

pub(crate) fn params(p: AstcPreset) -> Params {
    #[allow(unused_mut)]
    let mut prm = preset_params(p);
    // DEVELOPMENT ONLY (feature `tuning`): sweep the search limits through
    // RUSTY_DDS_ASTC_* environment variables. A shipped build never reads them.
    #[cfg(feature = "tuning")]
    {
        let get = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f32>().ok());
        if let Some(v) = get("RUSTY_DDS_ASTC_PARTS") { prm.max_partitions = v as u8; }
        if let Some(v) = get("RUSTY_DDS_ASTC_PART2") { prm.part_candidates[0] = v as usize; }
        if let Some(v) = get("RUSTY_DDS_ASTC_PART3") { prm.part_candidates[1] = v as usize; }
        if let Some(v) = get("RUSTY_DDS_ASTC_PART4") { prm.part_candidates[2] = v as usize; }
        if let Some(v) = get("RUSTY_DDS_ASTC_EXACT") { prm.exact_candidates = v as usize; }
        if let Some(v) = get("RUSTY_DDS_ASTC_REFINE") { prm.refine = v as u8; }
        if let Some(v) = get("RUSTY_DDS_ASTC_REFINE_TOP") { prm.refine_top = v as usize; }
        if let Some(v) = get("RUSTY_DDS_ASTC_DUAL") { prm.dual_plane = v != 0.0; }
        if let Some(v) = get("RUSTY_DDS_ASTC_DUAL_CORR") { prm.dual_corr_limit = v; }
        if let Some(v) = get("RUSTY_DDS_ASTC_GRID_ITERS") { prm.grid_iters = v as u8; }
        if let Some(v) = get("RUSTY_DDS_ASTC_SKIP") { prm.part2_skip_err = v; }
    }
    prm
}

fn preset_params(p: AstcPreset) -> Params {
    match p {
        AstcPreset::Fastest => Params {
            refine_top: 1,
            max_partitions: 2,
            part_candidates: [1, 0, 0],
            exact_candidates: 2,
            refine: 1,
            dual_plane: false,
            dual_corr_limit: 0.0,
            grid_iters: 1,
            part2_skip_err: 4.0,
        },
        AstcPreset::Fast => Params {
            refine_top: 1,
            max_partitions: 2,
            part_candidates: [2, 0, 0],
            exact_candidates: 3,
            refine: 2,
            dual_plane: true,
            dual_corr_limit: 0.65,
            grid_iters: 2,
            part2_skip_err: 2.0,
        },
        AstcPreset::Medium => Params {
            refine_top: 2,
            max_partitions: 2,
            part_candidates: [4, 0, 0],
            exact_candidates: 4,
            refine: 3,
            dual_plane: true,
            dual_corr_limit: 0.85,
            grid_iters: 3,
            part2_skip_err: 0.75,
        },
        AstcPreset::Thorough => Params {
            refine_top: 3,
            max_partitions: 4,
            part_candidates: [8, 4, 2],
            exact_candidates: 8,
            refine: 4,
            dual_plane: true,
            dual_corr_limit: 0.98,
            grid_iters: 4,
            part2_skip_err: 0.2,
        },
        AstcPreset::Exhaustive => Params {
            refine_top: 4,
            max_partitions: 4,
            part_candidates: [16, 8, 4],
            exact_candidates: 12,
            refine: 4,
            dual_plane: true,
            dual_corr_limit: 1.0,
            grid_iters: 4,
            part2_skip_err: 0.0,
        },
    }
}

fn check(pixels: &[u8], width: u32, height: u32, opts: &AstcEncodeOptions) -> Result<(), Error> {
    if width == 0 || height == 0 {
        return Err(Error::InvalidField("ASTC encode: zero dimension".into()));
    }
    if opts.footprint.is_3d() || !matches!(opts.profile, AstcProfile::Ldr | AstcProfile::LdrSrgb) {
        return Err(Error::UnsupportedFormat);
    }
    let need = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(Error::OutOfBounds)?;
    if pixels.len() != need {
        return Err(Error::OutOfBounds);
    }
    if opts.channel_weights.iter().any(|w| !(w.is_finite() && *w >= 0.0)) {
        return Err(Error::InvalidField("ASTC encode: channel weight".into()));
    }
    Ok(())
}

fn target(opts: &AstcEncodeOptions) -> Target {
    Target {
        profile: opts.profile,
        unorm8: opts.decode_unorm8 || opts.profile == AstcProfile::LdrSrgb,
        cw: opts.channel_weights,
    }
}

/// Encode tightly packed RGBA8 `pixels` to ASTC block data (row-major blocks).
pub fn encode_rgba8(pixels: &[u8], width: u32, height: u32, opts: &AstcEncodeOptions) -> Result<Vec<u8>, Error> {
    check(pixels, width, height, opts)?;
    let n = payload_len(width, height, 1, opts.footprint).ok_or(Error::OutOfBounds)?;
    let mut out = alloc::vec![0u8; n];
    encode_rgba8_into(pixels, width, height, opts, &mut out)?;
    Ok(out)
}

/// [`encode_rgba8`] into caller memory (`payload_len(width, height, 1, fp)` bytes).
pub fn encode_rgba8_into(
    pixels: &[u8],
    width: u32,
    height: u32,
    opts: &AstcEncodeOptions,
    out: &mut [u8],
) -> Result<(), Error> {
    let rows = block_rows(height, opts.footprint);
    encode_block_rows_into(pixels, width, height, opts, 0..rows, out)
}

/// The caller-threaded seam: encode block rows `rows` into `out`, which
/// receives exactly those rows of blocks. Output is identical however the
/// rows are split.
pub fn encode_block_rows_into(
    pixels: &[u8],
    width: u32,
    height: u32,
    opts: &AstcEncodeOptions,
    rows: core::ops::Range<u32>,
    out: &mut [u8],
) -> Result<(), Error> {
    check(pixels, width, height, opts)?;
    let fp = opts.footprint;
    let total = block_rows(height, fp);
    if rows.start > rows.end || rows.end > total {
        return Err(Error::OutOfBounds);
    }
    let (nbx, _, _) = fp.block_counts(width, height, 1);
    let need = (nbx as usize)
        .checked_mul((rows.end - rows.start) as usize)
        .and_then(|n| n.checked_mul(16))
        .ok_or(Error::OutOfBounds)?;
    if out.len() != need {
        return Err(Error::OutOfBounds);
    }
    let prm = params(opts.preset);
    let ctx = Ctx::new(fp, prm.max_partitions);
    let tgt = target(opts);
    let (bw, bh) = (fp.width(), fp.height());
    let mut px = [[0u8; 4]; 216];
    let mut o = 0usize;
    for by in rows {
        for bx in 0..nbx {
            // Gather with coordinates clamped to the image (edge blocks).
            let mut i = 0;
            for ty in 0..bh {
                let y = (by * bh + ty).min(height - 1) as usize;
                for tx in 0..bw {
                    let x = (bx * bw + tx).min(width - 1) as usize;
                    let s = (y * width as usize + x) * 4;
                    px[i] = [pixels[s], pixels[s + 1], pixels[s + 2], pixels[s + 3]];
                    i += 1;
                }
            }
            let best = encode_block(&ctx, &prm, tgt, &px[..i]);
            let blk = super::pack::pack(&best.sym, fp);
            debug_assert!(
                {
                    let t = super::parse::parse(&blk, fp);
                    !matches!(t.kind, BlockKind::Illegal(_))
                },
                "encoder emitted an illegal block"
            );
            out[o..o + 16].copy_from_slice(&blk);
            o += 16;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
