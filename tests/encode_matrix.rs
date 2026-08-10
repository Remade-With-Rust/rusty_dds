//! Phase 4: encode matrix — same C-* content × X-* contexts as decode.
//!
//! Gate: encode → decode round-trip.
//! - RGBA8 / BGRA8: bit-exact on all channels
//! - BC4: R only (G=B=0,A=255 after decode)
//! - BC5: R+G only (B=0,A=255)
//! - BC1–3 / BC7: full RGBA PSNR floor

use rusty_dds::*;

fn fill_for_content(
    content: DecodeContent,
    width: u32,
    height: u32,
    depth: u32,
    layers: u32,
) -> Vec<u8> {
    let mut v = Vec::with_capacity((width * height * depth * layers * 4) as usize);
    for layer in 0..layers {
        for z in 0..depth {
            for y in 0..height {
                for x in 0..width {
                    let px = match content {
                        DecodeContent::Bc4UNorm | DecodeContent::Bc4SNorm => {
                            // Prefer values that stay in snorm-friendly i8 range when cast.
                            let r = ((x * 200 + y * 17 + z * 3 + layer) % 200) as u8;
                            [r, 0, 0, 255]
                        }
                        DecodeContent::Bc5UNorm | DecodeContent::Bc5SNorm => {
                            let r = ((x * 200) / width.max(1)).min(200) as u8;
                            let g = ((y * 200) / height.max(1)).min(200) as u8;
                            [r, g, 0, 255]
                        }
                        DecodeContent::Bc1 => {
                            // Opaque RGB (BC1 alpha is punch-through only).
                            let r = ((x * 255) / width.max(1)) as u8;
                            let g = ((y * 255) / height.max(1)) as u8;
                            let b = ((z * 40 + layer * 17) % 256) as u8;
                            [r, g, b, 255]
                        }
                        _ => {
                            let r = ((x * 255) / width.max(1)) as u8;
                            let g = ((y * 255) / height.max(1)) as u8;
                            let b = ((z * 40 + layer * 17) % 256) as u8;
                            let a = 200u8.wrapping_add((x + y) as u8);
                            [r, g, b, a]
                        }
                    };
                    v.extend_from_slice(&px);
                }
            }
        }
    }
    v
}

fn solid_rgba(
    width: u32,
    height: u32,
    depth: u32,
    layers: u32,
    color: [u8; 4],
) -> Vec<u8> {
    let n = (width * height * depth * layers) as usize;
    let mut v = Vec::with_capacity(n * 4);
    for _ in 0..n {
        v.extend_from_slice(&color);
    }
    v
}

fn channels_for(content: DecodeContent) -> &'static [usize] {
    match content {
        DecodeContent::Bc4UNorm | DecodeContent::Bc4SNorm => &[0],
        DecodeContent::Bc5UNorm | DecodeContent::Bc5SNorm => &[0, 1],
        DecodeContent::Bc1 => &[0, 1, 2],
        _ => &[0, 1, 2, 3],
    }
}

fn psnr_channels(a: &[u8], b: &[u8], channels: &[usize]) -> Option<f64> {
    if a.len() != b.len() || a.len() % 4 != 0 {
        return None;
    }
    let mut sse = 0.0f64;
    let mut n = 0usize;
    for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        for &c in channels {
            let d = pa[c] as f64 - pb[c] as f64;
            sse += d * d;
            n += 1;
        }
    }
    if n == 0 {
        return None;
    }
    if sse == 0.0 {
        return Some(f64::INFINITY);
    }
    let mse = sse / n as f64;
    Some(10.0 * (255.0f64 * 255.0 / mse).log10())
}

fn snorm_bits_rgba_to_unorm(px: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(px.len());
    for c in px.chunks_exact(4) {
        out.push(snorm_u8_bits_to_unorm(c[0]));
        out.push(snorm_u8_bits_to_unorm(c[1]));
        out.push(c[2]);
        out.push(c[3]);
    }
    out
}

