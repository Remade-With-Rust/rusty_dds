//! BC6H encode timing, CPU, pinned. Single-threaded by construction.
use std::time::Instant;
use rusty_dds::Dds;
fn main() {
    let _ = rusty_dds_sim::os::pin_process(0x3c, true);
    const W: u32 = 256;
    let n = (W * W) as usize;
    let mut src = Vec::with_capacity(n * 4);
    for i in 0..n {
        let x = (i as u32 % W) as f32 / W as f32;
        let y = (i as u32 / W) as f32 / W as f32;
        src.extend_from_slice(&[
            x * 8.0 + (y * 32.0).sin().abs(),
            y * 4.0 + (x * 16.0).cos().abs(),
            (x * y * 12.0).fract() * 6.0,
            1.0,
        ]);
    }
    let _ = Dds::encode_bc6h_uf16(&src, W, W).expect("warm");
    const ITERS: usize = 40;
    let c0 = rusty_dds_sim::os::process_cpu_secs();
    let t = Instant::now();
    for _ in 0..ITERS {
        std::hint::black_box(Dds::encode_bc6h_uf16(&src, W, W).expect("enc").data.len());
    }
    let wall = t.elapsed().as_secs_f64() * 1e3 / ITERS as f64;
    let cpu = match (c0, rusty_dds_sim::os::process_cpu_secs()) {
        (Some(a), Some(b)) => (b - a) * 1e3 / ITERS as f64,
        _ => f64::NAN,
    };
    println!("{cpu:.4} {wall:.4}");
}
