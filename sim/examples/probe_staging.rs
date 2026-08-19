//! What is the harness's own "staging copy" actually made of?
//!
//! `NullRenderer::upload` copies each row and folds it into an FNV-1a hash. The
//! hash is the parity gate — it is what proves both stacks handed the GPU the
//! same bytes — but FNV is byte-at-a-time, and the run hashes 822 MiB. If the
//! instrument is a large share of what it measures, every board's absolute
//! numbers are inflated and the relative differences are diluted.

use std::time::Instant;
use rusty_dds_sim::hash::{bulk_hash, fnv1a_seed, FNV_OFFSET};

fn main() {
    // Pin before measuring. This box runs 70%+ busy from other processes, and
    // an unpinned probe competes with them for cores — which is why the same
    // change has read +34.5% and +3.5% on consecutive runs. Mask 0x3c is four
    // physical cores; HIGH_PRIORITY_CLASS keeps the scheduler off us.
    let pinned = rusty_dds_sim::os::pin_process(0x3c, true);
    eprintln!("[probe] pinned={pinned} mask=0x3c high_priority=true");

    const N: usize = 1_397_760; // one 1024^2 BC7 payload
    let src = vec![0xA5u8; N];
    let mut dst = Vec::with_capacity(N);
    const ITERS: u32 = 200;
    let row = 4096usize;

    // 1. copy only, row at a time (what a real staging upload does)
    let t = Instant::now();
    for _ in 0..ITERS {
        dst.clear();
        for c in src.chunks(row) {
            dst.extend_from_slice(c);
        }
        std::hint::black_box(dst.len());
    }
    let copy_ms = t.elapsed().as_secs_f64() * 1e3 / ITERS as f64;

    // 2. hash only
    let t = Instant::now();
    for _ in 0..ITERS {
        let mut h = FNV_OFFSET;
        for c in src.chunks(row) {
            h = fnv1a_seed(h, c);
        }
        std::hint::black_box(h);
    }
    let hash_ms = t.elapsed().as_secs_f64() * 1e3 / ITERS as f64;

    // 3. both, as the renderer does
    let t = Instant::now();
    for _ in 0..ITERS {
        dst.clear();
        let mut h = FNV_OFFSET;
        for c in src.chunks(row) {
            dst.extend_from_slice(c);
            h = fnv1a_seed(h, c);
        }
        std::hint::black_box((dst.len(), h));
    }
    let both_ms = t.elapsed().as_secs_f64() * 1e3 / ITERS as f64;

    println!("one {:.2} MiB subresource, {}-byte rows", N as f64 / (1 << 20) as f64, row);
    println!("  copy only   {copy_ms:.4} ms   ({:.1} GB/s)", N as f64 / (copy_ms / 1e3) / 1e9);
    println!("  hash only   {hash_ms:.4} ms   ({:.1} GB/s)", N as f64 / (hash_ms / 1e3) / 1e9);
    println!("  copy + hash {both_ms:.4} ms");
    // 4. the pipelined replacement
    let t = Instant::now();
    for _ in 0..ITERS {
        let mut h = FNV_OFFSET;
        for c in src.chunks(row) {
            h = bulk_hash(h, c);
        }
        std::hint::black_box(h);
    }
    let bulk_ms = t.elapsed().as_secs_f64() * 1e3 / ITERS as f64;
    println!("  bulk_hash   {bulk_ms:.4} ms   ({:.1} GB/s)", N as f64 / (bulk_ms / 1e3) / 1e9);
    println!("  -> the old parity hash was {:.0}% of the staging cost", hash_ms / both_ms * 100.0);
    println!("  -> bulk_hash is {:.1}x faster", hash_ms / bulk_ms.max(1e-9));
    println!("\nA traverse/high run stages 822 MiB, so the hash alone costs about");
    println!(
        "{:.0} ms of every board's `Staging copy` row; with bulk_hash, {:.0} ms.",
        822.0 * (1 << 20) as f64 / N as f64 * hash_ms,
        822.0 * (1 << 20) as f64 / N as f64 * bulk_ms
    );
}
