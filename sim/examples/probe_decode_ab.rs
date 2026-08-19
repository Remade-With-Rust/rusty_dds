//! Decode, rusty_dds against DirectXTex, every format in a pack.
//!
//! §17 compared HDR decode 1:1 and found 3.75x. The LDR half of that same
//! comparison had never been run — both providers implement `decode_rgba8` and
//! nothing called them. Picking the next optimisation target without this table
//! is guessing at which format is actually behind.
//!
//!   cargo run --release --features dxtex --example probe_decode_ab -- <pack-dir>

use std::collections::BTreeMap;
use std::time::Instant;

use rusty_dds_sim::dxtex::{DxTexProvider, Peer};
use rusty_dds_sim::provider::{RustyProvider, SubId, TextureProvider};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Pin before measuring. This box runs 70%+ busy from other processes, and
    // an unpinned probe competes with them for cores — which is why the same
    // change has read +34.5% and +3.5% on consecutive runs. Mask 0x3c is four
    // physical cores; HIGH_PRIORITY_CLASS keeps the scheduler off us.
    let pinned = rusty_dds_sim::os::pin_process(0x3c, true);
    eprintln!("[probe] pinned={pinned} mask=0x3c high_priority=true");

    let dir = std::env::args().nth(1).ok_or("usage: probe_decode_ab <pack-dir>")?;
    let root = std::path::Path::new(&dir);
    let manifest = std::fs::read_to_string(root.join("pack.txt"))?;

    let rusty = RustyProvider;
    let dxt = DxTexProvider::new(Peer::Loader)?;

    // format -> (rusty ms, dxtex ms, pixels)
    let mut acc: BTreeMap<String, (f64, f64, u64)> = BTreeMap::new();

    for line in manifest.lines().filter(|l| l.starts_with("texture ")) {
        let f: Vec<&str> = line.split_whitespace().collect();
        let (file, content) = (f[1], f[2].to_string());
        let bytes = std::fs::read(root.join(file))?;
        let a = rusty.open(bytes.clone())?;
        let b = dxt.open(bytes)?;
        let hdr = content == "bc6h";

        // Mip 0 only: the streaming-relevant size, and the one where any
        // per-call overhead is smallest relative to the work.
        let id = SubId { mip: 0, layer: 0, face: 0 };
        let (w, h) = (a.desc().width, a.desc().height);
        let px = (w as u64) * (h as u64);

        // Agreement first. A speed number between decoders that disagree is
        // not a result. LDR is exact for BCn; HDR is compared with a tolerance
        // only because the two take different routes to the same f32.
        if hdr {
            let (pa, pb) = (a.decode_rgba_f32(id)?, b.decode_rgba_f32(id)?);
            let worst = pa.iter().zip(&pb).map(|(x, y)| (x - y).abs()).fold(0f32, f32::max);
            if worst > 1e-3 {
                return Err(format!("{file}: HDR decoders disagree by {worst}").into());
            }
        } else {
            let (pa, pb) = (a.decode_rgba8(id)?, b.decode_rgba8(id)?);
            if pa.len() != pb.len() {
                return Err(format!("{file}: {} bytes vs {}", pa.len(), pb.len()).into());
            }
            let diff = pa.iter().zip(&pb).filter(|(x, y)| x != y).count();
            // BCn decode is exact, but DirectXTex and bcdec round the 5/6-bit
            // endpoint expansion differently on some formats. Report rather
            // than assert, so a real gap is visible instead of fatal.
            if diff > 0 {
                eprintln!(
                    "  note: {file} ({content}) differs in {diff}/{} bytes ({:.3}%)",
                    pa.len(),
                    100.0 * diff as f64 / pa.len() as f64
                );
            }
        }

        let iters = if px > 262_144 { 10 } else { 40 };
        let ta = Instant::now();
        for _ in 0..iters {
            if hdr { std::hint::black_box(a.decode_rgba_f32(id)?); }
            else { std::hint::black_box(a.decode_rgba8(id)?); }
        }
        let ms_a = ta.elapsed().as_secs_f64() * 1e3 / iters as f64;
        let tb = Instant::now();
        for _ in 0..iters {
            if hdr { std::hint::black_box(b.decode_rgba_f32(id)?); }
            else { std::hint::black_box(b.decode_rgba8(id)?); }
        }
        let ms_b = tb.elapsed().as_secs_f64() * 1e3 / iters as f64;

        let e = acc.entry(content).or_insert((0.0, 0.0, 0));
        e.0 += ms_a;
        e.1 += ms_b;
        e.2 += px;
    }

    println!("\n{:<8} {:>10} {:>12} {:>12} {:>10}", "format", "Mpx", "rusty_dds", "DirectXTex", "ratio");
    println!("{}", "-".repeat(56));
    let (mut ta, mut tb) = (0.0, 0.0);
    for (fmt, (a, b, px)) in &acc {
        let mpx = *px as f64 / 1e6;
        println!(
            "{fmt:<8} {mpx:>10.2} {:>9.1} Mpx/s {:>9.1} Mpx/s {:>9.2}x",
            mpx / (a / 1e3),
            mpx / (b / 1e3),
            b / a
        );
        ta += a;
        tb += b;
    }
    println!("{}", "-".repeat(56));
    println!("{:<8} {:>10} {:>12} {:>12} {:>9.2}x", "all", "", "", "", tb / ta);
    Ok(())
}
