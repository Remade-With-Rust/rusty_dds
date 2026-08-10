//! Side-by-side decode: rusty_dds vs Microsoft DirectXTex (official DDS stack).
//!
//! Protocol (identical for both peers): load DDS bytes from memory, produce RGBA8.
//! - rusty_dds: `Dds::read` + `decode_rgba8`
//! - DirectXTex: `LoadFromDDSMemory` + `Decompress`/`Convert` → `R8G8B8A8_UNORM`
//!
//! ```text
//! cargo run --release --example bench_vs_directxtex
//! ```
//!
//! Requires a built `tools/dxtex_decode_bench` binary (see that folder's CMake).

use rusty_dds::*;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

const ITERS: u32 = 50;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cases_dir = root.join("target/dxtex_bench_cases");
    let artifacts = root.join("docs/artifacts");
    fs::create_dir_all(&cases_dir).unwrap();
    fs::create_dir_all(&artifacts).unwrap();

    let cases = build_cases(&cases_dir);
    println!("wrote {} cases to {}", cases.len(), cases_dir.display());

    let mut rusty_rows = Vec::new();
    for case in &cases {
        let bytes = fs::read(&case.path).unwrap();
        // warmup
        for _ in 0..3 {
            let _ = decode_rusty(&bytes, case);
        }
        let t0 = Instant::now();
        let mut sink = 0usize;
        for _ in 0..ITERS {
            sink += decode_rusty(&bytes, case);
        }
        let ns = t0.elapsed().as_nanos() as f64 / ITERS as f64;
        println!("rusty_dds  {:<28} {:10.1} ns/iter", case.id, ns);
        rusty_rows.push(serde_json_row(&case.id, true, ns, sink, None));
    }

    let dxtex_exe = root.join("tools/dxtex_decode_bench/build/dxtex_decode_bench.exe");
    let alt = root.join("tools/dxtex_decode_bench/build/Release/dxtex_decode_bench.exe");
    let exe = if dxtex_exe.exists() {
        dxtex_exe
    } else if alt.exists() {
        alt
    } else {
        eprintln!(
            "ERROR: DirectXTex harness not built at tools/dxtex_decode_bench/build/\n\
             Clone: git clone --depth 1 https://github.com/microsoft/DirectXTex.git third_party/DirectXTex\n\
             Build (from VS x64 Native Tools):\n\
               cmake -S tools/dxtex_decode_bench -B tools/dxtex_decode_bench/build -G Ninja -DCMAKE_BUILD_TYPE=Release\n\
               cmake --build tools/dxtex_decode_bench/build"
        );
        std::process::exit(1);
    };
    run_dxtex(&exe, &cases_dir, &artifacts);

    let dxtex_raw: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(artifacts.join("dxtex_raw.json")).expect("dxtex_raw.json"),
    )
    .unwrap();

    let merged = merge_report(&cases, &rusty_rows, &dxtex_raw);
    let out = artifacts.join("decode-vs-directxtex.json");
    fs::write(&out, serde_json::to_string_pretty(&merged).unwrap()).unwrap();
    write_markdown_summary(&artifacts.join("decode-vs-directxtex.md"), &merged);
    println!("\nWrote {}", out.display());
    println!("Wrote {}", artifacts.join("decode-vs-directxtex.md").display());
}

fn run_dxtex(exe: &Path, cases_dir: &Path, artifacts: &Path) {
    let out = artifacts.join("dxtex_raw.json");
    let status = Command::new(exe)
        .arg(cases_dir)
        .arg(&out)
        .arg(ITERS.to_string())
        .status()
        .expect("spawn dxtex_decode_bench");
    if !status.success() {
        eprintln!("dxtex_decode_bench failed: {status}");
        std::process::exit(1);
    }
}

struct Case {
    id: String,
    content: String,
    context: String,
    path: PathBuf,
    /// Subresource to decode on the rusty_dds side (standalone files are always mip0/layer0).
    id_sub: SubresourceId,
}

