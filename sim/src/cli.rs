//! The CLI, shared by both binaries.
//!
//! It lives in the library rather than in `main.rs` because the global
//! allocator is a compile-time choice: `sim` links the system allocator and
//! `sim-ra` links `rusty_alloc`, and they must otherwise be the *same program*
//! or the allocator arm would be comparing two different harnesses.
//!
//! Original header:
//! `rusty_dds_sim` — the measurement harness behind
//! [`docs/plans/simulator-demo.md`](../../docs/plans/simulator-demo.md).
//!
//! **Phase 0 scope: the harness, and the null arm only.** There is no GPU
//! backend and no DirectXTex arm yet, by design: the plan's Phase 0 exit gate is
//! that `A` vs `A` is flat and that the per-frame hash streams are bit-stable.
//! Adding a second stack before that gate passes would mean reporting a
//! difference against an unmeasured noise floor.
//!
//! ```text
//! sim cook   --tier medium --textures 32 --out pack/medium
//! sim verify --pack pack/medium
//! sim bench  --pack pack/medium --scenario traverse --arms a,a2 --reps 7 --out runs/
//! sim board  --runs runs/ --out board.md
//! ```

use std::path::PathBuf;

use crate::provider::{SimError, SimResult};
use crate::scenario::{scenario_by_name, Tier, SCENARIOS};
use crate::sim::SimConfig;
use crate::{bench, board, metrics, os, pack, run};

