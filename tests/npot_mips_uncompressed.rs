//! Regression test for #24: uncompressed and packed mip chains must be sized
//! per level, `row_pitch(w_l) * h_l * d_l`, not as a quarter of the level
//! before. Companion to `npot_mips.rs`, which covers block compression (#12).

use dds::DataRegion;
use rusty_dds::{
    AlphaMode, D3D10ResourceDimension, D3DFormat, Dds, DdsView, DxgiFormat, NewD3dParams,
    NewDxgiParams, SubresourceId,
};
use std::io::Cursor;

/// NPOT, non-square power of two, 1-wide, and shapes that were always right.
const SIZES: &[(u32, u32)] = &[
    (100, 100),
    (13, 7),
    (5, 3),
    (257, 129),
    (1024, 64),
    (64, 1024),
    (1, 300),
    (255, 1),
    (1, 1),
    (64, 64),
    (128, 64),
];

fn full_levels(w: u32, h: u32, d: u32) -> u32 {
    32 - w.max(h).max(d).leading_zeros()
}

/// (format, bits per pixel). 4:2:2 `R8G8_B8G8` is sized by `row_bytes` instead.
fn formats() -> Vec<(DxgiFormat, u32)> {
    use DxgiFormat::*;
    vec![
        (R32G32B32A32_Float, 128),
        (R32G32B32_Float, 96),
        (R16G16B16A16_Float, 64),
        (R16G16B16A16_UNorm, 64),
        (R32G32_Float, 64),
        (R10G10B10A2_UNorm, 32),
        (R11G11B10_Float, 32),
        (R8G8B8A8_UNorm, 32),
        (R8G8B8A8_UNorm_sRGB, 32),
        (B8G8R8A8_UNorm, 32),
        (R9G9B9E5_SharedExp, 32),
        (R32_Float, 32),
        (R16_UNorm, 16),
        (R8G8_UNorm, 16),
        (B5G6R5_UNorm, 16),
        (B5G5R5A1_UNorm, 16),
        (B4G4R4A4_UNorm, 16),
        (R8_UNorm, 8),
        (A8_UNorm, 8),
        (R1_UNorm, 1),
        (R8G8_B8G8_UNorm, 16),
    ]
}

fn row_bytes(f: DxgiFormat, bpp: u32, w: u32) -> u32 {
    if f == DxgiFormat::R8G8_B8G8_UNorm {
        w.div_ceil(2) * 4
    } else {
        (w * bpp).div_ceil(8)
    }
}

/// Expected (offset, len) of each level in one slice.
fn chain(row: impl Fn(u32) -> u32, w: u32, h: u32, d: u32, levels: u32) -> Vec<(usize, usize)> {
    let mut offset = 0;
    (0..levels)
        .map(|l| {
            let dim = |v: u32| (v >> l).max(1);
            let len = (row(dim(w)) * dim(h) * dim(d)) as usize;
            offset += len;
            (offset - len, len)
        })
        .collect()
}

fn total(c: &[(usize, usize)]) -> usize {
    c.last().map(|l| l.0 + l.1).unwrap()
}

fn new(
    format: DxgiFormat,
    (w, h, depth): (u32, u32, Option<u32>),
    levels: u32,
    layers: Option<u32>,
    cube: bool,
) -> Dds {
    Dds::new_dxgi(NewDxgiParams {
        height: h,
        width: w,
        depth,
        format,
        mipmap_levels: Some(levels),
        array_layers: layers,
        caps2: None,
        is_cubemap: cube,
        resource_dimension: if depth.is_some() {
            D3D10ResourceDimension::Texture3D
        } else {
            D3D10ResourceDimension::Texture2D
        },
        alpha_mode: AlphaMode::Straight,
    })
    .unwrap()
}

fn bytes(d: &Dds) -> Vec<u8> {
    let mut b = Vec::new();
    d.write(&mut b).unwrap();
    b
}

fn ours<D: AsRef<[u8]>>(d: &rusty_dds::DdsBase<D>, layer: u32, face: u32) -> Vec<(usize, usize)> {
    (0..d.get_num_mipmap_levels())
        .map(|m| {
            let r = d
                .subresource_range(SubresourceId::new(m, layer, face))
                .unwrap();
            (r.start, r.len())
        })
        .collect()
}

#[test]
fn layout_matches_formula_and_dds_crate() {
    for (f, bpp) in formats() {
        for &(w, h) in SIZES {
            let levels = full_levels(w, h, 1);
            let label = format!("{f:?} {w}x{h}");
            let want = chain(|x| row_bytes(f, bpp, x), w, h, 1, levels);
            let d = new(f, (w, h, None), levels, None, false);
            assert_eq!(d.data.len(), total(&want), "{label}: allocation");
            assert_eq!(ours(&d, 0, 0), want, "{label}");

            let b = bytes(&d);
            let back = Dds::read(Cursor::new(&b)).unwrap();
            assert_eq!(ours(&back, 0, 0), want, "{label}: round trip");

            let dec = dds::Decoder::new(Cursor::new(&b)).unwrap_or_else(|e| panic!("{label}: {e}"));
            let theirs: Vec<_> = dec
                .layout()
                .texture()
                .unwrap()
                .iter_mips()
                .map(|m| (m.data_offset() as usize, m.data_len() as usize))
                .collect();
            assert_eq!(theirs, want, "{label}: dds crate");
        }
    }
}

