//! Encoder round-trip on arbitrary pixels, layouts and RDO strengths.
//!
//! Three properties:
//!   1. Encode never panics for any layout the validator accepts.
//!   2. Anything encoded decodes back at the declared dimensions — the encoder
//!      cannot emit a payload its own decoder rejects.
//!   3. `Rdo` never changes decodability, only the rate/quality point. That is
//!      the claim that makes RDO safe to ship, so it is asserted, not assumed.

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use rusty_dds::{DecodeContent, Dds, EncodeLayout, EncodeQuality, Rdo, SubresourceId};

#[derive(Arbitrary, Debug)]
struct Input {
    format: u8,
    width: u8,
    height: u8,
    fast: bool,
    /// Quantized so the fuzzer explores whole lambdas rather than float noise.
    lambda: u8,
    pixels: Vec<u8>,
}

fuzz_target!(|input: Input| {
    let content = match input.format % 10 {
        0 => DecodeContent::Bc1,
        1 => DecodeContent::Bc2,
        2 => DecodeContent::Bc3,
        3 => DecodeContent::Bc4UNorm,
        4 => DecodeContent::Bc4SNorm,
        5 => DecodeContent::Bc5UNorm,
        6 => DecodeContent::Bc5SNorm,
        7 => DecodeContent::Bc7,
        8 => DecodeContent::Rgba8,
        _ => DecodeContent::Bgra8,
    };
    // Keep the work bounded so the fuzzer spends its time on decisions rather
    // than on megapixels.
    let w = 1 + (input.width % 64) as u32;
    let h = 1 + (input.height % 64) as u32;

    let need = (w as usize) * (h as usize) * 4;
    let mut px = input.pixels;
    if px.len() < need {
        // Extend deterministically rather than bailing: short buffers are
        // already covered by the `TruncatedData` path below.
        let seed = px.len() as u8;
        while px.len() < need {
            px.push(seed.wrapping_mul(31).wrapping_add(px.len() as u8));
        }
    }

    let layout = EncodeLayout::flat_2d(content, w, h)
        .with_quality(if input.fast {
            EncodeQuality::Fast
        } else {
            EncodeQuality::Quality
        })
        .with_rdo(if input.lambda == 0 {
            Rdo::Off
        } else {
            Rdo::lambda(input.lambda as f32)
        });

    let dds = match Dds::encode_from_rgba8(&px, layout) {
        Ok(d) => d,
        Err(_) => return,
    };

    // Property 2 + 3: whatever came out must decode, at the declared size.
    let img = dds
        .decode_rgba8(SubresourceId::mip_layer(0, 0))
        .expect("encoder emitted a payload its own decoder rejects");
    assert_eq!(img.width, w);
    assert_eq!(img.height, h);
    assert_eq!(img.pixels.len(), need);

    // A short source must be refused, never silently padded.
    if need > 0 {
        assert!(Dds::encode_from_rgba8(&px[..need - 1], layout).is_err());
    }
});
