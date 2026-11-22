//! ASTC inside DDS: the de facto DXGI values 133..=187 (three per footprint:
//! `TYPELESS`, `UNORM`, `UNORM_SRGB`). This glue depends on [`crate::astc`];
//! the ASTC module itself knows nothing about DDS.

use crate::astc::{self, AstcFootprint, AstcProfile, AstcSurface};
use crate::{DdsBase, DecodeOptions, Error, SubresourceId};

/// Classification of an ASTC DDS. Separate from
/// [`DecodeContent`](crate::DecodeContent), as BC6H is, so the LDR matrix and
/// the BCn encode matrix are untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct AstcContent {
    pub footprint: AstcFootprint,
    /// The format is `UNORM_SRGB`.
    pub srgb: bool,
}

impl<D: AsRef<[u8]>> DdsBase<D> {
    /// Classify an ASTC DDS (DXGI 133..=187). `Err(UnsupportedFormat)` for
    /// every other format.
    pub fn astc_content(&self) -> Result<AstcContent, Error> {
        let f = self.get_dxgi_format().ok_or(Error::UnsupportedFormat)?;
        let (w, h, srgb) = f.astc_info().ok_or(Error::UnsupportedFormat)?;
        let footprint = AstcFootprint::new_2d(w, h).ok_or(Error::UnsupportedFormat)?;
        Ok(AstcContent { footprint, srgb })
    }

    /// One subresource of an ASTC DDS as an [`AstcSurface`]. The DXGI
    /// convention has no HDR tag, so `hdr` is `None`.
    pub fn astc_surface(&self, id: SubresourceId) -> Result<AstcSurface<'_>, Error> {
        let c = self.astc_content()?;
        let s = self.surface(id)?;
        Ok(AstcSurface::new(
            c.footprint,
            s.width,
            s.height,
            s.depth.max(1),
            Some(c.srgb),
            None,
            s.data,
        ))
    }

    /// Profile for the 8-bit path: the override, else `LdrSrgb` for
    /// `_UNORM_SRGB` and `Ldr` otherwise.
    pub(crate) fn astc_profile_u8(c: AstcContent, opts: DecodeOptions) -> AstcProfile {
        opts.astc_profile.unwrap_or(if c.srgb {
            AstcProfile::LdrSrgb
        } else {
            AstcProfile::Ldr
        })
    }

    /// Profile for the `f32` path: the override, else `LdrSrgb` for
    /// `_UNORM_SRGB` and `Hdr` otherwise (a linear tag cannot say whether the
    /// data is HDR; `Hdr` decodes LDR blocks identically and HDR blocks
    /// instead of failing on them).
    pub(crate) fn astc_profile_f32(c: AstcContent, opts: DecodeOptions) -> AstcProfile {
        opts.astc_profile.unwrap_or(if c.srgb {
            AstcProfile::LdrSrgb
        } else {
            AstcProfile::Hdr
        })
    }

    /// `decode_rgba8_into` for ASTC.
    pub(crate) fn astc_decode_rgba8_into(
        &self,
        id: SubresourceId,
        c: AstcContent,
        opts: DecodeOptions,
        dst: &mut Vec<u8>,
    ) -> Result<(u32, u32, u32), Error> {
        let s = self.astc_surface(id)?;
        let need =
            astc::payload_len(s.width, s.height, s.depth, s.footprint).ok_or(Error::OutOfBounds)?;
        if s.data.len() < need {
            return Err(Error::TruncatedData);
        }
        let n = (s.width as usize)
            .checked_mul(s.height as usize)
            .and_then(|v| v.checked_mul(s.depth as usize))
            .and_then(|v| v.checked_mul(4))
            .ok_or(Error::OutOfBounds)?;
        if dst.len() != n {
            dst.clear();
            dst.resize(n, 0);
        }
        astc::decode_rgba8_into(
            s.data,
            s.width,
            s.height,
            s.depth,
            s.footprint,
            Self::astc_profile_u8(c, opts),
            dst,
        )?;
        Ok((s.width, s.height, s.depth))
    }

    /// `decode_rgba_f32_into` for ASTC. sRGB decodes at 8 bits and widens as
    /// an exact `x / 255` (the crate's rule for formats defined at 8 bits);
    /// the other profiles widen the half-float decode exactly.
    pub(crate) fn astc_decode_rgba_f32_into(
        &self,
        id: SubresourceId,
        c: AstcContent,
        opts: DecodeOptions,
        dst: &mut Vec<f32>,
    ) -> Result<(u32, u32, u32), Error> {
        let s = self.astc_surface(id)?;
        let need =
            astc::payload_len(s.width, s.height, s.depth, s.footprint).ok_or(Error::OutOfBounds)?;
        if s.data.len() < need {
            return Err(Error::TruncatedData);
        }
        let n = (s.width as usize)
            .checked_mul(s.height as usize)
            .and_then(|v| v.checked_mul(s.depth as usize))
            .and_then(|v| v.checked_mul(4))
            .ok_or(Error::OutOfBounds)?;
        if dst.len() != n {
            dst.clear();
            dst.resize(n, 0.0);
        }
        let profile = Self::astc_profile_f32(c, opts);
        if profile == AstcProfile::LdrSrgb {
            let mut tmp = vec![0u8; n];
            astc::decode_rgba8_into(
                s.data,
                s.width,
                s.height,
                s.depth,
                s.footprint,
                profile,
                &mut tmp,
            )?;
            for (d, &v) in dst.iter_mut().zip(&tmp) {
                *d = v as f32 / 255.0;
            }
        } else {
            astc::decode_rgba_f32_into(
                s.data,
                s.width,
                s.height,
                s.depth,
                s.footprint,
                profile,
                dst,
            )?;
        }
        Ok((s.width, s.height, s.depth))
    }

    /// Block rows (footprint-height pixel rows) of an ASTC subresource.
    pub(crate) fn astc_block_rows(&self, id: SubresourceId, c: AstcContent) -> Result<u32, Error> {
        Ok(astc::block_rows(self.surface(id)?.height, c.footprint))
    }

    pub(crate) fn astc_decode_block_rows_into(
        &self,
        id: SubresourceId,
        c: AstcContent,
        rows: core::ops::Range<u32>,
        dst: &mut [u8],
    ) -> Result<(), Error> {
        let s = self.astc_surface(id)?;
        if s.depth != 1 {
            return Err(Error::UnsupportedFormat);
        }
        astc::decode_block_rows_into(
            s.data,
            s.width,
            s.height,
            s.footprint,
            Self::astc_profile_u8(c, DecodeOptions::new()),
            rows,
            dst,
        )?;
        Ok(())
    }

    pub(crate) fn astc_decode_block_rows_f32_into(
        &self,
        id: SubresourceId,
        c: AstcContent,
        rows: core::ops::Range<u32>,
        dst: &mut [f32],
    ) -> Result<(), Error> {
        let s = self.astc_surface(id)?;
        if s.depth != 1 {
            return Err(Error::UnsupportedFormat);
        }
        let profile = Self::astc_profile_f32(c, DecodeOptions::new());
        if profile == AstcProfile::LdrSrgb {
            let mut tmp = vec![0u8; dst.len()];
            astc::decode_block_rows_into(
                s.data,
                s.width,
                s.height,
                s.footprint,
                profile,
                rows,
                &mut tmp,
            )?;
            for (d, &v) in dst.iter_mut().zip(&tmp) {
                *d = v as f32 / 255.0;
            }
        } else {
            astc::decode_block_rows_f32_into(
                s.data,
                s.width,
                s.height,
                s.footprint,
                profile,
                rows,
                dst,
            )?;
        }
        Ok(())
    }
}

