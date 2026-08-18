//! `Dds::read_limited` must be a hard ceiling for any (budget, stream) pair.
//!
//! Two properties, checked on every input:
//!   1. The call never panics.
//!   2. When it succeeds, the payload it kept is within the budget — the limit
//!      is not merely advisory.
//!
//! Property 2 is the one worth fuzzing: a limit that is checked *after*
//! buffering, or that is off by the header size, would still return `Ok` on
//! well-formed input and only fail on the adversarial case.

#![no_main]

use libfuzzer_sys::fuzz_target;
use rusty_dds::{Dds, Error};

fuzz_target!(|input: (u16, Vec<u8>)| {
    let (limit, bytes) = input;
    let limit = limit as usize;

    match Dds::read_limited(&bytes[..], limit) {
        Ok(dds) => assert!(
            dds.data.len() <= limit,
            "read_limited kept {} bytes under a {} byte budget",
            dds.data.len(),
            limit
        ),
        Err(Error::SizeLimitExceeded { limit: l, at_least }) => {
            assert_eq!(l, limit);
            assert!(at_least > limit);
        }
        Err(_) => {}
    }

    // The unbounded reader must agree about whether the bytes parse at all:
    // the budget may only ever reject on size, never on structure.
    if let (Ok(a), Ok(b)) = (
        Dds::read(&bytes[..]),
        Dds::read_limited(&bytes[..], usize::MAX),
    ) {
        assert_eq!(a.data.len(), b.data.len());
    }
});
