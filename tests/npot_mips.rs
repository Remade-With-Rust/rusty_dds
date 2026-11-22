//! Regression test for #12: block-compressed mip chains of non-power-of-two
//! textures must be sized in whole 4x4 blocks per level,
//! `max(1, ceil(w / 4)) * max(1, ceil(h / 4)) * block_bytes`, the DXGI /
//! DirectXTex rule. The old chain took each level as a quarter of the
//! previous one, so a 100x100 BC1 mip 1 (50x50 px, 13x13 blocks) came out as
//! 12.5x12.5 blocks' worth of bytes and every later level was misplaced.
//!
//! Every chain is checked three ways: against the formula, against an
//! independent reader (image-rs's `dds` 0.2, the image-dds project), and
//! through a write-then-read round trip. Encoded chains are also decoded by
//! both libraries, which only agree if every level sits where both expect.

use dds::DataRegion;
use rusty_dds::{
    AlphaMode, D3D10ResourceDimension, D3DFormat, Dds, DecodeContent, DxgiFormat, EncodeLayout,
    NewD3dParams, NewDxgiParams, SubresourceId,
};
use std::io::Cursor;

const SIZES: &[(u32, u32)] = &[(100, 100), (13, 7), (1, 1), (5, 3), (257, 129)];

/// Full chain down to 1x1.
fn full_levels(w: u32, h: u32) -> u32 {
    32 - w.max(h).leading_zeros()
}

/// One expected level: (width, height, offset, len) in bytes within a slice.
fn expected_chain(
    w: u32,
    h: u32,
    d: u32,
    block_bytes: u32,
    levels: u32,
) -> Vec<(u32, u32, usize, usize)> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    for l in 0..levels {
        let (lw, lh, ld) = ((w >> l).max(1), (h >> l).max(1), (d >> l).max(1));
        let len = (lw.div_ceil(4).max(1) * lh.div_ceil(4).max(1) * block_bytes * ld) as usize;
        out.push((lw, lh, offset, len));
        offset += len;
    }
    out
}

fn bc_formats() -> Vec<(DxgiFormat, u32)> {
    vec![
        (DxgiFormat::BC1_UNorm, 8),
        (DxgiFormat::BC2_UNorm, 16),
        (DxgiFormat::BC3_UNorm, 16),
        (DxgiFormat::BC4_UNorm, 8),
        (DxgiFormat::BC5_UNorm, 16),
        (DxgiFormat::BC6H_UF16, 16),
        (DxgiFormat::BC7_UNorm, 16),
    ]
}

fn new_2d(format: DxgiFormat, w: u32, h: u32, levels: u32) -> Dds {
    Dds::new_dxgi(NewDxgiParams {
        height: h,
        width: w,
        depth: None,
        format,
        mipmap_levels: Some(levels),
        array_layers: None,
        caps2: None,
        is_cubemap: false,
        resource_dimension: D3D10ResourceDimension::Texture2D,
        alpha_mode: AlphaMode::Unknown,
    })
    .expect("new_dxgi")
}

fn to_bytes(dds: &Dds) -> Vec<u8> {
    let mut bytes = Vec::new();
    dds.write(&mut bytes).expect("write");
    bytes
}

/// Our ranges for every level of layer 0.
fn our_chain(dds: &Dds) -> Vec<(u32, u32, usize, usize)> {
    (0..dds.get_num_mipmap_levels())
        .map(|mip| {
            let s = dds
                .surface(SubresourceId::mip_layer(mip, 0))
                .expect("surface");
            let r = dds
                .subresource_range(SubresourceId::mip_layer(mip, 0))
                .unwrap();
            (s.width, s.height, r.start, r.len())
        })
        .collect()
}

/// The `dds` crate's view of the same file's 2D chain.
fn oracle_chain(bytes: &[u8]) -> Vec<(u32, u32, usize, usize)> {
    let dec = dds::Decoder::new(Cursor::new(bytes)).expect("dds crate parses our file");
    let tex = dec.layout().texture().expect("dds crate sees a 2D texture");
    tex.iter_mips()
        .map(|m| {
            (
                m.width(),
                m.height(),
                m.data_offset() as usize,
                m.data_len() as usize,
            )
        })
        .collect()
}