#[test]
fn array_and_cube_layers_start_after_exact_chains() {
    for (w, h, layers, cube) in [(50, 30, 3, false), (25, 25, 6, true)] {
        let levels = full_levels(w, h, 1);
        let one = chain(|x| x * 4, w, h, 1, levels);
        let stride = total(&one);
        let d = new(
            DxgiFormat::R8G8B8A8_UNorm,
            (w, h, None),
            levels,
            Some(layers),
            cube,
        );
        assert_eq!(d.data.len(), stride * layers as usize);
        for i in 0..layers {
            let want: Vec<_> = one
                .iter()
                .map(|&(o, l)| (o + stride * i as usize, l))
                .collect();
            let got = if cube { ours(&d, 0, i) } else { ours(&d, i, 0) };
            assert_eq!(got, want, "slice {i} cube={cube}");
        }
    }
}

#[test]
fn volumes_match_dds_crate() {
    for (w, h, dep) in [(13, 7, 5), (64, 64, 64), (16, 16, 4)] {
        let levels = full_levels(w, h, dep);
        let want = chain(|x| x * 4, w, h, dep, levels);
        let d = new(
            DxgiFormat::R8G8B8A8_UNorm,
            (w, h, Some(dep)),
            levels,
            None,
            false,
        );
        assert_eq!(d.data.len(), total(&want));
        assert_eq!(ours(&d, 0, 0), want);
        let b = bytes(&d);
        let dec = dds::Decoder::new(Cursor::new(&b)).unwrap();
        let theirs: Vec<_> = dec
            .layout()
            .volume()
            .unwrap()
            .iter_mips()
            .map(|m| {
                let s = m.get_depth_slice(0).unwrap();
                (
                    s.data_offset() as usize,
                    s.data_len() as usize * m.depth() as usize,
                )
            })
            .collect();
        assert_eq!(theirs, want);
    }
}

#[test]
fn legacy_headers_match_formula() {
    for (f, bpp) in [
        (D3DFormat::A8R8G8B8, 32),
        (D3DFormat::R8G8B8, 24),
        (D3DFormat::R5G6B5, 16),
        (D3DFormat::A8L8, 16),
        (D3DFormat::L8, 8),
        (D3DFormat::A16B16G16R16F, 64),
    ] {
        for &(w, h) in SIZES {
            let levels = full_levels(w, h, 1);
            let want = chain(|x| (x * bpp).div_ceil(8), w, h, 1, levels);
            let d = Dds::new_d3d(NewD3dParams {
                height: h,
                width: w,
                depth: None,
                format: f,
                mipmap_levels: Some(levels),
                caps2: None,
            })
            .unwrap();
            let back = Dds::read(Cursor::new(bytes(&d))).unwrap();
            assert_eq!(ours(&d, 0, 0), want, "{f:?} {w}x{h}");
            assert_eq!(ours(&back, 0, 0), want, "{f:?} {w}x{h}: round trip");
        }
    }
}

/// Every level gets its own colour, laid out by the formula rather than by our
/// own writer, so a misplaced level decodes to the wrong colour.
#[test]
fn decodes_the_right_pixels_at_every_level() {
    for &(w, h) in SIZES {
        let levels = full_levels(w, h, 1);
        let mut d = new(
            DxgiFormat::R8G8B8A8_UNorm,
            (w, h, None),
            levels,
            None,
            false,
        );
        let colour = |l: u32| [l as u8 + 1, 200 - l as u8, l as u8 * 9, 255];
        let mut data = Vec::new();
        for l in 0..levels {
            for _ in 0..(w >> l).max(1) * (h >> l).max(1) {
                data.extend_from_slice(&colour(l));
            }
        }
        d.data = data;
        for l in 0..levels {
            let img = d
                .decode_rgba8(SubresourceId::mip_layer(l, 0))
                .unwrap_or_else(|e| panic!("{w}x{h} mip {l}: {e}"));
            assert_eq!((img.width, img.height), ((w >> l).max(1), (h >> l).max(1)));
            assert!(
                img.pixels.chunks_exact(4).all(|p| p == colour(l)),
                "{w}x{h} mip {l}"
            );
        }
    }
}

/// A header may claim any mip count; nothing may panic or spin.
#[test]
fn absurd_mip_counts_are_bounded() {
    let mut d = new(DxgiFormat::R8_UNorm, (64, 64, None), 7, None, false);
    d.header.mip_map_count = Some(u32::MAX);
    d.data = vec![0; 8192];
    let b = bytes(&d);
    let v = DdsView::parse(&b).unwrap();
    for mip in [31, 32, 33, 40, 1 << 20, u32::MAX - 1] {
        let _ = v.surface(SubresourceId::mip_layer(mip, 0));
        assert_eq!(v.mip_dimensions(mip).unwrap(), (1, 1, 1));
    }
    // 64x64 down to 1x1 is 5461 bytes over 7 levels, then one byte a level.
    let r = v
        .subresource_range(SubresourceId::mip_layer(40, 0))
        .unwrap();
    assert_eq!(r, 5461 + 33..5461 + 34);
    assert!(v
        .subresource_range(SubresourceId::mip_layer(u32::MAX - 1, 0))
        .is_err());
}