impl DecodeOptions {
    /// Override the profile used for ASTC surfaces, for example to decode HDR
    /// data stored in a `_UNORM`-tagged DDS with [`AstcProfile::Hdr`], or to
    /// get astcenc's own half-float values for sRGB data on the `f32` path.
    pub const fn astc_profile(mut self, profile: AstcProfile) -> Self {
        self.astc_profile = Some(profile);
        self
    }
}

#[cfg(feature = "encode")]
impl crate::Dds {
    /// Encode tightly packed RGBA8 pixels as a single-level 2D ASTC DDS
    /// (`ASTC_*_UNORM`, or `_UNORM_SRGB` for [`AstcProfile::LdrSrgb`]). Like
    /// [`crate::Dds::encode_bc6h_uf16`]: a dedicated entry point for a format
    /// outside [`crate::DecodeContent`].
    pub fn encode_astc_from_rgba8(
        pixels: &[u8],
        width: u32,
        height: u32,
        opts: &astc::AstcEncodeOptions,
    ) -> Result<crate::Dds, Error> {
        let fp = opts.footprint;
        let srgb = opts.profile == AstcProfile::LdrSrgb;
        let format = crate::DxgiFormat::from_astc(fp.width(), fp.height(), srgb)
            .ok_or(Error::UnsupportedFormat)?;
        let mut dds = crate::Dds::new_dxgi(crate::NewDxgiParams {
            height,
            width,
            depth: None,
            format,
            mipmap_levels: None,
            array_layers: None,
            caps2: None,
            is_cubemap: false,
            resource_dimension: crate::D3D10ResourceDimension::Texture2D,
            alpha_mode: crate::AlphaMode::Straight,
        })?;
        astc::encode_rgba8_into(pixels, width, height, opts, &mut dds.data)?;
        Ok(dds)
    }
}
