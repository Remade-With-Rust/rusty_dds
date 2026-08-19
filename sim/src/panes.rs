//! The four-pane demo: spawn one `sim view` process per pane, tile their
//! windows, and aggregate their telemetry.
//!
//! One process per pane is not a design flourish — `#[global_allocator]` is a
//! compile-time choice, so an arm on `rusty_alloc` cannot share a process with
//! one on the system allocator. Since the panes must be separate processes
//! anyway, each also gets its own device, its own swapchain and its own
//! streaming pool, which is the cleanest possible isolation between arms.
//!
//! **What the grid is and is not.** Four panes rendering at once contend for
//! cores, for the GPU and for the page cache. The picture is comparable — every
//! pane replays the identical deterministic trace — but the *numbers* on screen
//! are indicative. Reportable figures come from `sim bench`, which runs one arm
//! at a time, pinned, with nothing else in the process. The grid is for seeing
//! the difference; the board is for measuring it.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::provider::{parse_arm, SimError, SimResult};

/// One pane of the grid: a graphics API crossed with an arm.
#[derive(Clone, Debug)]
pub struct PaneSpec {
    pub label: String,
    pub api: String,
    pub arm: String,
}

impl PaneSpec {
    pub fn new(api: &str, arm: &str, label: &str) -> PaneSpec {
        PaneSpec {
            label: label.to_string(),
            api: api.to_string(),
            arm: arm.to_string(),
        }
    }
}

/// The default grid: the conventional PC stack against ours, on both APIs.
///
/// Read down a column for "does swapping the stack matter on this API", and
/// across a row for "does the API change the answer".
pub fn default_grid() -> Vec<PaneSpec> {
    vec![
        PaneSpec::new("d3d11", "dxtex", "D3D11 · without"),
        PaneSpec::new("d3d11", "rusty+ra", "D3D11 · with"),
        PaneSpec::new("vulkan", "dxtex", "Vulkan · without"),
        PaneSpec::new("vulkan", "rusty+ra", "Vulkan · with"),
    ]
}

/// The 2x2 factorial on one API, for isolating each technology on its own.
pub fn isolation_grid(api: &str) -> Vec<PaneSpec> {
    vec![
        PaneSpec::new(api, "dxtex", "neither"),
        PaneSpec::new(api, "rusty", "rusty_dds only"),
        PaneSpec::new(api, "dxtex+ra", "rusty_alloc only"),
        PaneSpec::new(api, "rusty+ra", "both"),
    ]
}

/// Everything the panes share, so the only differences between them are the
/// axes under test.
#[derive(Clone, Debug)]
pub struct GridOptions {
    pub pack: PathBuf,
    pub scenario: String,
    pub frames: Option<u32>,
    pub workers: usize,
    pub peer: String,
    pub pool_mult: f64,
    /// Total area the grid tiles into, in logical pixels.
    pub screen_w: i32,
    pub screen_h: i32,
    pub origin_x: i32,
    pub origin_y: i32,
    /// Ceiling passed to every pane, so none can outlive the demo.
    pub max_run_secs: f64,
}

impl Default for GridOptions {
    fn default() -> Self {
        Self {
            pack: PathBuf::from("pack/high192"),
            scenario: "traverse".to_string(),
            frames: Some(3600),
            workers: 4,
            peer: "loader".to_string(),
            pool_mult: 3.0,
            screen_w: 1920,
            screen_h: 1040,
            origin_x: 0,
            origin_y: 0,
            max_run_secs: 300.0,
        }
    }
}

/// Live state of one pane, parsed from its `TELEM` lines.
#[derive(Clone, Default, Debug)]
pub struct PaneState {
    pub label: String,
    pub api: String,
    pub arm: String,
    pub allocator: String,
    pub running: bool,
    pub exited: bool,
    pub error: Option<String>,
    /// Raw key=value telemetry, last line wins.
    pub fields: BTreeMap<String, String>,
}

impl PaneState {
    pub fn get(&self, key: &str) -> &str {
        self.fields.get(key).map(|s| s.as_str()).unwrap_or("—")
    }

    pub fn num(&self, key: &str) -> f64 {
        self.fields
            .get(key)
            .and_then(|v| v.parse().ok())
            .unwrap_or(f64::NAN)
    }

    /// Which DDS stack this pane is exercising, in words.
    pub fn stack(&self) -> &'static str {
        match parse_arm(&self.arm) {
            Ok(a) => a.stack.name(),
            Err(_) => "?",
        }
    }
}

pub struct PaneGrid {
    children: Vec<Child>,
    state: Arc<Mutex<Vec<PaneState>>>,
    stop: Arc<AtomicBool>,
}

