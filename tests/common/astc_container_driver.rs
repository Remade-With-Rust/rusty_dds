//! Shared by `tests/astc_containers.rs` (stable, deterministic) and
//! `fuzz/fuzz_targets/astc_container.rs` (libFuzzer): feed arbitrary bytes to
//! every ASTC container parser and every operation reachable from a parsed
//! value. Property: `Ok` or `Err`, never a panic, and never an allocation the
//! input does not justify (decodes are bounded by the payload actually present).

#![allow(dead_code)]

use rusty_dds::astc::{self, AstcFile, AstcProfile, Ktx2View, KtxView};

/// Largest decode the driver attempts (texels), so a valid tiny header with a
/// real payload cannot turn the fuzzer into a decode benchmark.
const MAX_TEXELS: u64 = 1 << 16;

fn decode_surface(s: &astc::AstcSurface) {
    let t = s.width as u64 * s.height as u64 * s.depth as u64;
    if t == 0 || t > MAX_TEXELS {
        return;
    }
    let _ = s.decode_rgba8(s.default_profile());
    let _ = s.decode_rgba_f32(s.default_profile_f32());
    let _ = s.decode_rgba8(AstcProfile::Ldr);
}

pub fn exercise(data: &[u8]) {
    if let Ok(f) = AstcFile::parse(data) {
        let s = f.surface();
        assert_eq!(
            s.data.len(),
            astc::payload_len(s.width, s.height, s.depth, s.footprint).unwrap()
        );
        decode_surface(&s);
    }
    if let Ok(k) = KtxView::parse(data) {
        for l in 0..k.levels.min(16) {
            for y in 0..k.layers.min(8) {
                for f in 0..k.faces {
                    if let Ok(s) = k.astc_surface(l, y, f) {
                        decode_surface(&s);
                    }
                }
            }
        }
        let _ = k.astc_surface(k.levels, 0, 0);
    }
    if let Ok(k) = Ktx2View::parse(data) {
        for l in 0..k.levels.min(16) {
            for y in 0..k.layers.min(8) {
                for f in 0..k.faces {
                    if let Ok(s) = k.astc_surface(l, y, f) {
                        decode_surface(&s);
                    }
                }
            }
        }
    }
    #[cfg(feature = "ktx2-zstd")]
    if let Ok(v) = astc::ktx::ktx2_inflate(data) {
        let _ = Ktx2View::parse(&v);
    }
}
