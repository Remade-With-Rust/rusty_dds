//! Decode completeness Criterion matrix: `rusty_dds` vs `reference` (`bcdec_rs` tiling).
//!
//! Covers every LDR content type at X-2D, plus representative context arms.
//!
//! ```text
//! cargo bench --bench decode_ab
//! ```

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use rusty_dds::*;

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

fn fill(data: &mut [u8], content: DecodeContent) {
    for (i, b) in data.iter_mut().enumerate() {
        *b = ((i * 37 + 11) % 251) as u8;
    }
    // Prefer structured blocks for color formats so both paths stay busy.
    if let Some(bs) = content.block_bytes() {
        if matches!(
            content,
            DecodeContent::Bc1 | DecodeContent::Bc2 | DecodeContent::Bc3
        ) {
            let mut block = vec![0u8; bs];
            if content == DecodeContent::Bc1 {
                block[0..2].copy_from_slice(&0xF800u16.to_le_bytes());
            } else if content == DecodeContent::Bc2 {
                block[..8].fill(0xFF);
                block[8..10].copy_from_slice(&0xF800u16.to_le_bytes());
            } else {
                block[0] = 255;
                block[8..10].copy_from_slice(&0xF800u16.to_le_bytes());
            }
            for chunk in data.chunks_exact_mut(bs) {
                chunk.copy_from_slice(&block);
            }
        }
    }
}

fn make(
    content: DecodeContent,
    w: u32,
    h: u32,
    depth: Option<u32>,
    mips: Option<u32>,
    arrays: Option<u32>,
    cube: bool,
) -> Dds {
    let mut dds = Dds::new_dxgi(NewDxgiParams {
        height: h,
        width: w,
        depth,
        format: dxgi_for(content),
        mipmap_levels: mips,
        array_layers: arrays,
        caps2: if cube {
            Some(Caps2::CUBEMAP | Caps2::CUBEMAP_ALLFACES)
        } else {
            None
        },
        is_cubemap: cube,
        resource_dimension: if depth.unwrap_or(1) > 1 {
            D3D10ResourceDimension::Texture3D
        } else {
            D3D10ResourceDimension::Texture2D
        },
        alpha_mode: AlphaMode::Straight,
    })
    .unwrap();
    fill(&mut dds.data, content);
    dds
}

fn bench_pair(c: &mut Criterion, label: &str, dds: &Dds, id: SubresourceId) {
    let content = dds.decode_content().unwrap();
    let surf = dds.surface(id).unwrap();
    let raw = surf.data.to_vec();
    let (w, h, d) = (surf.width, surf.height, surf.depth);

    let mut group = c.benchmark_group(format!("decode/{label}"));
    group.bench_function(BenchmarkId::new("rusty_dds", content.name()), |b| {
        b.iter(|| {
            let img = dds.decode_rgba8(id).unwrap();
            black_box(img.pixels.len())
        })
    });
    group.bench_function(BenchmarkId::new("bcdec_ref", content.name()), |b| {
        b.iter(|| {
            let pixels = reference::reference_rgba8(content, &raw, w, h, d).unwrap();
            black_box(pixels.len())
        })
    });
    group.finish();
}

fn bench_decode_matrix(c: &mut Criterion) {
    // X-2D: every LDR content type
    for &content in DecodeContent::ALL_LDR {
        let dds = make(content, 64, 64, None, Some(1), None, false);
        bench_pair(
            c,
            &format!("X-2D/{}", content.name()),
            &dds,
            SubresourceId::mip_layer(0, 0),
        );
    }

    // X-MIP tip
    let dds = make(DecodeContent::Bc1, 64, 64, None, Some(7), None, false);
    let last = dds.get_num_mipmap_levels() - 1;
    bench_pair(c, "X-MIP/bc1_tip", &dds, SubresourceId::mip_layer(last, 0));

    // X-ARRAY
    let dds = make(DecodeContent::Bc3, 32, 32, None, Some(1), Some(4), false);
    bench_pair(c, "X-ARRAY/bc3_L2", &dds, SubresourceId::mip_layer(0, 2));

    // X-CUBE
    let dds = make(DecodeContent::Bc1, 32, 32, None, Some(1), Some(6), true);
    bench_pair(
        c,
        "X-CUBE/bc1_negz",
        &dds,
        SubresourceId::cubemap(0, 0, CubemapFace::NegativeZ),
    );

    // X-NPOT
    let dds = make(DecodeContent::Bc7, 2, 3, None, Some(1), None, false);
    bench_pair(c, "X-NPOT/bc7_2x3", &dds, SubresourceId::mip_layer(0, 0));

    // X-VOL
    let dds = make(DecodeContent::Rgba8, 16, 16, Some(8), Some(1), None, false);
    bench_pair(c, "X-VOL/rgba8_d8", &dds, SubresourceId::mip_layer(0, 0));
}

criterion_group!(benches, bench_decode_matrix);
criterion_main!(benches);
