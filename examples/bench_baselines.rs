//! Competitive baselines vs **Microsoft DirectXTex** only.
//!
//! Decode: LoadFromDDSMemory + Decompress|Convert → RGBA8  
//! Encode: RGBA8 ScratchImage + Compress|Convert (BC7 = `TEX_COMPRESS_BC7_QUICK`)
//!
//! ```text
//! cargo run --release --example bench_baselines
//! ```
//!
//! Requires `tools/dxtex_decode_bench` built (`dxtex_decode_bench` + `dxtex_encode_bench`).
//!
//! Writes:
//! - `docs/artifacts/decode-vs-baselines.{json,md}`
//! - `docs/artifacts/encode-vs-baselines.{json,md}`

use rusty_dds::*;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

const ITERS: u32 = 40;

#[derive(Clone, Copy)]
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
    let decode_cases = root.join("target/baseline_cases");
    let encode_cases = root.join("target/baseline_encode_cases");
    let artifacts = root.join("docs/artifacts");
    fs::create_dir_all(&decode_cases).unwrap();
    fs::create_dir_all(&encode_cases).unwrap();
    fs::create_dir_all(&artifacts).unwrap();

    let decode_report = run_decode_baselines(&root, &decode_cases, &artifacts);
    write_json_md(
        &artifacts,
        "decode-vs-baselines",
        &decode_report,
        "Decode (DDS bytes → RGBA8) vs Microsoft DirectXTex",
    );

    let encode_report = run_encode_baselines(&root, &encode_cases, &artifacts);
    write_json_md(
        &artifacts,
        "encode-vs-baselines",
        &encode_report,
        "Encode (RGBA8 → blocks) vs Microsoft DirectXTex",
    );

    println!("\nDone. See docs/artifacts/decode-vs-baselines.md and encode-vs-baselines.md");
}

