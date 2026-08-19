//! Encode timing with the threads taken out of the measurement.
//!
//! **Build both arms with `ENCODE_PARALLEL_MIN_BLOCKS` raised to `usize::MAX`**
//! (in `src/encode/blocks.rs`) so a production-sized surface encodes serially.
//! That matters on a contended box: with strips, total process CPU varies with
//! scheduling and work-stealing overhead even when the work is identical — 14%
//! spread was observed for one binary against itself.
//!
//! It also matters that the surface be production-*shaped*, not merely serial.
//! A 128^2 x7 probe fires the BC7 seed gate on 9.8% of blocks where 512^2 x10
//! fires it on 78.2%, because coarse sampling of the same generator raises
//! per-block error. Measuring the small shape reported a 1.5% REGRESSION for a
//! change that is +8.3% at the real one.
//!
//! Report CPU time, not wall, and compare with a paired win-rate + z-score.

use std::time::Instant;

use rusty_dds::{Dds, DecodeContent, EncodeLayout};

fn main() {
    let pinned = rusty_dds_sim::os::pin_process(0x3c, true);
    eprintln!("[probe] pinned={pinned}");

    const W: u32 = 512; // 1024 blocks — below the parallel threshold
    let n = (W * W) as usize;
    let alpha_struct = std::env::var("PROBE_ALPHA").is_ok();
    let opaque = std::env::var("PROBE_OPAQUE").is_ok();
    eprintln!("[probe] alpha_struct={alpha_struct} opaque={opaque}");
    let mut px = Vec::with_capacity(n * 4);
    for i in 0..n {
        let x = (i as u32 % W) as f32 / W as f32;
        let y = (i as u32 / W) as f32 / W as f32;
        let v = |a: f32| (a.clamp(0.0, 1.0) * 255.0) as u8;
        px.extend_from_slice(&[
            v(x + 0.2 * (y * 24.0).sin()),
            v(y + 0.2 * (x * 18.0).cos()),
            v(0.5 + 0.4 * ((x * 12.0).sin() * (y * 12.0).cos())),
            // PROBE_ALPHA=1 gives alpha real *per-block* structure. Without it
            // alpha is 0.6+0.4xy, which varies by under one code across a
            // 4-pixel span, so `a_hi - a_lo > 2` fails and BC7 mode 4 never
            // runs at all (measured: 0 calls in 16384 blocks) while mode 5 only
            // reaches its rotation path. A fixture that never enters a mode
            // cannot measure a change to it.
            // PROBE_OPAQUE=1: alpha exactly 255 everywhere. This is the most
            // common real texture and the ONLY one that reaches BC7 mode 1,
            // whose gate is `a_lo == 255`. It also closes modes 4 and 5, whose
            // gate is `a_hi - a_lo > 2`, so it exercises a completely different
            // mode mix from the other two fixtures.
            if opaque {
                255
            } else if alpha_struct {
                v(0.5 + 0.5 * ((x * 160.0).sin() * (y * 96.0).cos()))
            } else {
                v(0.6 + 0.4 * x * y)
            },
        ]);
    }

    // Format selectable so the same instrument serves every kernel.
    let content = match std::env::var("PROBE_FMT").unwrap_or_else(|_| "bc7".into()).as_str() {
        "bc1" => DecodeContent::Bc1,
        "bc2" => DecodeContent::Bc2,
        "bc4u" => DecodeContent::Bc4UNorm,
        "bc3" => DecodeContent::Bc3,
        "bc5u" => DecodeContent::Bc5UNorm,
        _ => DecodeContent::Bc7,
    };
    let layout = EncodeLayout::flat_2d(content, W, W).with_mips(10);
    let _ = Dds::encode_from_rgba8(&px, layout).expect("warm");

    // Enough iterations that the 15.625 ms process-CPU quantum is noise.
    const ITERS: usize = 12;
    let c0 = rusty_dds_sim::os::process_cpu_secs();
    let t = Instant::now();
    for _ in 0..ITERS {
        std::hint::black_box(Dds::encode_from_rgba8(&px, layout).expect("encode").data.len());
    }
    let wall = t.elapsed().as_secs_f64() * 1e3 / ITERS as f64;
    let cpu = match (c0, rusty_dds_sim::os::process_cpu_secs()) {
        (Some(a), Some(b)) => (b - a) * 1e3 / ITERS as f64,
        _ => f64::NAN,
    };
    println!("{cpu:.4} {wall:.4}");
}
