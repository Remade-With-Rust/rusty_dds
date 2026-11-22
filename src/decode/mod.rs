//! CPU decode of DDS subresources to tightly packed RGBA8.
//!
//! **sRGB policy:** channel bytes are returned as stored. Formats tagged `_sRGB`
//! are **not** converted to linear — callers that need linear light must convert.
//!
//! Block compression (BC1–BC5, BC7) uses the pure-Rust [`bcdec_rs`] crate (MIT),
//! the same decompression core as `image_dds`. Uncompressed layouts are handled
//! in-house.

mod bc6h;
pub(crate) mod float;
pub(crate) mod bcn;
#[cfg(all(feature = "simd", any(target_arch = "x86_64", target_arch = "aarch64")))]
mod interp_pack;
#[cfg(all(feature = "simd", target_arch = "aarch64"))]
pub(crate) mod neon;
#[cfg(all(feature = "simd", target_arch = "x86_64"))]
pub(crate) mod simd;
mod uncompressed;
pub mod reference;

use crate::content::{
    slice_payload_bytes, DecodeContent, HdrDecodeContent, ImageRgba8, ImageRgbaF32,
};
use float::{F32Kind, PlainFormat};
pub use float::{DecodeOptions, SingleChannel};
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
    /// *not* replicate for BC5, so only BC4 is affected. To get grey without a
    /// post-pass, call [`DdsBase::decode_rgba8_with`] with
    /// [`SingleChannel::Grey`].
    pub fn decode_rgba8(&self, id: SubresourceId) -> Result<ImageRgba8, Error> {
        #[cfg(feature = "astc")]
        if let Ok(c) = self.astc_content() {
            let mut pixels = Vec::new();
            let (width, height, depth) = self.astc_decode_rgba8_into(id, c, DecodeOptions::new(), &mut pixels)?;
            return Ok(ImageRgba8 {
                width,
                height,
                depth,
                pixels,
            });
        }
        let surf = self.surface(id)?;
        let (kind, opaque) = self.rgba8_content()?;
        let mut pixels =
            decode_surface_pixels(kind, surf.data, surf.width, surf.height, surf.depth)?;
        if opaque {
            force_opaque(&mut pixels);
        }
        Ok(ImageRgba8 {
            width: surf.width,
            height: surf.height,
            depth: surf.depth,
            pixels,
        })
    }

    /// [`DdsBase::decode_rgba8`] with [`DecodeOptions`] — today, the choice of
    /// `(R, 0, 0, 255)` or grey `(R, R, R, 255)` for BC4.
    ///
    /// ```
    /// # use rusty_dds::{Dds, DdsView, SubresourceId, DecodeOptions, SingleChannel};
    /// # let mut bytes = Vec::new();
    /// # Dds::new_dxgi(rusty_dds::NewDxgiParams {
    /// #     height: 8, width: 8, depth: None,
    /// #     format: rusty_dds::DxgiFormat::BC4_UNorm,
    /// #     mipmap_levels: None, array_layers: None, caps2: None, is_cubemap: false,
    /// #     resource_dimension: rusty_dds::D3D10ResourceDimension::Texture2D,
    /// #     alpha_mode: rusty_dds::AlphaMode::Straight,
    /// # })?.write(&mut bytes)?;
    /// let dds = DdsView::parse(&bytes)?;
    /// let grey = DecodeOptions::new().single_channel(SingleChannel::Grey);
    /// let img = dds.decode_rgba8_with(SubresourceId::mip_layer(0, 0), grey)?;
    /// let [r, g, b, _] = img.pixel(0, 0).unwrap();
    /// assert!(r == g && g == b);
    /// # Ok::<(), rusty_dds::Error>(())
    /// ```
    pub fn decode_rgba8_with(
        &self,
        id: SubresourceId,
        opts: DecodeOptions,
    ) -> Result<ImageRgba8, Error> {
        #[cfg(feature = "astc")]
        if let Ok(c) = self.astc_content() {
            let mut pixels = Vec::new();
            let (width, height, depth) = self.astc_decode_rgba8_into(id, c, opts, &mut pixels)?;
            return Ok(ImageRgba8 {
                width,
                height,
                depth,
                pixels,
            });
        }
        let mut img = self.decode_rgba8(id)?;
        apply_grey_u8(self.rgba8_content()?.0, opts, &mut img.pixels);
        Ok(img)
    }

    /// [`DdsBase::decode_rgba8_into`] with [`DecodeOptions`].
    pub fn decode_rgba8_into_with(
        &self,
        id: SubresourceId,
        dst: &mut Vec<u8>,
        opts: DecodeOptions,
    ) -> Result<(u32, u32, u32), Error> {
        #[cfg(feature = "astc")]
        if let Ok(c) = self.astc_content() {
            return self.astc_decode_rgba8_into(id, c, opts, dst);
        }
        let dims = self.decode_rgba8_into(id, dst)?;
        apply_grey_u8(self.rgba8_content()?.0, opts, dst);
        Ok(dims)
    }
}

