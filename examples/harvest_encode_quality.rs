//! Measure round-trip PSNR for the same single-surface C×X grid as
//! `bench_baselines` encode. Writes `docs/artifacts/encode-quality.json`.
//!
//! ```text
//! cargo run --release --example harvest_encode_quality
//! ```

use rusty_dds::*;
use std::fs;
use std::path::PathBuf;

struct Ctx {
    name: &'static str,
    width: u32,
    height: u32,
    depth: u32,
}

const CONTEXTS: &[Ctx] = &[
    Ctx {
        name: "X-2D",
        width: 32,
        height: 32,
        depth: 1,
    },
    Ctx {
        name: "X-MIP",
        width: 4,
        height: 4,
        depth: 1,
    },
    Ctx {
        name: "X-ARRAY",
        width: 16,
        height: 16,
        depth: 1,
    },
    Ctx {
        name: "X-CUBE",
        width: 16,
        height: 16,
        depth: 1,
    },
    Ctx {
        name: "X-NPOT",
        width: 2,
        height: 3,
        depth: 1,
    },
    Ctx {
        name: "X-VOL",
        width: 8,
        height: 8,
        depth: 4,
    },
];

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out = root.join("docs/artifacts/encode-quality.json");
    let mut rows = Vec::new();

    for ctx in CONTEXTS {
        for &content in DecodeContent::ALL_LDR {
            if ctx.name == "X-CUBE"
                && !matches!(
                    content,
                    DecodeContent::Bc1
                        | DecodeContent::Bc3
                        | DecodeContent::Bc7
                        | DecodeContent::Rgba8
                )
            {
                continue;
            }
            let id = format!("{}__{}", content.name(), ctx.name);
            let pixels = fill_rgba(content, ctx.width, ctx.height, ctx.depth);
            let layout = EncodeLayout {
                content,
                width: ctx.width,
                height: ctx.height,
                depth: ctx.depth,
                mipmap_levels: 1,
                array_layers: 1,
                is_cubemap: false,
        quality: EncodeQuality::Quality,
            };
            let floor = psnr_floor(content);
            let channels = channels_for(content);

            let result = (|| -> Result<(f64, bool, Option<u8>), String> {
                let dds = Dds::encode_from_rgba8(&pixels, layout).map_err(|e| e.to_string())?;
                let img = dds
                    .decode_rgba8(SubresourceId::mip_layer(0, 0))
                    .map_err(|e| e.to_string())?;
                match content {
                    DecodeContent::Rgba8 | DecodeContent::Bgra8 => {
                        let exact = img.pixels == pixels;
                        Ok((
                            if exact {
                                f64::INFINITY
                            } else {
                                psnr_channels(&img.pixels, &pixels, channels).unwrap_or(0.0)
                            },
                            exact,
                            max_abs_diff(&img.pixels, &pixels),
                        ))
                    }
                    _ => {
                        let psnr = psnr_channels(&img.pixels, &pixels, channels)
                            .ok_or_else(|| "psnr failed".to_string())?;
                        Ok((psnr, psnr >= floor, max_abs_diff(&img.pixels, &pixels)))
                    }
                }
            })();

            let (psnr, pass, max_abs, ok, err) = match result {
                Ok((psnr, pass, max_abs)) => (Some(psnr), pass, max_abs, true, None),
                Err(e) => (None, false, None, false, Some(e)),
            };

            rows.push(serde_json::json!({
                "id": id,
                "content": content.name(),
                "context": ctx.name,
                "psnr_db": finite_or_null(psnr),
                "psnr_infinite": psnr.map(|p| p.is_infinite()).unwrap_or(false),
                "floor_db": if floor.is_infinite() { serde_json::Value::Null } else { json_f(floor) },
                "bit_exact_gate": matches!(content, DecodeContent::Rgba8 | DecodeContent::Bgra8),
                "pass": pass,
                "max_abs": max_abs,
                "ok": ok,
                "error": err,
            }));
            println!(
                "{:<22} psnr={:>8}  floor={:>5}  pass={}",
                id,
                psnr
                    .map(|p| if p.is_infinite() {
                        "inf".into()
                    } else {
                        format!("{p:.2}")
                    })
                    .unwrap_or_else(|| "fail".into()),
                if floor.is_infinite() {
                    "exact".into()
                } else {
                    format!("{floor:.0}")
                },
                pass
            );
        }
    }

    let passed = rows.iter().filter(|r| r["pass"].as_bool() == Some(true)).count();
    let report = serde_json::json!({
        "protocol": "encode_from_rgba8 → decode_rgba8 round-trip PSNR on preserved channels",
        "notes": [
            "Same single-surface C×X sizes as bench_baselines encode.",
            "RGBA/BGRA: bit-exact (reported as infinite PSNR when exact).",
            "BC4: R only; BC5: R+G; BC1: RGB; else full RGBA.",
            "Floors match docs/artifacts/encode-quality.md / tests/encode_matrix.rs.",
        ],
        "rows": rows,
        "summary": {
            "cases": rows.len(),
            "passed": passed,
            "failed": rows.len() - passed,
        },
    });
    fs::write(&out, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    println!("Wrote {}", out.display());
}

fn finite_or_null(p: Option<f64>) -> serde_json::Value {
    match p {
        Some(v) if v.is_finite() => json_f(v),
        Some(_) => serde_json::Value::Null,
        None => serde_json::Value::Null,
    }
}

fn json_f(v: f64) -> serde_json::Value {
    serde_json::json!((v * 100.0).round() / 100.0)
}

fn psnr_floor(content: DecodeContent) -> f64 {
    match content {
        DecodeContent::Rgba8 | DecodeContent::Bgra8 => f64::INFINITY,
        DecodeContent::Bc7 => 22.0,
        DecodeContent::Bc1 | DecodeContent::Bc2 | DecodeContent::Bc3 => 18.0,
        DecodeContent::Bc4UNorm
        | DecodeContent::Bc4SNorm
        | DecodeContent::Bc5UNorm
        | DecodeContent::Bc5SNorm => 28.0,
    }
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
    let mse = sse / n as f64;
    Some(10.0 * (255.0f64 * 255.0 / mse).log10())
}

fn fill_rgba(content: DecodeContent, w: u32, h: u32, d: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * d * 4) as usize);
    for z in 0..d {
        for y in 0..h {
            for x in 0..w {
                let px = match content {
                    DecodeContent::Bc4UNorm | DecodeContent::Bc4SNorm => {
                        [((x * 200 / w.max(1)) as u8).min(200), 0, 0, 255]
                    }
                    DecodeContent::Bc5UNorm | DecodeContent::Bc5SNorm => [
                        ((x * 200) / w.max(1)).min(200) as u8,
                        ((y * 200) / h.max(1)).min(200) as u8,
                        0,
                        255,
                    ],
                    DecodeContent::Bc1 => [
                        ((x * 255) / w.max(1)) as u8,
                        ((y * 255) / h.max(1)) as u8,
                        ((z * 40) % 256) as u8,
                        255,
                    ],
                    _ => [
                        ((x * 255) / w.max(1)) as u8,
                        ((y * 255) / h.max(1)) as u8,
                        ((z * 40) % 256) as u8,
                        200u8.wrapping_add((x + y) as u8),
                    ],
                };
                v.extend_from_slice(&px);
            }
        }
    }
    v
}
