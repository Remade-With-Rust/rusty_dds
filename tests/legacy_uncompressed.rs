//! Legacy (non-DX10) uncompressed 32-bit headers through the RGBA8 path.
//!
//! The headers are written byte by byte, the way a third-party writer emits
//! them, rather than through `Dds::new_d3d`: the bug these pin was a reader
//! that classified `A8R8G8B8` / `X8R8G8B8` masks correctly and then refused to
//! decode them to RGBA8 (`UnsupportedFormat`), while `decode_rgba_f32`
//! accepted the same files. Every expected pixel below is a permutation of
//! the payload bytes, so the checks are exact.

use rusty_dds::{DdsView, DecodeContent, DxgiFormat, Error, SubresourceId};

const DDPF_ALPHAPIXELS: u32 = 0x1;
const DDPF_RGB: u32 = 0x40;

/// A legacy 128-byte header (`DDS ` + 124-byte DDS_HEADER) for a `w` x `h`
/// 32-bit masked-RGB surface with `mips` levels, followed by `payload`.
fn legacy_dds(
    w: u32,
    h: u32,
    mips: u32,
    pf_flags: u32,
    masks: [u32; 4],
    payload: &[u8],
) -> Vec<u8> {
    let mut b = Vec::new();
    let mut put = |v: u32| b.extend_from_slice(&v.to_le_bytes());
    put(0x2053_4444); // "DDS "
    put(124); // dwSize
              // CAPS | HEIGHT | WIDTH | PITCH | PIXELFORMAT (+ MIPMAPCOUNT)
    put(0x100f | if mips > 1 { 0x2_0000 } else { 0 });
    put(h);
    put(w);
    put(w * 4); // dwPitchOrLinearSize: row pitch
    put(0); // dwDepth
    put(mips);
    for _ in 0..11 {
        put(0); // dwReserved1
    }
    put(32); // ddspf.dwSize
    put(pf_flags);
    put(0); // dwFourCC
    put(32); // dwRGBBitCount
    for m in masks {
        put(m);
    }
    // TEXTURE (+ COMPLEX | MIPMAP)
    put(0x1000 | if mips > 1 { 0x40_0008 } else { 0 });
    for _ in 0..4 {
        put(0); // dwCaps2..4, dwReserved2
    }
    assert_eq!(b.len(), 128);
    b.extend_from_slice(payload);
    b
}

/// DDSPF_A8R8G8B8 / DDSPF_X8R8G8B8 / DDSPF_A8B8G8R8 / DDSPF_X8B8G8R8 masks
/// (DirectXTex `DDS.h`).
const ARGB: [u32; 4] = [0x00ff_0000, 0x0000_ff00, 0x0000_00ff, 0xff00_0000];
const XRGB: [u32; 4] = [0x00ff_0000, 0x0000_ff00, 0x0000_00ff, 0];
const ABGR: [u32; 4] = [0x0000_00ff, 0x0000_ff00, 0x00ff_0000, 0xff00_0000];
const XBGR: [u32; 4] = [0x0000_00ff, 0x0000_ff00, 0x00ff_0000, 0];

/// Distinct bytes everywhere, and padding bytes that are neither 0 nor 255
/// so an opaque-alpha bug cannot hide behind a lucky payload.
fn payload(w: u32, h: u32) -> Vec<u8> {
    (0..w * h * 4).map(|i| (i * 7 + 3) as u8).collect()
}

/// Payload in memory order (B, G, R, A) -> RGBA.
fn swap_rb(px: &[u8]) -> Vec<u8> {
    px.chunks_exact(4)
        .flat_map(|p| [p[2], p[1], p[0], p[3]])
        .collect()
}

fn opaque(mut px: Vec<u8>) -> Vec<u8> {
    for p in px.chunks_exact_mut(4) {
        p[3] = 255;
    }
    px
}

fn id0() -> SubresourceId {
    SubresourceId::mip_layer(0, 0)
}

/// Every RGBA8 entry point, and `decode_rgba_f32` as `x / 255` of it.
fn assert_decodes_to(bytes: &[u8], w: u32, h: u32, want: &[u8]) {
    let dds = DdsView::parse(bytes).unwrap();
    let img = dds.decode_rgba8(id0()).unwrap();
    assert_eq!((img.width, img.height, img.depth), (w, h, 1));
    assert_eq!(img.pixels, want, "decode_rgba8");

    let mut into = vec![0xAA; 3];
    assert_eq!(dds.decode_rgba8_into(id0(), &mut into).unwrap(), (w, h, 1));
    assert_eq!(into, want, "decode_rgba8_into");

    let rows = dds.block_rows(id0()).unwrap();
    assert_eq!(rows, h);
    let mut split = vec![0u8; want.len()];
    let half = rows / 2;
    let (top, bottom) = split.split_at_mut((w * half * 4) as usize);
    dds.decode_block_rows_into(id0(), 0..half, top).unwrap();
    dds.decode_block_rows_into(id0(), half..rows, bottom)
        .unwrap();
    assert_eq!(split, want, "decode_block_rows_into");

    let f = dds.decode_rgba_f32(id0()).unwrap();
    let widened: Vec<f32> = want.iter().map(|&v| v as f32 / 255.0).collect();
    assert_eq!(f.pixels, widened, "decode_rgba_f32");

    dds.upload_plan_decoded_rgba8(id0()).unwrap();
}

