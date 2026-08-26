//! Payload hash of a fixed BC4/BC5 encode (both signs), for a byte-identity
//! gate across a refactor. Companion to `probe_encbytes` (which stops at
//! BC5_UNORM): the signed path runs its own LS refit
//! (`ls_alpha_endpoints_s`), so a hash that never reaches it proves nothing
//! about it.
use rusty_dds::{Dds, DecodeContent, EncodeLayout};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    for (name, content) in [
        ("bc4u", DecodeContent::Bc4UNorm),
        ("bc4s", DecodeContent::Bc4SNorm),
        ("bc5s", DecodeContent::Bc5SNorm),
    ] {
        let side = 256u32;
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
        let dds = Dds::encode_from_rgba8(&px, EncodeLayout::flat_2d(content, side, side).with_mips(9))?;
        let mut h: u64 = 0xcbf29ce484222325;
        for b in &dds.data {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        println!("{name} {:016x} {} bytes", h, dds.data.len());
    }
    Ok(())
}
