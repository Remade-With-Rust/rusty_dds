//! Structure-aware robustness harness for the untrusted-input surface.
//!
//! `rusty_dds` parses bytes it did not create — a mod archive, a user upload, a
//! shader-cache blob. Safe Rust buys "no undefined behaviour"; it does not buy
//! "no panic", and a panic in an asset pipeline is still a denial of service.
//! The contract this file enforces on every public entry point that consumes
//! parsed bytes is: **return `Ok` or `Err`, never panic, and never allocate in
//! proportion to a header field rather than to the bytes actually present.**
//!
//! This is the always-on half of the fuzzing story: pure Rust, stable
//! toolchain, no C, deterministic, and it runs under a normal `cargo test`.
//! The unbounded half lives in `fuzz/` (cargo-fuzz, opt-in, excluded from the
//! published package) and calls the same `exercise` driver, so the two halves
//! cannot drift apart.
//!
//! A crash found by either half is added to `tests/fixtures/regressions/` and
//! replayed by `regression_corpus_is_clean` below, so it can never come back.

// One driver, two harnesses: the `fuzz/` targets include this same file, so a
// path exercised here is exercised there and neither can silently narrow.
#[path = "common/driver.rs"]
mod driver;
use driver::exercise;

use rusty_dds::Dds;

/// xorshift64, so any failure is reproducible from the printed seed alone.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}


fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut v = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return v,
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) == Some("dds") {
            if let Ok(b) = std::fs::read(&p) {
                let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
                v.push((name, b));
            }
        }
    }
    v.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(!v.is_empty(), "no .dds fixtures found - harness would be vacuous");
    v
}

/// Bit flips, byte splices and header-field overwrites on a valid file. Most
/// mutations target the 128-byte header, because that is where the parser makes
/// the size decisions everything downstream trusts.
fn mutate(base: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut b = base.to_vec();
    if b.is_empty() {
        return b;
    }
    let ops = 1 + rng.below(4);
    for _ in 0..ops {
        if b.is_empty() {
            break;
        }
        match rng.below(6) {
            0 => {
                // Single bit flip anywhere.
                let i = rng.below(b.len());
                b[i] ^= 1u8 << rng.below(8);
            }
            1 => {
                // Overwrite a header u32 with a hostile value: the sizes that
                // drive every downstream allocation live here.
                let field = rng.below(31) * 4 + 4; // skip the magic
                if field + 4 <= b.len() {
                    let v: u32 = match rng.below(6) {
                        0 => 0,
                        1 => 1,
                        2 => u32::MAX,
                        3 => 0x8000_0000,
                        4 => 0xFFFF,
                        _ => rng.next() as u32,
                    };
                    b[field..field + 4].copy_from_slice(&v.to_le_bytes());
                }
            }
            2 => {
                // Truncate: the classic way to make a header promise bytes
                // that are not there.
                let n = rng.below(b.len());
                b.truncate(n);
            }
            3 => {
                // Extend with junk.
                let n = rng.below(512);
                for _ in 0..n {
                    b.push(rng.next() as u8);
                }
            }
            4 => {
                // Splice a run of bytes.
                let i = rng.below(b.len());
                let n = (1 + rng.below(16)).min(b.len() - i);
                for k in 0..n {
                    b[i + k] = rng.next() as u8;
                }
            }
            _ => {
                // Keep the magic but scramble the rest of the header, so the
                // parser is forced past the cheap early-out.
                // A prior truncate can have left fewer than 4 bytes; the
                // magic-preserving scramble simply has nothing to do then.
                let end = 128.min(b.len());
                for byte in b.iter_mut().take(end).skip(4) {
                    if rng.below(4) == 0 {
                        *byte = rng.next() as u8;
                    }
                }
            }
        }
    }
    b
}

/// Reproduce and dump the exact bytes for one (fixture, seed) pair.
#[test]
#[ignore]
fn dump_repro() {
    let want_file = std::env::var("REPRO_FILE").unwrap_or_default();
    let want_seed: u64 = std::env::var("REPRO_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    for (name, base) in fixtures() {
        if name != want_file {
            continue;
        }
        let mut rng = Rng(want_seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let bytes = mutate(&base, &mut rng);
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/regressions")
            .join(format!("{name}.seed{want_seed}.bin"));
        std::fs::write(&out, &bytes).expect("write repro");
        eprintln!("wrote {} ({} bytes)", out.display(), bytes.len());
        exercise(&bytes);
    }
}

/// Iterations per fixture. The default keeps a normal `cargo test` fast; a
/// deep sweep (CI nightly, or after touching the parser) sets
/// `RUSTY_DDS_ROBUSTNESS_ITERS` higher. Test-only knob — the library itself
/// reads no environment.
fn iters(default: u64) -> u64 {
    std::env::var("RUSTY_DDS_ROBUSTNESS_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn run_without_panicking(bytes: &[u8]) -> bool {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = std::panic::catch_unwind(|| exercise(bytes));
    std::panic::set_hook(hook);
    r.is_ok()
}

#[test]
fn mutated_fixtures_never_panic() {
    let files = fixtures();
    // Deterministic: a failure names the exact (file, seed) that produced it.
    for (name, base) in &files {
        for seed in 0..iters(1_500) {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let bytes = mutate(base, &mut rng);
            assert!(
                run_without_panicking(&bytes),
                "panic on mutated {name} (seed {seed}); reproduce by replaying \
                 that file and seed through `mutate` then `exercise`"
            );
        }
    }
}

#[test]
fn arbitrary_bytes_never_panic() {
    // Pure noise, plus noise that happens to start with a valid magic - the
    // second class is what actually reaches the header parser.
    for seed in 0..iters(20_000) {
        let mut rng = Rng(seed.wrapping_mul(0xD1B5_4A32_D192_ED03) | 1);
        let len = rng.below(400);
        let mut b: Vec<u8> = Vec::with_capacity(len + 4);
        if seed % 2 == 0 {
            b.extend_from_slice(b"DDS ");
        }
        for _ in 0..len {
            b.push(rng.next() as u8);
        }
        assert!(
            run_without_panicking(&b),
            "panic on arbitrary input (seed {seed})"
        );
    }
}

/// `read_limited` must be a hard ceiling: a stream far larger than the budget
/// is rejected, while the same bytes parse fine when the budget allows them.
#[test]
fn read_limited_is_a_hard_ceiling() {
    let files = fixtures();
    let (_, base) = &files[0];
    let mut big = base.clone();
    big.resize(base.len() + 4 * 1024 * 1024, 0xAB);

    let err = Dds::read_limited(&big[..], 1024);
    assert!(
        matches!(err, Err(rusty_dds::Error::SizeLimitExceeded { .. })),
        "over-budget payload was not rejected: {err:?}"
    );

    assert!(Dds::read_limited(&big[..], 8 * 1024 * 1024).is_ok());
    // The unbounded read still accepts it, by design.
    assert!(Dds::read(&big[..]).is_ok());
}

/// Every input that ever crashed the parser lives here forever.
#[test]
fn regression_corpus_is_clean() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/regressions");
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        // No crashes found yet. Not a failure - the directory ships with a
        // README so the next finding has an obvious home.
        Err(_) => return,
    };
    let mut n = 0;
    for e in entries.flatten() {
        let p = e.path();
        if p.is_file() && p.extension().and_then(|s| s.to_str()) != Some("md") {
            let bytes = std::fs::read(&p).expect("read regression input");
            exercise(&bytes);
            n += 1;
        }
    }
    eprintln!("replayed {n} regression inputs");
}
