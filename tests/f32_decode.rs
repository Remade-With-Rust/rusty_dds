//! `decode_rgba_f32` beyond BC6H: uncompressed float / wide / packed formats,
//! full-precision BC4/BC5, the 8-bit block formats widened, and the
//! single-channel grey option (Remade-With-Rust/rusty_dds#2).
//!
//! Every uncompressed arm is checked against an oracle written independently of
//! the decoder: f64 arithmetic straight from the DXGI format definitions.

#![cfg(feature = "decode")]

use rusty_dds::*;

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn random_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut s = seed | 1;
    let mut v = Vec::with_capacity(len + 8);
    while v.len() < len {
        v.extend_from_slice(&xorshift(&mut s).to_le_bytes());
    }
    v.truncate(len);
    v
}

fn dxgi(format: DxgiFormat, w: u32, h: u32, seed: u64) -> Dds {
    let mut dds = Dds::new_dxgi(NewDxgiParams {
        height: h,
        width: w,
        depth: None,
        format,
        mipmap_levels: None,
        array_layers: None,
        caps2: None,
        is_cubemap: false,
        resource_dimension: D3D10ResourceDimension::Texture2D,
        alpha_mode: AlphaMode::Straight,
    })
    .expect("new_dxgi");
    let n = dds.data.len();
    dds.data = random_bytes(n, seed);
    dds
}

fn d3d(format: D3DFormat, w: u32, h: u32, seed: u64) -> Dds {
    let mut dds = Dds::new_d3d(NewD3dParams {
        height: h,
        width: w,
        depth: None,
        format,
        mipmap_levels: None,
        caps2: None,
    })
    .expect("new_d3d");
    let n = dds.data.len();
    dds.data = random_bytes(n, seed);
    dds
}

const ID: SubresourceId = SubresourceId::mip_layer(0, 0);

// ---- independent f64 oracles --------------------------------------------

fn half(h: u16) -> f64 {
    let s = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let e = ((h >> 10) & 0x1f) as i32;
    let m = (h & 0x3ff) as f64;
    match e {
        0 => s * m * 2f64.powi(-24),
        31 => {
            if m == 0.0 {
                s * f64::INFINITY
            } else {
                f64::NAN
            }
        }
        _ => s * (1.0 + m / 1024.0) * 2f64.powi(e - 15),
    }
}

/// Unsigned float with 5-bit exponent and `mb` mantissa bits.
fn ufloat(bits: u32, mb: u32) -> f64 {
    let e = (bits >> mb) as i32;
    let m = (bits & ((1 << mb) - 1)) as f64;
    let one = (1u32 << mb) as f64;
    match e {
        0 => m / one * 2f64.powi(-14),
        31 => {
            if m == 0.0 {
                f64::INFINITY
            } else {
                f64::NAN
            }
        }
        _ => (1.0 + m / one) * 2f64.powi(e - 15),
    }
}

fn un(x: u32, bits: u32) -> f64 {
    x as f64 / ((1u64 << bits) - 1) as f64
}

fn sn(x: u32, bits: u32) -> f64 {
    let max = ((1i64 << (bits - 1)) - 1) as f64;
    let v = ((x as i64) << (64 - bits)) >> (64 - bits);
    (v as f64 / max).max(-1.0)
}

fn rd(b: &[u8], i: usize, n: usize) -> u32 {
    let mut v = 0u32;
    for k in 0..n {
        v |= (b[i + k] as u32) << (8 * k);
    }
    v
}

