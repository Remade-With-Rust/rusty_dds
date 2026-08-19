//! CSV runs → a markdown board, in the house format used by `docs/artifacts/`.
//!
//! The board's job is to refuse as often as it reports:
//!
//! * runs whose pinned manifest fields disagree (pack, tier, workers, machine,
//!   binary) are **not comparable** and the board says so instead of averaging;
//! * runs whose `trace_hash` disagrees did **different work** — that is the
//!   work-count parity gate, and it rejects rather than reports;
//! * every metric is printed next to **its own null band**, measured as the
//!   run-to-run spread within a single arm, and a difference narrower than that
//!   band is reported as *inside the noise* rather than as a result.
//!
//! The per-metric band matters. Phase 0's first board showed a ±40% band on
//! median frame cost and a far tighter one on run CPU seconds — from the *same*
//! runs. A single headline band would have thrown away the metrics that work.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::metrics::{median, pct, sorted, HITCH_MS};
use crate::provider::{SimError, SimResult};

// ---------------------------------------------------------------- ingestion

/// One metric's per-run values, in run order.
type Series = Vec<f64>;

struct Run {
    arm: String,
    rep: u32,
    manifest: BTreeMap<String, String>,
    metrics: BTreeMap<&'static str, f64>,
    frames: u32,
}

impl Run {
    fn field(&self, key: &str) -> &str {
        self.manifest.get(key).map(|s| s.as_str()).unwrap_or("?")
    }
}

/// (key, label, unit, lower-is-better)
const METRICS: &[(&str, &str, &str)] = &[
    ("cpu_secs", "Run CPU time", "s"),
    ("stream_ms_total", "Streaming CPU, total", "ms"),
    ("parse_ms_total", "Container parse, total", "ms"),
    ("upload_ms_total", "Staging copy, total", "ms"),
    ("median_cpu_ms", "Frame cost, median", "ms"),
    ("p99_cpu_ms", "Frame cost, p99", "ms"),
    ("p999_cpu_ms", "Frame cost, p99.9", "ms"),
    ("max_cpu_ms", "Frame cost, max", "ms"),
    ("hitches", "Hitches (> 1 ms)", ""),
    ("peak_rss_mb", "Peak working set", "MiB"),
    ("alloc_count", "Allocations", ""),
    ("uploaded_mib", "Uploaded (parity check)", "MiB"),
];

fn load_run(csv: &Path) -> SimResult<Run> {
    let manifest_path = csv.with_extension("manifest");
    let manifest_text = std::fs::read_to_string(&manifest_path).map_err(|e| {
        SimError(format!(
            "{}: missing run manifest ({e}) — a CSV without its manifest is not admissible",
            manifest_path.display()
        ))
    })?;
    let mut manifest = BTreeMap::new();
    for line in manifest_text.lines() {
        if line.starts_with('#') {
            continue;
        }
        let mut it = line.splitn(2, ' ');
        if let (Some(k), Some(v)) = (it.next(), it.next()) {
            manifest.insert(k.to_string(), v.trim().to_string());
        }
    }

    let text = std::fs::read_to_string(csv)?;
    let mut lines = text.lines();
    let header: Vec<&str> = lines
        .next()
        .ok_or_else(|| SimError(format!("{}: empty CSV", csv.display())))?
        .split(',')
        .collect();
    let col = |name: &str| -> SimResult<usize> {
        header
            .iter()
            .position(|h| *h == name)
            .ok_or_else(|| SimError(format!("{}: CSV has no `{name}` column", csv.display())))
    };
    let (c_cpu, c_stream, c_parse, c_upload, c_rss, c_bytes, c_hitch, c_alloc) = (
        col("cpu_frame_ms")?,
        col("stream_cpu_ms")?,
        col("parse_ms")?,
        col("upload_ms")?,
        col("peak_rss_mb")?,
        col("bytes_uploaded")?,
        col("hitch")?,
        col("alloc_count")?,
    );

    let (mut frame_ms, mut stream, mut parse, mut upload) =
        (Vec::new(), 0.0f64, 0.0f64, 0.0f64);
    let (mut rss, mut uploaded, mut hitches, mut allocs, mut frames) = (0.0f64, 0u64, 0u32, 0u64, 0u32);

    for line in lines {
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(',').collect();
        let num = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        frame_ms.push(num(c_cpu));
        stream += num(c_stream);
        parse += num(c_parse);
        upload += num(c_upload);
        rss = rss.max(num(c_rss));
        uploaded += num(c_bytes) as u64;
        hitches += num(c_hitch) as u32;
        allocs += num(c_alloc) as u64;
        frames += 1;
    }
    if frames == 0 {
        return Err(SimError(format!("{}: no frames recorded", csv.display())));
    }

    let s = sorted(&frame_ms);
    let mut metrics = BTreeMap::new();
    metrics.insert(
        "cpu_secs",
        manifest
            .get("cpu_secs")
            .and_then(|v| v.parse().ok())
            .unwrap_or(f64::NAN),
    );
    metrics.insert("stream_ms_total", stream);
    metrics.insert("parse_ms_total", parse);
    metrics.insert("upload_ms_total", upload);
    metrics.insert("median_cpu_ms", median(&s));
    metrics.insert("p99_cpu_ms", pct(&s, 99.0));
    metrics.insert("p999_cpu_ms", pct(&s, 99.9));
    metrics.insert("max_cpu_ms", s.last().copied().unwrap_or(f64::NAN));
    metrics.insert("hitches", hitches as f64);
    metrics.insert("peak_rss_mb", rss);
    metrics.insert("alloc_count", allocs as f64);
    metrics.insert("uploaded_mib", uploaded as f64 / (1 << 20) as f64);

    Ok(Run {
        arm: manifest
            .get("arm")
            .cloned()
            .unwrap_or_else(|| "?".to_string()),
        rep: manifest
            .get("rep")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        manifest,
        metrics,
        frames,
    })
}