#[test]
fn a8r8g8b8_decodes_as_bgra8() {
    let (w, h) = (5, 3);
    let px = payload(w, h);
    let bytes = legacy_dds(w, h, 1, DDPF_RGB | DDPF_ALPHAPIXELS, ARGB, &px);
    let dds = DdsView::parse(&bytes).unwrap();
    assert_eq!(dds.decode_content().unwrap(), DecodeContent::Bgra8);
    assert_decodes_to(&bytes, w, h, &swap_rb(&px));
}

#[test]
fn a8b8g8r8_decodes_as_rgba8() {
    let (w, h) = (3, 4);
    let px = payload(w, h);
    let bytes = legacy_dds(w, h, 1, DDPF_RGB | DDPF_ALPHAPIXELS, ABGR, &px);
    let dds = DdsView::parse(&bytes).unwrap();
    assert_eq!(dds.decode_content().unwrap(), DecodeContent::Rgba8);
    assert_decodes_to(&bytes, w, h, &px);
}

#[test]
fn x8r8g8b8_decodes_opaque() {
    let (w, h) = (4, 4);
    let px = payload(w, h);
    let bytes = legacy_dds(w, h, 1, DDPF_RGB, XRGB, &px);
    assert_decodes_to(&bytes, w, h, &opaque(swap_rb(&px)));
    // Not an encode target: the LDR (decode + encode) matrix still refuses it.
    let dds = DdsView::parse(&bytes).unwrap();
    assert!(matches!(
        dds.decode_content(),
        Err(Error::UnsupportedFormat)
    ));
}

#[test]
fn x8b8g8r8_decodes_opaque() {
    let (w, h) = (2, 6);
    let px = payload(w, h);
    let bytes = legacy_dds(w, h, 1, DDPF_RGB, XBGR, &px);
    assert_decodes_to(&bytes, w, h, &opaque(px));
}

#[test]
fn padding_byte_zero_still_reads_opaque() {
    // The common real-world case: writers leave X as 0.
    let (w, h) = (2, 2);
    let px: Vec<u8> = (0..w * h).flat_map(|i| [i as u8, 0x40, 0x80, 0]).collect();
    let bytes = legacy_dds(w, h, 1, DDPF_RGB, XRGB, &px);
    let img = DdsView::parse(&bytes).unwrap().decode_rgba8(id0()).unwrap();
    assert!(img.pixels.chunks_exact(4).all(|p| p[3] == 255));
}

#[test]
fn dx10_b8g8r8x8_decodes_opaque() {
    let (w, h) = (4, 2);
    let px = payload(w, h);
    let mut bytes = Vec::new();
    rusty_dds::Dds::new_dxgi(rusty_dds::NewDxgiParams {
        height: h,
        width: w,
        depth: None,
        format: DxgiFormat::B8G8R8X8_UNorm,
        mipmap_levels: None,
        array_layers: None,
        caps2: None,
        is_cubemap: false,
        resource_dimension: rusty_dds::D3D10ResourceDimension::Texture2D,
        alpha_mode: rusty_dds::AlphaMode::Opaque,
    })
    .unwrap()
    .write(&mut bytes)
    .unwrap();
    let header = bytes.len() - (w * h * 4) as usize;
    bytes[header..].copy_from_slice(&px);
    assert_decodes_to(&bytes, w, h, &opaque(swap_rb(&px)));
}

#[test]
fn legacy_mip_chain_decodes_every_level() {
    // 4x4, 2x2, 1x1 tightly packed after the header.
    let levels = [(4u32, 4u32), (2, 2), (1, 1)];
    let mut px = Vec::new();
    for (i, &(w, h)) in levels.iter().enumerate() {
        px.extend((0..w * h * 4).map(|j| (j as usize * 5 + i * 31) as u8));
    }
    let bytes = legacy_dds(4, 4, 3, DDPF_RGB | DDPF_ALPHAPIXELS, ARGB, &px);
    let dds = DdsView::parse(&bytes).unwrap();
    let mut off = 0;
    for (mip, &(w, h)) in levels.iter().enumerate() {
        let n = (w * h * 4) as usize;
        let img = dds
            .decode_rgba8(SubresourceId::mip_layer(mip as u32, 0))
            .unwrap();
        assert_eq!((img.width, img.height), (w, h));
        assert_eq!(img.pixels, swap_rb(&px[off..off + n]), "mip {mip}");
        off += n;
    }
}

#[test]
fn short_payload_is_refused_not_padded() {
    let (w, h) = (8, 8);
    let px = payload(w, h);
    for masks in [ARGB, XRGB] {
        let flags = if masks[3] != 0 {
            DDPF_RGB | DDPF_ALPHAPIXELS
        } else {
            DDPF_RGB
        };
        let mut bytes = legacy_dds(w, h, 1, flags, masks, &px);
        bytes.truncate(bytes.len() - 4);
        // Either the parser or the decoder must refuse; neither may invent
        // the missing pixel.
        if let Ok(dds) = DdsView::parse(&bytes) {
            assert!(dds.decode_rgba8(id0()).is_err());
            let mut v = Vec::new();
            assert!(dds.decode_rgba8_into(id0(), &mut v).is_err());
        }
    }
}