/// Expected RGBA for one pixel of `fmt` stored at `p`.
fn oracle(fmt: &str, p: &[u8]) -> [f64; 4] {
    let f32_at = |i: usize| f32::from_le_bytes([p[i], p[i + 1], p[i + 2], p[i + 3]]) as f64;
    let h = |i: usize| half(rd(p, i, 2) as u16);
    match fmt {
        "rgba32f" => [f32_at(0), f32_at(4), f32_at(8), f32_at(12)],
        "rgb32f" => [f32_at(0), f32_at(4), f32_at(8), 1.0],
        "rg32f" => [f32_at(0), f32_at(4), 0.0, 1.0],
        "r32f" => [f32_at(0), 0.0, 0.0, 1.0],
        "rgba16f" => [h(0), h(2), h(4), h(6)],
        "rg16f" => [h(0), h(2), 0.0, 1.0],
        "r16f" => [h(0), 0.0, 0.0, 1.0],
        "rgba16un" => [0, 2, 4, 6].map(|i| un(rd(p, i, 2), 16)),
        "rg16un" => [un(rd(p, 0, 2), 16), un(rd(p, 2, 2), 16), 0.0, 1.0],
        "r16un" => [un(rd(p, 0, 2), 16), 0.0, 0.0, 1.0],
        "rgba16sn" => [0, 2, 4, 6].map(|i| sn(rd(p, i, 2), 16)),
        "rg16sn" => [sn(rd(p, 0, 2), 16), sn(rd(p, 2, 2), 16), 0.0, 1.0],
        "r16sn" => [sn(rd(p, 0, 2), 16), 0.0, 0.0, 1.0],
        "rgba8sn" => [0, 1, 2, 3].map(|i| sn(p[i] as u32, 8)),
        "bgrx8" => [
            un(p[2] as u32, 8),
            un(p[1] as u32, 8),
            un(p[0] as u32, 8),
            1.0,
        ],
        "rgbx8" => [
            un(p[0] as u32, 8),
            un(p[1] as u32, 8),
            un(p[2] as u32, 8),
            1.0,
        ],
        "bgra8" => [
            un(p[2] as u32, 8),
            un(p[1] as u32, 8),
            un(p[0] as u32, 8),
            un(p[3] as u32, 8),
        ],
        "rgba8" => [0, 1, 2, 3].map(|i| un(p[i] as u32, 8)),
        "rg8un" => [un(p[0] as u32, 8), un(p[1] as u32, 8), 0.0, 1.0],
        "rg8sn" => [sn(p[0] as u32, 8), sn(p[1] as u32, 8), 0.0, 1.0],
        "r8un" => [un(p[0] as u32, 8), 0.0, 0.0, 1.0],
        "r8sn" => [sn(p[0] as u32, 8), 0.0, 0.0, 1.0],
        "a8" => [0.0, 0.0, 0.0, un(p[0] as u32, 8)],
        "rgb10a2" | "bgr10a2" => {
            let v = rd(p, 0, 4);
            let lo = un(v & 0x3ff, 10);
            let g = un((v >> 10) & 0x3ff, 10);
            let hi = un((v >> 20) & 0x3ff, 10);
            let a = un(v >> 30, 2);
            if fmt == "rgb10a2" {
                [lo, g, hi, a]
            } else {
                [hi, g, lo, a]
            }
        }
        "rg11b10f" => {
            let v = rd(p, 0, 4);
            [
                ufloat(v & 0x7ff, 6),
                ufloat((v >> 11) & 0x7ff, 6),
                ufloat(v >> 22, 5),
                1.0,
            ]
        }
        "rgb9e5" => {
            let v = rd(p, 0, 4);
            let sc = 2f64.powi((v >> 27) as i32 - 24);
            [
                (v & 0x1ff) as f64 * sc,
                ((v >> 9) & 0x1ff) as f64 * sc,
                ((v >> 18) & 0x1ff) as f64 * sc,
                1.0,
            ]
        }
        "b5g6r5" => {
            let v = rd(p, 0, 2);
            [un(v >> 11, 5), un((v >> 5) & 0x3f, 6), un(v & 0x1f, 5), 1.0]
        }
        "b5g5r5a1" | "b5g5r5x1" => {
            let v = rd(p, 0, 2);
            let a = if fmt == "b5g5r5a1" {
                (v >> 15) as f64
            } else {
                1.0
            };
            [
                un((v >> 10) & 0x1f, 5),
                un((v >> 5) & 0x1f, 5),
                un(v & 0x1f, 5),
                a,
            ]
        }
        "b4g4r4a4" | "b4g4r4x4" => {
            let v = rd(p, 0, 2);
            let a = if fmt == "b4g4r4a4" {
                un(v >> 12, 4)
            } else {
                1.0
            };
            [
                un((v >> 8) & 0xf, 4),
                un((v >> 4) & 0xf, 4),
                un(v & 0xf, 4),
                a,
            ]
        }
        "l8" => {
            let l = un(p[0] as u32, 8);
            [l, l, l, 1.0]
        }
        "l16" => {
            let l = un(rd(p, 0, 2), 16);
            [l, l, l, 1.0]
        }
        "a8l8" => {
            let l = un(p[0] as u32, 8);
            [l, l, l, un(p[1] as u32, 8)]
        }
        "a4l4" => {
            let l = un((p[0] & 0xf) as u32, 4);
            [l, l, l, un((p[0] >> 4) as u32, 4)]
        }
        _ => unreachable!("{fmt}"),
    }
}

