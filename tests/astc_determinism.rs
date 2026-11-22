//! EC3: ASTC encoder output is a pure function of (pixels, options, crate
//! version). Frozen FNV-1a hashes per (image, footprint, preset, profile) on
//! synthetic content; the same hashes must come out with `simd` on and off
//! and on every target (x86_64, aarch64, wasm32).
//!
//! A deliberate encoder change regenerates them:
//! `UPDATE_ASTC_HASHES=1 cargo test --release --test astc_determinism`.

#![cfg(all(feature = "astc", feature = "decode", feature = "encode"))]

use rusty_dds::astc::{self, AstcEncodeOptions, AstcFootprint, AstcPreset, AstcProfile};
use std::fmt::Write as _;

fn image(kind: u32, w: u32, h: u32) -> Vec<u8> {
    let mut x: u32 = 0x1234_5678 ^ kind;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x
    };
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for xx in 0..w {
            let p: [u8; 4] = match kind {
                0 => [(xx * 255 / w) as u8, (y * 255 / h) as u8, ((xx ^ y) * 9) as u8, 255],
                1 => {
                    let n = next();
                    [n as u8, (n >> 8) as u8, (n >> 16) as u8, (n >> 24) as u8]
                }
                2 => {
                    let a = if (xx / 3 + y / 2) % 3 == 0 { 0 } else { 255 };
                    [(xx * 13) as u8, (y * 7) as u8, 90, a]
                }
                _ => {
                    let g = ((xx * 3 + y * 5) % 256) as u8;
                    [g, g, g, 255]
                }
            };
            v.extend_from_slice(&p);
        }
    }
    v
}

fn fnv(data: &[u8]) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

fn table() -> String {
    let mut out = String::new();
    let (w, h) = (29u32, 23u32);
    for kind in 0..4 {
        let img = image(kind, w, h);
        for fp in [AstcFootprint::B4X4, AstcFootprint::B6X5, AstcFootprint::B8X8, AstcFootprint::B12X10] {
            for preset in [AstcPreset::Fastest, AstcPreset::Fast, AstcPreset::Medium] {
                for prof in [AstcProfile::Ldr, AstcProfile::LdrSrgb] {
                    let opts = AstcEncodeOptions::new(fp, prof).with_preset(preset);
                    let data = astc::encode_rgba8(&img, w, h, &opts).unwrap();
                    let _ = writeln!(out, "{kind} {fp} {preset:?} {prof:?} {:016x}", fnv(&data));
                }
            }
        }
    }
    out
}

#[test]
fn encoder_output_is_frozen() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/astc/encode_hashes.txt");
    let now = table();
    if std::env::var_os("UPDATE_ASTC_HASHES").is_some() {
        std::fs::write(path, &now).unwrap();
        return;
    }
    // Embedded, so the wasm32 run needs no filesystem access (lines()
    // also accepts CRLF checkouts).
    let want = include_str!("fixtures/astc/encode_hashes.txt");
    for (a, b) in want.lines().zip(now.lines()) {
        assert_eq!(a, b, "encoder output changed");
    }
    assert_eq!(want.lines().count(), now.lines().count());
}
