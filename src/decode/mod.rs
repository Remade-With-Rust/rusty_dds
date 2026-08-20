//! CPU decode of DDS subresources to tightly packed RGBA8.
//!
//! **sRGB policy:** channel bytes are returned as stored. Formats tagged `_sRGB`
//! are **not** converted to linear — callers that need linear light must convert.
//!
//! Block compression (BC1–BC5, BC7) uses the pure-Rust [`bcdec_rs`] crate (MIT),
//! the same decompression core as `image_dds`. Uncompressed layouts are handled
//! in-house.

mod bc6h;
pub(crate) mod bcn;
#[cfg(all(feature = "simd", target_arch = "x86_64"))]
mod simd;
mod uncompressed;
pub mod reference;

use crate::content::{
    slice_payload_bytes, DecodeContent, HdrDecodeContent, ImageRgba8, ImageRgbaF32,
};
use crate::error::Error;
use crate::surface::SubresourceId;
use crate::DdsBase;

impl<D: AsRef<[u8]>> DdsBase<D> {
    /// Decode one subresource to tightly packed RGBA8.
    ///
    /// Volumes (`depth > 1`) decode every depth slice and stack them in
    /// [`ImageRgba8::pixels`]. sRGB-tagged formats return stored bytes without
    /// linearization.
    ///
    /// # Channel conventions for BC4 and BC5
    ///
    /// **BC4 decodes to `(R, 0, 0, 255)` and BC5 to `(R, G, 0, 255)`** — the
    /// values a GPU returns when sampling those formats, where the absent
    /// channels read as zero and alpha as one.
    ///
    /// This differs from Microsoft DirectXTex, which **replicates** BC4's single
    /// channel into green and blue, producing `(R, R, R, 255)`. That is a
    /// greyscale-viewer convention: it makes a roughness or height map look
    /// right in an image preview. Measured over a 512^2 BC4 surface, the two
    /// agree on R and A for every one of 262 144 pixels and disagree on G and B
    /// for every one of them.
    ///
    /// Neither is wrong; they answer different questions. If you are porting
    /// from `DirectXTex::Decompress` and your single-channel maps suddenly look
    /// red, this is why — replicate the red channel yourself. DirectXTex does
    /// *not* replicate for BC5, so only BC4 is affected.
    pub fn decode_rgba8(&self, id: SubresourceId) -> Result<ImageRgba8, Error> {
        let surf = self.surface(id)?;
        let kind = self.decode_content()?;
        let pixels = decode_surface_pixels(kind, surf.data, surf.width, surf.height, surf.depth)?;
        Ok(ImageRgba8 {
            width: surf.width,
            height: surf.height,
            depth: surf.depth,
            pixels,
        })
    }
}

impl<D: AsRef<[u8]>> DdsBase<D> {
    /// Decode one HDR subresource (BC6H) to tightly packed RGBA `f32`
    /// (`A = 1.0`). Volumes decode every depth slice, stacked. LDR content
    /// stays on [`Dds::decode_rgba8`]; each API fails closed on the other's
    /// formats.
    pub fn decode_rgba_f32(&self, id: SubresourceId) -> Result<ImageRgbaF32, Error> {
        let kind = self.hdr_decode_content()?;
        let signed = kind == HdrDecodeContent::Bc6hSf16;
        let surf = self.surface(id)?;
        let slice_bytes = {
            let bx = (surf.width as usize + 3) / 4;
            let by = (surf.height as usize + 3) / 4;
            bx.checked_mul(by)
                .and_then(|n| n.checked_mul(16))
                .ok_or(Error::OutOfBounds)?
        };
        let need = slice_bytes
            .checked_mul(surf.depth as usize)
            .ok_or(Error::OutOfBounds)?;
        if surf.data.len() < need {
            return Err(Error::TruncatedData);
        }
        // The 2D case is every case that matters, and `decode_bc6h` already
        // returns exactly the buffer we want. Building a second full-size `Vec`
        // and extending it into cost a whole extra allocation plus a copy of the
        // output — and this output is 16 bytes a pixel, four times RGBA8. The
        // LDR path has always short-circuited depth == 1; this one did not.
        if surf.depth <= 1 {
            return Ok(ImageRgbaF32 {
                width: surf.width,
                height: surf.height,
                depth: surf.depth,
                pixels: bc6h::decode_bc6h(&surf.data[..slice_bytes], surf.width, surf.height, signed)?,
            });
        }

        let mut pixels = Vec::with_capacity(
            (surf.width as usize)
                .saturating_mul(surf.height as usize)
                .saturating_mul(surf.depth as usize)
                .saturating_mul(4),
        );
        for z in 0..surf.depth as usize {
            let start = z * slice_bytes;
            pixels.extend(bc6h::decode_bc6h(
                &surf.data[start..start + slice_bytes],
                surf.width,
                surf.height,
                signed,
            )?);
        }
        Ok(ImageRgbaF32 {
            width: surf.width,
            height: surf.height,
            depth: surf.depth,
            pixels,
        })
    }