/// Bit-equal to the correctly rounded f32 of the oracle (NaN matches NaN).
fn check_against_oracle(dds: &Dds, fmt: &str, bpp: usize) {
    let (w, h) = (dds.get_width() as usize, dds.get_height() as usize);
    let img = dds
        .decode_rgba_f32(ID)
        .unwrap_or_else(|e| panic!("{fmt}: decode_rgba_f32 failed: {e}"));
    assert_eq!(img.pixels.len(), w * h * 4, "{fmt}: output length");
    for i in 0..w * h {
        let want = oracle(fmt, &dds.data[i * bpp..i * bpp + bpp]);
        for c in 0..4 {
            let got = img.pixels[i * 4 + c];
            let want = want[c] as f32;
            assert!(
                got.to_bits() == want.to_bits() || (got.is_nan() && want.is_nan()),
                "{fmt}: pixel {i} channel {c}: got {got:?}, want {want:?}"
            );
        }
    }
}

#[test]
fn dxgi_uncompressed_formats_match_the_oracle() {
    use DxgiFormat::*;
    let cases: &[(DxgiFormat, &str, usize)] = &[
        (R32G32B32A32_Float, "rgba32f", 16),
        (R32G32B32_Float, "rgb32f", 12),
        (R32G32_Float, "rg32f", 8),
        (R32_Float, "r32f", 4),
        (R16G16B16A16_Float, "rgba16f", 8),
        (R16G16_Float, "rg16f", 4),
        (R16_Float, "r16f", 2),
        (R16G16B16A16_UNorm, "rgba16un", 8),
        (R16G16_UNorm, "rg16un", 4),
        (R16_UNorm, "r16un", 2),
        (R16G16B16A16_SNorm, "rgba16sn", 8),
        (R16G16_SNorm, "rg16sn", 4),
        (R16_SNorm, "r16sn", 2),
        (R8G8B8A8_SNorm, "rgba8sn", 4),
        (B8G8R8X8_UNorm, "bgrx8", 4),
        (R8G8_UNorm, "rg8un", 2),
        (R8G8_SNorm, "rg8sn", 2),
        (R8_UNorm, "r8un", 1),
        (R8_SNorm, "r8sn", 1),
        (A8_UNorm, "a8", 1),
        (R10G10B10A2_UNorm, "rgb10a2", 4),
        (R11G11B10_Float, "rg11b10f", 4),
        (R9G9B9E5_SharedExp, "rgb9e5", 4),
        (B5G6R5_UNorm, "b5g6r5", 2),
        (B5G5R5A1_UNorm, "b5g5r5a1", 2),
        (B4G4R4A4_UNorm, "b4g4r4a4", 2),
        // The 8-bit LDR formats route through the existing kernels.
        (R8G8B8A8_UNorm, "rgba8", 4),
        (B8G8R8A8_UNorm, "bgra8", 4),
    ];
    // 64x64 is the issue's repro; 13x7 catches row-pitch and odd-size slips.
    for &(w, h) in &[(64u32, 64u32), (13, 7), (1, 1)] {
        for (k, &(f, name, bpp)) in cases.iter().enumerate() {
            let dds = dxgi(f, w, h, 0x9e37_79b9 ^ (k as u64) << 8 ^ w as u64);
            check_against_oracle(&dds, name, bpp);
        }
    }
}

