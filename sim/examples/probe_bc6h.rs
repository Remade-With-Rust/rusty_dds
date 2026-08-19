//! How much is on the table in BC6H decode?
//!
//! BC6H is the only decode path with no parallel seam and no `_into` variant.
//! This sizes that gap at a real texture size before anyone builds either.

use std::time::Instant;

use rusty_dds::{Dds, SubresourceId};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for &side in &[256u32, 512, 1024] {
        // An HDR gradient with enough local variation that the encoder does not
        // collapse every block to one mode.
        let n = (side * side) as usize;
        let mut src = Vec::with_capacity(n * 4);
        for i in 0..n {
            let x = (i as u32 % side) as f32 / side as f32;
            let y = (i as u32 / side) as f32 / side as f32;
            src.extend_from_slice(&[
                x * 8.0 + (y * 32.0).sin(),
                y * 4.0 + (x * 16.0).cos().abs(),
                (x * y * 12.0).fract() * 6.0,
                1.0,
            ]);
        }

        let dds = Dds::encode_bc6h_uf16(&src, side, side)?;

        let id = SubresourceId::mip_layer(0, 0);
        // Warm.
        let _ = dds.decode_rgba_f32(id)?;

        let iters = if side >= 1024 { 10 } else { 40 };
        let t0 = Instant::now();
        for _ in 0..iters {
            std::hint::black_box(dds.decode_rgba_f32(id)?);
        }
        let ms = t0.elapsed().as_secs_f64() * 1e3 / iters as f64;

        // Into a recycled buffer.
        let mut buf = Vec::new();
        dds.decode_rgba_f32_into(id, &mut buf)?;
        let t0 = Instant::now();
        for _ in 0..iters {
            dds.decode_rgba_f32_into(id, &mut buf)?;
            std::hint::black_box(&buf);
        }
        let ms_into = t0.elapsed().as_secs_f64() * 1e3 / iters as f64;

        // Caller-parallel, the way a job system would drive it.
        let rows = dds.block_rows_f32(id)?;
        let threads = std::thread::available_parallelism()?.get().min(rows as usize);
        let mut par = vec![0f32; n * 4];
        let dref = &dds;
        let run = |par: &mut Vec<f32>| -> Result<(), Box<dyn std::error::Error>> {
            let per = rows.div_ceil(threads as u32);
            let mut rest: &mut [f32] = par;
            let mut bands = Vec::new();
            for t in 0..threads as u32 {
                let a = (t * per).min(rows);
                let b = ((t + 1) * per).min(rows);
                let px = ((b - a) * 4).min(side - a * 4) as usize * side as usize * 4;
                let (head, tail) = rest.split_at_mut(px);
                bands.push((a..b, head));
                rest = tail;
            }
            std::thread::scope(|sc| {
                for (r, slot) in bands {
                    sc.spawn(move || dref.decode_block_rows_f32_into(id, r, slot).unwrap());
                }
            });
            Ok(())
        };
        run(&mut par)?;
        let t0 = Instant::now();
        for _ in 0..iters {
            run(&mut par)?;
        }
        let ms_par = t0.elapsed().as_secs_f64() * 1e3 / iters as f64;

        // Parity: the split must produce the same pixels as the whole.
        let whole = dds.decode_rgba_f32(id)?;
        assert_eq!(whole.pixels.len(), par.len(), "{side}: length");
        let bad = whole.pixels.iter().zip(&par).filter(|(a, b)| a != b).count();
        assert_eq!(bad, 0, "{side}: {bad} pixels differ between whole and split");

        let mpx = |t: f64| (n as f64 / 1e6) / (t / 1e3);
        println!(
            "BC6H {side}x{side}  serial {ms:7.3} ms ({:5.1} Mpx/s)   _into {ms_into:7.3} ms                {threads}-thread {ms_par:6.3} ms ({:6.1} Mpx/s)  -> {:.2}x  [parity ok]",
            mpx(ms), mpx(ms_par), ms / ms_par
        );
    }
    Ok(())
}
