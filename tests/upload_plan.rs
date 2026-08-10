//! Phase 3: UploadPlan pitches match wgpu / Vulkan tightly packed rules.

use rusty_dds::{
    D3D10ResourceDimension, Dds, DxgiFormat, NewDxgiParams, SubresourceId, UploadPath,
};

fn solid_bc1() -> [u8; 8] {
    // Opaque black-ish BC1 block (valid bitstream).
    [0x00, 0x00, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00]
}

fn make_bc1(width: u32, height: u32, mips: Option<u32>) -> Dds {
    let mut dds = Dds::new_dxgi(NewDxgiParams {
        height,
        width,
        depth: None,
        format: DxgiFormat::BC1_UNorm,
        mipmap_levels: mips,
        array_layers: None,
        caps2: None,
        is_cubemap: false,
        resource_dimension: D3D10ResourceDimension::Texture2D,
        alpha_mode: rusty_dds::AlphaMode::Straight,
    })
    .unwrap();
    let block = solid_bc1();
    let bx = (width + 3) / 4;
    let by = (height + 3) / 4;
    for i in 0..(bx * by) as usize {
        let o = i * 8;
        if o + 8 <= dds.data.len() {
            dds.data[o..o + 8].copy_from_slice(&block);
        }
    }
    dds
}

#[test]
fn bc1_32x32_compressed_pitches() {
    let dds = make_bc1(32, 32, None);
    let plan = dds
        .upload_plan_compressed(SubresourceId::mip_layer(0, 0))
        .unwrap();
    assert_eq!(plan.path, UploadPath::Compressed);
    assert!(plan.format.compressed);
    assert_eq!(plan.format.wgpu_name, "Bc1RgbaUnorm");
    assert_eq!(plan.format.vulkan_name, "VK_FORMAT_BC1_RGBA_UNORM_BLOCK");
    // 8 blocks wide × 8 bytes = 64 bytes per block-row; 8 block rows.
    assert_eq!(plan.bytes_per_row, 64);
    assert_eq!(plan.rows_per_image, 8);
    assert_eq!(plan.data_len, 64 * 8);
    assert_eq!(plan.data_offset, 0);
    assert_eq!(&dds.data[plan.data_offset..plan.data_offset + plan.data_len], &dds.data[..]);
}

#[test]
fn bc7_npot_block_rows_round_up() {
    let dds = Dds::new_dxgi(NewDxgiParams {
        height: 3,
        width: 2,
        depth: None,
        format: DxgiFormat::BC7_UNorm,
        mipmap_levels: None,
        array_layers: None,
        caps2: None,
        is_cubemap: false,
        resource_dimension: D3D10ResourceDimension::Texture2D,
        alpha_mode: rusty_dds::AlphaMode::Straight,
    })
    .unwrap();
    assert!(dds.data.len() >= 16);
    let plan = dds
        .upload_plan_compressed(SubresourceId::mip_layer(0, 0))
        .unwrap();
    // ceil(2/4)=1 block wide × 16 bytes; ceil(3/4)=1 block row.
    assert_eq!(plan.bytes_per_row, 16);
    assert_eq!(plan.rows_per_image, 1);
    assert_eq!(plan.data_len, 16);
    assert_eq!(plan.format.wgpu_name, "Bc7RgbaUnorm");
}

#[test]
fn rgba8_decoded_plan() {
    let dds = Dds::new_dxgi(NewDxgiParams {
        height: 8,
        width: 8,
        depth: None,
        format: DxgiFormat::R8G8B8A8_UNorm,
        mipmap_levels: None,
        array_layers: None,
        caps2: None,
        is_cubemap: false,
        resource_dimension: D3D10ResourceDimension::Texture2D,
        alpha_mode: rusty_dds::AlphaMode::Straight,
    })
    .unwrap();
    let plan = dds
        .upload_plan_decoded_rgba8(SubresourceId::mip_layer(0, 0))
        .unwrap();
    assert_eq!(plan.path, UploadPath::DecodedRgba8);
    assert_eq!(plan.bytes_per_row, 32);
    assert_eq!(plan.rows_per_image, 8);
    assert_eq!(plan.data_len, 8 * 8 * 4);
    assert_eq!(plan.data_offset, 0);
    assert_eq!(plan.format.wgpu_name, "Rgba8Unorm");

    let compressed = dds
        .upload_plan_compressed(SubresourceId::mip_layer(0, 0))
        .unwrap();
    assert_eq!(compressed.path, UploadPath::Compressed);
    assert!(!compressed.format.compressed);
    assert_eq!(compressed.bytes_per_row, 32);
    assert_eq!(compressed.rows_per_image, 8);
}

#[test]
fn mip_tip_uses_subresource_offset() {
    let dds = make_bc1(32, 32, Some(6));
    let tip = dds
        .upload_plan_compressed(SubresourceId::mip_layer(5, 0))
        .unwrap();
    assert!(tip.data_offset > 0);
    assert_eq!(tip.width, 1);
    assert_eq!(tip.height, 1);
    assert_eq!(tip.bytes_per_row, 8);
    assert_eq!(tip.rows_per_image, 1);
    assert_eq!(tip.data_len, 8);
}