/// Entry point for both binaries. `allocator` is the name of the global
/// allocator this binary linked, recorded into every run manifest so a board can
/// never silently mix allocator arms.
pub fn main(allocator: &'static str) -> ! {
    metrics::set_allocator_name(allocator);
    match real_main() {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

fn real_main() -> SimResult<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");
    let a = Args::parse(&args[1.min(args.len())..]);

    match cmd {
        "cook" => cmd_cook(&a),
        "run" => cmd_run(&a),
        "bench" => cmd_bench(&a),
        "board" => cmd_board(&a),
        "verify" => cmd_verify(&a),
        #[cfg(any(feature = "d3d11", feature = "vulkan"))]
        "view" => cmd_view(&a),
        #[cfg(any(feature = "d3d11", feature = "vulkan"))]
        "grid" => cmd_grid(&a),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => {
            print_help();
            Err(SimError(format!("unknown command `{other}`")))
        }
    }
}

// ------------------------------------------------------------ argument soup

/// Deliberately tiny: the harness takes no dependency it does not need, and a
/// CLI parser is not worth a supply-chain surface in a crate whose entire
/// purpose is to be trusted.
struct Args {
    flags: Vec<(String, Option<String>)>,
}

impl Args {
    fn parse(argv: &[String]) -> Args {
        let mut flags = Vec::new();
        let mut i = 0;
        while i < argv.len() {
            let a = &argv[i];
            if let Some(name) = a.strip_prefix("--") {
                let next = argv.get(i + 1);
                if let Some(v) = next.filter(|v| !v.starts_with("--")) {
                    flags.push((name.to_string(), Some(v.clone())));
                    i += 2;
                } else {
                    flags.push((name.to_string(), None));
                    i += 1;
                }
            } else {
                i += 1;
            }
        }
        Args { flags }
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.flags
            .iter()
            .find(|(k, _)| k == name)
            .and_then(|(_, v)| v.as_deref())
    }

    fn has(&self, name: &str) -> bool {
        self.flags.iter().any(|(k, _)| k == name)
    }

    fn num<T: std::str::FromStr>(&self, name: &str) -> SimResult<Option<T>> {
        match self.get(name) {
            None => Ok(None),
            Some(v) => v
                .parse()
                .map(Some)
                .map_err(|_| SimError(format!("--{name} expects a number, got `{v}`"))),
        }
    }

    fn path(&self, name: &str) -> Option<PathBuf> {
        self.get(name).map(PathBuf::from)
    }

    fn require_path(&self, name: &str) -> SimResult<PathBuf> {
        self.path(name)
            .ok_or_else(|| SimError(format!("--{name} is required")))
    }
}

fn print_help() {
    eprintln!(
        "rusty_dds_sim — deterministic DDS texture-streaming harness (Phase 0)

  cook    --tier <ultra|high|medium> --out <dir> [--textures N] [--size N] [--threads N]
  run     --pack <dir> --scenario <name> --arm <arm> [--peer loader|scratch] [--workers N] [--out <csv>]
          [--frames N] [--rep N] [--seed N] [--pool-mb F] [--pin [mask]] [--quiet]
  verify  --pack <dir> [--scenario <name>] [--frames N] [--workers N]
  bench   --pack <dir> --scenario <name> --out <dir> [--arms a,a2] [--reps N]
          [--workers N] [--frames N] [--seed N] [--pin [mask]]
  view    --pack <dir> --api <d3d11|vulkan> --arm <arm> [--scenario N] [--label L]
          [--x N --y N --width N --height N] [--turbo] [--pin] [--peer P]
          [--max-uploads-per-frame N] [--max-upload-mib-per-frame N]
          [--gpu-cache-mib N] [--frame-abort-ms N] [--max-run-secs N]
  grid    --pack <dir> [--scenario N] [--frames N] [--isolate <api>] [--peer P]
          [--workers N] [--pool-mult F] [--screen-w N --screen-h N] [--max-run-secs N]
  board   --runs <dir> [--out <md>]

scenarios: {}
arms:      a, a2     rusty_dds + system allocator (the null pair)
           rusty     same, explicit
           dxtex     Microsoft DirectXTex + system allocator   (feature `dxtex`)
           +ra       suffix: swap the system allocator for rusty_alloc,
                     e.g. rusty+ra, dxtex+ra  (runs the `sim-ra` binary)",
        SCENARIOS
            .iter()
            .map(|s| format!("{} ({} frames)", s.name, s.frames))
            .collect::<Vec<_>>()
            .join(", ")
    );
}

// ------------------------------------------------------------------ commands

fn cmd_cook(a: &Args) -> SimResult<()> {
    let tier = a
        .get("tier")
        .and_then(Tier::parse)
        .ok_or_else(|| SimError("--tier must be ultra, high or medium".into()))?;
    let opts = pack::CookOptions {
        tier,
        textures: a.num("textures")?.unwrap_or(32),
        size: a.num("size")?,
        out: a.require_path("out")?,
        threads: a
            .num("threads")?
            .unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)),
    };
    let started = std::time::Instant::now();
    let p = pack::cook(&opts)?;
    println!(
        "cooked {} textures, {} at {}x{}, rdo λ={} -> {:.1} MiB, hash {:016x} ({:.1}s)",
        p.textures.len(),
        p.tier.name(),
        p.size,
        p.size,
        p.rdo_lambda,
        p.total_bytes as f64 / (1 << 20) as f64,
        p.hash,
        started.elapsed().as_secs_f64(),
    );
    Ok(())
}

fn sim_config(a: &Args) -> SimResult<SimConfig> {
    let scenario_name = a.get("scenario").unwrap_or("traverse");
    let scenario = scenario_by_name(scenario_name)
        .ok_or_else(|| SimError(format!("unknown scenario `{scenario_name}`")))?;
    Ok(SimConfig {
        pack_dir: a.require_path("pack")?,
        scenario,
        arm: a.get("arm").unwrap_or("a").to_string(),
        workers: a.num("workers")?.unwrap_or(4),
        frames: a.num("frames")?,
        seed: a.num("seed")?.unwrap_or(0x5EED_1234),
        pool_mb: a.num("pool-mb")?,
        peer: a.get("peer").unwrap_or("loader").to_string(),
        pool_mult: a.num("pool-mult")?.unwrap_or(1.0),
        pool_buffers: a
            .num("pool-buffers")?
            .unwrap_or(crate::stream::DEFAULT_POOLED_BUFFERS),
    })
}

