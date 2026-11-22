//! ASTC containers: round trips, and the deterministic half of the
//! `astc_container` fuzz target (truncations, bit flips and arbitrary bytes
//! around valid `.astc`, KTX 1.0 and KTX 2.0 files never panic).

#![cfg(all(feature = "astc", feature = "decode"))]

#[path = "common/astc_container_driver.rs"]
mod driver;

use rusty_dds::astc::{self, AstcFile, AstcFootprint, AstcProfile, KtxView};

fn payload(n: usize, seed: u8) -> Vec<u8> {
    // Void-extent blocks with varying colours: legal for every footprint.
    let mut v = Vec::with_capacity(n);
    for i in 0..n / 16 {
        let c = (i as u8).wrapping_mul(seed);
        v.extend_from_slice(&[0xFC, 0xFD, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0, c, 0, c ^ 0x55, 0, 0x80, 0xFF, 0xFF]);
    }
    v
}

fn samples() -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for &(fp, w, h, d) in &[
        (AstcFootprint::B6X5, 13, 7, 1),
        (AstcFootprint::B4X4, 1, 1, 1),
        (AstcFootprint::B4X4X4, 5, 6, 7),
    ] {
        let p = payload(astc::payload_len(w, h, d, fp).unwrap(), 3);
        let mut f = Vec::new();
        AstcFile::write(&mut f, fp, w, h, d, &p).unwrap();
        out.push(f);
        let levels: Vec<Vec<u8>> = (0..3)
            .map(|l| {
                let m = |v: u32| (v >> l).max(1);
                payload(astc::payload_len(m(w), m(h), m(d), fp).unwrap(), 7 + l as u8)
            })
            .collect();
        let refs: Vec<&[u8]> = levels.iter().map(|v| &v[..]).collect();
        let mut k = Vec::new();
        KtxView::write(&mut k, fp, w, h, d, false, &refs).unwrap();
        out.push(k);
    }
    out
}

#[test]
fn round_trips() {
    let fp = AstcFootprint::B10X8;
    let p = payload(astc::payload_len(33, 17, 1, fp).unwrap(), 5);
    let mut f = Vec::new();
    AstcFile::write(&mut f, fp, 33, 17, 1, &p).unwrap();
    let a = AstcFile::parse(&f).unwrap();
    assert_eq!((a.footprint, a.width, a.height, a.depth, a.data), (fp, 33, 17, 1, &p[..]));
    assert_eq!(a.surface().default_profile(), AstcProfile::LdrSrgb);

    let mut k = Vec::new();
    KtxView::write(&mut k, fp, 33, 17, 1, true, &[&p]).unwrap();
    let v = KtxView::parse(&k).unwrap();
    assert!(v.srgb);
    assert_eq!(v.astc_surface(0, 0, 0).unwrap().data, &p[..]);
    assert_eq!(v.astc_surface(0, 0, 0).unwrap().default_profile(), AstcProfile::LdrSrgb);
    assert!(AstcFile::parse(&f[..f.len() - 1]).is_err());
    assert!(KtxView::parse(&k[..k.len() - 1]).is_err());
}

/// Every truncation, every single-bit flip in the first 128 bytes, and
/// seeded arbitrary bytes behind each container's magic.
#[test]
fn malformed_containers_never_panic() {
    for s in samples() {
        for n in 0..s.len() {
            driver::exercise(&s[..n]);
        }
        for bit in 0..(s.len().min(128) * 8) {
            let mut m = s.clone();
            m[bit / 8] ^= 1 << (bit % 8);
            driver::exercise(&m);
        }
    }
    let mut x: u64 = 0x5EED;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let magics: [&[u8]; 3] = [
        &[0x13, 0xAB, 0xA1, 0x5C],
        &[0xAB, 0x4B, 0x54, 0x58, 0x20, 0x31, 0x31, 0xBB, 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3, 4],
        &[0xAB, 0x4B, 0x54, 0x58, 0x20, 0x32, 0x30, 0xBB, 0x0D, 0x0A, 0x1A, 0x0A],
    ];
    for i in 0..3000 {
        let len = (next() % 400) as usize;
        let mut v: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        let m = magics[i % 3];
        if v.len() >= m.len() {
            v[..m.len()].copy_from_slice(m);
        }
        driver::exercise(&v);
    }
}
