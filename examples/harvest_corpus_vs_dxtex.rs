//! Encode speed + round-trip PSNR on the ambientCG proxy corpus vs DirectXTex.
//!
//! ```text
//! python corpus/fetch_ambientcg.py   # once
//! cargo run --release --example harvest_corpus_vs_dxtex
//! ```
//!
//! Requires `tools/dxtex_decode_bench/build/dxtex_roundtrip[.exe]`.
//! Writes `docs/artifacts/corpus-vs-directxtex.{json,md}`.

use rusty_dds::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

const ENCODE_ITERS: u32 = 3;
const TIE_DB: f64 = 0.25;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let corpus = root.join("corpus");
    let manifest_path = corpus.join("manifest.json");
    let artifacts = root.join("docs/artifacts");
    let work = root.join("target/corpus_vs_dxtex");
    fs::create_dir_all(&artifacts).unwrap();
    fs::create_dir_all(&work).unwrap();

    if !manifest_path.exists() {
        eprintln!("ERROR: missing {}. Run: python corpus/fetch_ambientcg.py", manifest_path.display());
        std::process::exit(1);
    }
    let man: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
    let entries = man["entries"].as_array().cloned().unwrap_or_default();
    if entries.is_empty() {
        eprintln!("ERROR: manifest has no entries. Run: python corpus/fetch_ambientcg.py");
        std::process::exit(1);
    }

    let dx_exe = match find_dxtex_roundtrip(&root) {
        Some(p) => p,
        None => {
            eprintln!("ERROR: dxtex_roundtrip not found. Build tools/dxtex_decode_bench first.");
            std::process::exit(1);
        }
    };

    let mut rows = Vec::new();
    let mut rusty_faster = 0usize;
    let mut dx_faster = 0usize;
    let mut speed_tie = 0usize;
    let mut rusty_q = 0usize;
    let mut dx_q = 0usize;
    let mut q_tie = 0usize;
    let mut compared_q = 0usize;

    for entry in &entries {
        let rel = entry["path"].as_str().unwrap();
        let png_path = corpus.join(rel);
        if !png_path.exists() {
            eprintln!("skip missing {}", png_path.display());
            continue;
        }
        let role = entry["role"].as_str().unwrap();
        let entry_id = entry["id"].as_str().unwrap();
        let (w, h, rgba) = match load_png_as_rgba(&png_path, role) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("skip {entry_id}: {e}");
                continue;
            }
        };

        let targets = entry["targets"].as_array().unwrap();
        for t in targets {
            let tname = t.as_str().unwrap();
            let content = match parse_content(tname) {
                Some(c) => c,
                None => continue,
            };
            let case_id = format!("{entry_id}__{tname}");
            let channels = channels_for(content);
            let dxgi = dxgi_name(content);

            let rusty_ns = time_rusty_encode(&rgba, content, w, h, ENCODE_ITERS);
            let rusty_q_res = rusty_roundtrip(&rgba, content, w, h, channels);
            let dx = dxtex_roundtrip(&dx_exe, &work, &case_id, &rgba, w, h, dxgi, channels);

            let dx_ns = dx.as_ref().ok().and_then(|d| d.encode_ns);
            let ratio = match (rusty_ns, dx_ns) {
                (Some(r), Some(d)) if d > 0.0 => Some(r / d),
                _ => None,
            };
            let speed_verdict = match ratio {
                Some(r) if r < 0.95 => {
                    rusty_faster += 1;
                    "rusty_faster"
                }
                Some(r) if r > 1.05 => {
                    dx_faster += 1;
                    "directxtex_faster"
                }
                Some(_) => {
                    speed_tie += 1;
                    "speed_tie"
                }
                None => "incomplete",
            };

            let rusty_psnr = rusty_q_res.as_ref().ok().and_then(|r| r.psnr);
            let dx_psnr = dx.as_ref().ok().and_then(|r| r.psnr);
            let delta = match (rusty_psnr, dx_psnr) {
                (Some(a), Some(b)) if a.is_finite() && b.is_finite() => Some(a - b),
                _ => None,
            };
            let q_verdict = match (rusty_psnr, dx_psnr) {
                (Some(a), Some(b)) if a.is_infinite() && b.is_infinite() => {
                    compared_q += 1;
                    q_tie += 1;
                    "tie_exact"
                }
                (Some(a), Some(b)) if a.is_infinite() && b.is_finite() => {
                    compared_q += 1;
                    rusty_q += 1;
                    "rusty_higher_psnr"
                }
                (Some(a), Some(b)) if a.is_finite() && b.is_infinite() => {
                    compared_q += 1;
                    dx_q += 1;
                    "directxtex_higher_psnr"
                }
                (Some(a), Some(b)) if a.is_finite() && b.is_finite() => {
                    compared_q += 1;
                    let d = a - b;
                    if d > TIE_DB {
                        rusty_q += 1;
                        "rusty_higher_psnr"
                    } else if d < -TIE_DB {
                        dx_q += 1;
                        "directxtex_higher_psnr"
                    } else {
                        q_tie += 1;
                        "tie"
                    }
                }
                _ => "incomplete",
            };

            println!(
                "{:<36} {:>8}µs / {:>8}µs  ratio={:<7}  PSNR {:>7} vs {:>7}  Δ={:>7}  {} | {}",
                case_id,
                rusty_ns.map(|n| format!("{:.0}", n / 1000.0)).unwrap_or_else(|| "fail".into()),
                dx_ns.map(|n| format!("{:.0}", n / 1000.0)).unwrap_or_else(|| "fail".into()),
                ratio.map(|r| format!("{r:.3}")).unwrap_or_else(|| "—".into()),
                fmt_psnr(rusty_psnr),
                fmt_psnr(dx_psnr),
                delta.map(|d| format!("{d:+.2}")).unwrap_or_else(|| "—".into()),
                speed_verdict,
                q_verdict,
            );

            rows.push(serde_json::json!({
                "id": case_id,
                "entry": entry_id,
                "asset": entry["asset"],
                "role": role,
                "map": entry["map"],
                "content": tname,
                "width": w,
                "height": h,
                "rusty_encode_ns": rusty_ns,
                "directxtex_encode_ns": dx_ns,
                "ratio_rusty_over_dx": ratio,
                "speed_verdict": speed_verdict,
                "rusty_psnr_db": finite_or_null(rusty_psnr),
                "rusty_psnr_inf": rusty_psnr.map(|v| v.is_infinite()).unwrap_or(false),
                "dxtex_psnr_db": finite_or_null(dx_psnr),
                "dxtex_psnr_inf": dx_psnr.map(|v| v.is_infinite()).unwrap_or(false),
                "delta_db": delta,
                "quality_verdict": q_verdict,
                "rusty_ok": rusty_q_res.is_ok(),
                "dxtex_ok": dx.as_ref().map(|d| d.ok).unwrap_or(false),
                "dxtex_error": dx.as_ref().err().cloned().or_else(|| dx.as_ref().ok().and_then(|d| d.error.clone())),
            }));
        }
    }

    let report = serde_json::json!({
        "protocol": "ambientCG CC0 PNGs → RGBA → encode → decode → PSNR; encode timed separately",
        "peers": ["rusty_dds", "Microsoft DirectXTex"],
        "corpus": "corpus/manifest.json",
        "notes": [
            "Proxy cook corpus (not Star Citizen / Cry). License: CC0 via ambientCG.",
            "Albedo Color → BC1/BC7; NormalGL → BC5U/S (R,G); Roughness → BC4U/S (R).",
            "BC7 peer: TEX_COMPRESS_BC7_QUICK. DirectXTex encode_ns from dxtex_roundtrip JSON.",
            "rusty encode: best of 3 iters (encode only). Quality: ±0.25 dB tie band.",
            "ratio < 1 ⇒ rusty_dds faster.",
            "rusty strip-parallel encode (≥4096 blocks); DX peer is TEX_COMPRESS_DEFAULT (no PARALLEL).",
        ],
        "rows": rows,
        "summary": {
            "cases": rows.len(),
            "speed": {
                "rusty_faster": rusty_faster,
                "directxtex_faster": dx_faster,
                "tie": speed_tie,
            },
            "quality": {
                "compared": compared_q,
                "rusty_higher_psnr": rusty_q,
                "directxtex_higher_psnr": dx_q,
                "tie": q_tie,
            },
        },
    });

    let json_path = artifacts.join("corpus-vs-directxtex.json");
    fs::write(&json_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    write_md(&artifacts, &report);
    println!("\n{}", serde_json::to_string_pretty(&report["summary"]).unwrap());
    println!("Wrote {}", json_path.display());
}

