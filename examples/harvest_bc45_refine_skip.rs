//! Drive BC4/5 refine harvest across the ambientCG proxy corpus.
//!
//! ```text
//! cargo run --release --example harvest_bc45_refine_skip
//! python target/sweep_bc45_refine_skip.py
//! ```

use rusty_dds::*;
use std::fs;
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let corpus = root.join("corpus");
    let csv = root.join("docs/artifacts/bc45-refine-skip-harvest.csv");
    fs::create_dir_all(csv.parent().unwrap()).ok();
    // SAFETY: single-threaded example; set before any encode touches OnceLock.
    std::env::set_var("RUSTY_DDS_BC45_REFINE_HARVEST", &csv);

    let manifest: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(corpus.join("manifest.json")).expect("manifest"),
    )
    .expect("json");

    for entry in manifest["entries"].as_array().unwrap() {
        let role = entry["role"].as_str().unwrap();
        if role != "normal" && role != "mask" {
            continue;
        }
        let id = entry["id"].as_str().unwrap();
        let png = corpus.join(entry["path"].as_str().unwrap());
        if !png.exists() {
            eprintln!("skip missing {}", png.display());
            continue;
        }
        let (w, h, rgba) = load_role_png(&png, role).unwrap_or_else(|e| panic!("{id}: {e}"));
        let targets = entry["targets"].as_array().unwrap();
        for t in targets {
            let label = t.as_str().unwrap();
            let content = match label {
                "bc4u" => DecodeContent::Bc4UNorm,
                "bc4s" => DecodeContent::Bc4SNorm,
                "bc5u" => DecodeContent::Bc5UNorm,
                "bc5s" => DecodeContent::Bc5SNorm,
                _ => continue,
            };
            let layout = EncodeLayout::flat_2d(content, w, h);
            let _ = Dds::encode_from_rgba8(&rgba, layout).expect("encode");
            eprintln!("harvested {id} ({label})");
        }
    }
    eprintln!("wrote {}", csv.display());
}

fn load_role_png(path: &std::path::Path, role: &str) -> Result<(u32, u32, Vec<u8>), String> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let decoder = png::Decoder::new(file);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let data = &buf[..info.buffer_size()];
    let data8: Vec<u8> = match info.bit_depth {
        png::BitDepth::Eight => data.to_vec(),
        png::BitDepth::Sixteen => data.chunks_exact(2).map(|c| c[0]).collect(),
        other => return Err(format!("{other:?}")),
    };
    let mut rgba = Vec::new();
    match (info.color_type, role) {
        (png::ColorType::Rgb, "normal") => {
            for c in data8.chunks_exact(3) {
                rgba.extend_from_slice(&[c[0], c[1], 0, 255]);
            }
        }
        (png::ColorType::Rgba, "normal") => {
            for c in data8.chunks_exact(4) {
                rgba.extend_from_slice(&[c[0], c[1], 0, 255]);
            }
        }
        (png::ColorType::Grayscale, "mask") => {
            for &g in &data8 {
                rgba.extend_from_slice(&[g, 0, 0, 255]);
            }
        }
        (png::ColorType::Rgb, "mask") => {
            for c in data8.chunks_exact(3) {
                rgba.extend_from_slice(&[c[0], 0, 0, 255]);
            }
        }
        (png::ColorType::Rgba, "mask") => {
            for c in data8.chunks_exact(4) {
                rgba.extend_from_slice(&[c[0], 0, 0, 255]);
            }
        }
        other => return Err(format!("{other:?}")),
    }
    Ok((info.width, info.height, rgba))
}