fn build_cases(dir: &Path) -> Vec<Case> {
    let mut cases = Vec::new();

    // --- X-2D: every LDR content ---
    for &content in DecodeContent::ALL_LDR {
        let id = format!("{}__X-2D", content.name());
        let path = dir.join(format!("{id}.dds"));
        let mut dds = make_dxgi(content, 64, 64, None, Some(1), None, false);
        fill_case(&mut dds.data, content);
        write_dds(&path, &dds);
        cases.push(Case {
            id,
            content: content.name().into(),
            context: "X-2D".into(),
            path,
            id_sub: SubresourceId::mip_layer(0, 0),
        });
    }

    // --- X-MIP tip as standalone small DDS ---
    {
        let content = DecodeContent::Bc1;
        let id = "bc1__X-MIP-tip".to_string();
        let path = dir.join(format!("{id}.dds"));
        // Tip of 64 mip chain ≈ 1x1 block → use 4x4 min BC block surface
        let mut dds = make_dxgi(content, 4, 4, None, Some(1), None, false);
        fill_case(&mut dds.data, content);
        write_dds(&path, &dds);
        cases.push(Case {
            id,
            content: content.name().into(),
            context: "X-MIP".into(),
            path,
            id_sub: SubresourceId::mip_layer(0, 0),
        });
    }

    // --- X-ARRAY: single layer exported as 2D ---
    {
        let content = DecodeContent::Bc3;
        let id = "bc3__X-ARRAY".to_string();
        let path = dir.join(format!("{id}.dds"));
        let mut dds = make_dxgi(content, 32, 32, None, Some(1), None, false);
        fill_case(&mut dds.data, content);
        write_dds(&path, &dds);
        cases.push(Case {
            id,
            content: content.name().into(),
            context: "X-ARRAY".into(),
            path,
            id_sub: SubresourceId::mip_layer(0, 0),
        });
    }

    // --- X-CUBE: one face as 2D ---
    {
        let content = DecodeContent::Bc1;
        let id = "bc1__X-CUBE-face".to_string();
        let path = dir.join(format!("{id}.dds"));
        let mut dds = make_dxgi(content, 32, 32, None, Some(1), None, false);
        fill_case(&mut dds.data, content);
        write_dds(&path, &dds);
        cases.push(Case {
            id,
            content: content.name().into(),
            context: "X-CUBE".into(),
            path,
            id_sub: SubresourceId::mip_layer(0, 0),
        });
    }

    // --- X-NPOT ---
    {
        let content = DecodeContent::Bc7;
        let id = "bc7__X-NPOT".to_string();
        let path = dir.join(format!("{id}.dds"));
        let mut dds = make_dxgi(content, 2, 3, None, Some(1), None, false);
        fill_case(&mut dds.data, content);
        write_dds(&path, &dds);
        cases.push(Case {
            id,
            content: content.name().into(),
            context: "X-NPOT".into(),
            path,
            id_sub: SubresourceId::mip_layer(0, 0),
        });
    }

    // --- X-VOL ---
    {
        let content = DecodeContent::Bc1;
        let id = "bc1__X-VOL".to_string();
        let path = dir.join(format!("{id}.dds"));
        let mut dds = make_dxgi(content, 16, 16, Some(4), Some(1), None, false);
        fill_case(&mut dds.data, content);
        write_dds(&path, &dds);
        cases.push(Case {
            id,
            content: content.name().into(),
            context: "X-VOL".into(),
            path,
            id_sub: SubresourceId::mip_layer(0, 0),
        });
    }

    cases
}

fn decode_rusty(bytes: &[u8], case: &Case) -> usize {
    let dds = Dds::read(std::io::Cursor::new(bytes)).expect("rusty parse");
    let img = dds.decode_rgba8(case.id_sub).expect("rusty decode");
    img.pixels.len()
}