struct PeerResult {
    psnr: Option<f64>,
    encode_ns: Option<f64>,
    ok: bool,
    error: Option<String>,
}

fn time_rusty_encode(
    pixels: &[u8],
    content: DecodeContent,
    w: u32,
    h: u32,
    iters: u32,
) -> Option<f64> {
    let layout = EncodeLayout {
        content,
        width: w,
        height: h,
        depth: 1,
        mipmap_levels: 1,
        array_layers: 1,
        is_cubemap: false,
        quality: EncodeQuality::Quality,
    };
    // Warmup
    let _ = Dds::encode_from_rgba8(pixels, layout);
    let mut best = f64::INFINITY;
    for _ in 0..iters {
        let t0 = Instant::now();
        let dds = Dds::encode_from_rgba8(pixels, layout).ok()?;
        let ns = t0.elapsed().as_secs_f64() * 1e9;
        std::hint::black_box(dds.data.len());
        best = best.min(ns);
    }
    Some(best)
}

fn rusty_roundtrip(
    pixels: &[u8],
    content: DecodeContent,
    w: u32,
    h: u32,
    channels: &[usize],
) -> Result<PeerResult, String> {
    let layout = EncodeLayout {
        content,
        width: w,
        height: h,
        depth: 1,
        mipmap_levels: 1,
        array_layers: 1,
        is_cubemap: false,
        quality: EncodeQuality::Quality,
    };
    let dds = Dds::encode_from_rgba8(pixels, layout).map_err(|e| e.to_string())?;
    let img = dds
        .decode_rgba8(SubresourceId::mip_layer(0, 0))
        .map_err(|e| e.to_string())?;
    // bcdec SNORM writes i8 bit patterns as u8; DirectXTex roundtrip returns UNORM.
    // Map SNORM bits → UNORM so PSNR matches the DX peer domain.
    let decoded = match content {
        DecodeContent::Bc4SNorm | DecodeContent::Bc5SNorm => {
            snorm_bits_rgba_to_unorm(&img.pixels)
        }
        _ => img.pixels.clone(),
    };
    let psnr = psnr_channels(&decoded, pixels, channels);
    Ok(PeerResult {
        psnr,
        encode_ns: None,
        ok: true,
        error: None,
    })
}