/// Refuse to run an arm this binary cannot honour.
///
/// `#[global_allocator]` is a compile-time choice, so `sim` cannot serve a `+ra`
/// arm and `sim-ra` cannot serve a plain one. Running it anyway would produce a
/// manifest that says one thing and a process that did another — the single
/// most damaging failure this harness could have.
fn check_arm_allocator(arm: &str) -> SimResult<()> {
    let want_ra = crate::provider::parse_arm(arm)?.wants_rusty_alloc;
    let have_ra = metrics::allocator_name() == "rusty_alloc";
    if want_ra != have_ra {
        return Err(SimError(format!(
            "arm `{arm}` wants the {} allocator but this binary linked {} — run it with `{}`",
            if want_ra { "rusty_alloc" } else { "system" },
            metrics::allocator_name(),
            if want_ra { "sim-ra" } else { "sim" },
        )));
    }
    Ok(())
}

fn run_options(a: &Args) -> SimResult<run::RunOptions> {
    Ok(run::RunOptions {
        cfg: sim_config(a)?,
        out: a.path("out"),
        rep: a.num("rep")?.unwrap_or(0),
        quiet: a.has("quiet"),
    })
}

/// Apply `--pin` before any measured work starts.
fn apply_pinning(a: &Args) -> SimResult<Option<usize>> {
    if !a.has("pin") {
        return Ok(None);
    }
    let mask: usize = match a.get("pin") {
        Some(v) => v
            .strip_prefix("0x")
            .map(|h| usize::from_str_radix(h, 16))
            .unwrap_or_else(|| v.parse())
            .map_err(|_| SimError(format!("--pin expects a mask, got `{v}`")))?,
        None => os::DEFAULT_AFFINITY_MASK,
    };
    if !os::pin_process(mask, true) {
        return Err(SimError(format!(
            "could not pin to mask {mask:#x} — refusing to run, because an unpinned run              would silently be compared against pinned ones"
        )));
    }
    Ok(Some(mask))
}

fn cmd_run(a: &Args) -> SimResult<()> {
    let pinned = apply_pinning(a)?;
    let opts = run_options(a)?;
    check_arm_allocator(&opts.cfg.arm)?;
    let s = run::run(&opts)?;
    if !opts.quiet {
        println!(
            "arm {} / {} — {} frames, trace {:016x}\n  \
             wall {:.2}s, cpu {:.2}s, hitches {}, peak RSS {:.0} MiB, peak live {:.0} MiB, allocs {}",
            s.arm,
            s.scenario,
            s.frames,
            s.trace_hash,
            s.wall_secs,
            s.cpu_secs,
            s.hitches,
            s.peak_working_set as f64 / (1 << 20) as f64,
            s.peak_live_bytes as f64 / (1 << 20) as f64,
            s.alloc_count,
        );
    }
    if let Some(csv) = s.csv {
        eprintln!("wrote {}", csv.display());
    }
    if let Some(m) = pinned {
        eprintln!("pinned to mask {m:#x}, high priority");
    }
    Ok(())
}

