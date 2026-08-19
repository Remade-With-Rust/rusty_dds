//! Where does BC7 encode actually start to profit from threads?
//!
//! `ENCODE_PARALLEL_MIN_BLOCKS` picks the crossover between the serial and
//! parallel encoders. Below it every surface encodes on one thread. The constant
//! has never been validated, so this sweeps surface sizes around it.
//!
//! **Wall time, not CPU.** Threading trades total CPU for latency, so process
//! CPU is the wrong metric here — it goes *up* when threading helps. Build one
//! arm with the threshold forced to `0` (always parallel) and one with
//! `usize::MAX` (always serial), and compare wall at each size.
//!
//!   cargo run --release --example probe_parallel --manifest-path sim/Cargo.toml
use std::time::Instant;

use rusty_dds::{Dds, DecodeContent, EncodeLayout};

fn main() {
    let _ = rusty_dds_sim::os::pin_process(0x3c, true);
    // 64^2 = 256 blocks .. 512^2 = 16384 blocks, bracketing the 4096 default.
    // One size per process so a paired runner can alternate arms on it.
    let sides: Vec<u32> = match std::env::var("PAR_SIDE") {
        Ok(v) => vec![v.parse().unwrap()],
        Err(_) => vec![48, 64, 80, 91, 104, 116, 128, 181, 256, 362, 512],
    };
    for side in sides {
        let blocks = ((side as usize + 3) / 4).pow(2);
        let n = (side * side) as usize;
        let mut px = Vec::with_capacity(n * 4);
        for i in 0..n {
            let x = (i as u32 % side) as f32 / side as f32;
            let y = (i as u32 / side) as f32 / side as f32;
            let v = |a: f32| (a.clamp(0.0, 1.0) * 255.0) as u8;
            px.extend_from_slice(&[
                v(x + 0.2 * (y * 24.0).sin()),
                v(y + 0.2 * (x * 18.0).cos()),
                v(0.5 + 0.4 * ((x * 12.0).sin() * (y * 12.0).cos())),
                v(0.5 + 0.5 * ((x * 160.0).sin() * (y * 96.0).cos())),
            ]);
        }
        let layout = EncodeLayout::flat_2d(DecodeContent::Bc7, side, side).with_mips(1);
        // Warm: first call pays feature detection and any lazy pool spin-up.
        let _ = Dds::encode_from_rgba8(&px, layout.clone()).unwrap();

        // Report the MINIMUM of several runs: threading's downside is variance,
        // and a mean would blend the two arms' different distributions.
        let mut best = f64::MAX;
        for _ in 0..9 {
            let t = Instant::now();
            let d = Dds::encode_from_rgba8(&px, layout.clone()).unwrap();
            let ms = t.elapsed().as_secs_f64() * 1e3;
            std::hint::black_box(&d);
            best = best.min(ms);
        }
        if std::env::var("PAR_SIDE").is_ok() {
            println!("{best:.4}");
        } else {
            println!("{side}^2 {blocks:>6} blocks  {best:8.3} ms");
        }
    }
}
