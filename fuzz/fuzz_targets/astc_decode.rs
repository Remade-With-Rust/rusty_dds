//! ASTC decode of arbitrary bytes: every footprint, every profile, both
//! output types, cropped surface sizes. Properties: no panic; an `Ok` decode
//! writes every output value; a second decode gives the same bytes and the
//! same report; the 8-bit and half-float paths agree on which blocks are
//! illegal.

#![no_main]

use libfuzzer_sys::fuzz_target;
use rusty_dds::astc::{self, AstcFootprint, AstcProfile};

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let fps: Vec<AstcFootprint> = AstcFootprint::ALL_2D
        .iter()
        .chain(AstcFootprint::ALL_3D.iter())
        .copied()
        .collect();
    let fp = fps[data[0] as usize % fps.len()];
    let profile = [
        AstcProfile::LdrSrgb,
        AstcProfile::Ldr,
        AstcProfile::HdrRgbLdrA,
        AstcProfile::Hdr,
    ][(data[0] as usize / fps.len()) % 4];
    let w = 1 + data[1] as u32 % 64;
    let h = 1 + data[2] as u32 % 64;
    let d = if fp.is_3d() { 1 + data[3] as u32 % 8 } else { 1 };
    let payload = &data[4..];
    let n = (w * h * d * 4) as usize;

    let mut f16 = vec![0x5A5Au16; n];
    let r16 = astc::decode_rgba_f16_into(payload, w, h, d, fp, profile, &mut f16);
    let mut again = vec![0xA5A5u16; n];
    let r16b = astc::decode_rgba_f16_into(payload, w, h, d, fp, profile, &mut again);
    match (&r16, &r16b) {
        (Ok(a), Ok(b)) => {
            assert_eq!(a, b);
            assert_eq!(f16, again, "decode is not deterministic or leaves values unwritten");
        }
        (Err(_), Err(_)) => {}
        _ => panic!("same input, different outcome"),
    }
    if matches!(profile, AstcProfile::LdrSrgb | AstcProfile::Ldr) {
        let mut u8s = vec![0u8; n];
        let r8 = astc::decode_rgba8_into(payload, w, h, d, fp, profile, &mut u8s);
        if let (Ok(a), Ok(b)) = (&r8, &r16) {
            assert_eq!(a.illegal_blocks, b.illegal_blocks);
            assert_eq!(a.hdr_in_ldr_blocks, b.hdr_in_ldr_blocks);
        }
        assert_eq!(r8.is_ok(), r16.is_ok());
    }
});
