//! BC7 decode across surface sizes, serial, into a recycled buffer.
//!
//! At 1024^2 BC7 decode is memory-bandwidth bound — it scales only 3.7x on 24
//! cores. If that is the limit, a faster block decoder cannot show up there no
//! matter how much ALU work it saves. Smaller surfaces stay in cache, where the
//! decoder itself is the limit. Running both tells you which you are optimising.

use std::time::Instant;

use rusty_dds::{DdsView, SubresourceId};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Pin before measuring. This box runs 70%+ busy from other processes, and
    // an unpinned probe competes with them for cores — which is why the same
    // change has read +34.5% and +3.5% on consecutive runs. Mask 0x3c is four
    // physical cores; HIGH_PRIORITY_CLASS keeps the scheduler off us.
    let pinned = rusty_dds_sim::os::pin_process(0x3c, true);
    eprintln!("[probe] pinned={pinned} mask=0x3c high_priority=true");

    let dir = std::env::args().nth(1).ok_or("usage: probe_bc7_speed <pack-dir>")?;
    let root = std::path::Path::new(&dir);
    let manifest = std::fs::read_to_string(root.join("pack.txt"))?;
    let file = manifest
        .lines()
        .filter(|l| l.starts_with("texture "))
        .find(|l| l.split_whitespace().nth(2) == Some("bc7"))
        .and_then(|l| l.split_whitespace().nth(1))
        .ok_or("no bc7 in pack")?;

    let bytes = std::fs::read(root.join(file))?;
    let view = DdsView::parse(&bytes)?;
    let mut buf = Vec::new();

    println!("{file}\n{:<12} {:>10} {:>12} {:>10}", "surface", "blocks", "ms/call", "Mpx/s");
    for mip in 0..view.get_num_mipmap_levels().min(5) {
        let id = SubresourceId::mip_layer(mip, 0);
        let surf = view.surface(id)?;
        let (w, h) = (surf.width, surf.height);
        let blocks = (w as u64).div_ceil(4) * (h as u64).div_ceil(4);
        let px = (w as u64) * (h as u64);
        view.decode_rgba8_into(id, &mut buf)?;
        let iters = if px > 262_144 { 30 } else { 300 };
        let t0 = Instant::now();
        for _ in 0..iters {
            view.decode_rgba8_into(id, &mut buf)?;
        }
        let ms = t0.elapsed().as_secs_f64() * 1e3 / iters as f64;
        println!(
            "{:<12} {blocks:>10} {ms:>11.4} {:>10.1}",
            format!("{w}x{h}"),
            (px as f64 / 1e6) / (ms / 1e3)
        );
    }
    Ok(())
}