/// bcdec signed BC4/5 stores reconstructed SNORM as `i8 as u8` bit patterns.
fn snorm_bits_rgba_to_unorm(px: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(px.len());
    for c in px.chunks_exact(4) {
        out.push(snorm_u8_bits_to_unorm(c[0]));
        out.push(snorm_u8_bits_to_unorm(c[1]));
        out.push(c[2]);
        out.push(c[3]);
    }
    out
}

fn snorm_u8_bits_to_unorm(b: u8) -> u8 {
    let s = (b as i8 as f32 / 127.0).clamp(-1.0, 1.0);
    ((s * 0.5 + 0.5) * 255.0).round().clamp(0.0, 255.0) as u8
}

fn dxtex_roundtrip(
    exe: &Path,
    work: &Path,
    id: &str,
    pixels: &[u8],
    w: u32,
    h: u32,
    dxgi: &str,
    channels: &[usize],
) -> Result<PeerResult, String> {
    let safe = id.replace(['/', '\\', ':'], "_");
    let in_path = work.join(format!("{safe}.rgba"));
    let out_path = work.join(format!("{safe}.out.rgba"));
    let json_path = work.join(format!("{safe}.json"));
    fs::write(&in_path, pixels).map_err(|e| e.to_string())?;

    // Best of 3 process runs for encode_ns (quality from last successful decode).
    let mut best_enc = f64::INFINITY;
    let mut last_decoded: Option<Vec<u8>> = None;
    for _ in 0..ENCODE_ITERS {
        let status = Command::new(exe)
            .arg(&in_path)
            .arg(w.to_string())
            .arg(h.to_string())
            .arg("1")
            .arg(dxgi)
            .arg(&out_path)
            .arg(&json_path)
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("dxtex_roundtrip exit {:?}", status.code()));
        }
        let meta: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&json_path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if let Some(ns) = meta["encode_ns"].as_f64() {
            best_enc = best_enc.min(ns);
        }
        last_decoded = Some(fs::read(&out_path).map_err(|e| e.to_string())?);
    }
    let decoded = last_decoded.ok_or_else(|| "no decode".to_string())?;
    if decoded.len() != pixels.len() {
        return Err(format!(
            "size mismatch: got {} want {}",
            decoded.len(),
            pixels.len()
        ));
    }
    let psnr = psnr_channels(&decoded, pixels, channels);
    Ok(PeerResult {
        psnr,
        encode_ns: if best_enc.is_finite() {
            Some(best_enc)
        } else {
            None
        },
        ok: true,
        error: None,
    })
}

