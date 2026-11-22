//! ASTC encode of arbitrary pixels: every 2D footprint, both LDR profiles,
//! sizes 1..=64, the three fast presets. Properties: encode succeeds, every
//! emitted block is legal by our parser, decode has the declared size, and
//! encoding is deterministic.

#![no_main]

use libfuzzer_sys::fuzz_target;
use rusty_dds::astc::{self, AstcEncodeOptions, AstcFootprint, AstcPreset, AstcProfile};

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let fp = AstcFootprint::ALL_2D[data[0] as usize % 14];
    let profile = if data[0] & 0x80 != 0 { AstcProfile::LdrSrgb } else { AstcProfile::Ldr };
    let preset = [AstcPreset::Fastest, AstcPreset::Fast, AstcPreset::Medium][data[1] as usize % 3];
    let w = 1 + data[2] as u32 % 64;
    let h = 1 + data[3] as u32 % 64;
    let n = (w * h * 4) as usize;
    let src = &data[4..];
    let px: Vec<u8> = (0..n).map(|i| if src.is_empty() { 0 } else { src[i % src.len()] }).collect();
    let opts = AstcEncodeOptions::new(fp, profile).with_preset(preset);
    let a = astc::encode_rgba8(&px, w, h, &opts).expect("encode");
    let b = astc::encode_rgba8(&px, w, h, &opts).expect("encode");
    assert_eq!(a, b, "encoding is not deterministic");
    for blk in a.chunks_exact(16) {
        let blk: &[u8; 16] = blk.try_into().unwrap();
        assert!(astc::illegal_reason(blk, fp).is_none(), "illegal block emitted");
    }
    let img = astc::decode_rgba8(&a, w, h, fp, profile).expect("decode");
    assert_eq!(img.pixels.len(), n);
});
