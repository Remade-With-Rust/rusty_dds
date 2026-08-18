//! Phase 2b: decode completeness matrix — every LDR content × applicable context.
//!
//! Oracle: [`rusty_dds::reference::reference_rgba8`] (direct `bcdec_rs` tiling).

use rusty_dds::*;
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn load(name: &str) -> Dds {
    let path = fixtures_dir().join(name);
    let bytes = fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "missing {}: {e}; run cargo run --example gen_fixtures",
            path.display()
        );
    });
    Dds::read(Cursor::new(bytes)).unwrap_or_else(|e| panic!("parse {name}: {e}"))
}

fn dxgi_for(content: DecodeContent) -> DxgiFormat {
    match content {
        DecodeContent::Bc1 => DxgiFormat::BC1_UNorm,
        DecodeContent::Bc2 => DxgiFormat::BC2_UNorm,
        DecodeContent::Bc3 => DxgiFormat::BC3_UNorm,
        DecodeContent::Bc4UNorm => DxgiFormat::BC4_UNorm,
        DecodeContent::Bc4SNorm => DxgiFormat::BC4_SNorm,
        DecodeContent::Bc5UNorm => DxgiFormat::BC5_UNorm,
        DecodeContent::Bc5SNorm => DxgiFormat::BC5_SNorm,
        DecodeContent::Bc7 => DxgiFormat::BC7_UNorm,
        DecodeContent::Rgba8 => DxgiFormat::R8G8B8A8_UNorm,
        DecodeContent::Bgra8 => DxgiFormat::B8G8R8A8_UNorm,
        // Exhaustive by intent: a new DecodeContent must be added to
        // this matrix, never silently skipped.
        other => panic!("unhandled DecodeContent: {other:?}"),
    }
}

fn fill_deterministic(data: &mut [u8], content: DecodeContent) {
    match content {
        DecodeContent::Bc1 => {
            let block = solid_red_bc1();
            for chunk in data.chunks_exact_mut(8) {
                chunk.copy_from_slice(&block);
            }
        }
        DecodeContent::Bc2 => {
            let block = solid_red_bc2();
            for chunk in data.chunks_exact_mut(16) {
                chunk.copy_from_slice(&block);
            }
        }
        DecodeContent::Bc3 => {
            let block = solid_red_bc3();
            for chunk in data.chunks_exact_mut(16) {
                chunk.copy_from_slice(&block);
            }
        }
        DecodeContent::Bc4UNorm | DecodeContent::Bc4SNorm => {
            let block = solid_bc4(content == DecodeContent::Bc4SNorm);
            for chunk in data.chunks_exact_mut(8) {
                chunk.copy_from_slice(&block);
            }
        }
        DecodeContent::Bc5UNorm | DecodeContent::Bc5SNorm => {
            let block = solid_bc5(content == DecodeContent::Bc5SNorm);
            for chunk in data.chunks_exact_mut(16) {
                chunk.copy_from_slice(&block);
            }
        }
        DecodeContent::Bc7 => {
            for (i, b) in data.iter_mut().enumerate() {
                *b = ((i * 37 + 11) % 251) as u8;
            }
        }
        DecodeContent::Rgba8 | DecodeContent::Bgra8 => {
            for (i, b) in data.iter_mut().enumerate() {
                *b = (i % 251) as u8;
            }
        }
        // Exhaustive by intent: a new DecodeContent must be added to
        // this matrix, never silently skipped.
        other => panic!("unhandled DecodeContent: {other:?}"),
    }
}

fn solid_red_bc1() -> [u8; 8] {
    let mut b = [0u8; 8];
    b[0..2].copy_from_slice(&0xF800u16.to_le_bytes());
    b
}

fn solid_red_bc2() -> [u8; 16] {
    let mut b = [0u8; 16];
    // Explicit alpha 0xF per nibble → 255
    for i in 0..8 {
        b[i] = 0xFF;
    }
    b[8..16].copy_from_slice(&solid_red_bc1());
    b
}

