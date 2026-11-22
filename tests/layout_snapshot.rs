//! Byte-identical gate for the DDS layout arithmetic (pitch, pitch height,
//! texture size, array stride and every subresource range).
//!
//! The snapshot `tests/fixtures/layout_snapshot.txt` was recorded with the
//! layout code as it was before block-dimension-aware layout (ASTC) was added,
//! over every committed fixture plus a matrix of synthesized headers (every
//! DXGI format x odd and power-of-two sizes x mips, arrays, cubes, volumes).
//! Any change to an existing format's layout fails this test.
//!
//! Regenerate only on purpose: `UPDATE_LAYOUT_SNAPSHOT=1 cargo test --test layout_snapshot`.

use rusty_dds::{
    AlphaMode, D3D10ResourceDimension, Dds, DdsView, DxgiFormat, NewDxgiParams, SubresourceId,
};
use std::fmt::Write as _;

fn describe<D: AsRef<[u8]>>(d: &rusty_dds::DdsBase<D>, out: &mut String) {
    let _ = write!(
        out,
        " pitch={:?} ph={} main={:?} stride={:?} levels={} layers={} cube={}",
        d.get_pitch(),
        d.get_pitch_height(),
        d.get_main_texture_size(),
        d.get_array_stride().ok(),
        d.get_num_mipmap_levels(),
        d.subresource_layer_count(),
        d.is_cubemap()
    );
    let faces = if d.is_cubemap() { 6 } else { 1 };
    let levels = d.get_num_mipmap_levels().min(16);
    for layer in 0..d.subresource_layer_count().min(8) {
        for face in 0..faces {
            for mip in 0..levels {
                match d.subresource_range(SubresourceId::new(mip, layer, face)) {
                    Ok(r) => {
                        let _ = write!(out, " {}:{}", r.start, r.end);
                    }
                    Err(e) => {
                        let _ = write!(out, " err({e:?})");
                    }
                }
            }
        }
    }
    out.push('\n');
}

fn snapshot() -> String {
    let mut out = String::new();
    // Committed fixtures.
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("dds"))
        .collect();
    files.sort();
    for p in files {
        let bytes = std::fs::read(&p).unwrap();
        let _ = write!(out, "file {}", p.file_name().unwrap().to_string_lossy());
        match DdsView::parse(&bytes) {
            Ok(d) => describe(&d, &mut out),
            Err(e) => {
                let _ = writeln!(out, " parse-err({e:?})");
            }
        }
    }
    // Synthesized headers.
    let sizes: &[(u32, u32, Option<u32>)] = &[
        (1, 1, None),
        (3, 5, None),
        (4, 4, None),
        (17, 9, None),
        (64, 64, None),
        (100, 100, None),
        (255, 1, None),
        (1, 300, None),
        (16, 16, Some(4)),
        (13, 7, Some(3)),
    ];
    for v in 0..=132u32 {
        let Some(format) = <DxgiFormat as num_traits::FromPrimitive>::from_u32(v) else {
            continue;
        };
        for &(w, h, depth) in sizes {
            for (mips, layers, cube) in [
                (None, None, false),
                (Some(0), None, false),
                (Some(4), Some(3), false),
                (Some(3), None, true),
            ] {
                if cube && (depth.is_some() || w != h) {
                    continue;
                }
                let params = NewDxgiParams {
                    height: h,
                    width: w,
                    depth,
                    format,
                    mipmap_levels: mips,
                    array_layers: layers,
                    caps2: None,
                    is_cubemap: cube,
                    resource_dimension: if depth.is_some() {
                        D3D10ResourceDimension::Texture3D
                    } else {
                        D3D10ResourceDimension::Texture2D
                    },
                    alpha_mode: AlphaMode::Straight,
                };
                let _ = write!(
                    out,
                    "dxgi {v} {w}x{h}x{depth:?} mips={mips:?} layers={layers:?} cube={cube}"
                );
                match Dds::new_dxgi(params) {
                    Ok(d) => {
                        let _ = write!(out, " data={}", d.data.len());
                        describe(&d, &mut out);
                    }
                    Err(e) => {
                        let _ = writeln!(out, " new-err({e:?})");
                    }
                }
            }
        }
    }
    out
}

fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// The snapshot is stored as one FNV-1a 64 hash per line (the text itself is
/// ~850 KB, too large for a packaged fixture); on a mismatch the test prints
/// the line as the current code computes it.
#[test]
fn layout_is_unchanged() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/layout_snapshot.txt"
    );
    let now = snapshot();
    let hashes: String = now
        .lines()
        .map(|l| {
            format!(
                "{:016x}
",
                fnv1a(l)
            )
        })
        .collect();
    if std::env::var_os("UPDATE_LAYOUT_SNAPSHOT").is_some() {
        std::fs::write(path, &hashes).unwrap();
        return;
    }
    let want = std::fs::read_to_string(path).expect("layout snapshot");
    let want: Vec<&str> = want.lines().collect();
    let lines: Vec<&str> = now.lines().collect();
    assert_eq!(want.len(), lines.len(), "snapshot line count");
    for (i, (w, l)) in want.iter().zip(&lines).enumerate() {
        assert_eq!(
            *w,
            format!("{:016x}", fnv1a(l)),
            "layout snapshot line {} differs; now: {l}",
            i + 1
        );
    }
}
