//! Round-trip PSNR: rusty_dds vs Microsoft DirectXTex on **identical** RGBA sources.
//!
//! ```text
//! cargo run --release --example harvest_encode_quality_vs_dxtex
//! ```
//!
//! Requires `tools/dxtex_decode_bench/build/dxtex_roundtrip[.exe]`.
//! Writes `docs/artifacts/encode-quality-vs-directxtex.{json,md}`.

use rusty_dds::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    let artifacts = root.join("docs/artifacts");
    let work = root.join("target/quality_vs_dxtex");
    fs::create_dir_all(&artifacts).unwrap();
    fs::create_dir_all(&work).unwrap();

    let dx_exe = find_dxtex_roundtrip(&root);
    if dx_exe.is_none() {
        eprintln!("ERROR: dxtex_roundtrip not found. Build tools/dxtex_decode_bench first.");
        std::process::exit(1);
    }
    let dx_exe = dx_exe.unwrap();

    let mut rows = Vec::new();
    let mut rusty_ahead = 0;
    let mut dx_ahead = 0;
    let mut tie = 0;
    let mut compared = 0;

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
            let channels = channels_for(content);
            let floor = psnr_floor(content);

            let rusty = rusty_roundtrip(&pixels, content, ctx);
            let dx = dxtex_roundtrip(
                &dx_exe,
                &work,
                &id,
                &pixels,
                ctx,
                dxgi_name(content),
                channels,
            );

            let verdict = match (
                rusty.as_ref().ok().and_then(|r| r.psnr),
                dx.as_ref().ok().and_then(|r| r.psnr),
            ) {
                (Some(a), Some(b)) if a.is_infinite() && b.is_infinite() => {
                    compared += 1;
                    tie += 1;
                    "tie_exact"
                }
                (Some(a), Some(b)) if a.is_infinite() && b.is_finite() => {
                    compared += 1;
                    rusty_ahead += 1;
                    "rusty_higher_psnr"
                }
                (Some(a), Some(b)) if a.is_finite() && b.is_infinite() => {
                    compared += 1;
                    dx_ahead += 1;
                    "directxtex_higher_psnr"
                }
                (Some(a), Some(b)) if a.is_finite() && b.is_finite() => {
                    compared += 1;
                    let da = a - b;
                    if da > 0.25 {
                        rusty_ahead += 1;
                        "rusty_higher_psnr"
                    } else if da < -0.25 {
                        dx_ahead += 1;
                        "directxtex_higher_psnr"
                    } else {
                        tie += 1;
                        "tie"
                    }
                }
                _ => "incomplete",
            };

            let rusty_psnr = rusty.as_ref().ok().and_then(|r| r.psnr);
            let dx_psnr = dx.as_ref().ok().and_then(|r| r.psnr);
            let delta = match (rusty_psnr, dx_psnr) {
                (Some(a), Some(b)) if a.is_finite() && b.is_finite() => Some(a - b),
                _ => None,
            };

            println!(
                "{:<22} rusty={:>8}  dxtex={:>8}  Δ={:>7}  {}",
                id,
                fmt_psnr(rusty_psnr),
                fmt_psnr(dx_psnr),
                delta
                    .map(|d| format!("{d:+.2}"))
                    .unwrap_or_else(|| "—".into()),
                verdict
            );

            rows.push(serde_json::json!({
                "id": id,
                "content": content.name(),
                "context": ctx.name,
                "floor_db": if floor.is_infinite() { serde_json::Value::Null } else { json_f(floor) },
                "channels": channels,
                "rusty_dds": peer_json(&rusty),
                "directxtex": peer_json(&dx),
                "delta_rusty_minus_dxtex_db": delta.map(json_f).unwrap_or(serde_json::Value::Null),
                "verdict": verdict,
            }));
        }
    }

    let report = serde_json::json!({
        "protocol": "Same RGBA8 source → encode → decode → PSNR vs source (preserved channels)",
        "peers": ["rusty_dds", "Microsoft DirectXTex"],
        "notes": [
            "Identical fill_rgba sources as encode-quality / bench_baselines.",
            "DirectXTex via dxtex_roundtrip: Compress|Convert then Decompress|Convert.",
            "BC7 peer flag: TEX_COMPRESS_BC7_QUICK (mode-6 class).",
            "BC4: R only; BC5: R+G; BC1: RGB; else full RGBA. RGBA/BGRA: bit-exact preferred.",
            "verdict uses ±0.25 dB deadband for tie.",
            "Δ = rusty_psnr − directxtex_psnr (positive ⇒ rusty_dds higher fidelity on this source).",
        ],
        "rows": rows,
        "summary": {
            "cases": rows.len(),
            "compared": compared,
            "rusty_higher_psnr": rusty_ahead,
            "directxtex_higher_psnr": dx_ahead,
            "tie": tie,
        },
    });

    let json_path = artifacts.join("encode-quality-vs-directxtex.json");
    fs::write(&json_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    write_md(&artifacts, &report);
    println!("Wrote {}", json_path.display());
}

