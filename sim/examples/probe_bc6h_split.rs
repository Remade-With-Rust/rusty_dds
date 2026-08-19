//! Where does BC6H decode time actually go?
//!
//! The decoder is `bcdec_rs::bc6h_float` into a 192-byte scratch, then a scatter
//! that widens RGB to RGBA into the output. Those are the only two things it
//! does. This measures them apart, so any further work targets the half that
//! costs something instead of the half that is convenient to change.

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
    let iters = 40;

    // A: bcdec only, into the scratch. No scatter, no output touched.
    let mut scratch = [0f32; 4 * 4 * 3];
    let t0 = Instant::now();
    for _ in 0..iters {
        for b in 0..blocks {
            bcdec_rs::bc6h_float(&data[b * 16..b * 16 + 16], &mut scratch, 4 * 3, false);
            std::hint::black_box(&scratch);
        }
    }
    let ms_decode = t0.elapsed().as_secs_f64() * 1e3 / iters as f64;

    // B: the scatter only. Same loop, same scratch, no bcdec call.
    let mut out = vec![0f32; n * 4];
    let bx = (side / 4) as usize;
    let t0 = Instant::now();
    for _ in 0..iters {
        for b in 0..blocks {
            let (px0, py0) = ((b % bx) * 4, (b / bx) * 4);
            for row in 0..4 {
                let s = row * 4 * 3;
                let d = ((py0 + row) * side as usize + px0) * 4;
                for i in 0..4 {
                    out[d + i * 4] = scratch[s + i * 3];
                    out[d + i * 4 + 1] = scratch[s + i * 3 + 1];
                    out[d + i * 4 + 2] = scratch[s + i * 3 + 2];
                    out[d + i * 4 + 3] = 1.0;
                }
            }
        }
        std::hint::black_box(&out);
    }
    let ms_scatter = t0.elapsed().as_secs_f64() * 1e3 / iters as f64;

    // C: the real thing, both together.
    let mut whole = Vec::new();
    let t0 = Instant::now();
    for _ in 0..iters {
        view.decode_rgba_f32_into(SubresourceId::mip_layer(0, 0), &mut whole)?;
    }
    let ms_total = t0.elapsed().as_secs_f64() * 1e3 / iters as f64;

    println!("BC6H {side}x{side}, {blocks} blocks");
    println!("  bcdec into scratch   {ms_decode:7.3} ms   {:5.1}%", 100.0 * ms_decode / ms_total);
    println!("  scatter RGB->RGBA    {ms_scatter:7.3} ms   {:5.1}%", 100.0 * ms_scatter / ms_total);
    println!("  measured together    {ms_total:7.3} ms");
    println!("\n  -> replacing bcdec can win at most {:.1}% of the call", 100.0 * ms_decode / ms_total);
    Ok(())
}
