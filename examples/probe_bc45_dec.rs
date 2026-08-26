//! BC4/BC5 decode: output hash + best-of-N warm-buffer timing, for the D1
//! palette-in-`__m128i` A/B (inline-exe §5.2 rank 6). 2048² so the serial
//! per-block chain dominates and a ~6-cycle chain change is visible; both
//! signs, both formats — the palette build runs once per BC4 block and twice
//! per BC5 block.
use rusty_dds::{DecodeContent, Dds, EncodeLayout, SubresourceId};
use std::time::Instant;

fn main() {
    for (name, content) in [
        ("bc4u", DecodeContent::Bc4UNorm),
        ("bc4s", DecodeContent::Bc4SNorm),
        ("bc5u", DecodeContent::Bc5UNorm),
        ("bc5s", DecodeContent::Bc5SNorm),
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
                0,
                255,
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