    /// Decode one HDR subresource to RGBA `f32` **into a buffer you own**.
    ///
    /// The HDR twin of [`DdsBase::decode_rgba8_into`], and it matters more here:
    /// this output is 16 bytes a pixel, four times RGBA8, so a 1024^2 surface
    /// hands back 16 MiB that the operating system zeroed for you and the
    /// decoder immediately overwrote. Recycle one buffer per worker instead.
    ///
    /// `dst` is resized to fit and fully overwritten. Returns
    /// `(width, height, depth)`.
    pub fn decode_rgba_f32_into(
        &self,
        id: SubresourceId,
        dst: &mut Vec<f32>,
    ) -> Result<(u32, u32, u32), Error> {
        let kind = self.hdr_decode_content()?;
        let signed = kind == HdrDecodeContent::Bc6hSf16;
        let surf = self.surface(id)?;
        let (w, h, d) = (surf.width, surf.height, surf.depth.max(1));
        let slice_bytes = hdr_slice_bytes(w, h)?;
        let need = slice_bytes.checked_mul(d as usize).ok_or(Error::OutOfBounds)?;
        if surf.data.len() < need {
            return Err(Error::TruncatedData);
        }

        let out_slice = (w as usize)
            .checked_mul(h as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or(Error::OutOfBounds)?;
        let total = out_slice.checked_mul(d as usize).ok_or(Error::OutOfBounds)?;
        // `resize` only zeroes what it adds, so a correctly sized buffer costs
        // nothing here — the whole point of handing one back in.
        if dst.len() != total {
            dst.clear();
            dst.resize(total, 0.0);
        }

        for z in 0..d as usize {
            let src = &surf.data[z * slice_bytes..(z + 1) * slice_bytes];
            let dst_z = dst
                .get_mut(z * out_slice..(z + 1) * out_slice)
                .ok_or(Error::OutOfBounds)?;
            bc6h::decode_bc6h_into(src, w, h, signed, dst_z)?;
        }
        Ok((w, h, d))
    }

    /// Decode a **range of block rows** of one HDR subresource into caller
    /// memory — the seam your job system splits on.
    ///
    /// BC6H is the most expensive decode this crate ships: at 1024^2 it is
    /// ~21 ms serial, against 2.6 ms for BC7 split across a caller's threads.
    /// Unlike BC7 it has no internal thread pool to fall back on, and it should
    /// not grow one — a texture library that seizes cores inside a frame is a
    /// library an engine has to work around. This hands you the split instead.
    ///
    /// `rows` is in block rows of 4 pixel rows each; `dst` receives only those
    /// rows, tightly packed at `width * 4` floats per pixel row. Use
    /// [`DdsBase::block_rows_f32`] for the count.
    ///
    /// # Split only what is worth splitting
    ///
    /// **Do not split a whole mip chain.** Measured over a cooked 512^2 pack,
    /// splitting every level across 24 threads is a **net 0.53x — slower than
    /// serial** — because a ten-level chain is mostly small mips, and entering
    /// `std::thread::scope` costs ~50 us even to spawn a single worker. That is
    /// more than the entire decode of every level past mip 4.
    ///
    /// Split above roughly **16 384 blocks (512x512)** and decode the rest on
    /// the calling thread. Same pack, same threads, that one rule turns 0.53x
    /// into **1.35x**; at 1024^2 mip 0 the split alone is worth 6.8x.
    ///
    /// ```text
    /// let blocks = w.div_ceil(4) * h.div_ceil(4);
    /// if blocks >= 16_384 { /* fan out */ } else { /* decode here */ }
    /// ```
    ///
    /// ```no_run
    /// # use rusty_dds::{Dds, SubresourceId};
    /// # fn f(dds: &Dds) -> Result<(), rusty_dds::Error> {
    /// let id = SubresourceId::mip_layer(0, 0);
    /// let rows = dds.block_rows_f32(id)?;
    /// let (w, h, _) = (1024usize, 1024usize, 1);
    /// let mut pixels = vec![0f32; w * h * 4];
    /// let (top, bottom) = pixels.split_at_mut(w * (h / 2) * 4);
    /// // In a real engine these two are two jobs on two cores.
    /// dds.decode_block_rows_f32_into(id, 0..rows / 2, top)?;
    /// dds.decode_block_rows_f32_into(id, rows / 2..rows, bottom)?;
    /// # Ok(()) }
    /// ```
    pub fn decode_block_rows_f32_into(
        &self,
        id: SubresourceId,
        rows: core::ops::Range<u32>,
        dst: &mut [f32],
    ) -> Result<(), Error> {
        let kind = self.hdr_decode_content()?;
        let signed = kind == HdrDecodeContent::Bc6hSf16;
        let surf = self.surface(id)?;
        if surf.depth.max(1) != 1 {
            // Volumes want a slice index alongside the row range; no caller has
            // needed it yet, and guessing the layout would be worse than failing.
            return Err(Error::UnsupportedFormat);
        }
        let total = surf.height.div_ceil(4);
        if rows.start > rows.end || rows.end > total {
            return Err(Error::OutOfBounds);
        }
        let y0 = rows.start * 4;
        let y1 = (rows.end * 4).min(surf.height);
        if y0 >= y1 {
            return Ok(());
        }

        // 16 bytes per block, one block row is `blocks_x` of them.
        let row_pitch = (surf.width as usize).div_ceil(4) * 16;
        let src = surf
            .data
            .get(rows.start as usize * row_pitch..rows.end as usize * row_pitch)
            .ok_or(Error::TruncatedData)?;
        bc6h::decode_bc6h_into(src, surf.width, y1 - y0, signed, dst)
    }

    /// Block rows in one HDR subresource — the unit
    /// [`DdsBase::decode_block_rows_f32_into`] splits on. Always 4 pixel rows.
    pub fn block_rows_f32(&self, id: SubresourceId) -> Result<u32, Error> {
        self.hdr_decode_content()?;
        Ok(self.surface(id)?.height.div_ceil(4))
    }
}

/// Payload bytes for one BC6H slice.
fn hdr_slice_bytes(width: u32, height: u32) -> Result<usize, Error> {
    let bx = (width as usize).div_ceil(4);
    let by = (height as usize).div_ceil(4);
    bx.checked_mul(by)
        .and_then(|n| n.checked_mul(16))
        .ok_or(Error::OutOfBounds)
}

impl<D: AsRef<[u8]>> DdsBase<D> {
    /// Decode one subresource to RGBA8 **into a buffer you own and recycle**.
    ///
    /// [`DdsBase::decode_rgba8`] allocates a fresh `Vec` every call. That buffer
    /// is handed over zeroed by the operating system and then immediately
    /// overwritten by the decoder, and on a 1024^2 BC7 surface that measured at
    /// **41% of the whole call** (1.44 ms of 3.52 ms). Reuse one buffer per
    /// worker and the cost is the decode alone.
    ///
    /// `dst` is resized to fit and fully overwritten. Returns
    /// `(width, height, depth)`.
    ///
    /// ```
    /// # use rusty_dds::{Dds, DdsView, SubresourceId};
    /// # let mut bytes = Vec::new();
    /// # Dds::new_dxgi(rusty_dds::NewDxgiParams {
    /// #     height: 64, width: 64, depth: None,
    /// #     format: rusty_dds::DxgiFormat::BC1_UNorm,
    /// #     mipmap_levels: None, array_layers: None, caps2: None, is_cubemap: false,
    /// #     resource_dimension: rusty_dds::D3D10ResourceDimension::Texture2D,
    /// #     alpha_mode: rusty_dds::AlphaMode::Straight,
    /// # })?.write(&mut bytes)?;
    /// let dds = DdsView::parse(&bytes)?;
    /// let mut pixels = Vec::new();          // hoisted out of the loop
    /// let (w, h, _) = dds.decode_rgba8_into(SubresourceId::mip_layer(0, 0), &mut pixels)?;
    /// assert_eq!(pixels.len(), (w * h * 4) as usize);
    /// # Ok::<(), rusty_dds::Error>(())
    /// ```
    pub fn decode_rgba8_into(
        &self,
        id: SubresourceId,
        dst: &mut Vec<u8>,
    ) -> Result<(u32, u32, u32), Error> {
        let surf = self.surface(id)?;
        let kind = self.decode_content()?;
        let (w, h, d) = (surf.width, surf.height, surf.depth.max(1));

        // Validate the SOURCE before sizing the destination. `w` and `h` come
        // from the header and are attacker-controlled; the output is 4 bytes a
        // pixel, so a corrupt header can name a surface that needs hundreds of
        // gigabytes. The individual decoders already refuse a short payload, but
        // they do it *after* this function would have allocated. Requiring the
        // payload to be large enough first bounds the destination by bytes that
        // actually exist. (Found by `parser_robustness`, which aborted on a
        // 256 GiB request before this check existed.)
        let slice_bytes = slice_payload_bytes(kind, w, h)?;
        let need_src = slice_bytes
            .checked_mul(d as usize)
            .ok_or(Error::OutOfBounds)?;
        if surf.data.len() < need_src {
            return Err(Error::TruncatedData);
        }

        let need = (w as usize)
            .checked_mul(h as usize)
            .and_then(|n| n.checked_mul(d as usize))
            .and_then(|n| n.checked_mul(4))
            .ok_or(Error::OutOfBounds)?;
        // `resize` only zeroes bytes it adds, so a buffer already the right
        // size costs nothing here — which is the point of recycling one.
        if dst.len() != need {
            dst.clear();
            dst.resize(need, 0);
        }

        if d == 1 {
            return decode_slice_into(kind, surf.data, w, h, dst).map(|()| (w, h, d));
        }
        let out_slice = (w as usize) * (h as usize) * 4;
        for z in 0..d as usize {
            let src = surf
                .data
                .get(z * slice_bytes..(z + 1) * slice_bytes)
                .ok_or(Error::TruncatedData)?;
            let dst_z = dst
                .get_mut(z * out_slice..(z + 1) * out_slice)
                .ok_or(Error::OutOfBounds)?;
            decode_slice_into(kind, src, w, h, dst_z)?;
        }
        Ok((w, h, d))
    }

