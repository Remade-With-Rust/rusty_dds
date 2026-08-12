//! Real-content encode gate: PNG corpus (ambientCG roles) + real .tif / CryTIF corpus.
//!
//! Quality (round-trip PSNR, preserved channels) is measured in ONE deterministic
//! pass; speed is a SEPARATE best-of-N encode-only timing pass (never fused —
//! deterministic and timed quantities take different N).
//!
//! ```text
//! cargo run --release --example bench_encode_corpus            # table + JSON
//! RUSTY_DDS_ITERS=9 ... bench_encode_corpus -- --json out.json # custom output
//! ```
//!
//! JSON rows are stable-keyed (`map__content`) so two runs (baseline vs candidate
//! binary) diff cleanly: quality per-row must not regress, total ns must drop.

use rusty_dds::*;
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::Instant;

struct Item {
    name: String,
    w: u32,
    h: u32,
    rgba: Vec<u8>,
    targets: Vec<DecodeContent>,
}

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let iters: u32 = std::env::var("RUSTY_DDS_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let json_out = {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mut out = root.join("target/encode_corpus_bench.json");
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if a == "--json" {
                if let Some(p) = it.next() {
                    out = PathBuf::from(p);
                }
            }
        }
        out
    };

    let mut items: Vec<Item> = Vec::new();

    // --- ambientCG PNG corpus (role-shaped) ------------------------------
    let raw = root.join("corpus/raw");
    if raw.exists() {
        let mut dirs: Vec<PathBuf> = fs::read_dir(&raw)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir() && !p.file_name().unwrap().to_string_lossy().starts_with('_'))
            .collect();
        dirs.sort();
        for dir in dirs {
            let asset = dir.file_name().unwrap().to_string_lossy().to_string();
            for entry in fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()) {
                let p = entry.path();
                let fname = p.file_name().unwrap().to_string_lossy().to_string();
                let (role, targets): (&str, Vec<DecodeContent>) = if fname.ends_with("Color.png") {
                    ("albedo", vec![DecodeContent::Bc1, DecodeContent::Bc7])
                } else if fname.ends_with("NormalGL.png") {
                    (
                        "normal",
                        vec![DecodeContent::Bc5UNorm, DecodeContent::Bc5SNorm],
                    )
                } else if fname.ends_with("Roughness.png") {
                    (
                        "mask",
                        vec![DecodeContent::Bc4UNorm, DecodeContent::Bc4SNorm],
                    )
                } else {
                    continue;
                };
                match load_png_rgba(&p) {
                    Ok((w, h, rgba)) => items.push(Item {
                        name: format!("{asset}_{role}"),
                        w,
                        h,
                        rgba,
                        targets,
                    }),
                    Err(e) => eprintln!("skip {}: {e}", p.display()),
                }
            }
        }
    }

    // --- real TIFF corpora -----------------------------------------------
    for (sub, tag) in [("corpus/raw_crytif", "crytif"), ("corpus/raw_tif", "tif")] {
        let dir = root.join(sub);
        if !dir.exists() {
            continue;
        }
        let mut files: Vec<PathBuf> = fs::read_dir(&dir)
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
            let stem = p.file_stem().unwrap().to_string_lossy().to_string();
            match load_tiff_rgba(&p) {
                Ok((w, h, rgba, gray)) => {
                    let targets = if gray {
                        vec![
                            DecodeContent::Bc4UNorm,
                            DecodeContent::Bc4SNorm,
                            DecodeContent::Bc1,
                        ]
                    } else {
                        vec![DecodeContent::Bc1, DecodeContent::Bc3, DecodeContent::Bc7]
                    };
                    items.push(Item {
                        name: format!("{tag}_{stem}"),
                        w,
                        h,
                        rgba,
                        targets,
                    });
                }
                Err(e) => eprintln!("skip {}: {e}", p.display()),
            }
        }
    }

    if items.is_empty() {
        eprintln!("ERROR: no corpus content found (corpus/raw, corpus/raw_crytif, corpus/raw_tif)");
        std::process::exit(1);
    }

    println!(
        "{} maps, iters={iters} (timing best-of-N; quality single pass)",
        items.len()
    );
    println!(
        "{:<38} {:>10} {:>12} {:>10}",
        "case", "psnr_db", "best_ns", "Mpx/s"
    );

    let filter = std::env::var("RUSTY_DDS_FILTER").ok();
    let mut rows = Vec::new();
    let mut total_best_ns = 0u128;

    for item in &items {
        for &content in &item.targets {
            let case = format!("{}__{}", item.name, content.name());
            if let Some(f) = &filter {
                if !case.contains(f.as_str()) {
                    continue;
                }
            }
            let layout = EncodeLayout::flat_2d(content, item.w, item.h);

            // Quality pass (deterministic, once).
            let (psnr, out_len, fnv) = match Dds::encode_from_rgba8(&item.rgba, layout) {
                Ok(dds) => {
                    let img = dds
                        .decode_rgba8(SubresourceId::mip_layer(0, 0))
                        .expect("decode");
                    let recon = if matches!(
                        content,
                        DecodeContent::Bc4SNorm | DecodeContent::Bc5SNorm
                    ) {
                        snorm_bits_rgba_to_unorm(&img.pixels)
                    } else {
                        img.pixels.clone()
                    };
                    (
                        psnr_channels(&recon, &item.rgba, channels_for(content)),
                        dds.data.len(),
                        fnv1a64(&dds.data),
                    )
                }
                Err(e) => {
                    eprintln!("{case}: encode failed: {e}");
                    continue;
                }
            };

            // Timing pass (best of N, encode only).
            let mut best = u128::MAX;
            for _ in 0..iters {
                let t0 = Instant::now();
                let dds = Dds::encode_from_rgba8(&item.rgba, layout).unwrap();
                let ns = t0.elapsed().as_nanos();
                std::hint::black_box(&dds.data);
                best = best.min(ns);
            }
            total_best_ns += best;
            let mpxs = (item.w as f64 * item.h as f64) / (best as f64 / 1e9) / 1e6;

            println!(
                "{:<38} {:>10} {:>12} {:>10.1}",
                case,
                psnr.map(|p| if p.is_infinite() {
                    "inf".into()
                } else {
                    format!("{p:.3}")
                })
                .unwrap_or_else(|| "-".into()),
                best,
                mpxs
            );

            rows.push(serde_json::json!({
                "case": case,
                "content": content.name(),
                "w": item.w,
                "h": item.h,
                "psnr_db": psnr.filter(|p| p.is_finite()),
                "psnr_infinite": psnr.map(|p| p.is_infinite()).unwrap_or(false),
                "best_ns": best as u64,
                "payload_bytes": out_len,
                "payload_fnv": format!("{fnv:016x}"),
            }));
        }
    }

    let report = serde_json::json!({
        "iters": iters,
        "method": "quality: single deterministic round-trip pass; speed: best-of-N encode-only wall (pin externally for A/B)",
        "total_best_ns": total_best_ns as u64,
        "rows": rows,
    });
    fs::create_dir_all(json_out.parent().unwrap()).ok();
    fs::write(&json_out, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    println!("\ntotal best_ns = {total_best_ns}");
    println!("wrote {}", json_out.display());
}