/// Alpha to 255 for a layout whose fourth byte is padding (`X8R8G8B8` and
/// kin): the payload's X byte is unspecified, often 0, and must not read as
/// transparency.
fn force_opaque(px: &mut [u8]) {
    for p in px.chunks_exact_mut(4) {
        p[3] = 255;
    }
}

/// Replicate red into green and blue for single-channel LDR content.
fn apply_grey_u8(kind: DecodeContent, opts: DecodeOptions, px: &mut [u8]) {
    if opts.grey() && matches!(kind, DecodeContent::Bc4UNorm | DecodeContent::Bc4SNorm) {
        for p in px.chunks_exact_mut(4) {
            p[1] = p[0];
            p[2] = p[0];
        }
    }
}

impl<D: AsRef<[u8]>> DdsBase<D> {
    /// Decode one subresource to tightly packed RGBA `f32`.
    ///
    /// Every format this crate decodes is accepted:
    ///
    /// * **BC6H** — HDR, unclamped, `A = 1.0`.
    /// * **Uncompressed float and wide formats** — `R32G32B32A32_FLOAT`,
    ///   `R16G16B16A16_FLOAT`, `R32_FLOAT`, `R16_UNORM`, `R16G16_UNORM`,
    ///   `R10G10B10A2_UNORM`, `R11G11B10_FLOAT`, `R9G9B9E5_SHAREDEXP`,
    ///   `B5G6R5_UNORM`, `R8_UNORM` and the rest of the 8/16/32-bit
    ///   UNORM/SNORM/FLOAT family, plus the legacy D3D9 equivalents
    ///   (`A16B16G16R16F`, `L16`, `R5G6B5`, …).
    /// * **BC4 and BC5** — decoded from the endpoints in `f32`, keeping the
    ///   interpolated levels that [`DdsBase::decode_rgba8`] rounds to 8 bits.
    /// * **BC1, BC2, BC3, BC7, RGBA8, BGRA8** — defined at 8 bits, widened as an
    ///   exact `x / 255`.
    ///
    /// Floats are unclamped; UNORM is `0.0..=1.0`; SNORM is `-1.0..=1.0`.
    /// Missing channels read `0.0` and missing alpha `1.0`, the GPU sampling
    /// convention — use [`DdsBase::decode_rgba_f32_with`] and
    /// [`SingleChannel::Grey`](crate::SingleChannel::Grey) to expand BC4 and
    /// other one-channel formats to grey. Volumes decode every depth slice,
    /// stacked. sRGB-tagged formats are not linearised.
    pub fn decode_rgba_f32(&self, id: SubresourceId) -> Result<ImageRgbaF32, Error> {
        self.decode_rgba_f32_with(id, DecodeOptions::new())
    }

    /// [`DdsBase::decode_rgba_f32`] with [`DecodeOptions`].
    pub fn decode_rgba_f32_with(
        &self,
        id: SubresourceId,
        opts: DecodeOptions,
    ) -> Result<ImageRgbaF32, Error> {
        let mut pixels = Vec::new();
        let (width, height, depth) = self.decode_rgba_f32_into_with(id, &mut pixels, opts)?;
        Ok(ImageRgbaF32 {
            width,
            height,
            depth,
            pixels,
        })
    }