fn snorm_u8_bits_to_unorm(b: u8) -> u8 {
    let s = (b as i8 as f32 / 127.0).clamp(-1.0, 1.0);
    ((s * 0.5 + 0.5) * 255.0).round().clamp(0.0, 255.0) as u8
}

fn assert_roundtrip(layout: EncodeLayout, pixels: &[u8], min_psnr: f64) {
    let dds = Dds::encode_from_rgba8(pixels, layout).unwrap_or_else(|e| {
        panic!("encode {}: {e}", layout.content.name())
    });
    assert_eq!(dds.decode_content().unwrap(), layout.content);

    let layers = dds.subresource_layer_count();
    let faces = dds.subresource_face_count();
    let mips = dds.get_num_mipmap_levels();
    let ch = channels_for(layout.content);
    let layer_stride = (layout.width * layout.height * layout.depth * 4) as usize;

    for layer in 0..layers {
        for face_i in 0..faces {
            let face = if dds.is_cubemap() {
                CubemapFace::from_index(face_i).unwrap()
            } else {
                CubemapFace::PositiveX
            };
            let id = if dds.is_cubemap() {
                SubresourceId::cubemap(0, layer, face)
            } else {
                SubresourceId::mip_layer(0, layer)
            };
            let img = dds.decode_rgba8(id).expect("decode");
            assert_eq!(img.width, layout.width);
            assert_eq!(img.height, layout.height);
            assert_eq!(img.depth, layout.depth);

            let phys = if layout.is_cubemap {
                layer * 6 + face_i
            } else {
                layer
            };
            let src = &pixels[phys as usize * layer_stride..(phys as usize + 1) * layer_stride];

            match layout.content {
                DecodeContent::Rgba8 | DecodeContent::Bgra8 => {
                    assert_eq!(img.pixels, src, "{} bit-exact", layout.content.name());
                }
                DecodeContent::Bc4SNorm | DecodeContent::Bc5SNorm => {
                    // encode_from_rgba8 treats input as UNORM (DX Compress style);
                    // bcdec returns SNORM i8 bit patterns — map back to UNORM for PSNR.
                    let decoded = snorm_bits_rgba_to_unorm(&img.pixels);
                    let psnr = psnr_channels(&decoded, src, ch).unwrap();
                    assert!(
                        psnr >= min_psnr,
                        "{} {:?} PSNR {psnr:.2} < {min_psnr} (max_abs={:?})",
                        layout.content.name(),
                        id,
                        max_abs_diff(&decoded, src)
                    );
                }
                _ => {
                    let psnr = psnr_channels(&img.pixels, src, ch).unwrap();
                    assert!(
                        psnr >= min_psnr,
                        "{} {:?} PSNR {psnr:.2} < {min_psnr} (max_abs={:?})",
                        layout.content.name(),
                        id,
                        max_abs_diff(&img.pixels, src)
                    );
                }
            }

            if mips > 1 {
                let tip = if dds.is_cubemap() {
                    SubresourceId::cubemap(mips - 1, layer, face)
                } else {
                    SubresourceId::mip_layer(mips - 1, layer)
                };
                let tip_img = dds.decode_rgba8(tip).expect("mip tip");
                let expected_w = (layout.width >> (mips - 1)).max(1);
                let expected_h = (layout.height >> (mips - 1)).max(1);
                assert_eq!(tip_img.width, expected_w);
                assert_eq!(tip_img.height, expected_h);
            }
        }
    }
}

fn psnr_floor(content: DecodeContent) -> f64 {
    match content {
        DecodeContent::Rgba8 | DecodeContent::Bgra8 => f64::INFINITY,
        DecodeContent::Bc7 => 22.0,
        DecodeContent::Bc1 => 18.0,
        DecodeContent::Bc2 | DecodeContent::Bc3 => 18.0,
        DecodeContent::Bc4UNorm | DecodeContent::Bc4SNorm => 28.0,
        DecodeContent::Bc5UNorm | DecodeContent::Bc5SNorm => 28.0,
    }
}

