//! Container parse + every operation reachable from a parsed `Dds`.
//!
//! The interesting inputs are headers whose *declared* geometry disagrees with
//! the payload actually present — that mismatch is what every size computation
//! downstream trusts.

#![no_main]

use libfuzzer_sys::fuzz_target;

#[path = "../../tests/common/driver.rs"]
mod driver;

fuzz_target!(|data: &[u8]| {
    driver::exercise(data);
});
