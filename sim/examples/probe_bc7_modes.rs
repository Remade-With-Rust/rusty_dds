//! Which BC7 modes actually appear?
//!
//! Specialising a block decoder is only worth the risk on modes that carry real
//! blocks. BC7's mode is unary-coded in the low bits of byte 0: the mode number
//! is the count of zero bits before the first set bit.

use std::collections::BTreeMap;

use rusty_dds::{DdsView, SubresourceId};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Pin before measuring. This box runs 70%+ busy from other processes, and
    // an unpinned probe competes with them for cores — which is why the same
    // change has read +34.5% and +3.5% on consecutive runs. Mask 0x3c is four
    // physical cores; HIGH_PRIORITY_CLASS keeps the scheduler off us.
    let pinned = rusty_dds_sim::os::pin_process(0x3c, true);
    eprintln!("[probe] pinned={pinned} mask=0x3c high_priority=true");

    let dir = std::env::args().nth(1).ok_or("usage: probe_bc7_modes <pack-dir>")?;
    let root = std::path::Path::new(&dir);
    let manifest = std::fs::read_to_string(root.join("pack.txt"))?;

    let mut hist: BTreeMap<i32, u64> = BTreeMap::new();
    let mut total = 0u64;
    for line in manifest.lines().filter(|l| l.starts_with("texture ")) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f[2] != "bc7" {
            continue;
        }
        let bytes = std::fs::read(root.join(f[1]))?;
        let view = DdsView::parse(&bytes)?;
        for mip in 0..view.get_num_mipmap_levels() {
            let surf = view.surface(SubresourceId::mip_layer(mip, 0))?;
            for blk in surf.data.chunks_exact(16) {
                // -1 marks the reserved encoding (byte 0 == 0 in the low 8 bits).
                let mode = if blk[0] == 0 { -1 } else { blk[0].trailing_zeros() as i32 };
                *hist.entry(mode).or_default() += 1;
                total += 1;
            }
        }
    }

    println!("{total} BC7 blocks\n");
    println!("{:<8} {:>12} {:>9}  {}", "mode", "blocks", "share", "shape");
    let shape = |m: i32| match m {
        0 => "3 subsets, 4-bit partition, RGB 4.4.4, 3-bit idx",
        1 => "2 subsets, 6-bit partition, RGB 6.6.6, 3-bit idx",
        2 => "3 subsets, 6-bit partition, RGB 5.5.5, 2-bit idx",
        3 => "2 subsets, 6-bit partition, RGB 7.7.7, 2-bit idx",
        4 => "1 subset, rotation, RGB 5.5.5 A6, 2+3-bit idx",
        5 => "1 subset, rotation, RGB 7.7.7 A8, 2+2-bit idx",
        6 => "1 subset, RGBA 7.7.7.7, 4-bit idx",
        7 => "2 subsets, 6-bit partition, RGBA 5.5.5.5, 2-bit idx",
        _ => "reserved (decodes to zero)",
    };
    for (mode, n) in &hist {
        println!("{mode:<8} {n:>12} {:>8.2}%  {}", 100.0 * *n as f64 / total as f64, shape(*mode));
    }

    let single: u64 = hist.iter().filter(|(m, _)| **m >= 4 && **m <= 6).map(|(_, n)| *n).sum();
    println!(
        "\nsingle-subset modes (4,5,6): {single} blocks, {:.2}% — no partition lookup, contiguous indices",
        100.0 * single as f64 / total as f64
    );
    Ok(())
}