struct PeerResult {
    psnr: Option<f64>,
    max_abs: Option<u8>,
    ok: bool,
    error: Option<String>,
}

fn peer_json(r: &Result<PeerResult, String>) -> serde_json::Value {
    match r {
        Ok(p) => serde_json::json!({
            "ok": p.ok,
            "psnr_db": finite_or_null(p.psnr),
            "psnr_infinite": p.psnr.map(|v| v.is_infinite()).unwrap_or(false),
            "max_abs": p.max_abs,
            "error": p.error,
        }),
        Err(e) => serde_json::json!({
            "ok": false,
            "psnr_db": null,
            "psnr_infinite": false,
            "max_abs": null,
            "error": e,
        }),
    }
}

fn rusty_roundtrip(pixels: &[u8], content: DecodeContent, ctx: &Ctx) -> Result<PeerResult, String> {
    let layout = EncodeLayout::flat_2d(content, ctx.width, ctx.height)
                .with_depth(ctx.depth);
    let dds = Dds::encode_from_rgba8(pixels, layout).map_err(|e| e.to_string())?;
    let img = dds
        .decode_rgba8(SubresourceId::mip_layer(0, 0))
        .map_err(|e| e.to_string())?;
    let ch = channels_for(content);
    // SNORM recon comes back as raw SNORM bytes; map to the UNORM source
    // domain before scoring (mirrors harvest_corpus_vs_dxtex and the DXT arm,
    // whose C++ tool already emits UNORM) — without this the board
    // under-scores our signed formats by ~35 dB.
    let recon = match content {
        DecodeContent::Bc4SNorm | DecodeContent::Bc5SNorm => {
            snorm_bits_rgba_to_unorm(&img.pixels)
        }
        _ => img.pixels.clone(),
    };
    let psnr = match content {
        DecodeContent::Rgba8 | DecodeContent::Bgra8 => {
            if recon == *pixels {
                Some(f64::INFINITY)
            } else {
                psnr_channels(&recon, pixels, ch)
            }
        }
        _ => psnr_channels(&recon, pixels, ch),
    };
    Ok(PeerResult {
        psnr,
        max_abs: max_abs_diff(&recon, pixels),
        ok: true,
        error: None,
    })
}

fn dxtex_roundtrip(
    exe: &Path,
    work: &Path,
    id: &str,
    pixels: &[u8],
    ctx: &Ctx,
    dxgi: &str,
    channels: &[usize],
) -> Result<PeerResult, String> {
    let in_path = work.join(format!("{id}.rgba"));
    let out_path = work.join(format!("{id}.out.rgba"));
    let json_path = work.join(format!("{id}.json"));
    fs::write(&in_path, pixels).map_err(|e| e.to_string())?;

    let status = Command::new(exe)
        .arg(&in_path)
        .arg(ctx.width.to_string())
        .arg(ctx.height.to_string())
        .arg(ctx.depth.to_string())
        .arg(dxgi)
        .arg(&out_path)
        .arg(&json_path)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("dxtex_roundtrip exit {:?}", status.code()));
    }
    let decoded = fs::read(&out_path).map_err(|e| e.to_string())?;
    if decoded.len() != pixels.len() {
        return Err(format!(
            "size mismatch: got {} want {}",
            decoded.len(),
            pixels.len()
        ));
    }
    let psnr = if decoded == pixels {
        Some(f64::INFINITY)
    } else {
        psnr_channels(&decoded, pixels, channels)
    };
    Ok(PeerResult {
        psnr,
        max_abs: max_abs_diff(&decoded, pixels),
        ok: true,
        error: None,
    })
}

