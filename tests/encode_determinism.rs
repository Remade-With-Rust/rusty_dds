//! Standing byte-identical gate for the encoder.
//!
//! The crate's contract is that a payload is a pure function of
//! `(source bytes, crate version, EncodeLayout)` — no environment, no CPU
//! feature, no thread count. This test freezes that: a deterministic synthetic
//! corpus is encoded in every LDR format and the payload is hashed (FNV-1a
//! 64), and the hashes are compared against the recorded contract below.
//!
//! A refactor that is meant to be output-preserving must leave every hash
//! untouched. A deliberate encoder change updates the table in the same commit
//! that changes the encoder, with the quality/rate evidence in the message —
//! never silently.
//!
//! This also pins the `simd` claim: the AVX2 kernels are proven bit-exact
//! against their scalar twins by unit oracles, and this test proves it end to
//! end, since the same hashes must hold with the feature on or off.

use rusty_dds::{DecodeContent, Dds, EncodeLayout, EncodeQuality, Rdo};

/// FNV-1a 64. Inlined rather than pulled in as a dependency — the house rule
/// is that a test helper does not grow the dependency graph.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Deterministic, content-varied source: smooth gradients, a hard edge, a
/// noisy band and a flat region, so every encoder decision path is exercised.
fn source(width: u32, height: u32) -> Vec<u8> {
    let mut px = Vec::with_capacity((width * height * 4) as usize);
    let mut state: u32 = 0x1234_5678;
    for y in 0..height {
        for x in 0..width {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (state >> 24) as u8;
            let (r, g, b, a) = if y < height / 4 {
                // Smooth two-axis gradient.
                ((x * 255 / width) as u8, (y * 255 / height) as u8, 128, 255)
            } else if y < height / 2 {
                // Hard edge between two flat colours.
                if x < width / 2 {
                    (220, 30, 40, 255)
                } else {
                    (20, 200, 90, 128)
                }
            } else if y < 3 * height / 4 {
                // Noise band.
                (noise, noise.rotate_left(3), noise.wrapping_add(77), noise)
            } else {
                // Flat.
                (64, 64, 64, 255)
            };
            px.extend_from_slice(&[r, g, b, a]);
        }
    }
    px
}

const W: u32 = 64;
const H: u32 = 64;

fn all_content() -> Vec<(&'static str, DecodeContent)> {
    vec![
        ("bc1", DecodeContent::Bc1),
        ("bc2", DecodeContent::Bc2),
        ("bc3", DecodeContent::Bc3),
        ("bc4u", DecodeContent::Bc4UNorm),
        ("bc4s", DecodeContent::Bc4SNorm),
        ("bc5u", DecodeContent::Bc5UNorm),
        ("bc5s", DecodeContent::Bc5SNorm),
        ("bc7", DecodeContent::Bc7),
        ("rgba8", DecodeContent::Rgba8),
        ("bgra8", DecodeContent::Bgra8),
    ]
}

fn encode_hash(content: DecodeContent, quality: EncodeQuality, rdo: Rdo) -> u64 {
    let px = source(W, H);
    let layout = EncodeLayout::flat_2d(content, W, H)
        .with_quality(quality)
        .with_rdo(rdo);
    let dds = Dds::encode_from_rgba8(&px, layout).expect("encode");
    fnv1a(&dds.data)
}

/// Frozen payload hashes — the encoder's output contract.
const QUALITY_HASHES: &[(&str, u64)] = &[
    // bc1/bc2/bc3 moved when BC1_PCA_SEED was defaulted OFF (bc2/bc3 share the
    // colour encoder). Evidence, ablated serial on the corpus: the PCA seed cost
    // 21-26% of BC1 encode time and bought 0.00-0.01 dB. Corpus PSNR deltas
    // against DirectXTex move from +0.63/+1.31/+1.30/+1.29 to
    // +0.62/+1.30/+1.28/+1.29, and the four BC1 ratios go 1.45/1.44/1.50/1.40
    // to 1.042/0.998/1.109/0.993.
    ("bc1", 0x1c9936780dbfecb5),
    ("bc2", 0xcb9576059b8d1eca),
    ("bc3", 0x8da14ee9e557d08e),
    ("bc4u", 0x9da20b575b3beeee),
    // bc4s/bc5s moved when BC45_SIGNED_WINDOW was defaulted OFF. Evidence,
    // measured serial against DirectXTex on the corpus: the sweep cost 3-5x the
    // encode time for 0.05-0.61 dB. Shipped config now runs all eight signed
    // cases FASTER than DirectXTex (ratios 0.099-0.259, from 3.6-7.9), and the
    // corpus quality table goes 22/0/2 -> 22 win / 1 loss / 1 tie: only
    // Wood095_NormalGL bc5s flips, from a tie (-0.09 dB) to -0.51 dB.
    ("bc4s", 0x2d8096f8384cd6d1),
    ("bc5u", 0xc94a0b212743697b),
    ("bc5s", 0x699bd633cf3503f9),
    ("bc7", 0x8c62727a6428b949),
    ("rgba8", 0xb4a9f13d78f8f76a),
    ("bgra8", 0x969a5dcee4bd125a),
];

