//! Regression test for #29: the AVX2 BC7 and BC1 encoder kernels read `i64` /
//! `i32` lanes out of arrays with a smaller alignment.
//!
//! x86 tolerates those reads, so this only detects the bug under Miri, where
//! the CI `miri` job runs it with AVX2 on and symbolic alignment checks:
//!
//! ```text
//! RUSTFLAGS="-C target-feature=+avx2" MIRIFLAGS=-Zmiri-symbolic-alignment-check \
//!     cargo +nightly miri test --test simd_alignment
//! ```
#![cfg(all(feature = "encode", feature = "decode"))]

use rusty_dds::{Dds, DecodeContent, EncodeLayout, SubresourceId};

/// 8x4 RGBA8: one noise block with varying alpha, one smooth opaque block.
fn pixels() -> Vec<u8> {
    let mut s: u32 = 0x1234_5678;
    let mut px = Vec::with_capacity(8 * 4 * 4);
    for y in 0..4u8 {
        for x in 0..8u8 {
            for c in 0..4u8 {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                px.push(match (x < 4, c) {
                    (true, _) => s as u8,
                    (false, 3) => 255,
                    (false, _) => x * 20 + y * 10 + c,
                });
            }
        }
    }
    px
}

fn round_trip(content: DecodeContent) {
    let dds = Dds::encode_from_rgba8(&pixels(), EncodeLayout::flat_2d(content, 8, 4))
        .unwrap_or_else(|e| panic!("{content:?}: {e}"));
    let img = dds
        .decode_rgba8(SubresourceId::default())
        .unwrap_or_else(|e| panic!("{content:?}: {e}"));
    assert_eq!((img.width, img.height), (8, 4), "{content:?}");
}

macro_rules! formats {
    ($($name:ident: $content:ident),* $(,)?) => {
        $(#[test] fn $name() { round_trip(DecodeContent::$content); })*
    };
}

formats! {
    bc1: Bc1, bc2: Bc2, bc3: Bc3,
    bc4_unorm: Bc4UNorm, bc4_snorm: Bc4SNorm,
    bc5_unorm: Bc5UNorm, bc5_snorm: Bc5SNorm,
    bc7: Bc7, rgba8: Rgba8, bgra8: Bgra8,
}