fn write_md(dir: &Path, report: &serde_json::Value) {
    let mut md = String::new();
    md.push_str("# Encode quality: rusty_dds vs DirectXTex (same sources)\n\n");
    if let Some(notes) = report["notes"].as_array() {
        for n in notes {
            md.push_str(&format!("- {}\n", n.as_str().unwrap_or("")));
        }
        md.push('\n');
    }
    md.push_str("## Summary\n\n");
    md.push_str(&format!(
        "```json\n{}\n```\n\n",
        serde_json::to_string_pretty(&report["summary"]).unwrap()
    ));
    md.push_str(
        "| Case | Content | Context | rusty PSNR | DirectXTex PSNR | Δ (rusty−dx) | Verdict |\n",
    );
    md.push_str(
        "|------|---------|---------|------------|-----------------|--------------|---------|\n",
    );
    for r in report["rows"].as_array().unwrap() {
        let rp = fmt_peer_psnr(&r["rusty_dds"]);
        let dp = fmt_peer_psnr(&r["directxtex"]);
        let delta = r["delta_rusty_minus_dxtex_db"]
            .as_f64()
            .map(|d| format!("{d:+.2}"))
            .unwrap_or_else(|| "—".into());
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            r["id"].as_str().unwrap_or(""),
            r["content"].as_str().unwrap_or(""),
            r["context"].as_str().unwrap_or(""),
            rp,
            dp,
            delta,
            r["verdict"].as_str().unwrap_or(""),
        ));
    }
    let path = dir.join("encode-quality-vs-directxtex.md");
    fs::write(&path, md).unwrap();
    println!("Wrote {}", path.display());
}

fn fmt_peer_psnr(p: &serde_json::Value) -> String {
    if p["psnr_infinite"].as_bool() == Some(true) {
        return "∞".into();
    }
    match p["psnr_db"].as_f64() {
        Some(v) => format!("{v:.2}"),
        None => "fail".into(),
    }
}

fn find_dxtex_roundtrip(root: &Path) -> Option<PathBuf> {
    [
        root.join("tools/dxtex_decode_bench/build/dxtex_roundtrip.exe"),
        root.join("tools/dxtex_decode_bench/build/Release/dxtex_roundtrip.exe"),
        root.join("tools/dxtex_decode_bench/build/dxtex_roundtrip"),
    ]
    .into_iter()
    .find(|p| p.exists())
}

fn dxgi_name(content: DecodeContent) -> &'static str {
    match content {
        DecodeContent::Bc1 => "BC1_UNORM",
        DecodeContent::Bc2 => "BC2_UNORM",
        DecodeContent::Bc3 => "BC3_UNORM",
        DecodeContent::Bc4UNorm => "BC4_UNORM",
        DecodeContent::Bc4SNorm => "BC4_SNORM",
        DecodeContent::Bc5UNorm => "BC5_UNORM",
        DecodeContent::Bc5SNorm => "BC5_SNORM",
        DecodeContent::Bc7 => "BC7_UNORM",
        DecodeContent::Rgba8 => "R8G8B8A8_UNORM",
        DecodeContent::Bgra8 => "B8G8R8A8_UNORM",
        // Exhaustive by intent: a new DecodeContent must be added to
        // this matrix, never silently skipped.
        other => panic!("unhandled DecodeContent: {other:?}"),
    }
}

fn fmt_psnr(p: Option<f64>) -> String {
    match p {
        Some(v) if v.is_infinite() => "inf".into(),
        Some(v) => format!("{v:.2}"),
        None => "fail".into(),
    }
}

fn finite_or_null(p: Option<f64>) -> serde_json::Value {
    match p {
        Some(v) if v.is_finite() => json_f(v),
        _ => serde_json::Value::Null,
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
        // Exhaustive by intent: a new DecodeContent must be added to
        // this matrix, never silently skipped.
        other => panic!("unhandled DecodeContent: {other:?}"),
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

/// SNORM-bit RGBA bytes → UNORM domain (matches harvest_corpus_vs_dxtex).
fn snorm_bits_rgba_to_unorm(px: &[u8]) -> Vec<u8> {
    let mut out = px.to_vec();
    for p in out.chunks_exact_mut(4) {
        for c in 0..3 {
            let s = (p[c] as i8 as i32).clamp(-127, 127);
            p[c] = ((((s + 127) * 255) + 127) / 254) as u8;
        }
    }
    out
}
