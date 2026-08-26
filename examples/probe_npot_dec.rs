//! What does the `w%4==0 && h%4==0` surface-kernel gate COST? (inline-exe D8.)
//!
//! Every BCn decode has a whole-surface SSSE3 kernel and a per-block scalar
//! fallback; the gate sends any NPOT surface to the fallback. This measures
//! the same pixel count both ways: 1024² (aligned, kernel) against 1023²
//! (one pixel short, scalar) and 1020×1023 (width aligned, height not).
//! Per-pixel rates are directly comparable — if the gate is over-strict, the
//! NPOT column is the size of the prize.
use rusty_dds::{DecodeContent, Dds, EncodeLayout, SubresourceId};
use std::time::Instant;

fn main() {
    for (name, content) in [
        ("bc1", DecodeContent::Bc1),
        ("bc2", DecodeContent::Bc2),
        ("bc3", DecodeContent::Bc3),
        ("bc4u", DecodeContent::Bc4UNorm),
        ("bc5u", DecodeContent::Bc5UNorm),
    ] {
        for (tag, w, h) in [
            ("aligned 1024x1024", 1024u32, 1024u32),
            ("npot    1023x1023", 1023, 1023),
            ("npot-h  1020x1023", 1020, 1023),
        ] {
            let n = (w * h) as usize;
            let mut px = Vec::with_capacity(n * 4);
            for i in 0..n {
                let x = (i as u32 % w) as f32 / w as f32;
                let y = (i as u32 / w) as f32 / h as f32;
                let v = |a: f32| (a.clamp(0.0, 1.0) * 255.0) as u8;
                px.extend_from_slice(&[
                    v(x + 0.2 * (y * 24.0).sin()),
                    v(y + 0.2 * (x * 18.0).cos()),
                    v(0.5 + 0.4 * ((x * 12.0).sin() * (y * 12.0).cos())),
                    v(0.5 + 0.5 * ((x * 160.0).sin() * (y * 96.0).cos())),
                ]);
            }
            let dds = Dds::encode_from_rgba8(&px, EncodeLayout::flat_2d(content, w, h)).unwrap();
            let id = SubresourceId::mip_layer(0, 0);
            let mut buf: Vec<u8> = Vec::new();
            dds.decode_rgba8_into(id, &mut buf).unwrap();
            let mut hh: u64 = 0xcbf29ce484222325;
            for &b in &buf {
                hh ^= b as u64;
                hh = hh.wrapping_mul(0x100000001b3);
            }
            let mut best = u128::MAX;
            for _ in 0..60 {
                let t = Instant::now();
                dds.decode_rgba8_into(id, &mut buf).unwrap();
                best = best.min(t.elapsed().as_nanos());
            }
            println!(
                "{name} {tag} {hh:016x} {:.4} ns/px  {:.1} Mpx/s",
                best as f64 / n as f64,
                n as f64 * 1000.0 / best as f64
            );
        }
    }
}
