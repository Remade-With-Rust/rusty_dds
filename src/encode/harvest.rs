//! Observe-only BC4/5 refine harvest — **development scaffolding**.
//!
//! Compiled out entirely unless the non-default `tuning` feature is on, so a
//! shipped build has no environment reads, no file handles and no branches on
//! this path. With the feature on, set `RUSTY_DDS_BC45_REFINE_HARVEST=<csv path>`
//! before encode; each row is a block that reached neighborhood search after LS.
//! Decisions are unchanged either way — this only observes.

#[cfg(not(feature = "tuning"))]
mod imp {
    #[inline(always)]
    pub fn enabled() -> bool {
        false
    }

    /// Disable LS-skip / simple early-exit / neighborhood search-skip (A/B quality).
    #[inline(always)]
    pub fn full_refine() -> bool {
        false
    }

    #[inline(always)]
    pub fn record(
        _signed: bool,
        _n_unique: usize,
        _span: i32,
        _null_err: i32,
        _best_err: i32,
        _axis: bool,
    ) {
    }
}

#[cfg(feature = "tuning")]
mod imp {
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
                let _ = writeln!(f, "signed,n_unique,span,null_err,best_err,gain,axis");
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
}

pub use imp::*;