fn content_allowed(content: DecodeContent, ctx: &Ctx) -> bool {
    if ctx.name == "X-CUBE"
        && !matches!(
            content,
            DecodeContent::Bc1 | DecodeContent::Bc3 | DecodeContent::Bc7 | DecodeContent::Rgba8
        )
    {
        return false;
    }
    true
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

// ---------------------------------------------------------------------------
// Decode
// ---------------------------------------------------------------------------

struct DecodeCase {
    id: String,
    content: DecodeContent,
    context: &'static str,
    path: PathBuf,
}

fn run_decode_baselines(root: &Path, cases_dir: &Path, artifacts: &Path) -> serde_json::Value {
    // Drop stale cases so DirectXTex does not pick up leftover IDs.
    let _ = fs::remove_dir_all(cases_dir);
    fs::create_dir_all(cases_dir).unwrap();
    let cases = build_decode_cases(cases_dir);
    println!("decode cases: {}", cases.len());

    let mut rows = Vec::new();
    for c in &cases {
        let bytes = fs::read(&c.path).unwrap();
        let rusty = time_result(|| {
            let dds = Dds::read(&mut std::io::Cursor::new(&bytes)).map_err(|e| e.to_string())?;
            let img = dds
                .decode_rgba8(SubresourceId::mip_layer(0, 0))
                .map_err(|e| e.to_string())?;
            Ok(img.pixels.len())
        });
        rows.push(serde_json::json!({
            "id": c.id,
            "content": c.content.name(),
            "context": c.context,
            "rusty_dds_ns": rusty.0,
            "rusty_dds_ok": rusty.1,
        }));
        println!("decode {:<22} rusty={:>8.0}", c.id, rusty.0);
    }

    let mut dxtex_note = "DirectXTex decode harness not found — skipped".to_string();
    if let Some(exe) = find_dxtex_exe(root, "dxtex_decode_bench") {
        let raw_path = artifacts.join("dxtex_baselines_raw.json");
        let status = Command::new(&exe)
            .arg(cases_dir)
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

    serde_json::json!({
        "iters": ITERS,
        "protocol": "Load DDS bytes + decode primary surface → RGBA8",
        "peers": ["rusty_dds", "Microsoft DirectXTex"],
        "notes": [
            "Official peer only: Microsoft DirectXTex (no Rust crate peers).",
            "Each case is a single-surface DDS materializing one content × context cell.",
            "X-MIP uses a 4×4 surface (tip-like). X-CUBE/X-ARRAY are single-face/layer 2D.",
            "ratio < 1 means rusty_dds is faster.",
            dxtex_note,
        ],
        "rows": rows,
        "summary": summarize_vs_dxtex(&rows),
    })
}

fn build_decode_cases(dir: &Path) -> Vec<DecodeCase> {
    let mut out = Vec::new();
    for ctx in CONTEXTS {
        for &content in DecodeContent::ALL_LDR {
            if !content_allowed(content, ctx) {
                continue;
            }
            let id = format!("{}__{}", content.name(), ctx.name);
            let path = dir.join(format!("{id}.dds"));
            let layout = EncodeLayout::flat_2d(content, ctx.width, ctx.height)
                .with_depth(ctx.depth);
            let pixels = fill_rgba(content, ctx.width, ctx.height, ctx.depth);
            let dds = Dds::encode_from_rgba8(&pixels, layout).expect("encode fixture");
            write_dds(&path, &dds);
            out.push(DecodeCase {
                id,
                content,
                context: ctx.name,
                path,
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Encode
// ---------------------------------------------------------------------------

fn run_encode_baselines(root: &Path, cases_dir: &Path, artifacts: &Path) -> serde_json::Value {
    let _ = fs::remove_dir_all(cases_dir);
    fs::create_dir_all(cases_dir).unwrap();

    let mut rows = Vec::new();
    for ctx in CONTEXTS {
        for &content in DecodeContent::ALL_LDR {
            if !content_allowed(content, ctx) {
                continue;
            }
            let id = format!("{}__{}", content.name(), ctx.name);
            let pixels = fill_rgba(content, ctx.width, ctx.height, ctx.depth);
            let layout = EncodeLayout::flat_2d(content, ctx.width, ctx.height)
                .with_depth(ctx.depth);

            // Case files for DirectXTex harness
            fs::write(cases_dir.join(format!("{id}.rgba")), &pixels).unwrap();
            fs::write(
                cases_dir.join(format!("{id}.meta")),
                format!(
                    "id={id}\nwidth={}\nheight={}\ndepth={}\ndxgi={}\n",
                    ctx.width,
                    ctx.height,
                    ctx.depth,
                    dxgi_name(content)
                ),
            )
            .unwrap();

            let rusty = time_result(|| {
                let dds = Dds::encode_from_rgba8(&pixels, layout).map_err(|e| e.to_string())?;
                Ok(dds.data.len())
            });

            rows.push(serde_json::json!({
                "id": id,
                "content": content.name(),
                "context": ctx.name,
                "rusty_dds_ns": rusty.0,
                "rusty_dds_ok": rusty.1,
            }));
            println!("encode {:<22} rusty={:>8.0}", id, rusty.0);
        }
    }

    let mut dxtex_note = "DirectXTex encode harness not found — skipped".to_string();
    if let Some(exe) = find_dxtex_exe(root, "dxtex_encode_bench") {
        let raw_path = artifacts.join("dxtex_encode_raw.json");
        let status = Command::new(&exe)
            .arg(cases_dir)
            .arg(&raw_path)
            .arg(ITERS.to_string())
            .status();
        if status.map(|s| s.success()).unwrap_or(false) {
            if let Ok(raw) = fs::read_to_string(&raw_path) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
                    merge_dxtex(&mut rows, &v);
                    dxtex_note = "DirectXTex encode included (BC7=TEX_COMPRESS_BC7_QUICK)".into();
                }
            }
        } else {
            dxtex_note = "DirectXTex encode harness failed".into();
        }
    }

    serde_json::json!({
        "iters": ITERS,
        "protocol": "RGBA8 pixels → Compress|Convert (DirectXTex) / encode_from_rgba8 (rusty_dds)",
        "peers": ["rusty_dds", "Microsoft DirectXTex"],
        "notes": [
            "Official peer only: Microsoft DirectXTex Compress / Convert.",
            "Same content × context grid as decode baselines (single surface).",
            "BC7 peer flag: TEX_COMPRESS_BC7_QUICK (mode-6 class, matches rusty_dds).",
            "Other BCn: TEX_COMPRESS_DEFAULT. Uncompressed: Convert or copy.",
            "ratio < 1 means rusty_dds is faster.",
            dxtex_note,
        ],
        "rows": rows,
        "summary": summarize_vs_dxtex(&rows),
    })
}

// ---------------------------------------------------------------------------
// DirectXTex merge / summary / IO
// ---------------------------------------------------------------------------

fn find_dxtex_exe(root: &Path, name: &str) -> Option<PathBuf> {
    [
        root.join(format!("tools/dxtex_decode_bench/build/{name}.exe")),
        root.join(format!("tools/dxtex_decode_bench/build/Release/{name}.exe")),
        root.join(format!("tools/dxtex_decode_bench/build/{name}")),
    ]
    .into_iter()
    .find(|p| p.exists())
}

fn merge_dxtex(rows: &mut [serde_json::Value], raw: &serde_json::Value) {
    let Some(arr) = raw
        .get("cases")
        .or_else(|| raw.get("rows"))
        .and_then(|r| r.as_array())
    else {
        return;
    };
    for row in rows.iter_mut() {
        let id = row["id"].as_str().unwrap_or("");
        if let Some(dx) = arr.iter().find(|r| r["id"].as_str() == Some(id)) {
            let ns = dx.get("ns_per_iter").or_else(|| dx.get("directxtex_ns"));
            let ok = dx.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
            row["directxtex_ok"] = serde_json::json!(ok);
            if let Some(n) = ns.and_then(|v| v.as_f64()) {
                row["directxtex_ns"] = serde_json::json!(n);
                if ok {
                    if let Some(r) = row["rusty_dds_ns"].as_f64() {
                        if n > 0.0 {
                            row["ratio_rusty_over_directxtex"] = serde_json::json!(r / n);
                        }
                    }
                }
            } else if !ok {
                row["directxtex_ns"] = serde_json::Value::Null;
            }
        }
    }
}

fn summarize_vs_dxtex(rows: &[serde_json::Value]) -> serde_json::Value {
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

fn time_result(f: impl Fn() -> Result<usize, String>) -> (f64, bool) {
    match f() {
        Err(_) => return (0.0, false),
        Ok(_) => {}
    }
    for _ in 0..2 {
        let _ = f();
    }
    let t0 = Instant::now();
    let mut sink = 0usize;
    for _ in 0..ITERS {
        sink = sink.wrapping_add(f().unwrap_or(0));
    }
    let _ = sink;
    (t0.elapsed().as_nanos() as f64 / ITERS as f64, true)
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
    if let Some(s) = report.get("summary") {
        md.push_str("## Summary\n\n");
        md.push_str(&format!(
            "```json\n{}\n```\n\n",
            serde_json::to_string_pretty(s).unwrap()
        ));
    }
    md.push_str(
        "| Case | Content | Context | rusty_dds (ns) | DirectXTex (ns) | Ratio (rusty/dx) |\n",
    );
    md.push_str(
        "|------|---------|---------|----------------|-----------------|------------------|\n",
    );
    let rows = report["rows"].as_array().cloned().unwrap_or_default();
    for r in &rows {
        let dx = peer_cell(r, "directxtex");
        let ratio = r["ratio_rusty_over_directxtex"]
            .as_f64()
            .map(|x| format!("{x:.3}"))
            .unwrap_or_else(|| "—".into());
        md.push_str(&format!(
            "| {} | {} | {} | {:.0} | {} | {} |\n",
            r["id"].as_str().unwrap_or(""),
            r["content"].as_str().unwrap_or(""),
            r["context"].as_str().unwrap_or(""),
            r["rusty_dds_ns"].as_f64().unwrap_or(0.0),
            dx,
            ratio,
        ));
    }
    fs::write(&md_path, md).unwrap();
    println!("Wrote {}", json_path.display());
    println!("Wrote {}", md_path.display());
}

fn peer_cell(r: &serde_json::Value, prefix: &str) -> String {
    let ok = r[format!("{prefix}_ok")].as_bool().unwrap_or(false);
    if !ok {
        return "fail".into();
    }
    match r[format!("{prefix}_ns")].as_f64() {
        Some(n) => format!("{n:.0}"),
        None => "—".into(),
    }
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

fn write_dds(path: &Path, dds: &Dds) {
    let mut f = File::create(path).unwrap();
    dds.write(&mut f).unwrap();
    let _ = f.flush();
}
