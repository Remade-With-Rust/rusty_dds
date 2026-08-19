//! The ABBA driver.
//!
//! Runs are separate **process launches**, executed strictly one at a time —
//! two arms running concurrently would contend for the same cores and page
//! cache, and the resulting board would measure the contention. Order is
//! `A B B A` per repetition so drift and thermals land on both arms equally.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::provider::{parse_arm, SimError, SimResult};
use crate::scenario::Scenario;

pub struct BenchOptions {
    pub pack_dir: PathBuf,
    pub scenario: Scenario,
    pub arms: Vec<String>,
    pub reps: u32,
    pub workers: usize,
    pub out: PathBuf,
    pub frames: Option<u32>,
    pub seed: u64,
    /// Forwarded to each child as `--pin [mask]`. Every arm is pinned the same
    /// way or none is; a mixed set would make the board meaningless.
    pub pin: Option<String>,
    pub peer: String,
}

pub fn bench(opts: &BenchOptions) -> SimResult<Vec<PathBuf>> {
    if opts.arms.len() < 2 {
        return Err(SimError("a bench needs at least two arms".into()));
    }
    std::fs::create_dir_all(&opts.out)?;
    let here = std::env::current_exe()?;
    let dir = here
        .parent()
        .ok_or_else(|| SimError("cannot locate the harness binaries".into()))?
        .to_path_buf();

    let mut produced = Vec::new();
    for rep in 0..opts.reps {
        // ABBA: forward on even reps, reversed on odd.
        let mut order: Vec<&String> = opts.arms.iter().collect();
        if rep % 2 == 1 {
            order.reverse();
        }
        for arm in order {
            let exe = exe_for_arm(&dir, arm)?;
            // `+ra` in the label is a filesystem-safe stand-in for the binary.
            let label = arm.replace('+', "_");
            let csv = opts
                .out
                .join(format!("{}__{}__rep{:02}.csv", opts.scenario.name, label, rep));
            eprintln!(
                "[bench] rep {rep} arm {arm} via {} -> {}",
                exe.file_name().unwrap_or_default().to_string_lossy(),
                csv.display()
            );
            run_child(&exe, opts, arm, rep, &csv)?;
            produced.push(csv);
        }
    }
    Ok(produced)
}

/// Which binary can serve this arm.
///
/// The allocator cannot be switched at runtime, so `+ra` arms run `sim-ra` and
/// everything else runs `sim`. A missing binary is an error rather than a silent
/// fall back to the wrong allocator.
fn exe_for_arm(dir: &Path, arm: &str) -> SimResult<PathBuf> {
    let want_ra = parse_arm(arm)?.wants_rusty_alloc;
    let name = if want_ra { "sim-ra" } else { "sim" };
    for candidate in [format!("{name}.exe"), name.to_string()] {
        let p = dir.join(candidate);
        if p.exists() {
            return Ok(p);
        }
    }
    Err(SimError(format!(
        "arm `{arm}` needs `{name}` next to the bench binary — build it with          `cargo build --release --features rusty-alloc`"
    )))
}

fn run_child(
    exe: &Path,
    opts: &BenchOptions,
    arm: &str,
    rep: u32,
    csv: &Path,
) -> SimResult<()> {
    let mut cmd = Command::new(exe);
    cmd.arg("run")
        .arg("--pack")
        .arg(&opts.pack_dir)
        .arg("--scenario")
        .arg(opts.scenario.name)
        .arg("--arm")
        .arg(arm)
        .arg("--rep")
        .arg(rep.to_string())
        .arg("--workers")
        .arg(opts.workers.to_string())
        .arg("--seed")
        .arg(opts.seed.to_string())
        .arg("--peer")
        .arg(&opts.peer)
        .arg("--out")
        .arg(csv)
        .arg("--quiet");
    if let Some(f) = opts.frames {
        cmd.arg("--frames").arg(f.to_string());
    }
    if let Some(mask) = &opts.pin {
        cmd.arg("--pin");
        if !mask.is_empty() {
            cmd.arg(mask);
        }
    }

    let status = cmd
        .status()
        .map_err(|e| SimError(format!("spawning `{} run`: {e}", exe.display())))?;
    if !status.success() {
        return Err(SimError(format!(
            "run failed (arm {arm}, rep {rep}): exit {status}"
        )));
    }
    Ok(())
}
