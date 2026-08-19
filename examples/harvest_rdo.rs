//! RDO rate-distortion ladder: for each lambda, encode the BC1 corpus,
//! deflate the payloads (the zip/p4k distribution channel), and report
//! compressed size vs round-trip PSNR.
//!
//! ```text
//! cargo run --release --example harvest_rdo
//! ```

use rusty_dds::*;
use std::io::BufReader;
use std::path::{Path, PathBuf};

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut maps: Vec<(String, u32, u32, Vec<u8>)> = Vec::new();

    // ambientCG albedo PNGs
    for asset in ["Bricks097", "Metal063", "Rock064", "Wood095"] {
        let p = root.join(format!("corpus/raw/{asset}/{asset}_1K-PNG_Color.png"));
        if let Ok((w, h, rgba)) = load_png_rgba(&p) {
            maps.push((format!("{asset}_albedo"), w, h, rgba));
        }
    }
    // CryTIF + SIPI color TIFFs
    for (dir, tag) in [("corpus/raw_crytif", "crytif"), ("corpus/raw_tif", "tif")] {
        let d = root.join(dir);
        if !d.exists() {
            continue;
        }
        let mut files: Vec<PathBuf> = std::fs::read_dir(&d)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension()
                    .map(|e| {
                        let e = e.to_string_lossy().to_ascii_lowercase();
                        e == "tif" || e == "tiff"
                    })
                    .unwrap_or(false)
            })
            .collect();
        files.sort();
        for p in files {
            if let Ok((w, h, rgba)) = load_tiff_rgba(&p) {
                let stem = p.file_stem().unwrap().to_string_lossy();
                maps.push((format!("{tag}_{stem}"), w, h, rgba));
            }
        }
    }
    assert!(!maps.is_empty(), "no corpus content");

    println!(
        "{} maps; lambda ladder over BC1; rate = deflate(level 8) of the DDS payload",
        maps.len()
    );
    println!(
        "{:>8} {:>12} {:>10} {:>10} {:>9} {:>9}",
        "lambda", "deflate_B", "vs l0", "raw_B", "psnr_dB", "d_psnr"
    );

    if std::env::var("RUSTY_DDS_RDO_PERMAP").is_ok() {
        per_map(&maps);
        return;
    }
    let content = match std::env::var("RUSTY_DDS_RDO_FMT").as_deref() {
        Ok("bc7") => DecodeContent::Bc7,
        _ => DecodeContent::Bc1,
    };
    println!("format: {}", content.name());
    let ladder = [0.0f32, 2.0, 4.0, 6.0, 10.0, 25.0, 50.0, 100.0, 200.0];
    let mut base_size = 0usize;
    let mut base_psnr = 0.0f64;
    for lam in ladder {
        let mut total_deflate = 0usize;
        let mut total_raw = 0usize;
        let mut sse = 0.0f64;
        let mut n = 0usize;
        for (_name, w, h, rgba) in &maps {
            let layout = EncodeLayout::flat_2d(content, *w, *h).with_rdo(Rdo::lambda(lam));
            let dds = Dds::encode_from_rgba8(rgba, layout).expect("encode");
            total_raw += dds.data.len();
            total_deflate +=
                miniz_oxide::deflate::compress_to_vec(&dds.data, 8).len();
            let img = dds
                .decode_rgba8(SubresourceId::mip_layer(0, 0))
                .expect("decode");
            let nch = if content == DecodeContent::Bc7 { 4 } else { 3 };
            for (a, b) in img.pixels.chunks_exact(4).zip(rgba.chunks_exact(4)) {
                for c in 0..nch {
                    let d = a[c] as f64 - b[c] as f64;
                    sse += d * d;
                    n += 1;
                }
            }
        }
        let psnr = 10.0 * (255.0f64 * 255.0 / (sse / n as f64)).log10();
        if lam == 0.0 {
            base_size = total_deflate;
            base_psnr = psnr;
        }
        println!(
            "{:>8} {:>12} {:>9.2}% {:>10} {:>9.3} {:>+9.3}",
            lam,
            total_deflate,
            100.0 * total_deflate as f64 / base_size as f64,
            total_raw,
            psnr,
            psnr - base_psnr
        );
    }
}

fn load_png_rgba(path: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut dec = png::Decoder::new(BufReader::new(f));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .chunks_exact(3)
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect(),
        other => return Err(format!("png color type {other:?}")),
    };
    Ok((info.width, info.height, rgba))
}

fn load_tiff_rgba(path: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    use tiff::decoder::DecodingResult;
    use tiff::ColorType;
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut dec = tiff::decoder::Decoder::new(BufReader::new(f)).map_err(|e| e.to_string())?;
    let (w, h) = dec.dimensions().map_err(|e| e.to_string())?;
    let ct = dec.colortype().map_err(|e| e.to_string())?;
    let img = dec.read_image().map_err(|e| e.to_string())?;
    let rgba = match (ct, img) {
        (ColorType::RGBA(8), DecodingResult::U8(v)) => v,
        (ColorType::RGB(8), DecodingResult::U8(v)) => v
            .chunks_exact(3)
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect(),
        (ColorType::Gray(8), DecodingResult::U8(v)) => {
            v.iter().flat_map(|&g| [g, g, g, 255]).collect()
        }
        (ct, _) => return Err(format!("tiff color type {ct:?}")),
    };
    Ok((w, h, rgba))
}

fn per_map(maps: &[(String, u32, u32, Vec<u8>)]) {
    let content = match std::env::var("RUSTY_DDS_RDO_FMT").as_deref() {
        Ok("bc7") => DecodeContent::Bc7,
        _ => DecodeContent::Bc1,
    };
    let lams: [f32; 3] = if content == DecodeContent::Bc7 {
        [0.0, 4.0, 10.0]
    } else {
        [0.0, 50.0, 100.0]
    };
    println!(
        "{:<34} {:>9} {:>9} {:>8} | {:>9} {:>9} {:>8}",
        "map", "l50_size%", "l50_dDB", "", "l100_size%", "l100_dDB", ""
    );
    for (name, w, h, rgba) in maps {
        let mut res = Vec::new();
        let nch = if content == DecodeContent::Bc7 { 4 } else { 3 };
        for lam in lams {
            let layout = EncodeLayout::flat_2d(content, *w, *h).with_rdo(Rdo::lambda(lam));
            let dds = Dds::encode_from_rgba8(rgba, layout).unwrap();
            let z = miniz_oxide::deflate::compress_to_vec(&dds.data, 8).len();
            let img = dds.decode_rgba8(SubresourceId::mip_layer(0, 0)).unwrap();
            let mut sse = 0.0f64;
            let mut n = 0usize;
            for (a, b) in img.pixels.chunks_exact(4).zip(rgba.chunks_exact(4)) {
                for c in 0..nch {
                    let d = a[c] as f64 - b[c] as f64;
                    sse += d * d;
                    n += 1;
                }
            }
            let psnr = 10.0 * (255.0f64 * 255.0 / (sse / n as f64)).log10();
            res.push((z, psnr));
        }
        let (z0, p0) = res[0];
        println!(
            "{:<34} {:>8.2}% {:>+9.3} {:>8} | {:>8.2}% {:>+9.3}",
            name,
            100.0 * res[1].0 as f64 / z0 as f64,
            res[1].1 - p0,
            "",
            100.0 * res[2].0 as f64 / z0 as f64,
            res[2].1 - p0,
        );
    }
}
