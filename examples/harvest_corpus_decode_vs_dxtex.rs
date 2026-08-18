//! Decode speed on ambientCG corpus DDS (same maps as encode corpus harvest).
//!
//! ```text
//! cargo run --release --example harvest_corpus_decode_vs_dxtex
//! ```
//!
//! Writes `docs/artifacts/decode-vs-baselines.{json,md}`.

use rusty_dds::*;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

const ITERS: u32 = 20;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let corpus = root.join("corpus");
    let manifest_path = corpus.join("manifest.json");
    let artifacts = root.join("docs/artifacts");
    let cases_dir = root.join("target/corpus_decode_cases");
    fs::create_dir_all(&artifacts).unwrap();
    let _ = fs::remove_dir_all(&cases_dir);
    fs::create_dir_all(&cases_dir).unwrap();

    let man: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
    let entries = man["entries"].as_array().cloned().unwrap_or_default();

    let mut rows = Vec::new();
    for entry in &entries {
        let rel = entry["path"].as_str().unwrap();
        let png_path = corpus.join(rel);
        if !png_path.exists() {
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
        for t in entry["targets"].as_array().unwrap() {
            let tname = t.as_str().unwrap();
            let content = match parse_content(tname) {
                Some(c) => c,
                None => continue,
            };
            let id = format!("{entry_id}__{tname}");
            let layout = EncodeLayout::flat_2d(content, w, h);
            let dds = match Dds::encode_from_rgba8(&rgba, layout) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("encode fail {id}: {e}");
                    continue;
                }
            };
            let path = cases_dir.join(format!("{id}.dds"));
            write_dds(&path, &dds);

            let bytes = fs::read(&path).unwrap();
            let rusty = time_decode(&bytes);
            rows.push(serde_json::json!({
                "id": id,
                "content": tname,
                "context": role,
                "asset": entry["asset"],
                "entry": entry_id,
                "width": w,
                "height": h,
                "rusty_dds_ns": rusty.0,
                "rusty_dds_ok": rusty.1,
            }));
            println!("decode {:<36} rusty={:>10.0}", id, rusty.0);
        }
    }

    let mut dxtex_note = "DirectXTex decode harness not found — skipped".to_string();
    if let Some(exe) = find_dxtex_exe(&root, "dxtex_decode_bench") {
        let raw_path = artifacts.join("dxtex_corpus_decode_raw.json");
        let status = Command::new(&exe)
            .arg(&cases_dir)
            .arg(&raw_path)
            .arg(ITERS.to_string())
            .status();
        if status.map(|s| s.success()).unwrap_or(false) {
            if let Ok(raw) = fs::read_to_string(&raw_path) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
                    merge_dxtex(&mut rows, &v);
                    dxtex_note = "DirectXTex decode included".into();
                }
            }
        } else {
            dxtex_note = "DirectXTex decode harness failed".into();
        }
    }

    let report = serde_json::json!({
        "iters": ITERS,
        "protocol": "Corpus DDS (rusty encode of ambientCG maps) → decode → RGBA8",
        "peers": ["rusty_dds", "Microsoft DirectXTex"],
        "notes": [
            "Primary decode baseline: ambientCG proxy corpus (~1024^2), not synthetic X-grid.",
            "Same DDS bytes for both peers (encoded by rusty_dds from corpus PNGs).",
            "Roles: albedo / normal / mask. ratio < 1 => rusty_dds faster.",
            dxtex_note,
        ],
        "rows": rows,
        "summary": summarize(&rows),
    });

    write_json_md(&artifacts, "decode-vs-baselines", &report, "Decode (corpus) vs Microsoft DirectXTex");
    println!("{}", serde_json::to_string_pretty(&report["summary"]).unwrap());
}