// --- loaders ---------------------------------------------------------------

fn load_png_rgba(path: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    let f = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut dec = png::Decoder::new(BufReader::new(f));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(info.buffer_size());
    let (w, h) = (info.width, info.height);
    let rgba = expand_to_rgba(&buf, info.color_type)?;
    if rgba.len() != (w as usize) * (h as usize) * 4 {
        return Err("unexpected png buffer size".into());
    }
    Ok((w, h, rgba))
}

fn expand_to_rgba(buf: &[u8], ct: png::ColorType) -> Result<Vec<u8>, String> {
    Ok(match ct {
        png::ColorType::Rgba => buf.to_vec(),
        png::ColorType::Rgb => buf
            .chunks_exact(3)
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf
            .chunks_exact(2)
            .flat_map(|c| [c[0], c[0], c[0], c[1]])
            .collect(),
        other => return Err(format!("unsupported png color type {other:?}")),
    })
}

/// Returns (w, h, rgba, is_grayscale).
fn load_tiff_rgba(path: &Path) -> Result<(u32, u32, Vec<u8>, bool), String> {
    use tiff::decoder::DecodingResult;
    use tiff::ColorType;
    let f = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut dec = tiff::decoder::Decoder::new(BufReader::new(f)).map_err(|e| e.to_string())?;
    let (w, h) = dec.dimensions().map_err(|e| e.to_string())?;
    let ct = dec.colortype().map_err(|e| e.to_string())?;
    let img = dec.read_image().map_err(|e| e.to_string())?;
    let (rgba, gray) = match (ct, img) {
        (ColorType::RGBA(8), DecodingResult::U8(v)) => (v, false),
        (ColorType::RGB(8), DecodingResult::U8(v)) => (
            v.chunks_exact(3)
                .flat_map(|c| [c[0], c[1], c[2], 255])
                .collect(),
            false,
        ),
        (ColorType::Gray(8), DecodingResult::U8(v)) => (
            v.iter().flat_map(|&g| [g, g, g, 255]).collect(),
            true,
        ),
        (ColorType::Gray(16), DecodingResult::U16(v)) => (
            v.iter()
                .flat_map(|&g| {
                    let b = (g >> 8) as u8;
                    [b, b, b, 255]
                })
                .collect(),
            true,
        ),
        (ColorType::RGB(16), DecodingResult::U16(v)) => (
            v.chunks_exact(3)
                .flat_map(|c| [(c[0] >> 8) as u8, (c[1] >> 8) as u8, (c[2] >> 8) as u8, 255])
                .collect(),
            false,
        ),
        (ColorType::RGBA(16), DecodingResult::U16(v)) => (
            v.chunks_exact(4)
                .flat_map(|c| {
                    [
                        (c[0] >> 8) as u8,
                        (c[1] >> 8) as u8,
                        (c[2] >> 8) as u8,
                        (c[3] >> 8) as u8,
                    ]
                })
                .collect(),
            false,
        ),
        (ct, _) => return Err(format!("unsupported tiff color type {ct:?}")),
    };
    if rgba.len() != (w as usize) * (h as usize) * 4 {
        return Err(format!(
            "tiff buffer size mismatch: {} vs {}x{}x4",
            rgba.len(),
            w,
            h
        ));
    }
    Ok((w, h, rgba, gray))
}

