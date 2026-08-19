//! Is BC6H decode cost content-dependent?
//!
//! The old half->float converter branched on the zero/denormal case. Content
//! full of zeros — a night sky, an unlit lightmap region, a masked probe —
//! takes that branch on most pixels; a bright sky takes it on none. A decoder
//! whose speed depends on how dark the texture is has a cliff, and a cliff is
//! worse than a constant cost even when the average is the same.

use std::time::Instant;

use rusty_dds::{Dds, DdsView, SubresourceId};

fn build(side: u32, f: &dyn Fn(f32, f32) -> [f32; 3]) -> Vec<u8> {
    let n = (side * side) as usize;
    let mut src = Vec::with_capacity(n * 4);
    for i in 0..n {
        let x = (i as u32 % side) as f32 / side as f32;
        let y = (i as u32 / side) as f32 / side as f32;
        let c = f(x, y);
        src.extend_from_slice(&[c[0], c[1], c[2], 1.0]);
    }
    let dds = Dds::encode_bc6h_uf16(&src, side, side).unwrap();
    let mut bytes = Vec::new();
    dds.write(&mut bytes).unwrap();
    bytes
}

fn time(bytes: &[u8]) -> f64 {
    let view = DdsView::parse(bytes).unwrap();
    let id = SubresourceId::mip_layer(0, 0);
    let mut out = Vec::new();
    view.decode_rgba_f32_into(id, &mut out).unwrap();
    let iters = 40;
    let t0 = Instant::now();
    for _ in 0..iters {
        view.decode_rgba_f32_into(id, &mut out).unwrap();
    }
    t0.elapsed().as_secs_f64() * 1e3 / iters as f64
}

fn main() {
    let side = 512u32;
    let zero = |_x: f32, _y: f32| [0.0, 0.0, 0.0];
    let night = |x: f32, y: f32| {
        let v = ((x * 24.0).sin() * (y * 24.0).cos()).max(0.0);
        [v * 0.01, v * 0.01, v * 0.02]
    };
    let sky = |x: f32, y: f32| [1.4 + x, 0.9 + y, 2.0 + x * y];
    let wide = |x: f32, y: f32| [0.5 + x * 900.0, 0.4 + y * 700.0, 0.8 + x * y * 1200.0];
    let cases: [(&str, &dyn Fn(f32, f32) -> [f32; 3]); 4] = [
        ("all zero (unlit)", &zero),
        ("mostly zero (night)", &night),
        ("bright sky (no zeros)", &sky),
        ("full HDR range", &wide),
    ];

    println!("BC6H {side}x{side}, decode_rgba_f32_into\n");
    let mut times = Vec::new();
    for (name, f) in cases {
        let bytes = build(side, f);
        let ms = time(&bytes);
        println!("  {name:<24} {ms:7.3} ms");
        times.push(ms);
    }
    let lo = times.iter().cloned().fold(f64::MAX, f64::min);
    let hi = times.iter().cloned().fold(0f64, f64::max);
    println!("\n  spread across content: {:.2}x  ({lo:.3} .. {hi:.3} ms)", hi / lo);
}
