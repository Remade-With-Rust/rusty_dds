//! CLI round trip (plan I1): `rusty-dds encode --format astc` then
//! `rusty-dds decode` equals the library's encode and decode.

#![cfg(all(feature = "astc", feature = "decode", feature = "encode"))]

use rusty_dds::astc::{self, AstcEncodeOptions, AstcFootprint, AstcPreset, AstcProfile};
use std::process::Command;

#[test]
fn cli_round_trip_matches_library() {
    let dir = std::env::temp_dir().join(format!("rusty_dds_astc_cli_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (w, h) = (21u32, 13u32);
    let px: Vec<u8> = (0..w * h)
        .flat_map(|i| {
            [
                (i * 11) as u8,
                (i * 5) as u8,
                (255u32.wrapping_sub(i)) as u8,
                200,
            ]
        })
        .collect();
    let raw = dir.join("in.rgba");
    std::fs::write(&raw, &px).unwrap();
    let exe = env!("CARGO_BIN_EXE_rusty-dds");
    for ext in ["astc", "ktx", "ktx2", "dds"] {
        let out = dir.join(format!("out.{ext}"));
        let st = Command::new(exe)
            .args([
                "encode",
                "--width",
                "21",
                "--height",
                "13",
                "--format",
                "astc",
                "--block",
                "5x4",
                "--preset",
                "fast",
                "--profile",
                "ldr",
            ])
            .arg(&raw)
            .arg("-o")
            .arg(&out)
            .status()
            .unwrap();
        assert!(st.success(), "{ext}: encode");
        let dec = dir.join(format!("out_{ext}.rgba"));
        let st = Command::new(exe)
            .arg("decode")
            .arg(&out)
            .args(["--profile", "ldr", "-o"])
            .arg(&dec)
            .status()
            .unwrap();
        assert!(st.success(), "{ext}: decode");
        let opts = AstcEncodeOptions::new(AstcFootprint::B5X4, AstcProfile::Ldr)
            .with_preset(AstcPreset::Fast);
        let lib = astc::encode_rgba8(&px, w, h, &opts).unwrap();
        let want = astc::decode_rgba8(&lib, w, h, AstcFootprint::B5X4, AstcProfile::Ldr)
            .unwrap()
            .pixels;
        assert_eq!(std::fs::read(&dec).unwrap(), want, "{ext}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
