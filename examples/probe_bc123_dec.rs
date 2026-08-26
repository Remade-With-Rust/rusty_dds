//! BC1/BC2/BC3 decode: output hash + best-of-N warm-buffer timing, for the
//! D3 alpha-palette-in-register A/B (inline-exe §5.2f.1). BC3 is the target
//! (its alpha palette build ceiling-probes at ~22% of decode); BC1 and BC2
//! anchor the same surface-kernel family.
use rusty_dds::{DecodeContent, Dds, EncodeLayout, SubresourceId};
use std::time::Instant;

fn main() {
    for (name, content) in [
        ("bc1", DecodeContent::Bc1),
        ("bc2", DecodeContent::Bc2),
        ("bc3", DecodeContent::Bc3),
    ] {
        let side = 2048u32;
        let n = (side * side) as usize;
        let mut px = Vec::with_capacity(n * 4);
        for i in 0..n {
            let x = (i as u32 % side) as f32 / side as f32;
            let y = (i as u32 / side) as f32 / side as f32;
            let v = |a: f32| (a.clamp(0.0, 1.0) * 255.0) as u8;
            px.extend_from_slice(&[
                v(x + 0.2 * (y * 24.0).sin()),
                v(y + 0.2 * (x * 18.0).cos()),
                v(0.5 + 0.4 * ((x * 12.0).sin() * (y * 12.0).cos())),
                v(0.5 + 0.5 * ((x * 160.0).sin() * (y * 96.0).cos())),
            ]);
        }
        let dds =
            Dds::encode_from_rgba8(&px, EncodeLayout::flat_2d(content, side, side)).unwrap();
        let id = SubresourceId::mip_layer(0, 0);
        let mut buf: Vec<u8> = Vec::new();
        dds.decode_rgba8_into(id, &mut buf).unwrap();
        let mut hh: u64 = 0xcbf29ce484222325;
        for &b in &buf {
            hh ^= b as u64;
            hh = hh.wrapping_mul(0x100000001b3);
        }
        let mut best = u128::MAX;
        for _ in 0..100 {
            let t = Instant::now();
            dds.decode_rgba8_into(id, &mut buf).unwrap();
            best = best.min(t.elapsed().as_nanos());
        }
        println!(
            "{name} {hh:016x} best {best} ns  {:.4} ns/px  {:.1} Mpx/s",
            best as f64 / n as f64,
            n as f64 * 1000.0 / best as f64
        );
    }
}
