//! The replay driver — one `step()` per simulated frame.
//!
//! Both consumers drive this same object: the headless `sim run` harness, which
//! is where reportable numbers come from, and the Dioxus cockpit, which watches
//! a run live. Sharing the driver is not a tidiness preference — if the cockpit
//! had its own loop, "what the demo shows" and "what the board measures" could
//! drift apart without anyone noticing.

use std::sync::Arc;
use std::time::Instant;

use crate::hash::{mix, FNV_OFFSET};
use crate::metrics::{alloc_snapshot, is_hitch, AllocSnapshot, FrameRecord};
use crate::os;
use crate::pack::Pack;
use crate::provider::{provider_for, SimError, SimResult, TextureProvider};
use crate::scenario::{camera, peak_demand_bytes, request_hash, requests, Scenario, World};
use crate::stream::Streamer;

#[derive(Clone)]
pub struct SimConfig {
    pub pack_dir: std::path::PathBuf,
    pub scenario: Scenario,
    pub arm: String,
    pub workers: usize,
    pub frames: Option<u32>,
    pub seed: u64,
    /// Override the tier's pool budget, in MiB. The manifest records the value
    /// actually used, so an overridden run can never be compared against a
    /// default one by accident.
    pub pool_mb: Option<f64>,
    /// Which DirectXTex path the peer arm uses (`loader` or `scratch`). Ignored
    /// by the rusty arm, recorded in the manifest either way.
    pub peer: String,
    /// Multiplier on the derived pool budget.
    ///
    /// The measured configuration is `1.0` and the board never moves off it. The
    /// live panes default higher, because a pool sized for streaming *pressure*
    /// holds only ~20 textures at once and the scene looks nearly empty. The
    /// multiplier is recorded in every manifest, so a demo-tuned run can never be
    /// mistaken for a measured one.
    pub pool_mult: f64,
    /// Payload buffers the streamer recycles. `0` disables reuse, which is how
    /// the reuse win is measured against itself.
    pub pool_buffers: usize,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            pack_dir: std::path::PathBuf::from("pack/medium"),
            scenario: crate::scenario::scenario_by_name("traverse").expect("traverse exists"),
            arm: "a".to_string(),
            workers: 4,
            frames: None,
            seed: 0x5EED_1234,
            pool_mb: None,
            peer: "loader".to_string(),
            pool_mult: 1.0,
            pool_buffers: crate::stream::DEFAULT_POOLED_BUFFERS,
        }
    }
}

pub struct Sim {
    pub pack: Pack,
    pub provider_name: &'static str,
    pub scenario: Scenario,
    pub frames: u32,
    pub pool_budget: u64,
    pub peak_demand: u64,

    world: World,
    streamer: Streamer,
    reqs: Vec<(u32, u32)>,
    alloc_prev: AllocSnapshot,

    frame: u32,
    trace_hash: u64,
    hitches: u32,
    started: Instant,
    cpu0: Option<f64>,
}

impl Sim {
    pub fn new(cfg: &SimConfig) -> SimResult<Sim> {
        let pack = Pack::load(&cfg.pack_dir)?;
        // Before anything is timed — see `Pack::warm`. Every measurement path
        // builds a `Sim`, so warming here covers `run`, `bench`, `view` and the
        // cockpit's panes without each having to remember.
        pack.warm();
        let boxed = provider_for(&cfg.arm, &cfg.peer)?;
        let provider_name = boxed.name();
        let provider: Arc<dyn TextureProvider> = Arc::from(boxed);
        let tier = pack.tier;

        let world = World::new(&pack.mips_per_texture(), cfg.seed);
        let sc = cfg.scenario;
        let frames = cfg.frames.unwrap_or(sc.frames);
        if frames <= sc.warmup {
            return Err(SimError(format!(
                "frames ({frames}) must exceed the scenario warm-up ({})",
                sc.warmup
            )));
        }

        // The pool budget is a property of the workload, derived before any IO
        // happens, so every arm gets the identical budget by construction.
        let contents: Vec<&'static str> = pack.textures.iter().map(|t| t.content).collect();
        let peak_demand = peak_demand_bytes(
            &world,
            &contents,
            pack.size,
            sc.kind,
            sc.dt,
            frames,
            tier.mip_bias(),
        );
        let pool_budget = match cfg.pool_mb {
            Some(mb) => (mb * (1 << 20) as f64) as u64,
            None => (peak_demand as f64 * tier.pool_pressure() * cfg.pool_mult.max(0.01)) as u64,
        };

        let streamer = Streamer::new(
            pack.clone(),
            tier,
            provider,
            cfg.workers,
            pool_budget,
            cfg.pool_buffers,
        );

        Ok(Sim {
            pack,
            provider_name,
            scenario: sc,
            frames,
            pool_budget,
            peak_demand,
            world,
            streamer,
            reqs: Vec::with_capacity(4096),
            alloc_prev: alloc_snapshot(),
            frame: 0,
            trace_hash: FNV_OFFSET,
            hitches: 0,
            started: Instant::now(),
            cpu0: os::process_cpu_secs(),
        })
    }

