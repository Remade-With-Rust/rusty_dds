//! Phase 1: subresource ranges and surface views — fail closed, no silent OOB.

use rusty_dds::*;
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn load(name: &str) -> Dds {
    let path = fixtures_dir().join(name);
    let bytes = fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "missing {}: {e}; run cargo run --example gen_fixtures",
            path.display()
        );
    });
    Dds::read(Cursor::new(bytes)).unwrap_or_else(|e| panic!("parse {name}: {e}"))
}

#[test]
fn rgba8_mip0_covers_entire_single_layer() {
    let dds = load("rgba8_64x64.dds");
    let id = SubresourceId::mip_layer(0, 0);
    let range = dds.subresource_range(id).expect("range");
    assert_eq!(range.start, 0);
    assert_eq!(range.end, dds.data.len());

    let surf = dds.surface(id).expect("surface");
    assert_eq!((surf.width, surf.height, surf.depth), (64, 64, 1));
    assert_eq!(surf.data.len(), 64 * 64 * 4);
}

#[test]
fn dxt1_mips_first_and_last() {
    let dds = load("dxt1_64x64_mips.dds");
    assert_eq!(dds.get_num_mipmap_levels(), 7);

    let mip0 = dds
        .surface(SubresourceId::mip_layer(0, 0))
        .expect("mip0");
    assert_eq!((mip0.width, mip0.height), (64, 64));
    assert_eq!(mip0.data.len(), 2048); // BC1 64x64

    let last = dds.get_num_mipmap_levels() - 1;
    let tip = dds
        .surface(SubresourceId::mip_layer(last, 0))
        .expect("last mip");
    assert_eq!((tip.width, tip.height), (1, 1));
    // Block formats clamp to one 8-byte BC1 block at the tip.
    assert_eq!(tip.data.len(), 8);

    let r0 = dds
        .subresource_range(SubresourceId::mip_layer(0, 0))
        .unwrap();
    let r_last = dds
        .subresource_range(SubresourceId::mip_layer(last, 0))
        .unwrap();
    assert!(r0.end <= r_last.start || r0.start < r_last.start);
    assert!(r_last.end <= dds.data.len());
    assert_eq!(r0.start, 0);
}

#[test]
fn array_layers_are_disjoint_and_ordered() {
    let dds = load("rgba8_32x32_array3.dds");
    assert!(!dds.is_cubemap());
    assert_eq!(dds.subresource_layer_count(), 3);
    assert_eq!(dds.physical_slice_count(), 3);

    let mut ranges = Vec::new();
    for layer in 0..3 {
        let id = SubresourceId::mip_layer(0, layer);
        let surf = dds.surface(id).expect("layer surface");
        assert_eq!((surf.width, surf.height), (32, 32));
        assert_eq!(surf.data.len(), 32 * 32 * 4);
        ranges.push(dds.subresource_range(id).unwrap());
    }

    assert_eq!(ranges[0].start, 0);
    assert_eq!(ranges[0].end, ranges[1].start);
    assert_eq!(ranges[1].end, ranges[2].start);
    assert_eq!(ranges[2].end, dds.data.len());

    // Pattern bytes differ across layers at the same local offset.
    let a = dds.surface(SubresourceId::mip_layer(0, 0)).unwrap().data[0];
    let b = dds.surface(SubresourceId::mip_layer(0, 1)).unwrap().data[0];
    assert_ne!(a, b);
}

#[test]
fn cubemap_faces_cover_full_payload() {
    let dds = load("bc1_32x32_cube.dds");
    assert!(dds.is_cubemap());
    assert_eq!(dds.cube_count(), 1);
    assert_eq!(dds.subresource_layer_count(), 1);
    assert_eq!(dds.subresource_face_count(), 6);
    assert_eq!(dds.physical_slice_count(), 6);
    assert_eq!(dds.get_num_mipmap_levels(), 2);

    let mut covered = 0_usize;
    for face in CubemapFace::ALL {
        for mip in 0..2 {
            let id = SubresourceId::cubemap(mip, 0, face);
            let surf = dds.surface(id).expect("cube surface");
            let expected_dim = (32_u32 >> mip).max(1);
            assert_eq!(surf.width, expected_dim);
            assert_eq!(surf.height, expected_dim);
            covered += surf.data.len();
        }
    }
    assert_eq!(covered, dds.data.len());

    let face0 = dds
        .subresource_range(SubresourceId::cubemap(0, 0, CubemapFace::PositiveX))
        .unwrap();
    let face1 = dds
        .subresource_range(SubresourceId::cubemap(0, 0, CubemapFace::NegativeX))
        .unwrap();
    assert_eq!(face0.start, 0);
    assert!(face1.start >= face0.end);
}

#[test]
fn oob_mip_layer_and_face_fail_closed() {
    let dds = load("rgba8_64x64.dds");

    assert!(matches!(
        dds.surface(SubresourceId::mip_layer(1, 0)),
        Err(Error::OutOfBounds)
    ));
    assert!(matches!(
        dds.surface(SubresourceId::mip_layer(0, 1)),
        Err(Error::OutOfBounds)
    ));
    assert!(matches!(
        dds.surface(SubresourceId::new(0, 0, 1)),
        Err(Error::OutOfBounds)
    ));

    let cube = load("bc1_32x32_cube.dds");
    assert!(matches!(
        cube.surface(SubresourceId::new(0, 0, 6)),
        Err(Error::OutOfBounds)
    ));
    assert!(matches!(
        cube.surface(SubresourceId::cubemap(0, 1, CubemapFace::PositiveX)),
        Err(Error::OutOfBounds)
    ));
}

#[test]
fn truncated_payload_errors() {
    let mut dds = load("rgba8_64x64.dds");
    dds.data.truncate(dds.data.len() / 2);
    assert!(matches!(
        dds.surface(SubresourceId::mip_layer(0, 0)),
        Err(Error::TruncatedData)
    ));
}

#[test]
fn surface_mut_writes_only_that_subresource() {
    let mut dds = load("rgba8_32x32_array3.dds");
    {
        let surf = dds
            .surface_mut(SubresourceId::mip_layer(0, 1))
            .expect("mut");
        surf.data.fill(0xAB);
    }
    let layer0 = dds.surface(SubresourceId::mip_layer(0, 0)).unwrap();
    let layer1 = dds.surface(SubresourceId::mip_layer(0, 1)).unwrap();
    let layer2 = dds.surface(SubresourceId::mip_layer(0, 2)).unwrap();
    assert_ne!(layer0.data[0], 0xAB);
    assert!(layer1.data.iter().all(|&b| b == 0xAB));
    assert_ne!(layer2.data[0], 0xAB);
}

#[test]
fn rgba_mip_chain_dimensions() {
    let dds = load("rgba8_256x256_mips.dds");
    assert_eq!(dds.get_num_mipmap_levels(), 9);
    for mip in 0..9 {
        let (w, h, d) = dds.mip_dimensions(mip).unwrap();
        assert_eq!(w, (256_u32 >> mip).max(1));
        assert_eq!(h, (256_u32 >> mip).max(1));
        assert_eq!(d, 1);
        let surf = dds.surface(SubresourceId::mip_layer(mip, 0)).unwrap();
        assert_eq!(surf.data.len(), (w * h * 4) as usize);
    }
}
