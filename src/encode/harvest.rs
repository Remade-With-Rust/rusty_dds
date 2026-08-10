//! Observe-only BC4/5 refine harvest (env-gated).
//!
//! Set `RUSTY_DDS_BC45_REFINE_HARVEST=<csv path>` before encode. Each row is a
//! block that reached neighborhood search after LS. Decisions are unchanged.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

static HARVEST: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();

fn file() -> Option<&'static Mutex<std::fs::File>> {
    HARVEST
        .get_or_init(|| {
            let path = std::env::var_os("RUSTY_DDS_BC45_REFINE_HARVEST")?;
            let mut f = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(path)
                .ok()?;
            let _ = writeln!(
                f,
                "signed,n_unique,span,null_err,best_err,gain,axis"
            );
            Some(Mutex::new(f))
        })
        .as_ref()
}

pub fn enabled() -> bool {
    file().is_some()
}

/// Disable LS-skip / simple early-exit / neighborhood search-skip (A/B quality).
pub fn full_refine() -> bool {
    static FULL: OnceLock<bool> = OnceLock::new();
    *FULL.get_or_init(|| {
        matches!(
            std::env::var("RUSTY_DDS_BC45_FULL_REFINE").as_deref(),
            Ok("1") | Ok("true") | Ok("TRUE")
        )
    })
}

pub fn record(
    signed: bool,
    n_unique: usize,
    span: i32,
    null_err: i32,
    best_err: i32,
    axis: bool,
) {
    let Some(f) = file() else {
        return;
    };
    let gain = null_err - best_err;
    if let Ok(mut g) = f.lock() {
        let _ = writeln!(
            g,
            "{},{},{},{},{},{},{}",
            signed as u8, n_unique, span, null_err, best_err, gain, axis as u8
        );
    }
}