    pub fn frame(&self) -> u32 {
        self.frame
    }

    pub fn streamer(&self) -> &Streamer {
        &self.streamer
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    pub fn done(&self) -> bool {
        self.frame >= self.frames
    }

    pub fn trace_hash(&self) -> u64 {
        self.trace_hash
    }

    pub fn hitches(&self) -> u32 {
        self.hitches
    }

    pub fn wall_secs(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    pub fn cpu_secs(&self) -> f64 {
        match (self.cpu0, os::process_cpu_secs()) {
            (Some(a), Some(b)) => b - a,
            _ => f64::NAN,
        }
    }

    /// Is this frame still inside the discarded warm-up?
    pub fn is_warmup(&self) -> bool {
        self.frame < self.scenario.warmup
    }

    /// Advance one frame. Returns `None` once the scenario is complete.
    ///
    /// A record is produced for **every** frame including warm-up — what the
    /// harness *does* must not depend on which frames get reported. Callers
    /// filter on [`FrameRecord::frame`] against `scenario.warmup`.
    pub fn step(&mut self) -> SimResult<Option<FrameRecord>> {
        if self.done() {
            return Ok(None);
        }
        let frame = self.frame;
        let sc = self.scenario;

        let t0 = Instant::now();
        let cam = camera(sc.kind, frame, sc.dt);
        requests(&self.world, cam, self.pack.tier.mip_bias(), &mut self.reqs);
        let rh = request_hash(&self.reqs);
        let work = {
            // Split borrow: `step` needs &mut streamer while `reqs` stays shared.
            let reqs = std::mem::take(&mut self.reqs);
            let out = self.streamer.step(frame, &reqs);
            self.reqs = reqs;
            out?
        };
        let cpu_frame_ms = t0.elapsed().as_nanos() as f64 / 1e6;

        self.trace_hash = mix(mix(self.trace_hash, rh), work.upload_hash);
        let hitch = is_hitch(cpu_frame_ms);
        if hitch && frame >= sc.warmup {
            self.hitches += 1;
        }

        let alloc = alloc_snapshot();
        let (d_count, d_bytes) = (
            alloc.count.saturating_sub(self.alloc_prev.count),
            alloc.bytes.saturating_sub(self.alloc_prev.bytes),
        );
        self.alloc_prev = alloc;
        let ws = os::working_set().unwrap_or((0, 0));

        self.frame += 1;
        Ok(Some(FrameRecord {
            frame,
            sim_time: frame as f32 * sc.dt,
            cpu_frame_ms,
            gpu_frame_ms: 0.0,
            present_ms: 0.0,
            stream_cpu_ms: work.read_ms + work.parse_ms + work.upload_ms,
            parse_ms: work.parse_ms,
            decode_ms: 0.0,
            upload_ms: work.upload_ms,
            bytes_read: work.bytes_read,
            bytes_uploaded: work.bytes_uploaded,
            requests: work.requests,
            resident_pct: work.resident_pct,
            alloc_count: d_count,
            alloc_bytes: d_bytes,
            peak_rss_mb: ws.1 as f64 / (1 << 20) as f64,
            vram_used_mb: 0.0,
            vram_budget_mb: 0.0,
            pool_used_mb: self.streamer.resident_bytes() as f64 / (1 << 20) as f64,
            pool_budget_mb: self.pool_budget as f64 / (1 << 20) as f64,
            evicted: work.evicted,
            request_hash: rh,
            upload_hash: work.upload_hash,
            hitch: hitch as u8,
        }))
    }
}