// ------------------------------------------------------------- aggregation

/// Run-to-run spread within one arm, as a fraction: `max/min - 1`.
///
/// This is the noise floor the arm carries on its own — the only honest yardstick
/// for a difference between two arms.
fn spread(series: &Series) -> f64 {
    let s: Vec<f64> = sorted(series).into_iter().filter(|v| v.is_finite()).collect();
    match (s.first(), s.last()) {
        (Some(&lo), Some(&hi)) if lo > 0.0 => hi / lo - 1.0,
        _ => f64::NAN,
    }
}

fn fmt(v: f64, unit: &str) -> String {
    if v.is_nan() {
        return "—".into();
    }
    let n = if v.abs() >= 100.0 {
        format!("{v:.1}")
    } else if v.abs() >= 1.0 {
        format!("{v:.3}")
    } else {
        format!("{v:.4}")
    };
    if unit.is_empty() {
        n
    } else {
        format!("{n} {unit}")
    }
}

// ------------------------------------------------------------------- render

pub fn board(dir: &Path, out: Option<&Path>) -> SimResult<String> {
    let mut csvs: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|e| e == "csv").unwrap_or(false))
        .collect();
    csvs.sort();
    if csvs.is_empty() {
        return Err(SimError(format!("no CSV runs in {}", dir.display())));
    }
    let runs: Vec<Run> = csvs.iter().map(|p| load_run(p)).collect::<SimResult<_>>()?;

    let mut by_arm: BTreeMap<String, Vec<&Run>> = BTreeMap::new();
    for r in &runs {
        by_arm.entry(r.arm.clone()).or_default().push(r);
    }
    let arms: Vec<String> = by_arm.keys().cloned().collect();

    let mut md = String::new();
    md.push_str("# Simulator board — Phase 0 (null arm, no GPU backend)\n\n");
    md.push_str(
        "Profile `stream`, renderer `null`. Phase 0 measures the CPU streaming path only: \
         container parse, subresource slicing, upload-plan construction and the staging copy. \
         There is no swapchain, so GPU columns are absent by construction rather than omitted — \
         the D3D11 backend lands in Phase 1.\n\n",
    );

    // --- gate 1: comparability ------------------------------------------------
    // Fields that must match across *every* run: the workload and the machine.
    let pinned_globally = [
        "pack_hash",
        "tier",
        "workers",
        "scenario",
        "frames",
        "os",
        "cpus",
        "alloc_counters",
        "seed",
        "pool_budget",
        "pinned",
        // A demo-tuned pool must never be averaged in with a measured one.
        "pool_mult",
        "pool_buffers",
    ];
    // Fields that must be constant *within* an arm but legitimately differ
    // between arms — an arm IS a choice of stack, allocator and binary. The
    // allocator arm cannot share a binary with the system arm, because
    // `#[global_allocator]` is chosen at compile time.
    let pinned_per_arm = ["exe_len", "exe_mtime", "allocator", "provider", "peer"];

    let mut mismatches = Vec::new();
    for key in pinned_globally {
        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        for r in &runs {
            *seen.entry(r.field(key)).or_insert(0) += 1;
        }
        if seen.len() > 1 {
            let detail = seen
                .iter()
                .map(|(v, n)| format!("`{v}` x{n}"))
                .collect::<Vec<_>>()
                .join(", ");
            mismatches.push(format!("- **{key}** differs across runs: {detail}"));
        }
    }
    for (arm, rs) in &by_arm {
        for key in pinned_per_arm {
            let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
            for r in rs {
                *seen.entry(r.field(key)).or_insert(0) += 1;
            }
            if seen.len() > 1 {
                let detail = seen
                    .iter()
                    .map(|(v, n)| format!("`{v}` x{n}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                mismatches.push(format!(
                    "- **{key}** differs *within* arm `{arm}`: {detail}"
                ));
            }
        }
    }

    // --- gate 2: work-count parity --------------------------------------------
    let mut traces: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for r in &runs {
        traces
            .entry(r.field("trace_hash"))
            .or_default()
            .push(format!("{}#{:02}", r.arm, r.rep));
    }

    md.push_str("## Gates\n\n");
    if mismatches.is_empty() {
        md.push_str(
            "- **Comparability**: PASS — every run pins the same pack, tier, worker count, \
             frame count, pool budget and machine; and within each arm, the same binary, \
             allocator, stack and peer.\n",
        );
    } else {
        md.push_str("- **Comparability**: **REJECT** — these runs are not comparable:\n");
        for m in &mismatches {
            md.push_str(&format!("  {m}\n"));
        }
    }
    if traces.len() == 1 {
        let (h, who) = traces.iter().next().unwrap();
        md.push_str(&format!(
            "- **Work-count parity**: PASS — all {} runs share `trace_hash = {h}`. \
             Every frame requested the same subresources and handed the renderer the same bytes.\n",
            who.len()
        ));
    } else {
        md.push_str("- **Work-count parity**: **REJECT** — runs did different work:\n");
        for (h, who) in &traces {
            md.push_str(&format!("  - `{h}` — {}\n", who.join(", ")));
        }
    }
    md.push('\n');

    // --- run pinning ----------------------------------------------------------
    let sample = &runs[0];
    let mib = |key: &str| -> String {
        sample
            .field(key)
            .parse::<f64>()
            .map(|b| format!("{:.1} MiB", b / (1 << 20) as f64))
            .unwrap_or_else(|_| "?".into())
    };
    md.push_str("## Run\n\n");
    md.push_str("| scenario | tier | workers | textures | pack | peak demand | pool budget | frames/arm | reps | arms | affinity |\n");
    md.push_str("|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|\n");
    md.push_str(&format!(
        "| `{}` | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n\n",
        sample.field("scenario"),
        sample.field("tier"),
        sample.field("workers"),
        sample.field("pack_textures"),
        mib("pack_bytes"),
        mib("peak_demand"),
        mib("pool_budget"),
        sample.frames,
        by_arm.values().next().map(|v| v.len()).unwrap_or(0),
        arms.join(", "),
        match sample.field("pinned") {
            "0x0" | "?" => "unpinned".to_string(),
            m => format!("`{m}` + high priority"),
        },
    ));

    // --- the metric table -----------------------------------------------------
    md.push_str("## Metrics, each against its own null band\n\n");
    md.push_str("| metric |");
    for a in &arms {
        md.push_str(&format!(" `{a}` |"));
    }
    // Name the pair the delta compares — with more than two arms the column
    // would otherwise be read as applying to whichever two the reader assumes.
    md.push_str(&format!(
        " delta (`{}` vs `{}`) | null band | verdict |\n|---|",
        arms.first().map(|s| s.as_str()).unwrap_or("?"),
        arms.get(1).map(|s| s.as_str()).unwrap_or("?"),
    ));
    for _ in &arms {
        md.push_str("---:|");
    }
    md.push_str("---:|---:|---|\n");

    let mut resolvable = Vec::new();
    let mut unresolvable = Vec::new();

    for (key, label, unit) in METRICS {
        let per_arm: Vec<(String, Series)> = arms
            .iter()
            .map(|a| {
                (
                    a.clone(),
                    by_arm[a]
                        .iter()
                        .map(|r| *r.metrics.get(key).unwrap_or(&f64::NAN))
                        .collect::<Series>(),
                )
            })
            .collect();

        // The band is the widest within-arm spread: the noise floor any
        // between-arm difference has to clear.
        let band = per_arm
            .iter()
            .map(|(_, s)| spread(s))
            .filter(|v| v.is_finite())
            .fold(0.0f64, f64::max);

        let centres: Vec<f64> = per_arm
            .iter()
            .map(|(_, s)| {
                let finite: Vec<f64> = s.iter().copied().filter(|v| v.is_finite()).collect();
                if finite.is_empty() {
                    f64::NAN
                } else {
                    median(&sorted(&finite))
                }
            })
            .collect();
        let delta = match (centres.first(), centres.get(1)) {
            (Some(&a), Some(&b)) if b != 0.0 && a.is_finite() && b.is_finite() => a / b - 1.0,
            _ => f64::NAN,
        };

        let all_zero = centres.iter().all(|c| *c == 0.0);
        let verdict = if all_zero {
            // 0 vs 0 is a genuine agreement, not missing data — the ratio is
            // just undefined.
            "identical (both zero)".to_string()
        } else if !delta.is_finite() || !band.is_finite() {
            "no data".to_string()
        } else if delta.abs() <= band {
            unresolvable.push(*label);
            "inside the noise".to_string()
        } else {
            resolvable.push((*label, band));
            "**outside the band**".to_string()
        };

        md.push_str(&format!("| {label} |"));
        for c in &centres {
            md.push_str(&format!(" {} |", fmt(*c, unit)));
        }
        md.push_str(&format!(
            " {:+.2}% | ±{:.2}% | {verdict} |\n",
            delta * 100.0,
            band * 100.0
        ));
    }

    // --- reading ---------------------------------------------------------------
    md.push_str("\n## Reading this board\n\n");
    md.push_str(&format!(
        "Both arms are **the same build** — `a` and `a2` differ only in their label. Every row \
         should therefore read *inside the noise*, and the `null band` column is the real \
         output: it is the smallest difference each metric can resolve on this machine, and \
         no later phase may report anything narrower than it.\n\n\
         `Uploaded` is a parity check rather than a result: in the stream profile both arms hand \
         the renderer byte-identical data, so any spread there means the runs are not comparable. \
         Hitches count frames costing more than {HITCH_MS} ms on the streaming path — Phase 0 has \
         no present, so it cannot yet use the definition studios use (a frame that missed its \
         deadline).\n\n",
    ));

    if !resolvable.is_empty() {
        md.push_str(
            "**Attention** — these rows landed outside their own band on a null comparison, \
             which means the band is understated, the machine was not quiet, or the metric is \
             unstable. Do not build a Phase 2 claim on them until a quiet re-run says otherwise:\n\n",
        );
        for (label, band) in &resolvable {
            md.push_str(&format!("- {label} (band ±{:.2}%)\n", band * 100.0));
        }
        md.push('\n');
    }

    md.push_str("---\n\nReproduce:\n\n```sh\ncd sim\ncargo run --release --bin sim -- cook --tier medium --textures 192 --out pack/medium192\ncargo run --release --bin sim -- verify --pack pack/medium192\ncargo run --release --bin sim -- bench --pack pack/medium192 --scenario traverse --arms a,a2 --reps 7 --out runs/traverse\ncargo run --release --bin sim -- board --runs runs/traverse\n```\n");

    if let Some(path) = out {
        std::fs::write(path, &md)?;
    }
    Ok(md)
}