/// The worked example from the issue, spelled out.
#[test]
fn bc1_100x100_chain_is_whole_blocks() {
    let dds = new_2d(DxgiFormat::BC1_UNorm, 100, 100, 7);
    let chain = our_chain(&dds);
    let dims: Vec<_> = chain.iter().map(|c| (c.0, c.1)).collect();
    let offsets: Vec<_> = chain.iter().map(|c| c.2).collect();
    let lens: Vec<_> = chain.iter().map(|c| c.3).collect();
    assert_eq!(
        dims,
        [
            (100, 100),
            (50, 50),
            (25, 25),
            (12, 12),
            (6, 6),
            (3, 3),
            (1, 1)
        ]
    );
    // 25x25, 13x13, 7x7, 3x3, 2x2, 1x1, 1x1 blocks of 8 bytes. The old chain
    // gave 5000, 1250, 312, 78, 19, 8, 8 (total 6675).
    assert_eq!(lens, [5000, 1352, 392, 72, 32, 8, 8]);
    assert_eq!(offsets, [0, 5000, 6352, 6744, 6816, 6848, 6856]);
    assert_eq!(dds.data.len(), 6864);
    assert_eq!(dds.get_array_stride().unwrap(), 6864);
}

#[test]
fn npot_bcn_layout_matches_formula_and_dds_crate() {
    for (format, bs) in bc_formats() {
        for &(w, h) in SIZES {
            let levels = full_levels(w, h);
            let label = format!("{format:?} {w}x{h} x{levels}");
            let want = expected_chain(w, h, 1, bs, levels);
            let total = want.last().map(|c| c.2 + c.3).unwrap();

            let dds = new_2d(format, w, h, levels);
            assert_eq!(dds.data.len(), total, "{label}: allocated payload");
            assert_eq!(our_chain(&dds), want, "{label}: our chain vs formula");

            let bytes = to_bytes(&dds);
            assert_eq!(oracle_chain(&bytes), want, "{label}: dds crate vs formula");

            // Round trip: our own reader on our own file.
            let back = Dds::read(Cursor::new(&bytes)).expect("read back");
            assert_eq!(our_chain(&back), want, "{label}: round-trip chain");
            assert_eq!(back.data.len(), total, "{label}: round-trip payload");
        }
    }
}

/// Arrays repeat the exact chain per layer, so layer `n` starts at `n * chain`.
#[test]
fn npot_bcn_array_stride_is_exact_chain() {
    let (w, h, layers) = (50, 30, 3);
    let levels = full_levels(w, h);
    let want = expected_chain(w, h, 1, 16, levels);
    let chain = want.last().map(|c| c.2 + c.3).unwrap();
    let dds = Dds::new_dxgi(NewDxgiParams {
        height: h,
        width: w,
        depth: None,
        format: DxgiFormat::BC7_UNorm,
        mipmap_levels: Some(levels),
        array_layers: Some(layers),
        caps2: None,
        is_cubemap: false,
        resource_dimension: D3D10ResourceDimension::Texture2D,
        alpha_mode: AlphaMode::Unknown,
    })
    .unwrap();
    assert_eq!(dds.data.len(), chain * layers as usize);
    for layer in 0..layers {
        for (mip, &(_, _, off, len)) in want.iter().enumerate() {
            let r = dds
                .subresource_range(SubresourceId::mip_layer(mip as u32, layer))
                .unwrap();
            assert_eq!(
                r,
                layer as usize * chain + off..layer as usize * chain + off + len
            );
        }
    }
}

/// Volumes: each level holds `max(1, d >> l)` slices of whole blocks.
#[test]
fn npot_bc1_volume_matches_dds_crate() {
    let (w, h, d) = (13, 7, 5);
    let levels = full_levels(w, h);
    let dds = Dds::new_dxgi(NewDxgiParams {
        height: h,
        width: w,
        depth: Some(d),
        format: DxgiFormat::BC1_UNorm,
        mipmap_levels: Some(levels),
        array_layers: None,
        caps2: None,
        is_cubemap: false,
        resource_dimension: D3D10ResourceDimension::Texture3D,
        alpha_mode: AlphaMode::Unknown,
    })
    .unwrap();
    let want = expected_chain(w, h, d, 8, levels);
    assert_eq!(dds.data.len(), want.last().map(|c| c.2 + c.3).unwrap());
    for (mip, &(_, _, off, len)) in want.iter().enumerate() {
        let r = dds
            .subresource_range(SubresourceId::mip_layer(mip as u32, 0))
            .unwrap();
        assert_eq!(r, off..off + len, "volume mip {mip}");
    }
    let bytes = to_bytes(&dds);
    let dec = dds::Decoder::new(Cursor::new(&bytes)).unwrap();
    let vol = dec.layout().volume().expect("dds crate sees a volume");
    let theirs: Vec<_> = vol
        .iter_mips()
        .map(|m| {
            let first = m.get_depth_slice(0).unwrap();
            (
                first.data_offset() as usize,
                first.data_len() as usize * m.depth() as usize,
            )
        })
        .collect();
    let ours: Vec<_> = want.iter().map(|c| (c.2, c.3)).collect();
    assert_eq!(theirs, ours);
}

