//! What do the two stacks put in G and B when decoding BC4 to RGBA8?
//!
//! The decode A/B found BC4 differing in exactly 50% of bytes — two channels of
//! four, on every pixel. That is a convention difference, not a rounding one,
//! and a studio swapping stacks would see it as a visual change on every
//! single-channel map they own. Worth knowing which convention is which.

use rusty_dds_sim::dxtex::{DxTexProvider, Peer};
use rusty_dds_sim::provider::{RustyProvider, SubId, TextureProvider};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Pin before measuring. This box runs 70%+ busy from other processes, and
    // an unpinned probe competes with them for cores — which is why the same
    // change has read +34.5% and +3.5% on consecutive runs. Mask 0x3c is four
    // physical cores; HIGH_PRIORITY_CLASS keeps the scheduler off us.
    let pinned = rusty_dds_sim::os::pin_process(0x3c, true);
    eprintln!("[probe] pinned={pinned} mask=0x3c high_priority=true");

    let dir = std::env::args().nth(1).ok_or("usage: probe_bc4_channels <pack-dir>")?;
    let root = std::path::Path::new(&dir);
    let manifest = std::fs::read_to_string(root.join("pack.txt"))?;
    let file = manifest
        .lines()
        .filter(|l| l.starts_with("texture "))
        .find(|l| l.split_whitespace().nth(2) == Some("bc4u"))
        .and_then(|l| l.split_whitespace().nth(1))
        .ok_or("no bc4u in pack")?;

    let bytes = std::fs::read(root.join(file))?;
    let a = RustyProvider.open(bytes.clone())?;
    let b = DxTexProvider::new(Peer::Loader)?.open(bytes)?;
    let id = SubId { mip: 0, layer: 0, face: 0 };
    let (pa, pb) = (a.decode_rgba8(id)?, b.decode_rgba8(id)?);

    println!("{file}, first 6 pixels as RGBA8\n");
    println!("{:<6} {:>18} {:>18}", "px", "rusty_dds", "DirectXTex");
    for i in 0..6 {
        let (x, y) = (&pa[i * 4..i * 4 + 4], &pb[i * 4..i * 4 + 4]);
        println!(
            "{i:<6} {:>18} {:>18}",
            format!("{},{},{},{}", x[0], x[1], x[2], x[3]),
            format!("{},{},{},{}", y[0], y[1], y[2], y[3])
        );
    }

    // Which channels ever disagree?
    let mut bad = [0usize; 4];
    for i in 0..pa.len() {
        if pa[i] != pb[i] {
            bad[i % 4] += 1;
        }
    }
    let n = pa.len() / 4;
    println!("\nper-channel disagreement over {n} pixels:");
    for (c, name) in ["R", "G", "B", "A"].iter().enumerate() {
        println!("  {name}: {}/{n}", bad[c]);
    }
    Ok(())
}