/// The Phase 0 gate: prove the harness is deterministic before it is ever used
/// to compare two stacks.
///
/// 1. **Repeatability** — the same arm run twice must agree.
/// 2. **Thread-independence** — inline (`--workers 0`) and pooled must agree.
///    Worker completion order varies; the hash must not.
/// 3. **Arm-independence** — `a` and `a2` are the same code and must agree.
fn cmd_verify(a: &Args) -> SimResult<()> {
    apply_pinning(a)?;
    let base = run::RunOptions {
        out: None,
        quiet: true,
        cfg: SimConfig {
            frames: Some(a.num("frames")?.unwrap_or(900)),
            ..sim_config(a)?
        },
        ..run_options(a)?
    };
    let workers = a.num("workers")?.unwrap_or(4);

    fn with(base: &run::RunOptions, f: impl FnOnce(&mut SimConfig)) -> run::RunOptions {
        let mut o = base.clone();
        f(&mut o.cfg);
        o
    }

    fn trace(label: &str, opts: run::RunOptions) -> SimResult<u64> {
        let s = run::run(&opts)?;
        println!(
            "  {label:<28} trace {:016x}  ({:.2}s wall)",
            s.trace_hash, s.wall_secs
        );
        Ok(s.trace_hash)
    }

    println!(
        "verify: {} frames of `{}`",
        base.cfg.frames.unwrap(),
        base.cfg.scenario.name
    );
    let h_inline_1 = trace(
        "inline (workers=0) #1",
        with(&base, |c| c.workers = 0),
    )?;
    let h_inline_2 = trace("inline (workers=0) #2", with(&base, |c| c.workers = 0))?;
    let h_pool = trace(
        &format!("pooled (workers={workers})"),
        with(&base, |c| c.workers = workers),
    )?;
    let h_arm2 = trace("arm a2 (pooled)", with(&base, |c| {
        c.workers = workers;
        c.arm = "a2".into();
    }))?;

    println!();
    let mut fail = false;
    let mut gate = |name: &str, ok: bool| {
        println!("  [{}] {name}", if ok { "PASS" } else { "FAIL" });
        if !ok {
            fail = true;
        }
    };
    gate("repeatability: same arm twice", h_inline_1 == h_inline_2);
    gate("thread-independence: inline == pooled", h_inline_1 == h_pool);
    gate("arm-independence: a == a2", h_pool == h_arm2);
    // `gate` borrows `fail`; end that borrow before reading it.
    let _ = gate;

    if fail {
        return Err(SimError(
            "determinism gate FAILED — the harness is not admissible for A/B until this passes"
                .into(),
        ));
    }
    println!("\nharness is deterministic: trace {h_inline_1:016x}");
    Ok(())
}

/// One live pane. The demo grid launches four of these.
#[cfg(any(feature = "d3d11", feature = "vulkan"))]
fn cmd_view(a: &Args) -> SimResult<()> {
    use crate::gpu::{Api, ViewportConfig};
    use crate::view::{view, ViewOptions};

    apply_pinning(a)?;
    let mut cfg = sim_config(a)?;
    // A pane sized for streaming pressure shows ~20 textures and looks empty.
    // The board stays at 1.0; only the live view loosens, and says so.
    if a.get("pool-mult").is_none() {
        cfg.pool_mult = 3.0;
    }
    check_arm_allocator(&cfg.arm)?;

    let api_name = a.get("api").unwrap_or("d3d11");
    let api = Api::parse(api_name)
        .ok_or_else(|| SimError(format!("unknown --api `{api_name}` — d3d11 or vulkan")))?;

    let label = a
        .get("label")
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{}/{}", api.name(), cfg.arm));

    // Ceilings are overridable, but every one of them defaults to a value that
    // keeps a pane from hammering the driver. See gpu::GpuLimits.
    let mut limits = crate::gpu::GpuLimits::default();
    if let Some(v) = a.num("max-uploads-per-frame")? {
        limits.max_uploads_per_frame = v;
    }
    if let Some(v) = a.num::<u64>("max-upload-mib-per-frame")? {
        limits.max_upload_bytes_per_frame = v << 20;
    }
    if let Some(v) = a.num::<u64>("gpu-cache-mib")? {
        limits.max_gpu_texture_bytes = v << 20;
    }
    if let Some(v) = a.num("frame-abort-ms")? {
        limits.frame_abort_ms = v;
    }
    if let Some(v) = a.num("max-run-secs")? {
        limits.max_run_secs = v;
    }

    let opts = ViewOptions {
        limits,
        viewport: ViewportConfig {
            title: format!("{label} — starting…"),
            x: a.num("x")?.unwrap_or(40),
            y: a.num("y")?.unwrap_or(40),
            width: a.num("width")?.unwrap_or(880),
            height: a.num("height")?.unwrap_or(560),
            overlay: true,
        },
        realtime: !a.has("turbo"),
        api,
        label,
        cfg,
    };
    view(&opts)
}