    /// Decode a **range of block-rows** of one subresource into caller memory.
    ///
    /// This is the seam for a caller that already has a job system. rusty_dds
    /// spawns one thread per core inside `decode_bc7`, and that costs ~0.98 ms
    /// per call on a 24-core box whatever the work — 34% of a 1024^2 BC7 decode,
    /// and more than the entire cost of the equivalent BC1 decode. Splitting the
    /// surface here instead means **the library owns no threads at all** and your
    /// scheduler keeps its cores.
    ///
    /// `rows` is in **block rows** (4 pixel rows each for BCn, 1 for uncompressed
    /// formats); `dst` receives only those rows, tightly packed at
    /// `width * 4` bytes per pixel row. Use [`DdsBase::block_rows`] for the count.
    ///
    /// ```
    /// # use rusty_dds::{Dds, DdsView, SubresourceId};
    /// # let mut bytes = Vec::new();
    /// # Dds::new_dxgi(rusty_dds::NewDxgiParams {
    /// #     height: 64, width: 64, depth: None,
    /// #     format: rusty_dds::DxgiFormat::BC1_UNorm,
    /// #     mipmap_levels: None, array_layers: None, caps2: None, is_cubemap: false,
    /// #     resource_dimension: rusty_dds::D3D10ResourceDimension::Texture2D,
    /// #     alpha_mode: rusty_dds::AlphaMode::Straight,
    /// # })?.write(&mut bytes)?;
    /// let dds = DdsView::parse(&bytes)?;
    /// let id = SubresourceId::mip_layer(0, 0);
    /// let rows = dds.block_rows(id)?;                 // 16 block rows for 64px
    /// let mut pixels = vec![0u8; 64 * 64 * 4];
    /// // Two halves — in a real engine these are two jobs on two cores.
    /// let (top, bottom) = pixels.split_at_mut(64 * 32 * 4);
    /// dds.decode_block_rows_into(id, 0..rows / 2, top)?;
    /// dds.decode_block_rows_into(id, rows / 2..rows, bottom)?;
    /// # Ok::<(), rusty_dds::Error>(())
    /// ```
    pub fn decode_block_rows_into(
        &self,
        id: SubresourceId,
        rows: core::ops::Range<u32>,
        dst: &mut [u8],
    ) -> Result<(), Error> {
        let surf = self.surface(id)?;
        let kind = self.decode_content()?;
        if surf.depth.max(1) != 1 {
            // Volume textures stack slices; splitting them wants a slice index
            // as well as a row range, and no caller has asked for it yet.
            return Err(Error::UnsupportedFormat);
        }
        let total = self.block_rows(id)?;
        if rows.start > rows.end || rows.end > total {
            return Err(Error::OutOfBounds);
        }
        let block_h = block_height(kind);
        let y0 = rows.start * block_h;
        let y1 = (rows.end * block_h).min(surf.height);
        if y0 >= y1 {
            return Ok(());
        }

        // Source bytes for these block rows.
        let row_pitch = block_row_bytes(kind, surf.width)?;
        let src = surf
            .data
            .get(rows.start as usize * row_pitch..rows.end as usize * row_pitch)
            .ok_or(Error::TruncatedData)?;

        decode_slice_into(kind, src, surf.width, y1 - y0, dst)
    }

