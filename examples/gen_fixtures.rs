//! Regenerate committed DDS fixtures under `tests/fixtures/`.
//!
//! Run from repo root:
//! ```text
//! cargo run --example gen_fixtures
//! ```
//!
//! Fixtures are valid DDS containers with deterministic payload bytes. They are
//! for parse / layout tests and benches — not golden pixel oracles.

use rusty_dds::*;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

fn main() {
    let out = fixtures_dir();
    fs::create_dir_all(&out).expect("create fixtures dir");

    write_d3d(&out, "dxt1_64x64.dds", D3DFormat::DXT1, 64, 64, None);
    write_d3d(&out, "dxt5_64x64.dds", D3DFormat::DXT5, 64, 64, None);
    write_d3d(
        &out,
        "dxt1_64x64_mips.dds",
        D3DFormat::DXT1,
        64,
        64,
        Some(7),
    );
    write_d3d(&out, "dxt3_32x32.dds", D3DFormat::DXT3, 32, 32, None);

    write_dxgi(
        &out,
        "bc1_64x64_dx10.dds",
        DxgiParams {
            format: DxgiFormat::BC1_UNorm,
            width: 64,
            height: 64,
            depth: None,
            mipmap_levels: None,
            array_layers: None,
            is_cubemap: false,
        },
    );
    write_dxgi(
        &out,
        "bc2_32x32_dx10.dds",
        DxgiParams {
            format: DxgiFormat::BC2_UNorm,
            width: 32,
            height: 32,
            depth: None,
            mipmap_levels: None,
            array_layers: None,
            is_cubemap: false,
        },
    );
    write_dxgi(
        &out,
        "bc3_64x64_dx10.dds",
        DxgiParams {
            format: DxgiFormat::BC3_UNorm,
            width: 64,
            height: 64,
            depth: None,
            mipmap_levels: None,
            array_layers: None,
            is_cubemap: false,
        },
    );
    write_dxgi(
        &out,
        "bc4_32x32_dx10.dds",
        DxgiParams {
            format: DxgiFormat::BC4_UNorm,
            width: 32,
            height: 32,
            depth: None,
            mipmap_levels: None,
            array_layers: None,
            is_cubemap: false,
        },
    );
    write_dxgi(
        &out,
        "bc5_32x32_dx10.dds",
        DxgiParams {
            format: DxgiFormat::BC5_UNorm,
            width: 32,
            height: 32,
            depth: None,
            mipmap_levels: None,
            array_layers: None,
            is_cubemap: false,
        },
    );
    write_dxgi(
        &out,
        "bc7_32x32_dx10.dds",
        DxgiParams {
            format: DxgiFormat::BC7_UNorm,
            width: 32,
            height: 32,
            depth: None,
            mipmap_levels: None,
            array_layers: None,
            is_cubemap: false,
        },
    );
    write_dxgi(
        &out,
        "rgba8_64x64.dds",
        DxgiParams {
            format: DxgiFormat::R8G8B8A8_UNorm,
            width: 64,
            height: 64,
            depth: None,
            mipmap_levels: None,
            array_layers: None,
            is_cubemap: false,
        },
    );
    write_dxgi(
        &out,
        "bgra8_32x32.dds",
        DxgiParams {
            format: DxgiFormat::B8G8R8A8_UNorm,
            width: 32,
            height: 32,
            depth: None,
            mipmap_levels: None,
            array_layers: None,
            is_cubemap: false,
        },
    );
    write_dxgi(
        &out,
        "rgba8_256x256_mips.dds",
        DxgiParams {
            format: DxgiFormat::R8G8B8A8_UNorm,
            width: 256,
            height: 256,
            depth: None,
            mipmap_levels: Some(9),
            array_layers: None,
            is_cubemap: false,
        },
    );
    write_dxgi(
        &out,
        "rgba8_32x32_array3.dds",
        DxgiParams {
            format: DxgiFormat::R8G8B8A8_UNorm,
            width: 32,
            height: 32,
            depth: None,
            mipmap_levels: None,
            array_layers: Some(3),
            is_cubemap: false,
        },
    );
    write_dxgi(
        &out,
        "bc1_32x32_cube.dds",
        DxgiParams {
            format: DxgiFormat::BC1_UNorm,
            width: 32,
            height: 32,
            depth: None,
            mipmap_levels: Some(2),
            array_layers: Some(6),
            is_cubemap: true,
        },
    );
    write_dxgi(
        &out,
        "rgba8_16x16x4_vol.dds",
        DxgiParams {
            format: DxgiFormat::R8G8B8A8_UNorm,
            width: 16,
            height: 16,
            depth: Some(4),
            mipmap_levels: None,
            array_layers: None,
            is_cubemap: false,
        },
    );
    write_dxgi(
        &out,
        "bc1_16x16x4_vol.dds",
        DxgiParams {
            format: DxgiFormat::BC1_UNorm,
            width: 16,
            height: 16,
            depth: Some(4),
            mipmap_levels: None,
            array_layers: None,
            is_cubemap: false,
        },
    );

    println!("Wrote fixtures to {}", out.display());
}

struct DxgiParams {
    format: DxgiFormat,
    width: u32,
    height: u32,
    depth: Option<u32>,
    mipmap_levels: Option<u32>,
    array_layers: Option<u32>,
    is_cubemap: bool,
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn fill_pattern(data: &mut [u8]) {
    for (i, b) in data.iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }
}

fn write_d3d(
    dir: &Path,
    name: &str,
    format: D3DFormat,
    width: u32,
    height: u32,
    mipmap_levels: Option<u32>,
) {
    let mut dds = Dds::new_d3d(NewD3dParams {
        height,
        width,
        depth: None,
        format,
        mipmap_levels,
        caps2: None,
    })
    .unwrap_or_else(|e| panic!("new_d3d {name}: {e}"));
    fill_pattern(&mut dds.data);
    write_dds(dir, name, &dds);
}

fn write_dxgi(dir: &Path, name: &str, params: DxgiParams) {
    let caps2 = if params.is_cubemap {
        Some(Caps2::CUBEMAP | Caps2::CUBEMAP_ALLFACES)
    } else {
        None
    };
    let mut dds = Dds::new_dxgi(NewDxgiParams {
        height: params.height,
        width: params.width,
        depth: params.depth,
        format: params.format,
        mipmap_levels: params.mipmap_levels,
        array_layers: params.array_layers,
        caps2,
        is_cubemap: params.is_cubemap,
        resource_dimension: if params.depth.unwrap_or(1) > 1 {
            D3D10ResourceDimension::Texture3D
        } else {
            D3D10ResourceDimension::Texture2D
        },
        alpha_mode: AlphaMode::Straight,
    })
    .unwrap_or_else(|e| panic!("new_dxgi {name}: {e}"));
    fill_pattern(&mut dds.data);
    write_dds(dir, name, &dds);
}

fn write_dds(dir: &Path, name: &str, dds: &Dds) {
    let path = dir.join(name);
    let mut file = BufWriter::new(File::create(&path).expect("create fixture file"));
    dds.write(&mut file)
        .unwrap_or_else(|e| panic!("write {name}: {e}"));
    println!("  {name} ({} data bytes)", dds.data.len());
}