    /// Decode one subresource to RGBA `f32` **into a buffer you own**.
    ///
    /// The `f32` twin of [`DdsBase::decode_rgba8_into`], and it matters more here:
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
        self.decode_rgba_f32_into_with(id, dst, DecodeOptions::new())
    }

    /// [`DdsBase::decode_rgba_f32_into`] with [`DecodeOptions`].
    pub fn decode_rgba_f32_into_with(
        &self,
        id: SubresourceId,
        dst: &mut Vec<f32>,
        opts: DecodeOptions,
    ) -> Result<(u32, u32, u32), Error> {
        #[cfg(feature = "astc")]
        if let Ok(c) = self.astc_content() {
            return self.astc_decode_rgba_f32_into(id, c, opts, dst);
        }
        let kind = self.f32_kind()?;
        let surf = self.surface(id)?;
        let (w, h, d) = (surf.width, surf.height, surf.depth.max(1));
        // Validate the source before sizing the destination: `w` and `h` are
        // header fields, and the output is 16 bytes a pixel.
        let slice_bytes = kind.slice_bytes(w, h)?;
        let need = slice_bytes.checked_mul(d as usize).ok_or(Error::OutOfBounds)?;
        if surf.data.len() < need {
            return Err(Error::TruncatedData);
        }

        let out_slice = (w as usize)
            .checked_mul(h as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or(Error::OutOfBounds)?;
        let total = out_slice.checked_mul(d as usize).ok_or(Error::OutOfBounds)?;
        // A correctly sized buffer costs nothing here — the whole point of
        // handing one back in. A buffer that must grow is replaced by a zeroed
        // allocation, which the OS hands over already zero; `resize` would
        // memset 16 bytes a pixel on top of the page faults.
        if dst.len() != total {
            if dst.capacity() < total {
                *dst = vec![0.0; total];
            } else {
                dst.clear();
                dst.resize(total, 0.0);
            }
        }

        for z in 0..d as usize {
            let src = &surf.data[z * slice_bytes..(z + 1) * slice_bytes];
            let dst_z = dst
                .get_mut(z * out_slice..(z + 1) * out_slice)
                .ok_or(Error::OutOfBounds)?;
            decode_rows_f32(kind, src, w, h, opts.grey(), true, dst_z)?;
        }
        Ok((w, h, d))
    }

    /// Decode a **range of block rows** of one subresource to RGBA `f32` in
    /// caller memory — the seam your job system splits on.
    ///
    /// BC6H is the most expensive decode this crate ships: at 1024^2 it is
    /// ~21 ms serial, against 2.6 ms for BC7 split across a caller's threads.
    /// Unlike BC7 it has no internal thread pool to fall back on, and it should
    /// not grow one — a texture library that seizes cores inside a frame is a
    /// library an engine has to work around. This hands you the split instead.
    ///
    /// `rows` is in block rows (4 pixel rows each for block-compressed formats,
    /// 1 for uncompressed); `dst` receives only those rows, tightly packed at
    /// `width * 4` floats per pixel row. Use [`DdsBase::block_rows_f32`] for the
    /// count.
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
        #[cfg(feature = "astc")]
        if let Ok(c) = self.astc_content() {
            return self.astc_decode_block_rows_f32_into(id, c, rows, dst);
        }
        let kind = self.f32_kind()?;
        let surf = self.surface(id)?;
        if surf.depth.max(1) != 1 {
            // Volumes want a slice index alongside the row range; no caller has
            // needed it yet, and guessing the layout would be worse than failing.
            return Err(Error::UnsupportedFormat);
        }
        let bh = kind.block_height();
        let total = surf.height.div_ceil(bh);
        if rows.start > rows.end || rows.end > total {
            return Err(Error::OutOfBounds);
        }
        let y0 = rows.start * bh;
        let y1 = (rows.end * bh).min(surf.height);
        if y0 >= y1 {
            return Ok(());
        }

        let row_pitch = kind.row_bytes(surf.width)?;
        let src = surf
            .data
            .get(rows.start as usize * row_pitch..rows.end as usize * row_pitch)
            .ok_or(Error::TruncatedData)?;
        let need = (surf.width as usize)
            .checked_mul((y1 - y0) as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or(Error::OutOfBounds)?;
        if dst.len() != need {
            return Err(Error::OutOfBounds);
        }
        // The caller is splitting the work across its own threads: never
        // spawn more underneath it.
        decode_rows_f32(kind, src, surf.width, y1 - y0, false, false, dst)
    }

    /// Block rows in one subresource — the unit
    /// [`DdsBase::decode_block_rows_f32_into`] splits on. Four pixel rows for
    /// block-compressed formats, one for uncompressed.
    pub fn block_rows_f32(&self, id: SubresourceId) -> Result<u32, Error> {
        #[cfg(feature = "astc")]
        if let Ok(c) = self.astc_content() {
            return self.astc_block_rows(id, c);
        }
        let kind = self.f32_kind()?;
        Ok(self.surface(id)?.height.div_ceil(kind.block_height()))
    }

    /// Classify for the `f32` path: BC6H first, then the LDR matrix, then the
    /// uncompressed float and wide layouts.
    pub(crate) fn f32_kind(&self) -> Result<F32Kind, Error> {
        if let Ok(k) = self.hdr_decode_content() {
            return Ok(F32Kind::Bc6h {
                signed: k == HdrDecodeContent::Bc6hSf16,
            });
        }
        if let Ok(k) = self.decode_content() {
            return Ok(F32Kind::Ldr(k));
        }
        if let Some(p) = self.get_dxgi_format().and_then(PlainFormat::from_dxgi) {
            return Ok(F32Kind::Plain(p));
        }
        if let Some(p) = self.get_d3d_format().and_then(PlainFormat::from_d3d) {
            return Ok(F32Kind::Plain(p));
        }
        Err(Error::UnsupportedFormat)
    }
}

