//! Live telemetry: run a [`Sim`] on a background thread and publish a snapshot
//! the cockpit can poll.
//!
//! Two rules keep the live view from corrupting what it watches:
//!
//! * the sim thread publishes on a **fixed wall interval** (not per frame), so
//!   lock traffic and snapshot cost do not scale with frame rate;
//! * percentiles come from a [`LogHistogram`], so neither thread ever sorts a
//!   growing sample buffer mid-run.
//!
//! Pacing does not change results — the simulation is fixed-timestep, so
//! `Realtime` and `Turbo` do identical work and produce the same `trace_hash`.
//! Only the wall-clock spacing between frames differs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::metrics::LogHistogram;
use crate::sim::{Sim, SimConfig};

/// How fast the replay advances in wall time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Speed {
    /// One frame per `scenario.dt` — watchable, and what a demo shows.
    Realtime,
    /// As fast as the machine allows.
    Turbo,
}

impl Speed {
    pub fn label(self) -> &'static str {
        match self {
            Speed::Realtime => "realtime",
            Speed::Turbo => "turbo",
        }
    }
}

#[derive(Clone, Default)]
pub struct LiveSnapshot {
    pub running: bool,
    pub done: bool,
    pub error: Option<String>,

    pub arm: String,
    pub provider: String,
    pub scenario: String,
    pub tier: String,
    pub workers: usize,
    pub speed: String,

    pub frame: u32,
    pub frames: u32,
    pub sim_fps: f64,

    pub pack_textures: usize,
    pub pack_mib: f64,
    pub peak_demand_mib: f64,
    pub pool_used_mib: f64,
    pub pool_budget_mib: f64,
    pub resident_pct: f32,
    pub requests: u32,

    pub p50_ms: f64,
    pub p99_ms: f64,
    pub p999_ms: f64,
    pub max_ms: f64,
    pub hitches: u32,
    pub recorded: u64,

    pub read_mib: f64,
    pub uploaded_mib: f64,
    pub upload_mib_s: f64,
    pub parse_ms_total: f64,

    pub alloc_count: u64,
    pub rss_mib: f64,

    pub trace_hash: u64,
    /// Recent per-frame CPU cost, oldest first — the sparkline.
    pub spark: Vec<f32>,
}

const SPARK_LEN: usize = 180;
const PUBLISH_EVERY: Duration = Duration::from_millis(66);