#[test]
fn legacy_d3d_formats_match_the_oracle() {
    use D3DFormat::*;
    let cases: &[(D3DFormat, &str, usize)] = &[
        (A32B32G32R32F, "rgba32f", 16),
        (G32R32F, "rg32f", 8),
        (R32F, "r32f", 4),
        (A16B16G16R16F, "rgba16f", 8),
        (G16R16F, "rg16f", 4),
        (R16F, "r16f", 2),
        (A16B16G16R16, "rgba16un", 8),
        (G16R16, "rg16un", 4),
        (A8B8G8R8, "rgba8", 4),
        (A8R8G8B8, "bgra8", 4),
        (X8R8G8B8, "bgrx8", 4),
        (X8B8G8R8, "rgbx8", 4),
        (A2B10G10R10, "rgb10a2", 4),
        (A2R10G10B10, "bgr10a2", 4),
        (R5G6B5, "b5g6r5", 2),
        (A1R5G5B5, "b5g5r5a1", 2),
        (X1R5G5B5, "b5g5r5x1", 2),
        (A4R4G4B4, "b4g4r4a4", 2),
        (X4R4G4B4, "b4g4r4x4", 2),
        (A8, "a8", 1),
        (L8, "l8", 1),
        (L16, "l16", 2),
        (A8L8, "a8l8", 2),
        (A4L4, "a4l4", 1),
    ];
    for (k, &(f, name, bpp)) in cases.iter().enumerate() {
        let dds = d3d(f, 21, 9, 0x51ed ^ (k as u64) << 12);
        // The pixel format must survive a write/parse round trip and still
        // classify the same way, so build the parsed view too.
        let mut bytes = Vec::new();
        dds.write(&mut bytes).unwrap();
        let parsed = Dds::read(&bytes[..]).unwrap();
        check_against_oracle(&parsed, name, bpp);
    }
}

#[test]
fn issue_2_repro_r16g16b16a16_float_64x64() {
    // Exact values: 1.0, -2.5, 65504 (max half) and +Inf survive unclamped.
    let mut dds = dxgi(DxgiFormat::R16G16B16A16_Float, 64, 64, 1);
    for px in dds.data.chunks_exact_mut(8) {
        px.copy_from_slice(&[0x00, 0x3c, 0x00, 0xc1, 0xff, 0x7b, 0x00, 0x7c]);
    }
    let img = dds.decode_rgba_f32(SubresourceId::new(0, 0, 0)).unwrap();
    assert_eq!(img.pixels.len(), 64 * 64 * 4);
    assert_eq!(&img.pixels[..4], &[1.0, -2.5, 65504.0, f32::INFINITY]);
}

#[test]
fn single_channel_grey_option() {
    let grey = DecodeOptions::new().single_channel(SingleChannel::Grey);
    for f in [
        DxgiFormat::R8_UNorm,
        DxgiFormat::R16_UNorm,
        DxgiFormat::R16_Float,
        DxgiFormat::R32_Float,
        DxgiFormat::R8_SNorm,
        DxgiFormat::BC4_UNorm,
        DxgiFormat::BC4_SNorm,
    ] {
        let dds = dxgi(f, 12, 8, f as u64);
        let red = dds.decode_rgba_f32(ID).unwrap();
        let g = dds.decode_rgba_f32_with(ID, grey).unwrap();
        for (r, g) in red.pixels.chunks_exact(4).zip(g.pixels.chunks_exact(4)) {
            assert_eq!(r[1], 0.0, "{f:?}: default green");
            assert_eq!(r[2], 0.0, "{f:?}: default blue");
            assert_eq!(r[3], 1.0, "{f:?}: alpha");
            let rr = r[0];
            assert!(
                g[0].to_bits() == rr.to_bits() || rr.is_nan(),
                "{f:?}: red kept"
            );
            assert!(
                g[1].to_bits() == g[0].to_bits() && g[2].to_bits() == g[0].to_bits(),
                "{f:?}: grey"
            );
            assert_eq!(g[3], 1.0);
        }
    }
    // Two-channel formats are unaffected.
    let dds = dxgi(DxgiFormat::BC5_UNorm, 8, 8, 5);
    assert_eq!(
        dds.decode_rgba_f32(ID).unwrap(),
        dds.decode_rgba_f32_with(ID, grey).unwrap()
    );

    // And the 8-bit path: BC4 only.
    let dds = dxgi(DxgiFormat::BC4_UNorm, 12, 8, 77);
    let red = dds.decode_rgba8(ID).unwrap();
    let g = dds.decode_rgba8_with(ID, grey).unwrap();
    for (r, g) in red.pixels.chunks_exact(4).zip(g.pixels.chunks_exact(4)) {
        assert_eq!([r[0], r[0], r[0], 255], [g[0], g[1], g[2], g[3]]);
    }
    let mut buf = Vec::new();
    dds.decode_rgba8_into_with(ID, &mut buf, grey).unwrap();
    assert_eq!(buf, g.pixels);
    let dds = dxgi(DxgiFormat::BC5_UNorm, 8, 8, 5);
    assert_eq!(
        dds.decode_rgba8(ID).unwrap(),
        dds.decode_rgba8_with(ID, grey).unwrap()
    );
}

