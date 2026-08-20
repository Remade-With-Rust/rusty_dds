//! Payload hash of RDO encodes across a lambda ladder, for a byte-identity gate.
//!
//! RDO output is a rate/quality tradeoff rather than a fixed target, so a
//! *refactor* of the RDO path must not move it at all. Every entry below is an
//! FNV-1a over the encoded payload, so any change of any block shows up.
//!
//! Both formats and several lambdas, because the RDO path branches on lambda
//! (λ=0 disables it entirely) and BC1 and BC7 run completely different drivers.
use rusty_dds::{Dds, DecodeContent, EncodeLayout, Rdo};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let side = 256u32;
    let n = (side * side) as usize;
    // Structured, repetitive content: RDO's job is finding reusable blocks, so a
    // source with real structure exercises the match paths rather than the
    // trivial ones.
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

    for (name, content) in [("bc1", DecodeContent::Bc1), ("bc7", DecodeContent::Bc7)] {
        for lam in [0.0f32, 4.0, 25.0, 100.0] {
            let rdo = if lam == 0.0 { Rdo::Off } else { Rdo::lambda(lam) };
            let layout = EncodeLayout::flat_2d(content, side, side)
                .with_mips(4)
                .with_rdo(rdo);
            let dds = Dds::encode_from_rgba8(&px, layout)?;
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for b in &dds.data {
                h ^= *b as u64;
                h = h.wrapping_mul(0x100_0000_01b3);
            }
            println!("{name} lambda={lam:<6} {h:016x} {} bytes", dds.data.len());
        }
    }
    Ok(())
}
