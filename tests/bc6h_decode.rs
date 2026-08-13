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