#[test]
fn matrix_x2d_all_content() {
    for &content in DecodeContent::ALL_LDR {
        let layout = EncodeLayout::flat_2d(content, 32, 32);
        let px = fill_for_content(content, 32, 32, 1, 1);
        assert_roundtrip(layout, &px, psnr_floor(content));
    }
}

#[test]
fn matrix_xmip_all_content() {
    for &content in DecodeContent::ALL_LDR {
        let layout = EncodeLayout::flat_2d(content, 32, 32).with_mips(6);
        let px = fill_for_content(content, 32, 32, 1, 1);
        assert_roundtrip(layout, &px, psnr_floor(content));
    }
}

#[test]
fn matrix_xarray_all_content() {
    for &content in DecodeContent::ALL_LDR {
        let layout = EncodeLayout::flat_2d(content, 16, 16).with_array(3);
        let px = fill_for_content(content, 16, 16, 1, 3);
        assert_roundtrip(layout, &px, psnr_floor(content));
    }
}

#[test]
fn matrix_xcube_selected_content() {
    for &content in &[
        DecodeContent::Bc1,
        DecodeContent::Bc3,
        DecodeContent::Bc7,
        DecodeContent::Rgba8,
    ] {
        let layout = EncodeLayout::flat_2d(content, 16, 16)
            .with_mips(2)
            .with_array(6)
            .cubemap();
        let px = fill_for_content(content, 16, 16, 1, 6);
        assert_roundtrip(layout, &px, psnr_floor(content));
    }
}

#[test]
fn matrix_xnpot_all_block_formats() {
    for &content in &[
        DecodeContent::Bc1,
        DecodeContent::Bc2,
        DecodeContent::Bc3,
        DecodeContent::Bc4UNorm,
        DecodeContent::Bc4SNorm,
        DecodeContent::Bc5UNorm,
        DecodeContent::Bc5SNorm,
        DecodeContent::Bc7,
    ] {
        let layout = EncodeLayout::flat_2d(content, 2, 3);
        let color = match content {
            DecodeContent::Bc4UNorm | DecodeContent::Bc4SNorm => [200, 0, 0, 255],
            DecodeContent::Bc5UNorm | DecodeContent::Bc5SNorm => [200, 40, 0, 255],
            DecodeContent::Bc1 => [200, 40, 90, 255],
            _ => [200, 40, 90, 255],
        };
        let px = solid_rgba(2, 3, 1, 1, color);
        assert_roundtrip(layout, &px, psnr_floor(content));
    }
}

#[test]
fn matrix_xvol_all_content() {
    for &content in DecodeContent::ALL_LDR {
        let layout = EncodeLayout::flat_2d(content, 8, 8).with_depth(4);
        let px = fill_for_content(content, 8, 8, 4, 1);
        assert_roundtrip(layout, &px, psnr_floor(content));
    }
}

#[test]
fn solid_color_bc7_high_psnr() {
    let layout = EncodeLayout::flat_2d(DecodeContent::Bc7, 16, 16);
    let px = solid_rgba(16, 16, 1, 1, [10, 20, 30, 240]);
    let dds = Dds::encode_from_rgba8(&px, layout).unwrap();
    let img = dds
        .decode_rgba8(SubresourceId::mip_layer(0, 0))
        .unwrap();
    let psnr = psnr_rgba8(&img.pixels, &px).unwrap();
    assert!(psnr > 40.0, "solid BC7 PSNR {psnr}");
}

#[test]
fn image_rgba8_encode_helper() {
    let img = ImageRgba8 {
        width: 8,
        height: 8,
        depth: 1,
        pixels: fill_for_content(DecodeContent::Bc3, 8, 8, 1, 1),
    };
    let dds = img.encode_dds(DecodeContent::Bc3).unwrap();
    let back = dds.decode_rgba8(SubresourceId::mip_layer(0, 0)).unwrap();
    assert!(psnr_channels(&back.pixels, &img.pixels, &[0, 1, 2, 3]).unwrap() >= 18.0);
}