/// Decode `height` pixel rows (whole block rows, the last possibly partial) of
/// one slice into `dst`, exactly `width * height * 4` floats.
fn decode_rows_f32(
    kind: F32Kind,
    src: &[u8],
    width: u32,
    height: u32,
    grey: bool,
    threads: bool,
    dst: &mut [f32],
) -> Result<(), Error> {
    let w = width as usize;
    let need = w
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(Error::OutOfBounds)?;
    if dst.len() != need {
        return Err(Error::OutOfBounds);
    }
    match kind {
        F32Kind::Bc6h { signed } => bc6h::decode_bc6h_into(src, width, height, signed, dst),
        F32Kind::Ldr(DecodeContent::Bc4UNorm) => {
            float::decode_bc45_f32(src, width, height, false, false, grey, dst)
        }
        F32Kind::Ldr(DecodeContent::Bc4SNorm) => {
            float::decode_bc45_f32(src, width, height, true, false, grey, dst)
        }
        F32Kind::Ldr(DecodeContent::Bc5UNorm) => {
            float::decode_bc45_f32(src, width, height, false, true, grey, dst)
        }
        F32Kind::Ldr(DecodeContent::Bc5SNorm) => {
            float::decode_bc45_f32(src, width, height, true, true, grey, dst)
        }
        // Uncompressed 8-bit needs no 8-bit decode first: widen straight from
        // the payload instead of through a scratch band.
        F32Kind::Ldr(DecodeContent::Rgba8) => {
            decode_rows_f32(F32Kind::Plain(PlainFormat::Rgba8Un), src, width, height, grey, threads, dst)
        }
        F32Kind::Ldr(DecodeContent::Bgra8) => {
            decode_rows_f32(F32Kind::Plain(PlainFormat::Bgra8), src, width, height, grey, threads, dst)
        }
        F32Kind::Ldr(k) => decode_ldr_widen(k, src, width, height, threads, dst),
        F32Kind::Plain(p) => {
            let row = kind.row_bytes(width)?;
            let need_src = row.checked_mul(height as usize).ok_or(Error::OutOfBounds)?;
            if src.len() < need_src {
                return Err(Error::TruncatedData);
            }
            // Rows are tightly packed in the payload and in `dst`, so the whole
            // band is one call. Row-at-a-time measured 2.0x slower on
            // R32G32B32A32_FLOAT: a 16 KiB `memcpy` per row never reaches the
            // streaming-store path that a whole-surface copy does.
            float::convert_row(p, &src[..need_src], dst, grey);
            Ok(())
        }
    }
}