fn load_png_as_rgba(path: &Path, role: &str) -> Result<(u32, u32, Vec<u8>), String> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let decoder = png::Decoder::new(file);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let w = info.width;
    let h = info.height;
    let rgba = expand_to_rgba(&buf[..info.buffer_size()], info.color_type, info.bit_depth, role)?;
    if rgba.len() != (w as usize) * (h as usize) * 4 {
        return Err(format!(
            "rgba len {} want {}",
            rgba.len(),
            (w as usize) * (h as usize) * 4
        ));
    }
    Ok((w, h, rgba))
}

fn expand_to_rgba(
    data: &[u8],
    color: png::ColorType,
    depth: png::BitDepth,
    role: &str,
) -> Result<Vec<u8>, String> {
    let data8 = match depth {
        png::BitDepth::Eight => data.to_vec(),
        png::BitDepth::Sixteen => downscale_16_to_8(data),
        other => return Err(format!("unsupported bit depth {other:?}")),
    };
    let mut out = Vec::with_capacity(data8.len().saturating_mul(4));
    match (color, role) {
        (png::ColorType::Rgb, "albedo") => {
            for c in data8.chunks_exact(3) {
                out.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        (png::ColorType::Rgba, "albedo") => {
            out.extend_from_slice(&data8);
        }
        (png::ColorType::Grayscale, "mask") => {
            for &g in &data8 {
                out.extend_from_slice(&[g, 0, 0, 255]);
            }
        }
        (png::ColorType::Rgb, "mask") => {
            for c in data8.chunks_exact(3) {
                out.extend_from_slice(&[c[0], 0, 0, 255]);
            }
        }
        (png::ColorType::Rgba, "mask") => {
            for c in data8.chunks_exact(4) {
                out.extend_from_slice(&[c[0], 0, 0, 255]);
            }
        }
        (png::ColorType::Rgb, "normal") => {
            for c in data8.chunks_exact(3) {
                out.extend_from_slice(&[c[0], c[1], 0, 255]);
            }
        }
        (png::ColorType::Rgba, "normal") => {
            for c in data8.chunks_exact(4) {
                out.extend_from_slice(&[c[0], c[1], 0, 255]);
            }
        }
        (png::ColorType::GrayscaleAlpha, "mask") => {
            for c in data8.chunks_exact(2) {
                out.extend_from_slice(&[c[0], 0, 0, 255]);
            }
        }
        _ => {
            return Err(format!(
                "unsupported color {:?} for role {role}",
                color
            ))
        }
    }
    Ok(out)
}

/// PNG 16-bit samples are big-endian; keep the high byte (≈ /256).
fn downscale_16_to_8(data: &[u8]) -> Vec<u8> {
    data.chunks_exact(2).map(|c| c[0]).collect()
}

fn write_md(dir: &Path, report: &serde_json::Value) {
    let mut md = String::new();
    md.push_str("# Corpus encode: rusty_dds vs DirectXTex\n\n");
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
        "| Case | Role | rusty µs | DX µs | Ratio | rusty PSNR | DX PSNR | Δ | Speed | Quality |\n",
    );
    md.push_str(
        "|------|------|----------|-------|-------|------------|---------|---|-------|----------|\n",
    );
    for r in report["rows"].as_array().unwrap() {
        let ru = r["rusty_encode_ns"]
            .as_f64()
            .map(|n| format!("{:.0}", n / 1000.0))
            .unwrap_or_else(|| "fail".into());
        let du = r["directxtex_encode_ns"]
            .as_f64()
            .map(|n| format!("{:.0}", n / 1000.0))
            .unwrap_or_else(|| "fail".into());
        let ratio = r["ratio_rusty_over_dx"]
            .as_f64()
            .map(|v| format!("{v:.3}"))
            .unwrap_or_else(|| "—".into());
        let rp = if r["rusty_psnr_inf"].as_bool() == Some(true) {
            "∞".into()
        } else {
            r["rusty_psnr_db"]
                .as_f64()
                .map(|v| format!("{v:.2}"))
                .unwrap_or_else(|| "fail".into())
        };
        let dp = if r["dxtex_psnr_inf"].as_bool() == Some(true) {
            "∞".into()
        } else {
            r["dxtex_psnr_db"]
                .as_f64()
                .map(|v| format!("{v:.2}"))
                .unwrap_or_else(|| "fail".into())
        };
        let delta = r["delta_db"]
            .as_f64()
            .map(|d| format!("{d:+.2}"))
            .unwrap_or_else(|| "—".into());
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            r["id"].as_str().unwrap_or(""),
            r["role"].as_str().unwrap_or(""),
            ru,
            du,
            ratio,
            rp,
            dp,
            delta,
            r["speed_verdict"].as_str().unwrap_or(""),
            r["quality_verdict"].as_str().unwrap_or(""),
        ));
    }
    let path = dir.join("corpus-vs-directxtex.md");
    fs::write(&path, md).unwrap();
    println!("Wrote {}", path.display());
}

fn parse_content(name: &str) -> Option<DecodeContent> {
    Some(match name {
        "bc1" => DecodeContent::Bc1,
        "bc3" => DecodeContent::Bc3,
        "bc4u" => DecodeContent::Bc4UNorm,
        "bc4s" => DecodeContent::Bc4SNorm,
        "bc5u" => DecodeContent::Bc5UNorm,
        "bc5s" => DecodeContent::Bc5SNorm,
        "bc7" => DecodeContent::Bc7,
        _ => return None,
    })
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

fn fmt_psnr(p: Option<f64>) -> String {
    match p {
        Some(v) if v.is_infinite() => "inf".into(),
        Some(v) => format!("{v:.2}"),
        None => "fail".into(),
    }
}

fn finite_or_null(p: Option<f64>) -> serde_json::Value {
    match p {
        Some(v) if v.is_finite() => serde_json::json!((v * 100.0).round() / 100.0),
        _ => serde_json::Value::Null,
    }
}
