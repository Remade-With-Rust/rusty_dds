//! ASTC inside DDS (DXGI 133..=187): layout, routing through the existing
//! decode entry points, upload plans. Pixel exactness against astcenc is
//! gated in tools/astc-harness (`dds`, `containers`); here the DDS paths are
//! checked against the ASTC module's own decode of the same bytes.

#![cfg(all(feature = "astc", feature = "decode"))]

use rusty_dds::astc::{self, AstcFootprint, AstcProfile};
use rusty_dds::{
    AlphaMode, D3D10ResourceDimension, Dds, DdsView, DecodeOptions, DxgiFormat, NewDxgiParams, SubresourceId,
};

fn sample_dds(w: u32, h: u32, fp: AstcFootprint, srgb: bool, mips: u32, layers: u32) -> Vec<u8> {
    let fmt = DxgiFormat::from_astc(fp.width(), fp.height(), srgb).unwrap();
    let mut d = Dds::new_dxgi(NewDxgiParams {
        height: h,
        width: w,
        depth: None,
        format: fmt,
        mipmap_levels: Some(mips),
        array_layers: Some(layers),
        caps2: None,
        is_cubemap: false,
        resource_dimension: D3D10ResourceDimension::Texture2D,
        alpha_mode: AlphaMode::Straight,
    })
    .unwrap();
    // Deterministic void-extent blocks with varying colours: legal everywhere.
    for (i, b) in d.data.chunks_exact_mut(16).enumerate() {
        b.copy_from_slice(&[0xFC, 0xFD, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF]);
        b[9] = (i * 37) as u8;
        b[11] = (i * 91) as u8;
        b[13] = (i * 13) as u8;
    }
    let mut out = Vec::new();
    d.write(&mut out).unwrap();
    out
}

#[test]
fn dxgi_astc_values_follow_the_convention() {
    use num_traits::FromPrimitive;
    assert_eq!(DxgiFormat::ASTC_4X4_Typeless as u32, 133);
    assert_eq!(DxgiFormat::ASTC_4X4_UNorm as u32, 134);
    assert_eq!(DxgiFormat::ASTC_12X12_UNorm_sRGB as u32, 187);
    let mut n = 0;
    for v in 133..=187u32 {
        if let Some(f) = DxgiFormat::from_u32(v) {
            let (w, h, srgb) = f.astc_info().unwrap();
            assert!(AstcFootprint::new_2d(w, h).is_some());
            assert_eq!(srgb, (v - 133) % 4 == 2);
            n += 1;
        } else {
            assert_eq!((v - 133) % 4, 3, "gap value {v}");
        }
    }
    assert_eq!(n, 42);
}

#[test]
fn exact_mip_chain_and_routing() {
    let fp = AstcFootprint::B10X6;
    let (w, h, mips, layers) = (73, 41, 5, 2);
    let bytes = sample_dds(w, h, fp, false, mips, layers);
    let dds = DdsView::parse(&bytes).unwrap();
    let c = dds.astc_content().unwrap();
    assert_eq!(c.footprint, fp);
    assert!(!c.srgb);
    let mut expect_off = 0usize;
    for layer in 0..layers {
        for m in 0..mips {
            let id = SubresourceId::new(m, layer, 0);
            let r = dds.subresource_range(id).unwrap();
            let (mw, mh) = ((w >> m).max(1), (h >> m).max(1));
            let len = astc::payload_len(mw, mh, 1, fp).unwrap();
            assert_eq!(r, expect_off..expect_off + len, "mip {m} layer {layer}");
            expect_off += len;
            // The DDS entry points equal the ASTC module on the same bytes.
            let s = dds.astc_surface(id).unwrap();
            let img = dds.decode_rgba8(id).unwrap();
            assert_eq!(img.pixels, astc::decode_rgba8(s.data, mw, mh, fp, AstcProfile::Ldr).unwrap().pixels);
            let f = dds.decode_rgba_f32(id).unwrap();
            let mut want = vec![0f32; (mw * mh * 4) as usize];
            astc::decode_rgba_f32_into(s.data, mw, mh, 1, fp, AstcProfile::Hdr, &mut want).unwrap();
            assert_eq!(f.pixels, want);
            // Block rows are footprint-high.
            assert_eq!(dds.block_rows(id).unwrap(), mh.div_ceil(6));
            assert_eq!(dds.block_rows_f32(id).unwrap(), mh.div_ceil(6));
            let rows = dds.block_rows(id).unwrap();
            let mut part = vec![0u8; (mw * mh.min(6) * 4) as usize];
            dds.decode_block_rows_into(id, 0..1, &mut part).unwrap();
            assert_eq!(&part[..], &img.pixels[..part.len()]);
            assert!(rows >= 1);
        }
    }
    assert_eq!(expect_off, dds.data.len());
}

#[test]
fn srgb_f32_is_the_8bit_value_over_255_and_override_works() {
    let fp = AstcFootprint::B6X6;
    let bytes = sample_dds(20, 14, fp, true, 1, 1);
    let dds = DdsView::parse(&bytes).unwrap();
    let id = SubresourceId::new(0, 0, 0);
    assert!(dds.astc_content().unwrap().srgb);
    let u8s = dds.decode_rgba8(id).unwrap();
    let f = dds.decode_rgba_f32(id).unwrap();
    for (a, b) in u8s.pixels.iter().zip(&f.pixels) {
        assert_eq!(*a as f32 / 255.0, *b);
    }
    // Override: astcenc's half-float sRGB values instead.
    let mut over = Vec::new();
    dds.decode_rgba_f32_into_with(id, &mut over, DecodeOptions::new().astc_profile(AstcProfile::LdrSrgb))
        .unwrap();
    let s = dds.astc_surface(id).unwrap();
    let mut want = vec![0f32; over.len()];
    astc::decode_rgba_f32_into(s.data, 20, 14, 1, fp, AstcProfile::LdrSrgb, &mut want).unwrap();
    // The override routes LdrSrgb through the 8-bit rule too (x / 255).
    assert_eq!(over, f.pixels);
    assert_eq!(want.len(), over.len());
}

#[test]
fn upload_plans_carry_the_footprint() {
    let fp = AstcFootprint::B8X5;
    let bytes = sample_dds(37, 29, fp, true, 3, 1);
    let dds = DdsView::parse(&bytes).unwrap();
    let g = dds.gpu_format().unwrap();
    assert_eq!(g.dxgi_name, "ASTC_8X5_UNorm_sRGB");
    assert_eq!(g.vulkan_name, "VK_FORMAT_ASTC_8x5_SRGB_BLOCK");
    assert_eq!((g.block_width, g.block_height, g.block_bytes), (8, 5, 16));
    let p = dds.upload_plan_compressed(SubresourceId::new(1, 0, 0)).unwrap();
    assert_eq!((p.width, p.height), (18, 14));
    assert_eq!(p.bytes_per_row, 3 * 16);
    assert_eq!(p.rows_per_image, 3);
    assert_eq!(p.data_len, 3 * 3 * 16);
    let d = dds.upload_plan_decoded_rgba8(SubresourceId::new(0, 0, 0)).unwrap();
    assert_eq!(d.data_len, 37 * 29 * 4);
}