impl PaneGrid {
    pub fn snapshot(&self) -> Vec<PaneState> {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn all_exited(&self) -> bool {
        self.snapshot().iter().all(|p| p.exited)
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for c in &mut self.children {
            let _ = c.kill();
        }
        for c in &mut self.children {
            let _ = c.wait();
        }
    }
}

impl Drop for PaneGrid {
    fn drop(&mut self) {
        // A pane must never outlive the window that launched it.
        self.stop();
    }
}

/// Tile `n` panes into the configured area, 2 across.
fn tile(i: usize, n: usize, o: &GridOptions) -> (i32, i32, u32, u32) {
    let cols = if n <= 1 { 1 } else { 2 };
    let rows = n.div_ceil(cols);
    let w = o.screen_w / cols as i32;
    let h = o.screen_h / rows as i32;
    let col = (i % cols) as i32;
    let row = (i / cols) as i32;
    (
        o.origin_x + col * w,
        o.origin_y + row * h,
        (w - 8).max(320) as u32,
        (h - 8).max(240) as u32,
    )
}

/// Which binary can serve an arm. `+ra` arms need the rusty_alloc build.
fn exe_for(dir: &std::path::Path, arm: &str) -> SimResult<PathBuf> {
    let want_ra = parse_arm(arm)?.wants_rusty_alloc;
    let name = if want_ra { "sim-ra" } else { "sim" };
    for cand in [format!("{name}.exe"), name.to_string()] {
        let p = dir.join(cand);
        if p.exists() {
            return Ok(p);
        }
    }
    Err(SimError(format!(
        "pane `{arm}` needs `{name}` — build it with \
         `cargo build --release --features \"demo\"`"
    )))
}

pub fn launch(specs: &[PaneSpec], opts: &GridOptions) -> SimResult<PaneGrid> {
    let here = std::env::current_exe()?;
    let dir = here
        .parent()
        .ok_or_else(|| SimError("cannot locate the pane binaries".into()))?
        .to_path_buf();

    let state = Arc::new(Mutex::new(
        specs
            .iter()
            .map(|s| PaneState {
                label: s.label.clone(),
                api: s.api.clone(),
                arm: s.arm.clone(),
                running: true,
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    ));
    let stop = Arc::new(AtomicBool::new(false));
    let mut children = Vec::with_capacity(specs.len());

    for (i, spec) in specs.iter().enumerate() {
        let exe = exe_for(&dir, &spec.arm)?;
        let (x, y, w, h) = tile(i, specs.len(), opts);

        let mut cmd = Command::new(&exe);
        cmd.arg("view")
            .args(["--pack", &opts.pack.display().to_string()])
            .args(["--api", &spec.api])
            .args(["--arm", &spec.arm])
            .args(["--scenario", &opts.scenario])
            .args(["--label", &spec.label])
            .args(["--peer", &opts.peer])
            .args(["--workers", &opts.workers.to_string()])
            .args(["--pool-mult", &opts.pool_mult.to_string()])
            .args(["--max-run-secs", &opts.max_run_secs.to_string()])
            .args(["--x", &x.to_string()])
            .args(["--y", &y.to_string()])
            .args(["--width", &w.to_string()])
            .args(["--height", &h.to_string()])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(f) = opts.frames {
            cmd.args(["--frames", &f.to_string()]);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| SimError(format!("spawning pane `{}`: {e}", spec.label)))?;

        // One reader thread per pane. Telemetry is line-oriented key=value, so
        // the aggregator needs no schema and no socket.
        if let Some(out) = child.stdout.take() {
            let state = Arc::clone(&state);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                for line in BufReader::new(out).lines() {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(line) = line else { break };
                    let mut fields = BTreeMap::new();
                    let mut kind = "";
                    for (n, tok) in line.split_whitespace().enumerate() {
                        if n == 0 {
                            kind = match tok {
                                "TELEM" | "TELEM_START" | "TELEM_DONE" => tok,
                                _ => break,
                            };
                            continue;
                        }
                        if let Some((k, v)) = tok.split_once('=') {
                            fields.insert(k.to_string(), v.to_string());
                        }
                    }
                    if kind.is_empty() {
                        continue;
                    }
                    if let Ok(mut st) = state.lock() {
                        if let Some(p) = st.get_mut(i) {
                            if let Some(a) = fields.get("alloc") {
                                p.allocator = a.clone();
                            }
                            if kind == "TELEM_DONE" {
                                p.running = false;
                                p.exited = true;
                            }
                            p.fields.extend(fields);
                        }
                    }
                }
                if let Ok(mut st) = state.lock() {
                    if let Some(p) = st.get_mut(i) {
                        p.running = false;
                        p.exited = true;
                    }
                }
            });
        }

        // Stderr carries the rails' own warnings (slow frames, watchdog stops).
        // Losing those would hide exactly the failures the rails exist to catch.
        if let Some(err) = child.stderr.take() {
            let state = Arc::clone(&state);
            std::thread::spawn(move || {
                for line in BufReader::new(err).lines().map_while(Result::ok) {
                    if let Ok(mut st) = state.lock() {
                        if let Some(p) = st.get_mut(i) {
                            p.error = Some(line);
                        }
                    }
                }
            });
        }

        children.push(child);
    }

    Ok(PaneGrid {
        children,
        state,
        stop,
    })
}