fn make_dxgi(
    content: DecodeContent,
    width: u32,
    height: u32,
    depth: Option<u32>,
    mips: Option<u32>,
    array_layers: Option<u32>,
    is_cubemap: bool,
) -> Dds {
    Dds::new_dxgi(NewDxgiParams {
        height,
        width,
        depth,
        format: dxgi_for(content),
        mipmap_levels: mips,
        array_layers,
        caps2: None,
        is_cubemap,
        resource_dimension: if depth.unwrap_or(1) > 1 {
            D3D10ResourceDimension::Texture3D
        } else {
            D3D10ResourceDimension::Texture2D
        },
        alpha_mode: AlphaMode::Straight,
    })
    .unwrap()
}

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
    }
}

fn fill_case(data: &mut [u8], content: DecodeContent) {
    for (i, b) in data.iter_mut().enumerate() {
        *b = ((i * 37 + 11) % 251) as u8;
    }
    if let Some(bs) = content.block_bytes() {
        if matches!(
            content,
            DecodeContent::Bc1 | DecodeContent::Bc2 | DecodeContent::Bc3
        ) {
            let mut block = vec![0u8; bs];
            match content {
                DecodeContent::Bc1 => block[0..2].copy_from_slice(&0xF800u16.to_le_bytes()),
                DecodeContent::Bc2 => {
                    block[..8].fill(0xFF);
                    block[8..10].copy_from_slice(&0xF800u16.to_le_bytes());
                }
                DecodeContent::Bc3 => {
                    block[0] = 255;
                    block[8..10].copy_from_slice(&0xF800u16.to_le_bytes());
                }
                _ => {}
            }
            for chunk in data.chunks_exact_mut(bs) {
                chunk.copy_from_slice(&block);
            }
        }
    }
}

fn write_dds(path: &Path, dds: &Dds) {
    let mut f = BufWriter::new(File::create(path).unwrap());
    dds.write(&mut f).unwrap();
    f.flush().unwrap();
}

fn serde_json_row(
    id: &str,
    ok: bool,
    ns: f64,
    sink: usize,
    hr: Option<u64>,
) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert("id".into(), id.into());
    m.insert("ok".into(), ok.into());
    m.insert("ns_per_iter".into(), ns.into());
    m.insert("sink".into(), sink.into());
    if let Some(hr) = hr {
        m.insert("hr".into(), hr.into());
    }
    serde_json::Value::Object(m)
}