fn solid_red_bc3() -> [u8; 16] {
    let mut b = [0u8; 16];
    b[0] = 255;
    b[1] = 0;
    b[8..16].copy_from_slice(&solid_red_bc1());
    b
}

fn solid_bc4(signed: bool) -> [u8; 8] {
    let mut b = [0u8; 8];
    if signed {
        b[0] = 127u8; // i8 max
        b[1] = 0;
    } else {
        b[0] = 200;
        b[1] = 0;
    }
    b
}

fn solid_bc5(signed: bool) -> [u8; 16] {
    let mut b = [0u8; 16];
    let r = solid_bc4(signed);
    let g = solid_bc4(signed);
    b[..8].copy_from_slice(&r);
    b[8..].copy_from_slice(&g);
    b
}

fn make_dxgi(
    content: DecodeContent,
    width: u32,
    height: u32,
    depth: Option<u32>,
    mips: Option<u32>,
    array_layers: Option<u32>,
    is_cubemap: bool,
) -> Dds {
    let caps2 = if is_cubemap {
        Some(Caps2::CUBEMAP | Caps2::CUBEMAP_ALLFACES)
    } else {
        None
    };
    let mut dds = Dds::new_dxgi(NewDxgiParams {
        height,
        width,
        depth,
        format: dxgi_for(content),
        mipmap_levels: mips,
        array_layers,
        caps2,
        is_cubemap,
        resource_dimension: if depth.unwrap_or(1) > 1 {
            D3D10ResourceDimension::Texture3D
        } else {
            D3D10ResourceDimension::Texture2D
        },
        alpha_mode: AlphaMode::Straight,
    })
    .unwrap_or_else(|e| panic!("new_dxgi {}: {e}", content.name()));
    fill_deterministic(&mut dds.data, content);
    dds
}

fn assert_matches_oracle(dds: &Dds, id: SubresourceId) {
    let content = dds.decode_content().expect("content");
    let surf = dds.surface(id).expect("surface");
    let img = dds.decode_rgba8(id).expect("decode");
    let oracle = reference::reference_rgba8(
        content,
        surf.data,
        surf.width,
        surf.height,
        surf.depth,
    )
    .expect("oracle");
    assert_eq!(img.width, surf.width);
    assert_eq!(img.height, surf.height);
    assert_eq!(img.depth, surf.depth);
    assert_eq!(
        img.pixels, oracle,
        "oracle mismatch content={} id={:?}",
        content.name(),
        id
    );
}

#[test]
fn matrix_x2d_all_content() {
    for &content in DecodeContent::ALL_LDR {
        let dds = make_dxgi(content, 32, 32, None, Some(1), None, false);
        assert_matches_oracle(&dds, SubresourceId::mip_layer(0, 0));
    }
}

#[test]
fn matrix_xmip_all_bc_and_rgba() {
    for &content in &[
        DecodeContent::Bc1,
        DecodeContent::Bc2,
        DecodeContent::Bc3,
        DecodeContent::Bc4UNorm,
        DecodeContent::Bc5UNorm,
        DecodeContent::Bc7,
        DecodeContent::Rgba8,
        DecodeContent::Bgra8,
    ] {
        let dds = make_dxgi(content, 32, 32, None, Some(6), None, false);
        let last = dds.get_num_mipmap_levels() - 1;
        assert_matches_oracle(&dds, SubresourceId::mip_layer(0, 0));
        assert_matches_oracle(&dds, SubresourceId::mip_layer(last, 0));
    }
}

#[test]
fn matrix_xarray_all_content() {
    for &content in DecodeContent::ALL_LDR {
        let dds = make_dxgi(content, 16, 16, None, Some(1), Some(3), false);
        for layer in 0..3 {
            assert_matches_oracle(&dds, SubresourceId::mip_layer(0, layer));
        }
    }
}

