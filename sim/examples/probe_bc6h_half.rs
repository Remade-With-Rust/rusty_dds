//! How much of BC6H decode is the half -> float conversion?
//!
//! `bcdec_rs::bc6h_float` is `bc6h_half` into a `[u16; 48]` scratch, then 48
//! calls to `half_to_float_quick` — two branches each. If that tail is a real
//! share of the call, it can be replaced with a branchless (and vectorisable)
//! conversion that writes RGBA directly, deleting our scatter at the same time.

use std::time::Instant;

use rusty_dds::{Dds, DdsView, SubresourceId};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let side = 512u32;
    let n = (side * side) as usize;
    let mut src = Vec::with_capacity(n * 4);
    for i in 0..n {
        let x = (i as u32 % side) as f32 / side as f32;
        let y = (i as u32 / side) as f32 / side as f32;
        src.extend_from_slice(&[x * 8.0 + (y * 32.0).sin(), y * 4.0, (x * y * 12.0).fract() * 6.0, 1.0]);
    }
    let dds = Dds::encode_bc6h_uf16(&src, side, side)?;
    let mut bytes = Vec::new();
    dds.write(&mut bytes)?;
    let view = DdsView::parse(&bytes)?;
    let surf = view.surface(SubresourceId::mip_layer(0, 0))?;
    let data = surf.data;
    let blocks = ((side / 4) * (side / 4)) as usize;
    let iters = 60;

    // A: bc6h_half only — the block decode, no float conversion at all.
    let mut halves = [0u16; 4 * 4 * 3];
    let t0 = Instant::now();
    for _ in 0..iters {
        for b in 0..blocks {
            bcdec_rs::bc6h_half(&data[b * 16..b * 16 + 16], &mut halves, 4 * 3, false);
            std::hint::black_box(&halves);
        }
    }
    let ms_half = t0.elapsed().as_secs_f64() * 1e3 / iters as f64;

    // B: bc6h_float — the same, plus 48 half->float conversions per block.
    let mut floats = [0f32; 4 * 4 * 3];
    let t0 = Instant::now();
    for _ in 0..iters {
        for b in 0..blocks {
            bcdec_rs::bc6h_float(&data[b * 16..b * 16 + 16], &mut floats, 4 * 3, false);
            std::hint::black_box(&floats);
        }
    }
    let ms_float = t0.elapsed().as_secs_f64() * 1e3 / iters as f64;

    println!("BC6H {side}x{side}, {blocks} blocks");
    println!("  bc6h_half  (block decode only)  {ms_half:7.3} ms");
    println!("  bc6h_float (+ 48 half->float)   {ms_float:7.3} ms");
    let conv = ms_float - ms_half;
    println!("  -> conversion tail              {conv:7.3} ms   {:5.1}% of bc6h_float", 100.0 * conv / ms_float);
    Ok(())
}