fn merge_report(
    cases: &[Case],
    rusty: &[serde_json::Value],
    dxtex: &serde_json::Value,
) -> serde_json::Value {
    let dx_cases = dxtex["cases"].as_array().cloned().unwrap_or_default();
    let mut by_id = std::collections::BTreeMap::new();
    for c in dx_cases {
        if let Some(id) = c.get("id").and_then(|v| v.as_str()) {
            by_id.insert(id.to_string(), c);
        }
    }

    let mut rows = Vec::new();
    for (case, r) in cases.iter().zip(rusty.iter()) {
        let d = by_id.get(&case.id);
        let rusty_ns = r.get("ns_per_iter").and_then(|v| v.as_f64());
        let dx_ok = d.and_then(|v| v.get("ok")).and_then(|v| v.as_bool());
        let dx_ns = d.and_then(|v| v.get("ns_per_iter")).and_then(|v| v.as_f64());
        let ratio = match (rusty_ns, dx_ns, dx_ok) {
            (Some(a), Some(b), Some(true)) if b > 0.0 => Some(a / b),
            _ => None,
        };
        let struggle = ratio.map(|r| r > 1.25).unwrap_or(true);
        rows.push(serde_json::json!({
            "id": case.id,
            "content": case.content,
            "context": case.context,
            "rusty_dds_ns": rusty_ns,
            "directxtex_ns": dx_ns,
            "directxtex_ok": dx_ok,
            "ratio_rusty_over_dxtex": ratio,
            "rusty_slower": struggle,
            "verdict": match ratio {
                Some(r) if r <= 0.9 => "ahead",
                Some(r) if r <= 1.25 => "parity",
                Some(_) => "behind",
                None => "dxtex_failed_or_missing",
            }
        }));
    }

    let behind: Vec<_> = rows
        .iter()
        .filter(|r| r["verdict"] == "behind")
        .cloned()
        .collect();
    let ahead_n = rows.iter().filter(|r| r["verdict"] == "ahead").count();
    let parity_n = rows.iter().filter(|r| r["verdict"] == "parity").count();
    let failed_n = rows
        .iter()
        .filter(|r| r["verdict"] == "dxtex_failed_or_missing")
        .count();
    let mut worst = behind.clone();
    worst.sort_by(|a, b| {
        let ra = a["ratio_rusty_over_dxtex"].as_f64().unwrap_or(0.0);
        let rb = b["ratio_rusty_over_dxtex"].as_f64().unwrap_or(0.0);
        rb.partial_cmp(&ra).unwrap()
    });
    worst.truncate(8);

    serde_json::json!({
        "title": "rusty_dds vs Microsoft DirectXTex — DDS decode to RGBA8",
        "peer": {
            "name": "Microsoft DirectXTex",
            "repo": "https://github.com/microsoft/DirectXTex",
            "protocol": "LoadFromDDSMemory + Decompress|Convert -> R8G8B8A8_UNORM"
        },
        "ours": {
            "name": "rusty_dds",
            "protocol": "Dds::read + decode_rgba8 -> RGBA8"
        },
        "iters": ITERS,
        "notes": [
            "Same .dds case files for both peers.",
            "Both paths include DDS parse + decode to RGBA8 each iteration.",
            "ratio > 1 means rusty_dds is slower. Struggle threshold: ratio > 1.25.",
            "BC6H / float HDR excluded from this LDR RGBA8 gate."
        ],
        "summary": {
            "cases": rows.len(),
            "ahead": ahead_n,
            "parity": parity_n,
            "behind": behind.len(),
            "dxtex_failed": failed_n
        },
        "worst_behind": worst,
        "rows": rows
    })
}

fn write_markdown_summary(path: &Path, report: &serde_json::Value) {
    let mut md = String::new();
    md.push_str("# rusty_dds vs Microsoft DirectXTex\n\n");
    md.push_str(&format!(
        "Peer: [{}]({})\n\n",
        report["peer"]["name"].as_str().unwrap(),
        report["peer"]["repo"].as_str().unwrap()
    ));
    md.push_str(&format!(
        "Protocol: `{}` vs `{}`\n\n",
        report["ours"]["protocol"].as_str().unwrap(),
        report["peer"]["protocol"].as_str().unwrap()
    ));
    let s = &report["summary"];
    md.push_str(&format!(
        "Summary: {} cases — **{} ahead**, {} parity, **{} behind**, {} dxtex-failed\n\n",
        s["cases"], s["ahead"], s["parity"], s["behind"], s["dxtex_failed"]
    ));
    md.push_str("| Case | Content | Context | rusty_dds (ns) | DirectXTex (ns) | Ratio | Verdict |\n");
    md.push_str("|------|---------|---------|----------------|-----------------|-------|---------|\n");
    if let Some(rows) = report["rows"].as_array() {
        for r in rows {
            md.push_str(&format!(
                "| {} | {} | {} | {:.0} | {:.0} | {:.2} | {} |\n",
                r["id"].as_str().unwrap_or(""),
                r["content"].as_str().unwrap_or(""),
                r["context"].as_str().unwrap_or(""),
                r["rusty_dds_ns"].as_f64().unwrap_or(0.0),
                r["directxtex_ns"].as_f64().unwrap_or(0.0),
                r["ratio_rusty_over_dxtex"].as_f64().unwrap_or(0.0),
                r["verdict"].as_str().unwrap_or(""),
            ));
        }
    }
    fs::write(path, md).unwrap();
}