#[test]
fn matrix_xcube_compressed_and_rgba() {
    for &content in &[
        DecodeContent::Bc1,
        DecodeContent::Bc3,
        DecodeContent::Bc7,
        DecodeContent::Rgba8,
    ] {
        let dds = make_dxgi(content, 16, 16, None, Some(2), Some(6), true);
        assert!(dds.is_cubemap());
        for face in CubemapFace::ALL {
            assert_matches_oracle(&dds, SubresourceId::cubemap(0, 0, face));
            assert_matches_oracle(&dds, SubresourceId::cubemap(1, 0, face));
        }
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
        let dds = make_dxgi(content, 2, 3, None, Some(1), None, false);
        assert_matches_oracle(&dds, SubresourceId::mip_layer(0, 0));
    }
}

#[test]
fn matrix_xvol_all_content() {
    for &content in DecodeContent::ALL_LDR {
        let dds = make_dxgi(content, 8, 8, Some(4), Some(1), None, false);
        let img = dds
            .decode_rgba8(SubresourceId::mip_layer(0, 0))
            .expect("vol decode");
        assert_eq!(img.depth, 4);
        assert_matches_oracle(&dds, SubresourceId::mip_layer(0, 0));
    }
}

#[test]
fn legacy_dxt_formats() {
    for (format, content) in [
        (D3DFormat::DXT1, DecodeContent::Bc1),
        (D3DFormat::DXT3, DecodeContent::Bc2),
        (D3DFormat::DXT5, DecodeContent::Bc3),
    ] {
        let mut dds = Dds::new_d3d(NewD3dParams {
            height: 8,
            width: 8,
            depth: None,
            format,
            mipmap_levels: Some(1),
            caps2: None,
        })
        .unwrap();
        fill_deterministic(&mut dds.data, content);
        assert_eq!(dds.decode_content().unwrap(), content);
        assert_matches_oracle(&dds, SubresourceId::mip_layer(0, 0));
    }
}

#[test]
fn srgb_tag_is_stored_bytes() {
    let mut dds = Dds::new_dxgi(NewDxgiParams {
        height: 4,
        width: 4,
        depth: None,
        format: DxgiFormat::R8G8B8A8_UNorm_sRGB,
        mipmap_levels: Some(1),
        array_layers: None,
        caps2: None,
        is_cubemap: false,
        resource_dimension: D3D10ResourceDimension::Texture2D,
        alpha_mode: AlphaMode::Straight,
    })
    .unwrap();
    dds.data[..4].copy_from_slice(&[10, 20, 30, 40]);
    let img = dds
        .decode_rgba8(SubresourceId::mip_layer(0, 0))
        .unwrap();
    assert_eq!(img.pixel(0, 0), Some([10, 20, 30, 40]));
}

#[test]
fn committed_fixtures_decode_where_applicable() {
    // Pattern-filled fixtures still must decode (oracle agrees on those bytes).
    for name in [
        "dxt1_64x64.dds",
        "dxt5_64x64.dds",
        "bc1_64x64_dx10.dds",
        "bc3_64x64_dx10.dds",
        "rgba8_64x64.dds",
        "rgba8_32x32_array3.dds",
        "bc1_32x32_cube.dds",
        "dxt1_64x64_mips.dds",
        "rgba8_256x256_mips.dds",
    ] {
        let dds = load(name);
        if dds.decode_content().is_err() {
            continue;
        }
        let mips = dds.get_num_mipmap_levels();
        let layers = dds.subresource_layer_count();
        let faces = dds.subresource_face_count();
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
                assert_matches_oracle(&dds, id);
                if mips > 1 {
                    let id_last = if dds.is_cubemap() {
                        SubresourceId::cubemap(mips - 1, layer, face)
                    } else {
                        SubresourceId::mip_layer(mips - 1, layer)
                    };
                    assert_matches_oracle(&dds, id_last);
                }
            }
        }
    }
}
