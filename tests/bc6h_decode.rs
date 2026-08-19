//! BC6H decode matrix: container plumbing + tiler proven against direct
//! per-block `bcdec_rs::bc6h_float` calls (the same oracle discipline as
//! the LDR decode matrix), across 2D / NPOT / mips / array / volume and
//! both signednesses. LDR/HDR APIs must fail closed on each other.

#![cfg(feature = "decode")]

use rusty_dds::*;

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn random_payload(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    let mut v = Vec::with_capacity(len);
    while v.len() < len {
        let r = xorshift(&mut state);
        v.extend_from_slice(&r.to_le_bytes());
    }
    v.truncate(len);
    v
}

/// Oracle: decode a full slice block-by-block with direct bcdec calls,
/// clamped copy for NPOT edges, RGB -> RGBA (A = 1.0).
fn oracle_slice(data: &[u8], w: usize, h: usize, signed: bool) -> Vec<f32> {
    let bx = (w + 3) / 4;
    let by = (h + 3) / 4;
    let mut out = vec![0f32; w * h * 4];
    let mut scratch = [0f32; 4 * 4 * 3];
    for yb in 0..by {
        for xb in 0..bx {
            let bi = (yb * bx + xb) * 16;
            bcdec_oracle(&data[bi..bi + 16], &mut scratch, signed);
            for row in 0..4 {
                let y = yb * 4 + row;
                if y >= h {
                    break;
                }
                for col in 0..4 {
                    let x = xb * 4 + col;
                    if x >= w {
                        break;
                    }
                    let s = (row * 4 + col) * 3;
                    let d = (y * w + x) * 4;
                    out[d] = scratch[s];
                    out[d + 1] = scratch[s + 1];
                    out[d + 2] = scratch[s + 2];
                    out[d + 3] = 1.0;
                }
            }
        }
    }
    out
}

fn bcdec_oracle(block: &[u8], scratch: &mut [f32; 48], signed: bool) {
    bcdec_rs_shim::bc6h_float(block, scratch, 4 * 3, signed);
}

// Use the same bcdec_rs the crate uses (dev-dependency path: it is a public
// dep of the `decode` feature, re-exported nowhere, so declare our own).
mod bcdec_rs_shim {
    pub fn bc6h_float(block: &[u8], out: &mut [f32], pitch: usize, signed: bool) {
        bcdec_rs::bc6h_float(block, out, pitch, signed);
    }
}

fn make_dds(
    w: u32,
    h: u32,
    depth: Option<u32>,
    mips: u32,
    layers: u32,
    signed: bool,
    payload_seed: u64,
) -> Dds {
    let format = if signed {
        DxgiFormat::BC6H_SF16
    } else {
        DxgiFormat::BC6H_UF16
    };
    let mut dds = Dds::new_dxgi(NewDxgiParams {
        height: h,
        width: w,
        depth,
        format,
        mipmap_levels: if mips > 1 { Some(mips) } else { None },
        array_layers: if layers > 1 { Some(layers) } else { None },
        caps2: None,
        is_cubemap: false,
        resource_dimension: if depth.is_some() {
            D3D10ResourceDimension::Texture3D
        } else {
            D3D10ResourceDimension::Texture2D
        },
        alpha_mode: AlphaMode::Opaque,
    })
    .expect("new_dxgi BC6H");
    let payload = random_payload(dds.data.len(), payload_seed);
    dds.data.copy_from_slice(&payload);
    dds
}

#[test]
fn bc6h_2d_matches_oracle_both_signs() {
    for (signed, seed) in [(false, 0x1111), (true, 0x2222)] {
        let dds = make_dds(32, 32, None, 1, 1, signed, seed);
        let img = dds
            .decode_rgba_f32(SubresourceId::mip_layer(0, 0))
            .expect("decode");
        let surf = dds.surface(SubresourceId::mip_layer(0, 0)).expect("surf");
        let want = oracle_slice(surf.data, 32, 32, signed);
        assert_eq!(img.pixels, want);
        assert_eq!((img.width, img.height, img.depth), (32, 32, 1));
    }
}

#[test]
fn bc6h_npot_matches_oracle() {
    let dds = make_dds(10, 6, None, 1, 1, false, 0x3333);
    let img = dds
        .decode_rgba_f32(SubresourceId::mip_layer(0, 0))
        .expect("decode");
    let surf = dds.surface(SubresourceId::mip_layer(0, 0)).expect("surf");
    assert_eq!(img.pixels, oracle_slice(surf.data, 10, 6, false));
}

