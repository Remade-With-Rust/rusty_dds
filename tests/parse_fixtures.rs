//! Smoke: every committed fixture parses with rusty_dds and crates.io ddsfile.

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;

const FIXTURES: &[&str] = &[
    "dxt1_64x64.dds",
    "dxt3_32x32.dds",
    "dxt5_64x64.dds",
    "dxt1_64x64_mips.dds",
    "bc1_64x64_dx10.dds",
    "bc2_32x32_dx10.dds",
    "bc3_64x64_dx10.dds",
    "bc4_32x32_dx10.dds",
    "bc5_32x32_dx10.dds",
    "bc7_32x32_dx10.dds",
    "rgba8_64x64.dds",
    "bgra8_32x32.dds",
    "rgba8_256x256_mips.dds",
    "rgba8_32x32_array3.dds",
    "bc1_32x32_cube.dds",
    "rgba8_16x16x4_vol.dds",
    "bc1_16x16x4_vol.dds",
];

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

#[test]
fn all_fixtures_parse_with_rusty_dds_and_ddsfile() {
    let dir = fixtures_dir();
    for name in FIXTURES {
        let bytes = fs::read(dir.join(name)).unwrap_or_else(|e| {
            panic!("missing {name}: {e}; run cargo run --example gen_fixtures");
        });

        let local = rusty_dds::Dds::read(Cursor::new(bytes.as_slice()))
            .unwrap_or_else(|e| panic!("rusty_dds failed on {name}: {e}"));
        let upstream = ddsfile::Dds::read(Cursor::new(bytes.as_slice()))
            .unwrap_or_else(|e| panic!("ddsfile failed on {name}: {e}"));

        assert_eq!(
            local.data.len(),
            upstream.data.len(),
            "{name}: data length mismatch"
        );
        assert_eq!(local.header.width, upstream.header.width, "{name}: width");
        assert_eq!(local.header.height, upstream.header.height, "{name}: height");
    }
}
