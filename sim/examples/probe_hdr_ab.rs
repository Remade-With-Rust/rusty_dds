//! BC6H decode, rusty_dds against DirectXTex, 1:1.
//!
//! The harness could compare LDR decode on both stacks and HDR on neither. That
//! asymmetry is why "BC6H is slow" sat unexamined: slow *against what*? This
//! answers it, and checks the two decoders agree on the pixels first — a speed
//! number between two decoders that disagree is meaningless.
//!
//!   cargo run --release --features dxtex --example probe_hdr_ab -- <pack-dir>

use std::time::Instant;

use rusty_dds_sim::provider::{SubId, TextureProvider};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args().nth(1).ok_or("usage: probe_hdr_ab <pack-dir>")?;
    let manifest = std::fs::read_to_string(std::path::Path::new(&dir).join("pack.txt"))?;
    let hdr: Vec<&str> = manifest
        .lines()
        .filter(|l| l.starts_with("texture "))
        .filter(|l| l.split_whitespace().nth(2) == Some("bc6h"))
        .filter_map(|l| l.split_whitespace().nth(1))
        .collect();
    if hdr.is_empty() {
        return Err("pack has no HDR content".into());
    }

    let rusty = rusty_dds_sim::provider::RustyProvider;
    let dxt = rusty_dds_sim::dxtex::DxTexProvider::new(rusty_dds_sim::dxtex::Peer::Loader)?;

    println!("{:<22} {:>10} {:>12} {:>12} {:>9}", "texture / mip", "pixels", "rusty_dds", "DirectXTex", "ratio");
    let (mut tr, mut td, mut tp) = (0f64, 0f64, 0u64);

    for file in &hdr {
        let bytes = std::fs::read(std::path::Path::new(&dir).join(file))?;
        let a = rusty.open(bytes.clone())?;
        let b = dxt.open(bytes)?;
        let mips = a.desc().mips;

        for mip in 0..mips {
            let id = SubId { mip, layer: 0, face: 0 };
            let (w, h) = (a.desc().width >> mip, a.desc().height >> mip);
            let px = (w.max(1) as u64) * (h.max(1) as u64);

            let pa = a.decode_rgba_f32(id)?;
            let pb = b.decode_rgba_f32(id)?;
            if pa.len() != pb.len() {
                return Err(format!("{file} mip {mip}: {} floats vs {}", pa.len(), pb.len()).into());
            }
            // BC6H decode is exact — both are decoding the same bits by the same
            // spec — so anything but agreement is a bug in one of them.
            let worst = pa.iter().zip(&pb).map(|(x, y)| (x - y).abs()).fold(0f32, f32::max);
            if worst > 1e-3 {
                return Err(format!("{file} mip {mip}: decoders disagree by {worst}").into());
            }

            let iters = if px > 65_536 { 20 } else { 100 };
            let t0 = Instant::now();
            for _ in 0..iters { std::hint::black_box(a.decode_rgba_f32(id)?); }
            let ms_a = t0.elapsed().as_secs_f64() * 1e3 / iters as f64;
            let t0 = Instant::now();
            for _ in 0..iters { std::hint::black_box(b.decode_rgba_f32(id)?); }
            let ms_b = t0.elapsed().as_secs_f64() * 1e3 / iters as f64;

            if mip < 3 {
                println!(
                    "{:<22} {px:>10} {ms_a:>9.3} ms {ms_b:>9.3} ms {:>8.2}x",
                    format!("{} mip{mip}", &file[..file.len().min(14)]),
                    ms_b / ms_a
                );
            }
            tr += ms_a; td += ms_b; tp += px;
        }
    }
    let mpx = |t: f64| (tp as f64 / 1e6) / (t / 1e3);
    println!(
        "\nall HDR, all mips:  rusty_dds {tr:.3} ms ({:.1} Mpx/s)   DirectXTex {td:.3} ms ({:.1} Mpx/s)   -> {:.2}x",
        mpx(tr), mpx(td), td / tr
    );
    Ok(())
}
