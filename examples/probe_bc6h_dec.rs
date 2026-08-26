//! BC6H decode: output hash + best-of-N warm-buffer timing, for the D5
//! dispatch-hoist A/B (inline-exe §5.2 rank 2). Two surfaces: 1024² is all
//! interior blocks (the planar-scatter fast path), 1023² adds the edge-block
//! tail path. The hash is the byte-identity gate; the timing is the win.
use rusty_dds::{Dds, SubresourceId};
use std::time::Instant;

fn main() {
    for (w, h) in [(1024u32, 1024u32), (1023, 1023)] {
        let n = (w * h) as usize;
        let mut px = Vec::with_capacity(n * 4);
        for i in 0..n {
            let x = (i as u32 % w) as f32 / w as f32;
            let y = (i as u32 / w) as f32 / h as f32;
            px.extend_from_slice(&[
                x * 8.0 + (y * 32.0).sin().abs(),
                y * 4.0 + (x * 16.0).cos().abs(),
                (x * y * 12.0).fract() * 6.0,
                1.0,
            ]);
        }
        let dds = Dds::encode_bc6h_uf16(&px, w, h).unwrap();
        let id = SubresourceId::mip_layer(0, 0);
        let mut buf: Vec<f32> = Vec::new();
        dds.decode_rgba_f32_into(id, &mut buf).unwrap();
        let mut hh: u64 = 0xcbf29ce484222325;
        for f in &buf {
            for b in f.to_le_bytes() {
                hh ^= b as u64;
                hh = hh.wrapping_mul(0x100000001b3);
            }
        }
        // Best-of-200: on this box the floor is only reachable through a large
        // N when other sessions are building — best-of-N converges to the
        // uncontended floor, which is the comparable number.
        let mut best = u128::MAX;
        for _ in 0..200 {
            let t = Instant::now();
            dds.decode_rgba_f32_into(id, &mut buf).unwrap();
            best = best.min(t.elapsed().as_nanos());
        }
        println!(
            "bc6h {w}x{h} into  {hh:016x} best {best} ns  {:.4} ns/px",
            best as f64 / n as f64
        );
        // The ALLOC form: what `decode_rgba_f32` really costs a caller who
        // doesn't recycle a buffer — decode plus a fresh 16-bytes-per-pixel
        // allocation each call. This is the arm the zero-fill tax lives in.
        let ha = {
            let img = dds.decode_rgba_f32(id).unwrap();
            let mut hh: u64 = 0xcbf29ce484222325;
            for f in &img.pixels {
                for b in f.to_le_bytes() {
                    hh ^= b as u64;
                    hh = hh.wrapping_mul(0x100000001b3);
                }
            }
            hh
        };
        let mut best_a = u128::MAX;
        for _ in 0..100 {
            let t = Instant::now();
            let img = dds.decode_rgba_f32(id).unwrap();
            best_a = best_a.min(t.elapsed().as_nanos());
            drop(img);
        }
        println!(
            "bc6h {w}x{h} alloc {ha:016x} best {best_a} ns  {:.4} ns/px",
            best_a as f64 / n as f64
        );
    }
}
