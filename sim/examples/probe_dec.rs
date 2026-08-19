//! Per-format decode timing: CPU, pinned. Every decoder here is serial (only
//! BC7 has a parallel path), so no forced-serial build is needed.
//!
//!   DEC_FMT=bc1|bc2|bc3|bc4|bc5|bc6h  cargo run --release --example probe_dec
use std::time::Instant;
use rusty_dds::{Dds, DdsView, DecodeContent, EncodeLayout, SubresourceId};

fn main() {
    let _ = rusty_dds_sim::os::pin_process(0x3c, true);
    let fmt = std::env::var("DEC_FMT").unwrap_or_else(|_| "bc1".into());
    const W: u32 = 512;
    let n = (W * W) as usize;

    let bytes = if fmt == "bc6h" {
        let mut src = Vec::with_capacity(n * 4);
        for i in 0..n {
            let x = (i as u32 % W) as f32 / W as f32;
            let y = (i as u32 / W) as f32 / W as f32;
            src.extend_from_slice(&[x * 8.0 + (y * 32.0).sin().abs(),
                y * 4.0 + (x * 16.0).cos().abs(), (x * y * 12.0).fract() * 6.0, 1.0]);
        }
        let d = Dds::encode_bc6h_uf16(&src, W, W).unwrap();
        let mut b = Vec::new(); d.write(&mut b).unwrap(); b
    } else {
        let content = match fmt.as_str() {
            "bc2" => DecodeContent::Bc2, "bc3" => DecodeContent::Bc3,
            "bc4" => DecodeContent::Bc4UNorm, "bc5" => DecodeContent::Bc5UNorm,
            _ => DecodeContent::Bc1,
        };
        let mut px = Vec::with_capacity(n * 4);
        for i in 0..n {
            let x = (i as u32 % W) as f32 / W as f32;
            let y = (i as u32 / W) as f32 / W as f32;
            let v = |a: f32| (a.clamp(0.0, 1.0) * 255.0) as u8;
            px.extend_from_slice(&[v(x + 0.2 * (y * 24.0).sin()), v(y + 0.2 * (x * 18.0).cos()),
                v(0.5 + 0.4 * ((x * 12.0).sin() * (y * 12.0).cos())), v(0.6 + 0.4 * x * y)]);
        }
        let d = Dds::encode_from_rgba8(&px, EncodeLayout::flat_2d(content, W, W).with_mips(1)).unwrap();
        let mut b = Vec::new(); d.write(&mut b).unwrap(); b
    };

    let view = DdsView::parse(&bytes).unwrap();
    let id = SubresourceId::mip_layer(0, 0);
    // Windows process-CPU granularity is 15.625 ms; enough iterations that one
    // tick is a small fraction of the total, or the quantum is the measurement.
    let iters: usize = if fmt == "bc6h" { 400 } else { 4000 };
    if fmt == "bc6h" {
        // `decode_rgba_f32` allocates and zeroes a fresh 4 MiB Vec per call; the
        // LDR arms reuse a buffer. Measuring the allocator against a decoder is
        // not a comparison, so this reuses one too.
        let mut fbuf = Vec::new();
        view.decode_rgba_f32_into(id, &mut fbuf).unwrap();
        let c0 = rusty_dds_sim::os::process_cpu_secs();
        let t = Instant::now();
        for _ in 0..iters { view.decode_rgba_f32_into(id, &mut fbuf).unwrap(); std::hint::black_box(&fbuf); }
        report(c0, t, iters);
    } else {
        let mut buf = Vec::new();
        view.decode_rgba8_into(id, &mut buf).unwrap();
        let c0 = rusty_dds_sim::os::process_cpu_secs();
        let t = Instant::now();
        for _ in 0..iters { view.decode_rgba8_into(id, &mut buf).unwrap(); std::hint::black_box(&buf); }
        report(c0, t, iters);
    }
}

fn report(c0: Option<f64>, t: Instant, iters: usize) {
    let wall = t.elapsed().as_secs_f64() * 1e3 / iters as f64;
    let cpu = match (c0, rusty_dds_sim::os::process_cpu_secs()) {
        (Some(a), Some(b)) => (b - a) * 1e3 / iters as f64, _ => f64::NAN };
    println!("{cpu:.4} {wall:.4}");
}