fn time_decode(bytes: &[u8]) -> (f64, bool) {
    let once = || -> Result<usize, String> {
        let dds = Dds::read(&mut std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
        let img = dds
            .decode_rgba8(SubresourceId::mip_layer(0, 0))
            .map_err(|e| e.to_string())?;
        Ok(img.pixels.len())
    };
    if once().is_err() {
        return (0.0, false);
    }
    for _ in 0..2 {
        let _ = once();
    }
    let t0 = Instant::now();
    let mut sink = 0usize;
    for _ in 0..ITERS {
        sink = sink.wrapping_add(once().unwrap_or(0));
    }
    let _ = sink;
    (t0.elapsed().as_nanos() as f64 / ITERS as f64, true)
}

fn merge_dxtex(rows: &mut [serde_json::Value], raw: &serde_json::Value) {
    let Some(arr) = raw.get("cases").and_then(|r| r.as_array()) else {
        return;
    };
    for row in rows.iter_mut() {
        let id = row["id"].as_str().unwrap_or("");
        if let Some(dx) = arr.iter().find(|r| r["id"].as_str() == Some(id)) {
            let ok = dx.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
            row["directxtex_ok"] = serde_json::json!(ok);
            if let Some(n) = dx.get("ns_per_iter").and_then(|v| v.as_f64()) {
                row["directxtex_ns"] = serde_json::json!(n);
                if ok {
                    if let Some(r) = row["rusty_dds_ns"].as_f64() {
                        if n > 0.0 {
                            row["ratio_rusty_over_directxtex"] = serde_json::json!(r / n);
                        }
                    }
                }
            }
        }
    }
}

fn summarize(rows: &[serde_json::Value]) -> serde_json::Value {
    let mut ahead = 0;
    let mut behind = 0;
    let mut peer_ok = 0;
    for r in rows {
        if r["directxtex_ok"].as_bool() == Some(true) {
            peer_ok += 1;
            if let Some(x) = r["ratio_rusty_over_directxtex"].as_f64() {
                if x < 0.95 {
                    ahead += 1;
                } else if x > 1.05 {
                    behind += 1;
                }
            }
        }
    }
    serde_json::json!({
        "cases": rows.len(),
        "vs_directxtex": {
            "peer_ok_cases": peer_ok,
            "ahead": ahead,
            "behind": behind,
        }
    })
}

fn write_json_md(dir: &Path, stem: &str, report: &serde_json::Value, title: &str) {
    let json_path = dir.join(format!("{stem}.json"));
    let md_path = dir.join(format!("{stem}.md"));
    fs::write(&json_path, serde_json::to_string_pretty(report).unwrap()).unwrap();
    let mut md = String::new();
    md.push_str(&format!("# {title}\n\n"));
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
        "| Case | Content | Role | rusty_dds (ns) | DirectXTex (ns) | Ratio (rusty/dx) |\n",
    );
    md.push_str(
        "|------|---------|------|----------------|-----------------|------------------|\n",
    );
    for r in report["rows"].as_array().unwrap() {
        let rn = r["rusty_dds_ns"]
            .as_f64()
            .map(|v| format!("{v:.0}"))
            .unwrap_or_else(|| "fail".into());
        let dn = r["directxtex_ns"]
            .as_f64()
            .map(|v| format!("{v:.0}"))
            .unwrap_or_else(|| "—".into());
        let ratio = r["ratio_rusty_over_directxtex"]
            .as_f64()
            .map(|v| format!("{v:.3}"))
            .unwrap_or_else(|| "—".into());
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            r["id"].as_str().unwrap_or(""),
            r["content"].as_str().unwrap_or(""),
            r["context"].as_str().unwrap_or(""),
            rn,
            dn,
            ratio
        ));
    }
    fs::write(&md_path, md).unwrap();
    println!("Wrote {}", json_path.display());
    println!("Wrote {}", md_path.display());
}

fn write_dds(path: &Path, dds: &Dds) {
    let mut f = File::create(path).unwrap();
    dds.write(&mut f).unwrap();
    f.flush().unwrap();
}

fn find_dxtex_exe(root: &Path, name: &str) -> Option<PathBuf> {
    [
        root.join(format!("tools/dxtex_decode_bench/build/{name}.exe")),
        root.join(format!("tools/dxtex_decode_bench/build/Release/{name}.exe")),
        root.join(format!("tools/dxtex_decode_bench/build/{name}")),
    ]
    .into_iter()
    .find(|p| p.exists())
}

fn parse_content(name: &str) -> Option<DecodeContent> {
    Some(match name {
        "bc1" => DecodeContent::Bc1,
        "bc4u" => DecodeContent::Bc4UNorm,
        "bc4s" => DecodeContent::Bc4SNorm,
        "bc5u" => DecodeContent::Bc5UNorm,
        "bc5s" => DecodeContent::Bc5SNorm,
        "bc7" => DecodeContent::Bc7,
        _ => return None,
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
    let data8 = match info.bit_depth {
        png::BitDepth::Eight => buf[..info.buffer_size()].to_vec(),
        png::BitDepth::Sixteen => buf[..info.buffer_size()]
            .chunks_exact(2)
            .map(|c| c[0])
            .collect(),
        other => return Err(format!("bit depth {other:?}")),
    };
    let mut out = Vec::new();
    match (info.color_type, role) {
        (png::ColorType::Rgb, "albedo") => {
            for c in data8.chunks_exact(3) {
                out.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        (png::ColorType::Rgba, "albedo") => out.extend_from_slice(&data8),
        (png::ColorType::Grayscale, "mask") => {
            for &g in &data8 {
                out.extend_from_slice(&[g, 0, 0, 255]);
            }
        }
        (png::ColorType::Rgb, "mask") | (png::ColorType::Rgba, "mask") => {
            let step = if info.color_type == png::ColorType::Rgb {
                3
            } else {
                4
            };
            for c in data8.chunks_exact(step) {
                out.extend_from_slice(&[c[0], 0, 0, 255]);
            }
        }
        (png::ColorType::Rgb, "normal") | (png::ColorType::Rgba, "normal") => {
            let step = if info.color_type == png::ColorType::Rgb {
                3
            } else {
                4
            };
            for c in data8.chunks_exact(step) {
                out.extend_from_slice(&[c[0], c[1], 0, 255]);
            }
        }
        _ => return Err(format!("{:?} / {role}", info.color_type)),
    }
    Ok((w, h, out))
}
