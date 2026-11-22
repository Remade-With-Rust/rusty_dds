//! `.astc`, KTX 1.0 and KTX 2.0 parsers and everything reachable from a
//! parsed value. The stable half is `tests/astc_containers.rs`; both drive
//! `tests/common/astc_container_driver.rs`.

#![no_main]

use libfuzzer_sys::fuzz_target;

#[path = "../../tests/common/astc_container_driver.rs"]
mod driver;

fuzz_target!(|data: &[u8]| {
    driver::exercise(data);
});
