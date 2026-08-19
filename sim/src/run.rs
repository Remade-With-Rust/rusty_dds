//! The headless run: drive [`Sim`] to completion, write the CSV and the
//! manifest that pins it.
//!
//! This is where reportable numbers come from. The cockpit drives the same
//! [`Sim`], but with a UI attached to the process the timings carry the UI's
//! own cost — see the note in `bin/cockpit.rs`.

use std::path::{Path, PathBuf};

use crate::metrics::{alloc_snapshot, FrameRecord, CSV_HEADER};
use crate::os;
use crate::provider::SimResult;
use crate::sim::{Sim, SimConfig};

#[derive(Clone)]
pub struct RunOptions {
    pub cfg: SimConfig,
    pub out: Option<PathBuf>,
    pub rep: u32,
    pub quiet: bool,
}

pub struct RunSummary {
    pub arm: String,
    pub scenario: &'static str,
    pub frames: u32,
    /// Fold of every frame's `(request_hash, upload_hash)`. Two runs that agree
    /// here did identical work in identical order — the parity gate reduced to
    /// one number.
    pub trace_hash: u64,
    pub cpu_secs: f64,
    pub wall_secs: f64,
    pub peak_working_set: u64,
    pub peak_live_bytes: u64,
    pub alloc_count: u64,
    pub hitches: u32,
    pub csv: Option<PathBuf>,
}

pub fn run(opts: &RunOptions) -> SimResult<RunSummary> {
    let mut sim = Sim::new(&opts.cfg)?;
    let warmup = sim.scenario.warmup;

    let mut records: Vec<FrameRecord> =
        Vec::with_capacity(sim.frames.saturating_sub(warmup) as usize);
    let alloc0 = alloc_snapshot();

    while let Some(rec) = sim.step()? {
        if rec.frame >= warmup {
            records.push(rec);
        }
    }

    let alloc = alloc_snapshot();
    let ws = os::working_set().unwrap_or((0, 0));
    let csv = match &opts.out {
        Some(path) => {
            write_csv(path, &records)?;
            write_manifest(path, opts, &sim)?;
            Some(path.clone())
        }
        None => None,
    };

    Ok(RunSummary {
        arm: opts.cfg.arm.clone(),
        scenario: sim.scenario.name,
        frames: sim.frames,
        trace_hash: sim.trace_hash(),
        cpu_secs: sim.cpu_secs(),
        wall_secs: sim.wall_secs(),
        peak_working_set: ws.1,
        peak_live_bytes: alloc.peak_live,
        alloc_count: alloc.count.saturating_sub(alloc0.count),
        hitches: sim.hitches(),
        csv,
    })
}

fn write_csv(path: &Path, records: &[FrameRecord]) -> SimResult<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut s = String::with_capacity(records.len() * 180 + CSV_HEADER.len() + 2);
    s.push_str(CSV_HEADER);
    s.push('\n');
    for r in records {
        s.push_str(&r.to_csv());
        s.push('\n');
    }
    std::fs::write(path, s)?;
    Ok(())
}

/// The run manifest. `board` refuses to compare runs whose pinned fields differ
/// — that is what stops a board being assembled from two different machines,
/// two different packs, or two different binaries.
fn write_manifest(csv: &Path, opts: &RunOptions, sim: &Sim) -> SimResult<()> {
    let exe = std::env::current_exe().ok();
    let (exe_len, exe_mtime) = exe
        .as_ref()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| {
            (
                m.len(),
                m.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            )
        })
        .unwrap_or((0, 0));

    let cfg = &opts.cfg;
    let mut s = String::new();
    s.push_str("# rusty_dds_sim run manifest v1\n");
    s.push_str(&format!("arm {}\n", cfg.arm));
    s.push_str(&format!("provider {}\n", sim.provider_name));
    s.push_str("renderer null\n");
    s.push_str("profile stream\n");
    s.push_str(&format!("scenario {}\n", sim.scenario.name));
    s.push_str(&format!("frames {}\n", sim.frames));
    s.push_str(&format!("rep {}\n", opts.rep));
    s.push_str(&format!("workers {}\n", cfg.workers));
    s.push_str(&format!("seed {}\n", cfg.seed));
    s.push_str(&format!("tier {}\n", sim.pack.tier.name()));
    s.push_str(&format!("pack_hash {:016x}\n", sim.pack.hash));
    s.push_str(&format!("pack_bytes {}\n", sim.pack.total_bytes));
    s.push_str(&format!("pack_textures {}\n", sim.pack.textures.len()));
    s.push_str(&format!("pool_budget {}\n", sim.pool_budget));
    s.push_str(&format!("peak_demand {}\n", sim.peak_demand));
    s.push_str(&format!("trace_hash {:016x}\n", sim.trace_hash()));
    // Run-level CPU is the robust verdict on a loaded box; the encoder campaign
    // saw wall swing 2-3x while CPU held. The board reads both.
    s.push_str(&format!("cpu_secs {:.6}\n", sim.cpu_secs()));
    s.push_str(&format!("wall_secs {:.6}\n", sim.wall_secs()));
    // Pinned vs unpinned is the easiest way to manufacture a fake difference,
    // so the board treats this as a pinned field and refuses to mix them.
    s.push_str(&format!("pinned {:#x}\n", os::applied_affinity_mask()));
    s.push_str(&format!("os {}\n", os::os_name()));
    s.push_str(&format!(
        "cpus {}\n",
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(0)
    ));
    s.push_str(&format!("exe_len {exe_len}\n"));
    s.push_str(&format!("exe_mtime {exe_mtime}\n"));
    s.push_str(&format!(
        "alloc_counters {}\n",
        cfg!(feature = "alloc-counters")
    ));
    s.push_str(&format!("sim_version {}\n", env!("CARGO_PKG_VERSION")));

    std::fs::write(csv.with_extension("manifest"), s)?;
    Ok(())
}