// --- scoring ---------------------------------------------------------------

fn fnv1a64(data: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// SNORM-bit reconstructions come back as raw SNORM bytes; map to UNORM domain
/// so PSNR compares in the source domain (matches harvest_corpus_vs_dxtex).
fn snorm_bits_rgba_to_unorm(px: &[u8]) -> Vec<u8> {
    let mut out = px.to_vec();
    for p in out.chunks_exact_mut(4) {
        p[0] = snorm_u8_bits_to_unorm(p[0]);
        p[1] = snorm_u8_bits_to_unorm(p[1]);
        p[2] = snorm_u8_bits_to_unorm(p[2]);
    }
    out
}

fn snorm_u8_bits_to_unorm(b: u8) -> u8 {
    let s = (b as i8 as i32).clamp(-127, 127);
    ((((s + 127) * 255) + 127) / 254) as u8
}

fn channels_for(content: DecodeContent) -> &'static [usize] {
    match content {
        DecodeContent::Bc4UNorm | DecodeContent::Bc4SNorm => &[0],
        DecodeContent::Bc5UNorm | DecodeContent::Bc5SNorm => &[0, 1],
        DecodeContent::Bc1 => &[0, 1, 2],
        _ => &[0, 1, 2, 3],
    }
}

fn psnr_channels(a: &[u8], b: &[u8], channels: &[usize]) -> Option<f64> {
    if a.len() != b.len() || a.len() % 4 != 0 {
        return None;
    }
    let mut sse = 0.0f64;
    let mut n = 0usize;
    for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        for &c in channels {
            let d = pa[c] as f64 - pb[c] as f64;
            sse += d * d;
            n += 1;
        }
    }
    if n == 0 {
        return None;
    }
    if sse == 0.0 {
        return Some(f64::INFINITY);
    }
    Some(10.0 * (255.0f64 * 255.0 / (sse / n as f64)).log10())
}
