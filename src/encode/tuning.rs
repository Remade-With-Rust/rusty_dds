//! Encoder tuning constants.
//!
//! Every value here was chosen by the 2026-08 encoder campaign and is
//! **frozen** in a normal build: a shipped cook must not change its output
//! because of a stray environment variable in someone's shell. Determinism is
//! part of the contract — the same source bytes and the same crate version
//! produce the same payload on every machine.
//!
//! The non-default `tuning` feature re-opens each constant to a `RUSTY_DDS_*`
//! environment override (read once per process) so the campaign harnesses can
//! still sweep them. It is a development feature; it is never on by default and
//! never on in a published build.

/// Mode-1 trial gate: trial BC7 mode 1 when the mode-6 residual exceeds this.
/// `0` = trial whenever the block is imperfect.
pub(crate) const BC7_M1_MIN_ERR: i64 = 0;

/// BC4/BC5 **unsigned** windowed endpoint sweep. Off: the unsigned path did not
/// pay for the window in the corpus sweep.
pub(crate) const BC45_UNSIGNED_WINDOW: bool = false;

/// BC4/BC5 **signed** windowed endpoint sweep. Off, for the same reason its
/// unsigned twin is off — and the signed case is the more expensive of the two.
///
/// Measured serial against DirectXTex on the corpus, this sweep cost **3-5x the
/// encode time for 0.05-0.61 dB**, on maps we already led. Turning it off takes
/// the seven signed cases from ratios of 3.6-7.9 down to 0.8-1.9, and six of the
/// eight still hold higher PSNR than DirectXTex without it.
///
/// The exception, recorded so it is not rediscovered: `Wood095_NormalGL` bc5s
/// goes from a tie (-0.09 dB, inside the 0.25 dB deadband) to a loss (-0.51 dB).
/// That is the whole price. Opt back in with `RUSTY_DDS_BC45S_WINDOW=1` under
/// the `tuning` feature.
pub(crate) const BC45_SIGNED_WINDOW: bool = false;

/// BC2/BC3 alpha endpoint selection search (worth +1.8..+3.2 dB on the CryTIF set).
pub(crate) const ALPHA_SELECT: bool = true;

/// BC1 565-lattice contract-refine rounds.
pub(crate) const BC1_LATTICE_ROUNDS: u32 = 3;

/// BC1 lattice gate: refine when the residual exceeds this. `0` = whenever non-zero.
pub(crate) const BC1_LATTICE_MIN_ERR: i32 = 0;

#[cfg(not(feature = "tuning"))]
mod imp {
    use super::*;

    #[inline(always)]
    pub(crate) fn bc7_m1_min_err() -> i64 {
        BC7_M1_MIN_ERR
    }
    #[inline(always)]
    pub(crate) fn unsigned_window_enabled() -> bool {
        BC45_UNSIGNED_WINDOW
    }
    #[inline(always)]
    pub(crate) fn signed_window_enabled() -> bool {
        BC45_SIGNED_WINDOW
    }
    #[inline(always)]
    pub(crate) fn alpha_sel_enabled() -> bool {
        ALPHA_SELECT
    }
    #[inline(always)]
    pub(crate) fn bc1_lattice_rounds() -> u32 {
        BC1_LATTICE_ROUNDS
    }
    #[inline(always)]
    pub(crate) fn bc1_lattice_min_err() -> i32 {
        BC1_LATTICE_MIN_ERR
    }
}

#[cfg(feature = "tuning")]
mod imp {
    use super::*;
    use std::sync::OnceLock;

    /// Parse a `RUSTY_DDS_*` override, falling back to the frozen constant.
    fn env_or<T: std::str::FromStr>(key: &str, default: T) -> T {
        std::env::var(key)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(default)
    }

    pub(crate) fn bc7_m1_min_err() -> i64 {
        static T: OnceLock<i64> = OnceLock::new();
        *T.get_or_init(|| env_or("RUSTY_DDS_BC7_M1_T", BC7_M1_MIN_ERR))
    }

    pub(crate) fn unsigned_window_enabled() -> bool {
        static E: OnceLock<bool> = OnceLock::new();
        *E.get_or_init(|| {
            std::env::var("RUSTY_DDS_BC45U_WINDOW")
                .map(|v| v == "1")
                .unwrap_or(BC45_UNSIGNED_WINDOW)
        })
    }

    pub(crate) fn signed_window_enabled() -> bool {
        static E: OnceLock<bool> = OnceLock::new();
        *E.get_or_init(|| {
            std::env::var("RUSTY_DDS_BC45S_WINDOW")
                .map(|v| v == "1")
                .unwrap_or(BC45_SIGNED_WINDOW)
        })
    }

    pub(crate) fn alpha_sel_enabled() -> bool {
        static E: OnceLock<bool> = OnceLock::new();
        *E.get_or_init(|| {
            std::env::var("RUSTY_DDS_ALPHA_SEL")
                .map(|v| v != "0")
                .unwrap_or(ALPHA_SELECT)
        })
    }

    pub(crate) fn bc1_lattice_rounds() -> u32 {
        static R: OnceLock<u32> = OnceLock::new();
        *R.get_or_init(|| env_or("RUSTY_DDS_BC1_LATTICE_ROUNDS", BC1_LATTICE_ROUNDS))
    }

    pub(crate) fn bc1_lattice_min_err() -> i32 {
        static T: OnceLock<i32> = OnceLock::new();
        *T.get_or_init(|| env_or("RUSTY_DDS_BC1_LATTICE_T", BC1_LATTICE_MIN_ERR))
    }
}

pub(crate) use imp::*;
