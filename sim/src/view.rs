//! `sim view` — one live pane.
//!
//! Drives the same deterministic [`Sim`] the board measures, but presents each
//! frame through a real swapchain. One process per pane, because the allocator
//! is a compile-time choice and two allocators cannot coexist in one process.
//!
//! Telemetry goes to stdout as `TELEM key=value ...` lines so the cockpit can
//! aggregate several panes without a socket. Panes contend for CPU and GPU when
//! run side by side, which is exactly what makes the picture comparable and the
//! *numbers* indicative — the board still comes from detached, one-at-a-time
//! `sim bench` runs.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::gpu::scene::{self, Quad};
use crate::gpu::{Api, GpuLimits, GpuTimings, Viewport, ViewportConfig};
use crate::metrics::{is_hitch, LogHistogram};
use crate::provider::{SimError, SimResult, SubId};
use crate::sim::{Sim, SimConfig};

pub struct ViewOptions {
    pub cfg: SimConfig,
    pub api: Api,
    pub viewport: ViewportConfig,
    /// Advance one frame per `scenario.dt` of wall time. Off = as fast as the
    /// machine allows. Fixed timestep either way, so the work is identical.
    pub realtime: bool,
    /// Label printed on telemetry lines and in the window title.
    pub label: String,
    /// Ceilings that stop a pane turning into a driver stress test.
    pub limits: GpuLimits,
}

fn make_viewport(api: Api, cfg: &ViewportConfig) -> SimResult<Box<dyn Viewport>> {
    match api {
        #[cfg(feature = "d3d11")]
        Api::D3D11 => Ok(Box::new(crate::gpu::d3d11::D3d11Viewport::new(cfg)?)),
        #[cfg(not(feature = "d3d11"))]
        Api::D3D11 => Err(SimError("built without the `d3d11` feature".into())),
        #[cfg(feature = "vulkan")]
        Api::Vulkan => Ok(Box::new(crate::gpu::vulkan::VulkanViewport::new(cfg)?)),
        #[cfg(not(feature = "vulkan"))]
        Api::Vulkan => Err(SimError("built without the `vulkan` feature".into())),
    }
}