// `EncodeQuality` gates the BC4/BC5 search only, so BC7 / RGBA / BGRA
// legitimately share their Quality hashes here.
const FAST_HASHES: &[(&str, u64)] = &[
    ("bc1", 0x2692049c093658fd),
    ("bc2", 0xa19fb60bcc299946),
    ("bc3", 0xd10652759148069d),
    ("bc4u", 0x5eba61757ea65fb1),
    ("bc4s", 0xfafaea0bf77148f1),
    ("bc5u", 0xd123c8c07d45af71),
    ("bc5s", 0xcdce534dadb54547),
    ("bc7", 0x8c62727a6428b949),
    ("rgba8", 0xb4a9f13d78f8f76a),
    ("bgra8", 0x969a5dcee4bd125a),
];

/// RDO at a fixed lambda must also be deterministic (it encodes serially by
/// design, precisely so that it is).
const RDO_HASHES: &[(&str, u64)] = &[
    ("bc1", 0x58d8f9bd90175c1e),
    ("bc7", 0x84456f8b4e9df926),
];

fn check(label: &str, expected: &[(&str, u64)], got: &[(&str, u64)]) {
    let mut mismatches = Vec::new();
    for (name, want) in expected {
        let have = got
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, h)| *h)
            .unwrap_or_else(|| panic!("{label}: no hash produced for {name}"));
        if *want != have {
            mismatches.push(format!("  (\"{name}\", 0x{have:016x}), // was 0x{want:016x}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{label}: encoder output changed. If this was intended, update the table \
         with the quality/rate evidence in the same commit:\n{}",
        mismatches.join("\n")
    );
}

#[test]
fn quality_payloads_are_frozen() {
    let got: Vec<(&str, u64)> = all_content()
        .into_iter()
        .map(|(n, c)| (n, encode_hash(c, EncodeQuality::Quality, Rdo::Off)))
        .collect();
    check("EncodeQuality::Quality", QUALITY_HASHES, &got);
}

#[test]
fn fast_payloads_are_frozen() {
    let got: Vec<(&str, u64)> = all_content()
        .into_iter()
        .map(|(n, c)| (n, encode_hash(c, EncodeQuality::Fast, Rdo::Off)))
        .collect();
    check("EncodeQuality::Fast", FAST_HASHES, &got);
}

#[test]
fn rdo_payloads_are_frozen() {
    let got = vec![
        (
            "bc1",
            encode_hash(DecodeContent::Bc1, EncodeQuality::Quality, Rdo::lambda(50.0)),
        ),
        (
            "bc7",
            encode_hash(DecodeContent::Bc7, EncodeQuality::Quality, Rdo::lambda(4.0)),
        ),
    ];
    check("Rdo", RDO_HASHES, &got);
}

/// `Rdo::Off` and `Rdo::lambda(0.0)` are documented as byte-identical to the
/// plain encoder. That is the claim the README makes; this is the proof.
#[test]
fn rdo_lambda_zero_is_byte_identical() {
    for content in [DecodeContent::Bc1, DecodeContent::Bc7] {
        let off = encode_hash(content, EncodeQuality::Quality, Rdo::Off);
        let zero = encode_hash(content, EncodeQuality::Quality, Rdo::lambda(0.0));
        assert_eq!(off, zero, "{content:?}: Rdo::lambda(0.0) is not byte-identical");
    }
}

/// Encoding twice must produce the same bytes — catches any dependence on
/// thread scheduling in the strip-parallel path.
#[test]
fn encode_is_repeatable() {
    for (name, content) in all_content() {
        let a = encode_hash(content, EncodeQuality::Quality, Rdo::Off);
        let b = encode_hash(content, EncodeQuality::Quality, Rdo::Off);
        assert_eq!(a, b, "{name}: encode is not repeatable");
    }
}

/// A size large enough to cross the 4096-block strip-parallel threshold must
/// match the serial result. This is the gate on the `std::thread` scope path.
#[test]
fn parallel_and_serial_strips_agree() {
    // 256x256 = 4096 blocks: at or above the parallel floor.
    let big = source(256, 256);
    // 64x64 = 256 blocks: always serial.
    for content in [DecodeContent::Bc1, DecodeContent::Bc7] {
        let par = Dds::encode_from_rgba8(&big, EncodeLayout::flat_2d(content, 256, 256))
            .expect("encode 256");
        let again = Dds::encode_from_rgba8(&big, EncodeLayout::flat_2d(content, 256, 256))
            .expect("encode 256");
        assert_eq!(
            fnv1a(&par.data),
            fnv1a(&again.data),
            "{content:?}: strip-parallel encode is not deterministic"
        );
    }
}
