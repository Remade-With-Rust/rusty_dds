//! Payload hash of a fixed encode, for a byte-identity gate across a refactor.
use rusty_dds::{Dds, DecodeContent, EncodeLayout};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Three fixtures, because the mode gates partition on alpha: varying alpha
    // reaches BC7 modes 4/5, and ONLY fully-opaque alpha reaches mode 1
    // (its gate is `a_lo == 255`). A hash over one fixture proves nothing about
    // the modes the others unlock.
    for (kind, tag) in [(0, ""), (1, "-alpha"), (2, "-opaque")] {
    for (name, content) in [("bc7", DecodeContent::Bc7), ("bc1", DecodeContent::Bc1),
                            ("bc3", DecodeContent::Bc3), ("bc5u", DecodeContent::Bc5UNorm)] {
        let side = 256u32;
        let n = (side * side) as usize;
        let mut px = Vec::with_capacity(n * 4);
        for i in 0..n {
            let x = (i as u32 % side) as f32 / side as f32;
            let y = (i as u32 / side) as f32 / side as f32;
            let v = |a: f32| (a.clamp(0.0, 1.0) * 255.0) as u8;
            px.extend_from_slice(&[v(x + 0.2 * (y * 24.0).sin()), v(y + 0.2 * (x * 18.0).cos()),
                                   v(0.5 + 0.4 * ((x * 12.0).sin() * (y * 12.0).cos())), match kind { 2 => 255, 1 => v(0.5 + 0.5 * ((x * 160.0).sin() * (y * 96.0).cos())), _ => v(0.6 + 0.4 * x * y) }]);
        }
        let dds = Dds::encode_from_rgba8(&px, EncodeLayout::flat_2d(content, side, side).with_mips(9))?;
        let mut h: u64 = 0xcbf29ce484222325;
        for b in &dds.data { h ^= *b as u64; h = h.wrapping_mul(0x100000001b3); }
        println!("{name}{tag} {:016x} {} bytes", h, dds.data.len());
    }
    }
    Ok(())
}