#[test]
fn bc6h_mips_and_arrays_match_oracle() {
    let dds = make_dds(16, 16, None, 3, 2, true, 0x4444);
    for layer in 0..2 {
        for mip in 0..3 {
            let id = SubresourceId::mip_layer(mip, layer);
            let img = dds.decode_rgba_f32(id).expect("decode");
            let surf = dds.surface(id).expect("surf");
            let w = img.width as usize;
            let h = img.height as usize;
            assert_eq!(img.pixels, oracle_slice(surf.data, w, h, true));
        }
    }
}

#[test]
fn bc6h_volume_stacks_slices() {
    let dds = make_dds(8, 8, Some(4), 1, 1, false, 0x5555);
    let id = SubresourceId::mip_layer(0, 0);
    let img = dds.decode_rgba_f32(id).expect("decode");
    assert_eq!(img.depth, 4);
    let surf = dds.surface(id).expect("surf");
    let slice_bytes = 2 * 2 * 16; // 8x8 -> 2x2 blocks
    let px_per_slice = 8 * 8 * 4;
    for z in 0..4usize {
        let want = oracle_slice(
            &surf.data[z * slice_bytes..(z + 1) * slice_bytes],
            8,
            8,
            false,
        );
        assert_eq!(
            &img.pixels[z * px_per_slice..(z + 1) * px_per_slice],
            &want[..]
        );
    }
}

#[test]
fn apis_fail_closed_across_domains() {
    // LDR API refuses BC6H…
    let hdr = make_dds(8, 8, None, 1, 1, false, 0x6666);
    assert!(hdr.decode_rgba8(SubresourceId::mip_layer(0, 0)).is_err());
    // …and the HDR API refuses LDR content.
    let img = ImageRgba8 {
        width: 8,
        height: 8,
        depth: 1,
        pixels: vec![128; 8 * 8 * 4],
    };
    let ldr = img.encode_dds(DecodeContent::Bc7).expect("encode bc7");
    assert!(ldr.decode_rgba_f32(SubresourceId::mip_layer(0, 0)).is_err());
}

/// The caller-parallel seam must produce byte-identical pixels to the whole-surface
/// decode, at aligned and NPOT sizes, and for every split that divides the rows.
///
/// This is the guard on a 9.6x win: BC6H at 1024^2 went from 26.4 ms serial to
/// 2.7 ms across 24 caller threads. A split that quietly decodes different pixels
/// than the whole would be worse than no split at all.
#[test]
fn hdr_block_row_split_matches_whole_surface() {
    for &(w, h) in &[(64u32, 64u32), (128, 64), (37, 53), (16, 100)] {
        let n = (w * h) as usize;
        let mut src = Vec::with_capacity(n * 4);
        for i in 0..n {
            let x = (i as u32 % w) as f32 / w as f32;
            let y = (i as u32 / w) as f32 / h as f32;
            src.extend_from_slice(&[x * 7.0, y * 3.0 + 0.25, (x + y) * 2.0, 1.0]);
        }
        let dds = Dds::encode_bc6h_uf16(&src, w, h).expect("encode");
        let id = SubresourceId::mip_layer(0, 0);

        let whole = dds.decode_rgba_f32(id).expect("whole");
        let rows = dds.block_rows_f32(id).expect("rows");
        assert_eq!(rows, h.div_ceil(4), "{w}x{h}: block row count");

        // `_into` must agree with the allocating call.
        let mut buf = Vec::new();
        dds.decode_rgba_f32_into(id, &mut buf).expect("into");
        assert_eq!(buf, whole.pixels, "{w}x{h}: _into diverged");

        // Every split point, not just the middle.
        for cut in 0..=rows {
            let mut split = vec![0f32; n * 4];
            let px = (cut * 4).min(h) as usize * w as usize * 4;
            let (top, bottom) = split.split_at_mut(px);
            dds.decode_block_rows_f32_into(id, 0..cut, top).expect("top");
            dds.decode_block_rows_f32_into(id, cut..rows, bottom)
                .expect("bottom");
            assert_eq!(split, whole.pixels, "{w}x{h}: split at row {cut} diverged");
        }
    }
}

/// Out-of-range row spans fail closed rather than reading past the payload.
#[test]
fn hdr_block_rows_reject_bad_ranges() {
    let src = vec![0.5f32; 64 * 64 * 4];
    let dds = Dds::encode_bc6h_uf16(&src, 64, 64).expect("encode");
    let id = SubresourceId::mip_layer(0, 0);
    let rows = dds.block_rows_f32(id).expect("rows");
    let mut out = vec![0f32; 64 * 64 * 4];

    assert!(dds.decode_block_rows_f32_into(id, 0..rows + 1, &mut out).is_err());
    assert!(dds.decode_block_rows_f32_into(id, 2..1, &mut out).is_err());
    // A destination that does not match the requested rows is refused, not
    // silently truncated.
    let mut small = vec![0f32; 8];
    assert!(dds.decode_block_rows_f32_into(id, 0..rows, &mut small).is_err());
}