/// BC4/BC5 in f32: within float rounding of bcdec's own `bc4_float` /
/// `bc5_float`, and rounding to bytes lands within one level of `decode_rgba8`.
#[test]
fn bc45_f32_matches_bcdec_float_and_the_8bit_path() {
    for (f, signed, two) in [
        (DxgiFormat::BC4_UNorm, false, false),
        (DxgiFormat::BC4_SNorm, true, false),
        (DxgiFormat::BC5_UNorm, false, true),
        (DxgiFormat::BC5_SNorm, true, true),
    ] {
        for &(w, h) in &[(64u32, 64u32), (13, 7)] {
            let dds = dxgi(f, w, h, 0xbc45 ^ w as u64 ^ (f as u64) << 20);
            let img = dds.decode_rgba_f32(ID).unwrap();
            let (w, h) = (w as usize, h as usize);
            let bx = w.div_ceil(4);
            let bb = if two { 16 } else { 8 };
            let mut fewer_than_8bit = 0usize;
            for y in 0..h {
                for x in 0..w {
                    let blk = &dds.data[((y / 4) * bx + x / 4) * bb..][..bb];
                    let k = (y % 4) * 4 + x % 4;
                    let mut r = [0f32; 16];
                    let mut rg = [0f32; 32];
                    let (er, eg) = if two {
                        bcdec_rs::bc5_float(blk, &mut rg, 8, signed);
                        (rg[k * 2], rg[k * 2 + 1])
                    } else {
                        bcdec_rs::bc4_float(blk, &mut r, 4, signed);
                        (r[k], 0.0)
                    };
                    let p = &img.pixels[(y * w + x) * 4..][..4];
                    assert!(
                        (p[0] - er).abs() <= 2e-7,
                        "{f:?} ({x},{y}) R {} vs {er}",
                        p[0]
                    );
                    assert!(
                        (p[1] - eg).abs() <= 2e-7,
                        "{f:?} ({x},{y}) G {} vs {eg}",
                        p[1]
                    );
                    assert_eq!([p[2], p[3]], [0.0, 1.0], "{f:?} B/A");
                    let scale = if signed { 127.0 } else { 255.0 };
                    if (p[0] * scale - (p[0] * scale).round()).abs() > 1e-3 {
                        fewer_than_8bit += 1;
                    }
                }
            }
            // Random endpoints: most interpolants sit between 8-bit levels,
            // which is exactly the precision the f32 path exists to keep.
            assert!(fewer_than_8bit > 0, "{f:?}: no sub-level values survived");

            let ldr = dds.decode_rgba8(ID).unwrap();
            for (q, p) in ldr.pixels.chunks_exact(4).zip(img.pixels.chunks_exact(4)) {
                let to8 = |v: f32| {
                    if signed {
                        (v * 127.0).round() as i8 as u8
                    } else {
                        (v * 255.0).round() as u8
                    }
                };
                let d = (to8(p[0]) as i8 as i32 - q[0] as i8 as i32).abs();
                let d = if signed {
                    d
                } else {
                    (to8(p[0]) as i32 - q[0] as i32).abs()
                };
                assert!(
                    d <= 1,
                    "{f:?}: f32 {} rounds >1 level from 8-bit {}",
                    p[0],
                    q[0]
                );
            }
        }
    }
}