    /// Block rows in one subresource — the unit [`DdsBase::decode_block_rows_into`]
    /// splits on. Four pixel rows each for BCn, one for uncompressed formats.
    pub fn block_rows(&self, id: SubresourceId) -> Result<u32, Error> {
        let surf = self.surface(id)?;
        let bh = block_height(self.decode_content()?);
        Ok(surf.height.div_ceil(bh))
    }
}

/// Pixel rows per block row: 4 for BCn, 1 otherwise.
fn block_height(kind: DecodeContent) -> u32 {
    match kind {
        DecodeContent::Rgba8 | DecodeContent::Bgra8 => 1,
        _ => 4,
    }
}

/// Payload bytes for one block row of `width` pixels.
fn block_row_bytes(kind: DecodeContent, width: u32) -> Result<usize, Error> {
    let blocks_x = match kind {
        DecodeContent::Rgba8 | DecodeContent::Bgra8 => width as usize,
        _ => (width as usize).div_ceil(4),
    };
    let per = match kind {
        DecodeContent::Bc1 | DecodeContent::Bc4UNorm | DecodeContent::Bc4SNorm => 8,
        DecodeContent::Rgba8 | DecodeContent::Bgra8 => 4,
        _ => 16,
    };
    blocks_x.checked_mul(per).ok_or(Error::OutOfBounds)
}

pub(crate) fn decode_surface_pixels(
    kind: DecodeContent,
    data: &[u8],
    width: u32,
    height: u32,
    depth: u32,
) -> Result<Vec<u8>, Error> {
    if depth == 0 {
        return Err(Error::InvalidField("zero depth".into()));
    }
    if depth == 1 {
        return decode_slice(kind, data, width, height);
    }
    let slice_bytes = slice_payload_bytes(kind, width, height)?;
    let need = slice_bytes
        .checked_mul(depth as usize)
        .ok_or(Error::OutOfBounds)?;
    if data.len() < need {
        return Err(Error::TruncatedData);
    }
    let mut out = Vec::with_capacity(
        (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(depth as usize)
            .saturating_mul(4),
    );
    for z in 0..depth as usize {
        let start = z * slice_bytes;
        out.extend(decode_slice(
            kind,
            &data[start..start + slice_bytes],
            width,
            height,
        )?);
    }
    Ok(out)
}

/// Decode one 2D slice into caller memory. `out` must be exactly
/// `width * height * 4` bytes.
pub(crate) fn decode_slice_into(
    kind: DecodeContent,
    data: &[u8],
    width: u32,
    height: u32,
    out: &mut [u8],
) -> Result<(), Error> {
    match kind {
        DecodeContent::Bc1 => bcn::decode_bc1_into(data, width, height, out),
        DecodeContent::Bc2 => bcn::decode_bc2_into(data, width, height, out),
        DecodeContent::Bc3 => bcn::decode_bc3_into(data, width, height, out),
        DecodeContent::Bc4UNorm => bcn::decode_bc4_into(data, width, height, false, out),
        DecodeContent::Bc4SNorm => bcn::decode_bc4_into(data, width, height, true, out),
        DecodeContent::Bc5UNorm => bcn::decode_bc5_into(data, width, height, false, out),
        DecodeContent::Bc5SNorm => bcn::decode_bc5_into(data, width, height, true, out),
        DecodeContent::Bc7 => bcn::decode_bc7_into(data, width, height, out),
        DecodeContent::Rgba8 => uncompressed::decode_rgba8_into(data, width, height, out),
        DecodeContent::Bgra8 => uncompressed::decode_bgra8_into(data, width, height, out),
    }
}

fn decode_slice(
    kind: DecodeContent,
    data: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<u8>, Error> {
    match kind {
        DecodeContent::Bc1 => bcn::decode_bc1(data, width, height),
        DecodeContent::Bc2 => bcn::decode_bc2(data, width, height),
        DecodeContent::Bc3 => bcn::decode_bc3(data, width, height),
        DecodeContent::Bc4UNorm => bcn::decode_bc4(data, width, height, false),
        DecodeContent::Bc4SNorm => bcn::decode_bc4(data, width, height, true),
        DecodeContent::Bc5UNorm => bcn::decode_bc5(data, width, height, false),
        DecodeContent::Bc5SNorm => bcn::decode_bc5(data, width, height, true),
        DecodeContent::Bc7 => bcn::decode_bc7(data, width, height),
        DecodeContent::Rgba8 => uncompressed::decode_rgba8(data, width, height),
        DecodeContent::Bgra8 => uncompressed::decode_bgra8(data, width, height),
    }
}