/// Legacy (non-DX10) FourCC files take the same exact chain.
#[test]
fn npot_legacy_dxt_layout_matches_dds_crate() {
    for (format, bs) in [(D3DFormat::DXT1, 8), (D3DFormat::DXT5, 16)] {
        for &(w, h) in SIZES {
            let levels = full_levels(w, h);
            let label = format!("{format:?} {w}x{h}");
            let dds = Dds::new_d3d(NewD3dParams {
                height: h,
                width: w,
                depth: None,
                format,
                mipmap_levels: Some(levels),
                caps2: None,
            })
            .unwrap();
            let want = expected_chain(w, h, 1, bs, levels);
            assert_eq!(our_chain(&dds), want, "{label}");
            assert_eq!(dds.data.len(), want.last().map(|c| c.2 + c.3).unwrap());
            let bytes = to_bytes(&dds);
            assert_eq!(oracle_chain(&bytes), want, "{label}: dds crate");
            let back = Dds::read(Cursor::new(&bytes)).unwrap();
            assert_eq!(our_chain(&back), want, "{label}: round trip");
        }
    }
}

fn source(w: u32, h: u32) -> Vec<u8> {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    let mut s: u32 = 0x9e37_79b9;
    for y in 0..h {
        for x in 0..w {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let n = (s >> 26) as u8; // small noise so blocks are not flat
            px.extend_from_slice(&[
                ((x * 255) / w.max(1)) as u8 ^ n,
                ((y * 255) / h.max(1)) as u8,
                (((x + y) * 7) & 0xff) as u8,
                if (x / 3 + y / 2) % 2 == 0 { 255 } else { 96 },
            ]);
        }
    }
    px
}

/// Channels both decoders define the same way for this content.
fn compared_channels(content: DecodeContent) -> &'static [usize] {
    match content {
        DecodeContent::Bc4UNorm | DecodeContent::Bc4SNorm => &[0],
        DecodeContent::Bc5UNorm | DecodeContent::Bc5SNorm => &[0, 1],
        DecodeContent::Bc1 => &[0, 1, 2],
        _ => &[0, 1, 2, 3],
    }
}

/// Encode a full NPOT chain, write it, read it back, and decode every level
/// with both rusty_dds and the `dds` crate. Before #12 the encoder refused
/// these chains (the level slices were too short for the blocks).
#[test]
fn npot_encoded_chain_round_trips_and_decodes_in_dds_crate() {
    let contents = [
        (DecodeContent::Bc1, 8),
        (DecodeContent::Bc3, 16),
        (DecodeContent::Bc7, 16),
        (DecodeContent::Bc4UNorm, 8),
        (DecodeContent::Bc5UNorm, 16),
    ];
    for (content, bs) in contents {
        for &(w, h) in SIZES {
            let levels = full_levels(w, h);
            let label = format!("{content:?} {w}x{h} x{levels}");
            let layout = EncodeLayout::flat_2d(content, w, h).with_mips(levels);
            let enc = Dds::encode_from_rgba8(&source(w, h), layout)
                .unwrap_or_else(|e| panic!("{label}: encode: {e}"));
            let want = expected_chain(w, h, 1, bs, levels);
            assert_eq!(our_chain(&enc), want, "{label}: encoded chain");

            let bytes = to_bytes(&enc);
            let back = Dds::read(Cursor::new(&bytes)).unwrap();
            assert_eq!(back.data, enc.data, "{label}: payload round trip");
            assert_eq!(oracle_chain(&bytes), want, "{label}: dds crate chain");

            let mut dec = dds::Decoder::new(Cursor::new(&bytes)).unwrap();
            for (mip, &(lw, lh, _, _)) in want.iter().enumerate() {
                let ours = back
                    .decode_rgba8(SubresourceId::mip_layer(mip as u32, 0))
                    .unwrap_or_else(|e| panic!("{label}: decode mip {mip}: {e}"));
                assert_eq!((ours.width, ours.height), (lw, lh));
                let mut theirs = vec![0u8; (lw * lh * 4) as usize];
                let view = dds::ImageViewMut::new(
                    &mut theirs,
                    dds::Size::new(lw, lh),
                    dds::ColorFormat::RGBA_U8,
                )
                .unwrap();
                dec.read_surface(view)
                    .unwrap_or_else(|e| panic!("{label}: dds crate mip {mip}: {e}"));
                let mut worst = 0u8;
                for (a, b) in ours.pixels.chunks_exact(4).zip(theirs.chunks_exact(4)) {
                    for &c in compared_channels(content) {
                        worst = worst.max(a[c].abs_diff(b[c]));
                    }
                }
                // Decoder rounding differs by a few codes at most; a
                // misplaced level decodes someone else's blocks.
                assert!(worst <= 4, "{label}: mip {mip} differs by {worst}");
            }
        }
    }
}
