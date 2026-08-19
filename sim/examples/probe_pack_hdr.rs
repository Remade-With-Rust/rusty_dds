//! Decode every HDR texture in a cooked pack, serial against caller-parallel.
//!
//! `probe_bc6h` measures synthetic surfaces. This one runs the real cooked
//! content the streamer serves, across the whole mip chain, and asserts the
//! split is bit-identical to the whole at every level. The 9.6x BC6H win was
//! found on synthetic data; this is what proves it on the pack.
//!
//!   cargo run --release --example probe_pack_hdr -- <pack-dir>

use std::time::Instant;

use rusty_dds::{DdsView, SubresourceId};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args().nth(1).ok_or("usage: probe_pack_hdr <pack-dir>")?;
    let manifest = std::fs::read_to_string(std::path::Path::new(&dir).join("pack.txt"))?;

    let hdr: Vec<&str> = manifest
        .lines()
        .filter(|l| l.starts_with("texture "))
        .filter(|l| l.split_whitespace().nth(2) == Some("bc6h"))
        .filter_map(|l| l.split_whitespace().nth(1))
        .collect();
    if hdr.is_empty() {
        println!("pack has no HDR content — the blind spot is back");
        return Ok(());
    }
    println!("{} HDR textures in the pack\n", hdr.len());

    let threads = std::thread::available_parallelism()?.get();
    let (mut tot_serial, mut tot_par, mut tot_px) = (0f64, 0f64, 0u64);

    for file in &hdr {
        let bytes = std::fs::read(std::path::Path::new(&dir).join(file))?;
        let dds = DdsView::parse(&bytes)?;
        let mips = dds.get_num_mipmap_levels();

        for mip in 0..mips {
            let id = SubresourceId::mip_layer(mip, 0);
            let surf = dds.surface(id)?;
            let (w, h) = (surf.width, surf.height);
            let px = (w as u64) * (h as u64);

            // Serial, into a recycled buffer.
            let mut whole = Vec::new();
            dds.decode_rgba_f32_into(id, &mut whole)?;
            let iters = if px > 65_536 { 8 } else { 64 };
            let t0 = Instant::now();
            for _ in 0..iters {
                dds.decode_rgba_f32_into(id, &mut whole)?;
            }
            let ms_serial = t0.elapsed().as_secs_f64() * 1e3 / iters as f64;

            // Caller-parallel across the block rows.
            // Split only where the surface is big enough to pay for the
            // threads. Splitting a whole mip chain blindly is a NET LOSS —
            // measured 0.53x on this pack, because a 10-level chain is mostly
            // tiny mips where the spawn is pure overhead. 16 384 blocks is
            // 512x512, the same crossover rusty_dds measured for BC7.
            const MIN_BLOCKS_TO_SPLIT: u64 = 16_384;
            let rows = dds.block_rows_f32(id)?;
            let blocks = (w as u64).div_ceil(4) * (h as u64).div_ceil(4);
            let n = if blocks >= MIN_BLOCKS_TO_SPLIT {
                threads.min(rows.max(1) as usize)
            } else {
                1
            };
            let mut split = vec![0f32; (px * 4) as usize];
            let dref = &dds;
            let run = |dst: &mut [f32]| {
                if n == 1 {
                    // Below the threshold, decode on this thread. Entering
                    // `thread::scope` at all costs ~50 us even to spawn one
                    // worker, which is more than the entire decode of every mip
                    // past level 4 — the split has to be skipped, not narrowed.
                    dref.decode_block_rows_f32_into(id, 0..rows, dst).unwrap();
                    return;
                }
                let per = rows.div_ceil(n as u32);
                let mut rest = &mut dst[..];
                let mut bands = Vec::new();
                for t in 0..n as u32 {
                    let (a, b) = ((t * per).min(rows), ((t + 1) * per).min(rows));
                    let take = (((b - a) * 4).min(h.saturating_sub(a * 4))) as usize * w as usize * 4;
                    let (head, tail) = rest.split_at_mut(take);
                    bands.push((a..b, head));
                    rest = tail;
                }
                std::thread::scope(|sc| {
                    for (r, slot) in bands {
                        sc.spawn(move || dref.decode_block_rows_f32_into(id, r, slot).unwrap());
                    }
                });
            };
            run(&mut split);
            let t0 = Instant::now();
            for _ in 0..iters {
                run(&mut split);
            }
            let ms_par = t0.elapsed().as_secs_f64() * 1e3 / iters as f64;

            assert_eq!(split, whole, "{file} mip {mip} ({w}x{h}): split diverged from whole");

            println!(
                "{file}  mip{mip} {w:4}x{h:<4}  serial {ms_serial:7.3} ms  {n:2}-thread {ms_par:7.3} ms  -> {:5.2}x  {}",
                ms_serial / ms_par,
                if n == 1 { "below split threshold" } else { "[parity ok]" }
            );
            tot_serial += ms_serial;
            tot_par += ms_par;
            tot_px += px;
        }
    }

    let mpx = |t: f64| (tot_px as f64 / 1e6) / (t / 1e3);
    println!(
        "\nwhole pack, all mips: serial {tot_serial:.3} ms ({:.1} Mpx/s)  \
         caller-parallel {tot_par:.3} ms ({:.1} Mpx/s)  -> {:.2}x, every level bit-identical",
        mpx(tot_serial),
        mpx(tot_par),
        tot_serial / tot_par
    );
    Ok(())
}
