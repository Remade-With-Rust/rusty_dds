//! The shared robustness driver.
//!
//! Included by BOTH `tests/parser_robustness.rs` (always-on, stable toolchain,
//! deterministic) and every target under `fuzz/fuzz_targets/` (cargo-fuzz,
//! opt-in, nightly). Keeping it in one file is the point: a public entry point
//! added to one harness and not the other is how untested surface appears.
//!
//! Not a test module itself - it lives under `tests/common/` precisely so cargo
//! does not compile it as its own test binary.

#![allow(dead_code)]

use rusty_dds::{Dds, SubresourceId};

/// Every public operation reachable from parsed bytes. Any panic here is a bug.
pub fn exercise(bytes: &[u8]) {
    // Both read paths, including the byte-budgeted one.
    let _ = Dds::read_limited(bytes, 1 << 20);
    let dds = match Dds::read(bytes) {
        Ok(d) => d,
        Err(_) => return,
    };

    // Metadata: must not panic on any header the parser accepted.
    let _ = dds.get_format();
    let _ = dds.get_bits_per_pixel();
    let _ = dds.get_pitch();
    let _ = dds.get_main_texture_size();
    let _ = dds.get_array_stride();
    let _ = dds.decode_content();
    let _ = dds.hdr_decode_content();
    let _ = dds.is_cubemap();
    let _ = dds.cube_count();
    let _ = dds.physical_slice_count();
    let _ = dds.subresource_layer_count();
    let _ = dds.subresource_face_count();
    let _ = dds.get_data(0);

    // Subresource addressing, including out-of-range coordinates, which must
    // fail closed rather than index.
    for mip in [0u32, 1, 7, u32::MAX] {
        for layer in [0u32, 1, 5, u32::MAX] {
            for face in [0u32, 1, 5, 6, u32::MAX] {
                let id = SubresourceId::new(mip, layer, face);
                let _ = dds.mip_dimensions(mip);
                let _ = dds.subresource_range(id);
                let _ = dds.surface(id);
                let _ = dds.upload_plan_compressed(id);
                let _ = dds.upload_plan_decoded_rgba8(id);
                let _ = dds.decode_rgba8(id);
                let _ = dds.decode_rgba_f32(id);
            }
        }
    }

    // Round-trip: anything that parsed must re-serialize without panicking.
    let mut out = Vec::new();
    let _ = dds.write(&mut out);
}