/// The four-pane demo, from the command line.
#[cfg(any(feature = "d3d11", feature = "vulkan"))]
fn cmd_grid(a: &Args) -> SimResult<()> {
    use crate::panes::{default_grid, isolation_grid, launch, GridOptions};

    let opts = GridOptions {
        pack: a.require_path("pack")?,
        scenario: a.get("scenario").unwrap_or("traverse").to_string(),
        frames: a.num("frames")?,
        workers: a.num("workers")?.unwrap_or(4),
        peer: a.get("peer").unwrap_or("loader").to_string(),
        pool_mult: a.num("pool-mult")?.unwrap_or(3.0),
        screen_w: a.num("screen-w")?.unwrap_or(1920),
        screen_h: a.num("screen-h")?.unwrap_or(1040),
        origin_x: a.num("x")?.unwrap_or(0),
        origin_y: a.num("y")?.unwrap_or(0),
        max_run_secs: a.num("max-run-secs")?.unwrap_or(300.0),
    };

    // `--isolate <api>` runs the 2x2 factorial on one API: neither, rusty_dds
    // alone, rusty_alloc alone, both. `--grid` (default) crosses stack x API.
    let specs = match a.get("isolate") {
        Some(api) => isolation_grid(api),
        None => default_grid(),
    };

    println!("launching {} panes:", specs.len());
    for s in &specs {
        println!("  {:<20} api={:<7} arm={}", s.label, s.api, s.arm);
    }

    let mut grid = launch(&specs, &opts)?;
    let started = std::time::Instant::now();
    // Poll rather than block: the panes are children and the grid must be able
    // to report their state and tear them down together.
    while !grid.all_exited() {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if started.elapsed().as_secs_f64() > opts.max_run_secs + 30.0 {
            eprintln!("grid watchdog: panes outlived their budget, stopping");
            break;
        }
    }
    let final_state = grid.snapshot();
    grid.stop();

    println!("
{:<22} {:>10} {:>10} {:>10} {:>9} {:>10}", "pane", "cpu p50", "cpu p99", "gpu p50", "hitches", "vram MB");
    for p in &final_state {
        println!(
            "{:<22} {:>10} {:>10} {:>10} {:>9} {:>10}",
            p.label,
            p.get("p50"),
            p.get("p99"),
            p.get("gpu_p50"),
            p.get("hitches"),
            p.get("vram_mb"),
        );
        if let Some(e) = &p.error {
            println!("  ! {e}");
        }
    }
    println!(
        "
These are grid figures: four panes contending for cores and one GPU.          The picture is comparable, the numbers are indicative — run `sim bench`          for anything reportable."
    );
    Ok(())
}

fn cmd_bench(a: &Args) -> SimResult<()> {
    let scenario_name = a
        .get("scenario")
        .ok_or_else(|| SimError("--scenario is required".into()))?;
    let scenario = scenario_by_name(scenario_name)
        .ok_or_else(|| SimError(format!("unknown scenario `{scenario_name}`")))?;
    let opts = bench::BenchOptions {
        pack_dir: a.require_path("pack")?,
        scenario,
        arms: a
            .get("arms")
            .unwrap_or("a,a2")
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        reps: a.num("reps")?.unwrap_or(7),
        workers: a.num("workers")?.unwrap_or(4),
        out: a.require_path("out")?,
        frames: a.num("frames")?,
        seed: a.num("seed")?.unwrap_or(0x5EED_1234),
        pin: if a.has("pin") {
            Some(a.get("pin").unwrap_or("").to_string())
        } else {
            None
        },
        peer: a.get("peer").unwrap_or("loader").to_string(),
    };
    let produced = bench::bench(&opts)?;
    println!("{} runs written to {}", produced.len(), opts.out.display());
    println!("next: sim board --runs {}", opts.out.display());
    Ok(())
}

fn cmd_board(a: &Args) -> SimResult<()> {
    let dir = a.require_path("runs")?;
    let out = a.path("out");
    let md = board::board(&dir, out.as_deref())?;
    if let Some(p) = out {
        println!("wrote {}", p.display());
    } else {
        print!("{md}");
    }
    Ok(())
}
