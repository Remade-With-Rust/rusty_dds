//! RDO encode timing, pinned, process CPU.
//!
//! The RDO path is a *second* encode stage layered on the normal one: it refits,
//! polishes and scores candidate blocks against a dictionary of recent ones.
//! Nothing in this campaign has ever measured what that stage costs, so this
//! probe exists to attribute it.
//!
//!   RDO_FMT=bc1|bc7  RDO_LAMBDA=25  cargo run --release \
//!       --example probe_rdo_speed --manifest-path sim/Cargo.toml
//!
//! Reports CPU ms and wall ms for one 512^2 encode, best of N. Compare arms
//! measured in the SAME session only — absolute numbers drift with machine load.
use std::time::Instant;

use rusty_dds::{Dds, DecodeContent, EncodeLayout, Rdo};

fn main() {
    let _ = rusty_dds_sim::os::pin_process(0x3c, true);
    let fmt = std::env::var("RDO_FMT").unwrap_or_else(|_| "bc1".into());
    let lambda: f32 = std::env::var("RDO_LAMBDA")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(25.0);
    let content = match fmt.as_str() {
        "bc7" => DecodeContent::Bc7,
        _ => DecodeContent::Bc1,
    };

    const W: u32 = 512;
    let n = (W * W) as usize;
    let mut px = Vec::with_capacity(n * 4);
    for i in 0..n {
        let x = (i as u32 % W) as f32 / W as f32;
        let y = (i as u32 / W) as f32 / W as f32;
        let v = |a: f32| (a.clamp(0.0, 1.0) * 255.0) as u8;
        // Repetitive-but-not-flat content: RDO's whole job is finding blocks it
        // can reuse, so a source with real structure is the only honest fixture.
        px.extend_from_slice(&[
            v(x + 0.2 * (y * 24.0).sin()),
            v(y + 0.2 * (x * 18.0).cos()),
            v(0.5 + 0.4 * ((x * 12.0).sin() * (y * 12.0).cos())),
            255,
        ]);
    }

    let layout = EncodeLayout::flat_2d(content, W, W)
        .with_mips(1)
        .with_rdo(Rdo::lambda(lambda));
    // Warm: first call pays feature detection and any lazy table build.
    let _ = Dds::encode_from_rgba8(&px, layout.clone()).unwrap();

    let iters = if fmt == "bc7" { 3 } else { 12 };
    let c0 = rusty_dds_sim::os::process_cpu_secs().unwrap_or(0.0);
    let t = Instant::now();
    for _ in 0..iters {
        std::hint::black_box(Dds::encode_from_rgba8(&px, layout.clone()).unwrap());
    }
    let cpu = (rusty_dds_sim::os::process_cpu_secs().unwrap_or(0.0) - c0) * 1e3 / iters as f64;
    let wall = t.elapsed().as_secs_f64() * 1e3 / iters as f64;
    println!("{cpu:.4} {wall:.4}");
}