/// Formats defined at 8 bits (BC1/2/3/7, RGBA8, BGRA8): decode with the 8-bit
/// kernels a band at a time into a small scratch and widen with an exact table.
///
/// The band keeps the scratch cache-resident instead of materialising a whole
/// RGBA8 surface (64 MiB at 4096^2) beside the 256 MiB `f32` one. BC7 is the
/// one format slow enough to want threads; large BC7 surfaces split the bands
/// across cores, sized so no band crosses the 8-bit decoder's own parallel
/// threshold and spawns a second, nested pool.
fn decode_ldr_widen(
    kind: DecodeContent,
    src: &[u8],
    width: u32,
    height: u32,
    threads: bool,
    dst: &mut [f32],
) -> Result<(), Error> {
    let w = width as usize;
    let bh = block_height(kind) as usize;
    let row_src = block_row_bytes(kind, width)?;
    let rows_total = (height as usize).div_ceil(bh);
    let need_src = row_src.checked_mul(rows_total).ok_or(Error::OutOfBounds)?;
    if src.len() < need_src {
        return Err(Error::TruncatedData);
    }
    let src = &src[..need_src];

    let blocks_x = if bh == 1 { w } else { w.div_ceil(4) };
    // Block rows per band: ~16 for small widths, fewer for wide surfaces so a
    // BC7 band stays under `BC7_PARALLEL_MIN_BLOCKS`.
    let band_rows = (16usize)
        .min((bcn::BC7_PARALLEL_MIN_BLOCKS - 1) / blocks_x.max(1))
        .max(1)
        * if bh == 1 { 4 } else { 1 };

    // One serial worker over block rows [r0, r1) into its own pixel rows.
    let run = |r0: usize, r1: usize, out: &mut [f32]| -> Result<(), Error> {
        let mut scratch: Vec<u8> = Vec::new();
        let mut r = r0;
        let mut out_off = 0usize;
        while r < r1 {
            let r_end = (r + band_rows).min(r1);
            let py0 = r * bh;
            let py1 = (r_end * bh).min(height as usize);
            let px_rows = py1 - py0;
            let n = w * px_rows * 4;
            scratch.resize(n, 0);
            decode_slice_into(
                kind,
                &src[r * row_src..r_end * row_src],
                width,
                px_rows as u32,
                &mut scratch[..n],
            )?;
            float::widen_u8(&scratch[..n], &mut out[out_off..out_off + n]);
            out_off += n;
            r = r_end;
        }
        Ok(())
    };

    let parallel = threads
        && kind == DecodeContent::Bc7
        && rows_total >= 2
        && blocks_x.saturating_mul(rows_total) >= bcn::BC7_PARALLEL_MIN_BLOCKS;
    if !parallel {
        return run(0, rows_total, dst);
    }

    static CORES: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let cores = *CORES.get_or_init(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    });
    let workers = cores.clamp(1, rows_total);
    let base = rows_total / workers;
    let extra = rows_total % workers;
    std::thread::scope(|s| {
        let mut rest: &mut [f32] = dst;
        let mut start = 0usize;
        let mut handles = Vec::with_capacity(workers);
        for k in 0..workers {
            let len = base + usize::from(k < extra);
            let (r0, r1) = (start, start + len);
            start = r1;
            let px_rows = (r1 * bh).min(height as usize) - r0 * bh;
            let (band, tail) = rest.split_at_mut(w * px_rows * 4);
            rest = tail;
            let run = &run;
            handles.push(s.spawn(move || run(r0, r1, band)));
        }
        for h in handles {
            h.join().map_err(|_| Error::OutOfBounds)??;
        }
        Ok(())
    })
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
        #[cfg(feature = "astc")]
        if let Ok(c) = self.astc_content() {
            return self.astc_decode_rgba8_into(id, c, DecodeOptions::new(), dst);
        }
        let (kind, opaque) = self.rgba8_content()?;
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
            decode_slice_into(kind, surf.data, w, h, dst)?;
        } else {
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
        }
        if opaque {
            force_opaque(dst);
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
        #[cfg(feature = "astc")]
        if let Ok(c) = self.astc_content() {
            return self.astc_decode_block_rows_into(id, c, rows, dst);
        }
        let surf = self.surface(id)?;
        let (kind, opaque) = self.rgba8_content()?;
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

        decode_slice_into(kind, src, surf.width, y1 - y0, dst)?;
        if opaque {
            force_opaque(dst);
        }
        Ok(())
    }

    /// Block rows in one subresource — the unit [`DdsBase::decode_block_rows_into`]
    /// splits on. Four pixel rows each for BCn, one for uncompressed formats.
    pub fn block_rows(&self, id: SubresourceId) -> Result<u32, Error> {
        #[cfg(feature = "astc")]
        if let Ok(c) = self.astc_content() {
            return self.astc_block_rows(id, c);
        }
        let surf = self.surface(id)?;
        let bh = block_height(self.rgba8_content()?.0);
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