pub fn view(opts: &ViewOptions) -> SimResult<()> {
    let mut sim = Sim::new(&opts.cfg)?;
    let mut vp = make_viewport(opts.api, &opts.viewport)?;

    let aspect = opts.viewport.width as f32 / opts.viewport.height.max(1) as f32;
    let dt = Duration::from_secs_f32(sim.scenario.dt);
    let warmup = sim.scenario.warmup;

    let mut hist = LogHistogram::new();
    // GPU frame time gets the same treatment as CPU frame time: accumulate over
    // the whole run. A single frame's `gpu_ms` is a sample of a noisy quantity
    // (vsync phase, driver scheduling, whichever pane won the GPU that instant),
    // and a table refreshed four times a second showing the newest sample is a
    // flicker, not a measurement.
    let mut gpu_hist = LogHistogram::new();
    let mut quads: Vec<Quad> = Vec::with_capacity(512);
    let mut resident: Vec<u32> = Vec::with_capacity(512);
    let mut hitches = 0u32;
    let mut uploaded_bytes = 0u64;
    let mut gpu_ms_last = f64::NAN;
    let mut timings = GpuTimings::default();

    let limits = opts.limits;
    // Uploads the GPU has not caught up with yet. Bounded per frame so one
    // frame can never push hundreds of megabytes at the driver.
    let mut pending: VecDeque<(u32, u32)> = VecDeque::with_capacity(256);
    let mut slow_streak = 0u32;
    let mut trimmed_total = 0usize;
    let mut deferred_peak = 0usize;
    let (mut described, mut described_sub) = (false, false);

    // Say the pacing mode out loud. The first version of this loop ran
    // unthrottled while believing it was paced, and nothing in the output said so.
    println!(
        "TELEM_START label={} api={} arm={} pacing={} vsync=on max_uploads/frame={}          max_upload_mib/frame={} gpu_cache_mib={} frame_abort_ms={} max_run_s={}",
        opts.label,
        opts.api.name(),
        opts.cfg.arm,
        if opts.realtime { "realtime" } else { "free-run" },
        limits.max_uploads_per_frame,
        limits.max_upload_bytes_per_frame / (1 << 20),
        limits.max_gpu_texture_bytes / (1 << 20),
        limits.frame_abort_ms,
        limits.max_run_secs,
    );

    let started = Instant::now();
    let mut last_report = Instant::now();
    let report_every = Duration::from_millis(250);

    while !sim.done() {
        if !vp.pump() {
            break;
        }

        // Rail: wall-clock watchdog. A pane that outlives its budget stops,
        // whatever the frame counter says.
        if started.elapsed().as_secs_f64() > limits.max_run_secs {
            eprintln!(
                "[{}] stopping: exceeded max_run_secs ({:.0}s)",
                opts.label, limits.max_run_secs
            );
            break;
        }

        let frame_start = Instant::now();

        // Open the frame BEFORE touching any GPU resource. Everything below —
        // texture creation, uploads, min-LOD view changes, trimming — mutates
        // objects the previous frame's command buffer may still reference, and
        // this is what guarantees it has finished with them.
        vp.begin_frame()?;

        let Some(rec) = sim.step()? else { break };

        // Queue what became resident; drain it under the per-frame ceiling.
        for &(tex, mip) in sim.streamer().newly_resident() {
            pending.push_back((tex, mip));
        }
        for &tex in sim.streamer().closed_textures() {
            vp.release_texture(tex);
        }

        let mut uploads = 0usize;
        let mut bytes_this_frame = 0u64;
        while uploads < limits.max_uploads_per_frame
            && bytes_this_frame < limits.max_upload_bytes_per_frame
        {
            let Some((tex, mip)) = pending.pop_front() else {
                break;
            };
            // It may have been evicted again while queued; that is not an error,
            // it just means the upload is no longer wanted.
            let Some(open) = sim.streamer().open_texture(tex) else {
                continue;
            };
            if sim.streamer().min_resident_mip(tex).map_or(true, |m| mip < m) {
                continue;
            }
            // One-shot description of the first texture the pane uploads. A
            // format or pitch mismatch between the two providers shows up here
            // as a difference in one line, rather than as a GPU fault later.
            if !described {
                let d = open.desc();
                eprintln!(
                    "[{}] first texture: {}x{} mips={} fmt={} ({}) block={}B compressed={}",
                    opts.label, d.width, d.height, d.mips, d.dxgi_name, d.vulkan_name,
                    d.block_bytes, d.compressed
                );
                described = true;
            }
            vp.ensure_texture(tex, open.desc())?;
            let sub = open.subresource(SubId::mip(mip))?;
            if !described_sub {
                eprintln!(
                    "[{}] first subresource: mip={} {}x{} len={} row_pitch={} rows={}",
                    opts.label, mip, sub.width, sub.height, sub.bytes.len(),
                    sub.bytes_per_row, sub.rows_per_image
                );
                described_sub = true;
            }
            bytes_this_frame += sub.bytes.len() as u64;
            uploaded_bytes += sub.bytes.len() as u64;
            vp.upload(tex, SubId::mip(mip), &sub)?;
            uploads += 1;
        }
        deferred_peak = deferred_peak.max(pending.len());

        sim.streamer().resident_textures(&mut resident);
        for &tex in &resident {
            if let Some(m) = sim.streamer().min_resident_mip(tex) {
                vp.set_min_lod(tex, m);
            }
        }

        let view = scene::view_for(sim.scenario.kind, rec.frame, sim.scenario.dt, aspect);
        scene::visible_quads(sim.world(), &view, &resident, &mut quads);
        timings = vp.frame(&view, &quads)?;
        gpu_ms_last = timings.gpu_ms;

        // Rail: trim the GPU cache, at most a couple of resources per frame.
        trimmed_total += vp.trim(&limits);

        // Two different numbers, deliberately.
        //
        // `wall_ms` is the whole frame including the vsync wait inside Present,
        // and it is what the circuit breaker judges: a system in trouble is
        // measured door to door.
        //
        // `cpu_ms` subtracts that wait, because it is what the *CPU streaming
        // path* cost. Leaving the vblank wait in would make every frame read
        // ~16.7 ms and trip the 1 ms hitch rule, reporting 100% hitches on a
        // perfectly healthy pane — a number that looks alarming and means
        // nothing.
        let wall_ms = frame_start.elapsed().as_secs_f64() * 1e3;
        let cpu_ms = (wall_ms - timings.present_ms).max(0.0);
        if rec.frame >= warmup {
            hist.record(cpu_ms);
            if is_hitch(cpu_ms) {
                hitches += 1;
            }
            // NaN means the timestamp pair was disjoint, not that the GPU was
            // idle. Recording it would bucket a lie at the bottom of the range.
            if timings.gpu_ms.is_finite() {
                gpu_hist.record(timings.gpu_ms);
            }
        }

        // Rail: circuit breaker. A frame this slow means the driver is in
        // trouble; backing off and then giving up beats continuing to hammer it.
        if wall_ms > limits.frame_abort_ms {
            slow_streak += 1;
            eprintln!(
                "[{}] slow frame {:.0} ms wall ({:.0} ms cpu) ({}/{}) — backing off",
                opts.label, wall_ms, cpu_ms, slow_streak, limits.abort_after_slow_frames
            );
            std::thread::sleep(Duration::from_millis(50));
            if slow_streak >= limits.abort_after_slow_frames {
                report(opts, &sim, &hist, &gpu_hist, hitches, uploaded_bytes, &timings, gpu_ms_last);
                return Err(SimError(format!(
                    "{}: aborted after {} consecutive frames over {:.0} ms — the GPU path is                      not keeping up, and continuing would only punish the driver",
                    opts.label, slow_streak, limits.frame_abort_ms
                )));
            }
        } else {
            slow_streak = 0;
        }

        if last_report.elapsed() >= report_every {
            report(opts, &sim, &hist, &gpu_hist, hitches, uploaded_bytes, &timings, gpu_ms_last);
            vp.set_caption(&caption(
                &opts.label,
                opts.api,
                &opts.cfg.arm,
                crate::metrics::allocator_name(),
                hist.percentile(99.0),
                gpu_hist.percentile(50.0),
            ));
            last_report = Instant::now();
        }

        if opts.realtime {
            // Deadline pacing for long-run accuracy, plus a per-frame floor so a
            // mistake in the deadline arithmetic cannot free-run the loop.
            let deadline = started + dt.mul_f32(sim.frame() as f32);
            if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait.min(dt * 4));
            }
            // Present already blocked for a vblank; only top up the remainder.
            let spent = frame_start.elapsed();
            if spent < dt {
                std::thread::sleep(dt - spent);
            }
        }
    }

    eprintln!(
        "[{}] finished: {} frames, {} GPU textures trimmed, deferred-upload peak {}",
        opts.label,
        sim.frame(),
        trimmed_total,
        deferred_peak
    );
    report(opts, &sim, &hist, &gpu_hist, hitches, uploaded_bytes, &timings, gpu_ms_last);
    println!("TELEM_DONE label={}", opts.label);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn report(
    opts: &ViewOptions,
    sim: &Sim,
    hist: &LogHistogram,
    gpu_hist: &LogHistogram,
    hitches: u32,
    uploaded: u64,
    t: &GpuTimings,
    // The newest frame's GPU time, kept as a raw sample beside the run-wide
    // gpu_p50/gpu_p99 — the tables read the percentiles.
    gpu_ms: f64,
) {
    const MIB: f64 = (1 << 20) as f64;
    let alloc = crate::metrics::alloc_snapshot();
    let ws = crate::os::working_set().unwrap_or((0, 0));

    // One line, key=value, so the cockpit can parse several panes without a
    // socket or a schema.
    println!(
        "TELEM label={} api={} arm={} alloc={} frame={} frames={} p50={:.4} p99={:.4} p999={:.4} \
         max={:.4} hitches={} gpu_p50={:.4} gpu_p99={:.4} gpu_ms={:.4} present_ms={:.4} \
         vram_mb={:.1} vram_budget_mb={:.1} \
         uploaded_mib={:.2} pool_mib={:.2} pool_budget_mib={:.2} rss_mib={:.1} allocs={} trace={:016x}",
        opts.label,
        opts.api.name(),
        opts.cfg.arm,
        crate::metrics::allocator_name(),
        sim.frame(),
        sim.frames,
        hist.percentile(50.0),
        hist.percentile(99.0),
        hist.percentile(99.9),
        hist.max(),
        hitches,
        gpu_hist.percentile(50.0),
        gpu_hist.percentile(99.0),
        gpu_ms,
        t.present_ms,
        t.vram_used_mb,
        t.vram_budget_mb,
        uploaded as f64 / MIB,
        sim.streamer().resident_bytes() as f64 / MIB,
        sim.pool_budget as f64 / MIB,
        ws.1 as f64 / MIB,
        alloc.count,
        sim.trace_hash(),
    );
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
}

/// Window caption: the pane must be self-describing in a screen recording.
///
/// Both figures are run-wide — cpu p99 and gpu p50 over every
/// frame since warmup — so a glance at a pane reads the same as the cockpit row
/// next to it, and neither jumps around with the newest frame.
pub fn caption(label: &str, api: Api, arm: &str, allocator: &str, p99: f64, gpu_ms: f64) -> String {
    let dds = if arm.starts_with("rusty") || arm == "a" || arm == "a2" {
        "rusty_dds"
    } else {
        "DirectXTex"
    };
    let gpu = if gpu_ms.is_nan() {
        "gpu n/a".to_string()
    } else {
        format!("gpu {gpu_ms:.2} ms")
    };
    format!("{label} — {} · {dds} + {allocator} · cpu p99 {p99:.2} ms · {gpu}", api.name())
}