/// Formats defined at 8 bits: f32 is exactly the 8-bit result over 255,
/// including the threaded BC7 path and NPOT edges.
#[test]
fn eight_bit_block_formats_widen_exactly() {
    for (f, sizes) in [
        (DxgiFormat::BC1_UNorm, &[(64u32, 64u32), (13, 7)][..]),
        (DxgiFormat::BC2_UNorm, &[(64, 64), (13, 7)][..]),
        (DxgiFormat::BC3_UNorm, &[(64, 64), (13, 7)][..]),
        // 1024^2 and 1030x518 cross the BC7 parallel threshold.
        (
            DxgiFormat::BC7_UNorm,
            &[(64, 64), (13, 7), (1024, 1024), (1030, 518)][..],
        ),
    ] {
        for &(w, h) in sizes {
            let dds = dxgi(f, w, h, 0xb7 ^ w as u64 ^ h as u64);
            let ldr = dds.decode_rgba8(ID).unwrap();
            let hdr = dds.decode_rgba_f32(ID).unwrap();
            assert_eq!(hdr.pixels.len(), ldr.pixels.len(), "{f:?} {w}x{h}");
            for (i, (&b, &v)) in ldr.pixels.iter().zip(&hdr.pixels).enumerate() {
                assert_eq!(v, b as f32 / 255.0, "{f:?} {w}x{h} index {i}");
            }
        }
    }
}

/// The row-split seam reassembles to the whole-surface decode for the new
/// formats as it does for BC6H.
#[test]
fn block_row_split_matches_whole_surface() {
    for (f, w, h) in [
        (DxgiFormat::R16G16B16A16_Float, 37u32, 23u32),
        (DxgiFormat::R10G10B10A2_UNorm, 64, 64),
        (DxgiFormat::BC4_UNorm, 37, 23),
        (DxgiFormat::BC5_SNorm, 64, 64),
        (DxgiFormat::BC1_UNorm, 37, 23),
        (DxgiFormat::R8G8B8A8_UNorm, 37, 23),
    ] {
        let dds = dxgi(f, w, h, 0x5917 ^ f as u64);
        let whole = dds.decode_rgba_f32(ID).unwrap().pixels;
        let rows = dds.block_rows_f32(ID).unwrap();
        let bh = if rows == h { 1 } else { 4 };
        for cut in [1, rows / 2, rows - 1] {
            let split_px = ((cut * bh).min(h) * w * 4) as usize;
            let mut buf = vec![0f32; whole.len()];
            let (top, bottom) = buf.split_at_mut(split_px);
            dds.decode_block_rows_f32_into(ID, 0..cut, top).unwrap();
            dds.decode_block_rows_f32_into(ID, cut..rows, bottom)
                .unwrap();
            assert!(
                buf.iter()
                    .zip(&whole)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "{f:?}: split at {cut} differs"
            );
        }
        let mut into = Vec::new();
        dds.decode_rgba_f32_into(ID, &mut into).unwrap();
        assert!(into
            .iter()
            .zip(&whole)
            .all(|(a, b)| a.to_bits() == b.to_bits()));
    }
}

#[test]
fn truncated_payloads_fail_closed() {
    for f in [
        DxgiFormat::R32G32B32A32_Float,
        DxgiFormat::BC4_UNorm,
        DxgiFormat::BC7_UNorm,
    ] {
        let mut dds = dxgi(f, 16, 16, 3);
        let n = dds.data.len();
        dds.data.truncate(n - 1);
        assert!(dds.decode_rgba_f32(ID).is_err(), "{f:?}");
    }
    // Still unsupported: typeless and integer layouts have no defined float.
    for f in [
        DxgiFormat::R16G16B16A16_Typeless,
        DxgiFormat::R16G16B16A16_UInt,
    ] {
        let dds = dxgi(f, 4, 4, 3);
        assert!(
            matches!(dds.decode_rgba_f32(ID), Err(Error::UnsupportedFormat)),
            "{f:?}"
        );
    }
}