pub struct LiveHandle {
    state: Arc<Mutex<LiveSnapshot>>,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl LiveHandle {
    pub fn snapshot(&self) -> LiveSnapshot {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn is_finished(&self) -> bool {
        self.join.as_ref().map(|j| j.is_finished()).unwrap_or(true)
    }
}

impl Drop for LiveHandle {
    fn drop(&mut self) {
        self.stop();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Spawn a run. The returned handle stops the run when dropped.
pub fn start(cfg: SimConfig, speed: Speed) -> LiveHandle {
    let state = Arc::new(Mutex::new(LiveSnapshot {
        running: true,
        arm: cfg.arm.clone(),
        scenario: cfg.scenario.name.to_string(),
        workers: cfg.workers,
        speed: speed.label().to_string(),
        spark: Vec::new(),
        ..Default::default()
    }));
    let stop = Arc::new(AtomicBool::new(false));

    let join = std::thread::spawn({
        let state = Arc::clone(&state);
        let stop = Arc::clone(&stop);
        move || drive(cfg, speed, state, stop)
    });

    LiveHandle {
        state,
        stop,
        join: Some(join),
    }
}

fn drive(cfg: SimConfig, speed: Speed, state: Arc<Mutex<LiveSnapshot>>, stop: Arc<AtomicBool>) {
    let mut sim = match Sim::new(&cfg) {
        Ok(s) => s,
        Err(e) => {
            if let Ok(mut st) = state.lock() {
                st.running = false;
                st.done = true;
                st.error = Some(e.0);
            }
            return;
        }
    };

    // Static facts, published once.
    if let Ok(mut st) = state.lock() {
        st.provider = sim.provider_name.to_string();
        st.tier = sim.pack.tier.name().to_string();
        st.frames = sim.frames;
        st.pack_textures = sim.pack.textures.len();
        st.pack_mib = sim.pack.total_bytes as f64 / MIB;
        st.peak_demand_mib = sim.peak_demand as f64 / MIB;
        st.pool_budget_mib = sim.pool_budget as f64 / MIB;
    }

    let warmup = sim.scenario.warmup;
    let dt = Duration::from_secs_f32(sim.scenario.dt);
    let mut hist = LogHistogram::new();
    let mut spark: Vec<f32> = Vec::with_capacity(SPARK_LEN);

    let mut read_bytes = 0u64;
    let mut uploaded_bytes = 0u64;
    let mut parse_ms_total = 0.0f64;
    let mut alloc_count = 0u64;
    let mut last_pub = Instant::now();
    let mut last_pub_frame = 0u32;
    let mut last_pub_uploaded = 0u64;
    let started = Instant::now();
    let mut err: Option<String> = None;

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let rec = match sim.step() {
            Ok(Some(r)) => r,
            Ok(None) => break,
            Err(e) => {
                err = Some(e.0);
                break;
            }
        };

        read_bytes += rec.bytes_read;
        uploaded_bytes += rec.bytes_uploaded;
        parse_ms_total += rec.parse_ms;
        alloc_count += rec.alloc_count;
        if rec.frame >= warmup {
            hist.record(rec.cpu_frame_ms);
        }
        if spark.len() == SPARK_LEN {
            spark.remove(0);
        }
        spark.push(rec.cpu_frame_ms as f32);

        let now = Instant::now();
        if now.duration_since(last_pub) >= PUBLISH_EVERY {
            let elapsed = now.duration_since(last_pub).as_secs_f64();
            if let Ok(mut st) = state.lock() {
                st.frame = rec.frame;
                st.sim_fps = (rec.frame - last_pub_frame) as f64 / elapsed;
                st.pool_used_mib = rec.pool_used_mb;
                st.resident_pct = rec.resident_pct;
                st.requests = rec.requests;
                st.p50_ms = hist.percentile(50.0);
                st.p99_ms = hist.percentile(99.0);
                st.p999_ms = hist.percentile(99.9);
                st.max_ms = hist.max();
                st.recorded = hist.len();
                st.hitches = sim.hitches();
                st.read_mib = read_bytes as f64 / MIB;
                st.uploaded_mib = uploaded_bytes as f64 / MIB;
                st.upload_mib_s =
                    (uploaded_bytes - last_pub_uploaded) as f64 / MIB / elapsed.max(1e-6);
                st.parse_ms_total = parse_ms_total;
                st.alloc_count = alloc_count;
                st.rss_mib = rec.peak_rss_mb;
                st.trace_hash = sim.trace_hash();
                st.spark.clear();
                st.spark.extend_from_slice(&spark);
            }
            last_pub = now;
            last_pub_frame = rec.frame;
            last_pub_uploaded = uploaded_bytes;
        }

        if speed == Speed::Realtime {
            // Sleep to the frame's wall deadline. Fixed-timestep simulation means
            // this changes only pacing, never work — Realtime and Turbo produce
            // the same trace_hash.
            let deadline = started + dt.mul_f64(sim.frame() as f64);
            if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
        }
    }

    if let Ok(mut st) = state.lock() {
        st.running = false;
        st.done = true;
        st.error = err;
        st.frame = sim.frame();
        st.p50_ms = hist.percentile(50.0);
        st.p99_ms = hist.percentile(99.0);
        st.p999_ms = hist.percentile(99.9);
        st.max_ms = hist.max();
        st.recorded = hist.len();
        st.hitches = sim.hitches();
        st.read_mib = read_bytes as f64 / MIB;
        st.uploaded_mib = uploaded_bytes as f64 / MIB;
        st.parse_ms_total = parse_ms_total;
        st.alloc_count = alloc_count;
        st.trace_hash = sim.trace_hash();
        st.spark.clear();
        st.spark.extend_from_slice(&spark);
    }
}

const MIB: f64 = (1 << 20) as f64;
