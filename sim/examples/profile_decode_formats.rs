//! Decode throughput and allocation count, per BCn format.
//!
//! `decode_bc7` has a parallel path; the other formats appear not to. A real
//! transcode workload is mostly BC1 albedo and BC5 normals, so if those are
//! single-threaded there are cores sitting idle on the profile where rusty_dds
//! is already ahead.
//!
//! The allocation count is the tell: a parallel decode spawns threads, and each
//! spawn allocates. 1 allocation means serial.
//!
//! Run: `cargo run --release --example profile_decode_formats -- pack/high192`

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

use rusty_dds::{DdsView, SubresourceId};

static ALLOCS: AtomicU64 = AtomicU64::new(0);

struct Counting;

// SAFETY: forwards to `System` with the layout it was given; the counter is a
// plain atomic and never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "pack/high192".into());
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {dir}: {e}"))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|e| e == "dds").unwrap_or(false))
        .collect();
    files.sort();

    // One file per distinct format in the pack.
    let mut seen: Vec<String> = Vec::new();
    let mut picks: Vec<(String, std::path::PathBuf)> = Vec::new();
    for f in files {
        let name = f.file_name().unwrap_or_default().to_string_lossy().to_string();
        let fmt = name
            .rsplit_once('_')
            .map(|(_, t)| t.trim_end_matches(".dds").to_string())
            .unwrap_or_default();
        if !fmt.is_empty() && !seen.contains(&fmt) {
            seen.push(fmt.clone());
            picks.push((fmt, f));
        }
    }

    let picks_first = picks[0].1.clone();
    println!(
        "{:<8} {:>10} {:>8} {:>10} {:>8}  {}",
        "format", "Mpx/s", "allocs", "ms/call", "blocks", "path"
    );

    for (fmt, path) in picks {
        let bytes = std::fs::read(&path).expect("read");
        let view = DdsView::parse(&bytes).expect("parse");
        let id = SubresourceId::mip_layer(0, 0);
        let Ok(first) = view.decode_rgba8(id) else {
            println!("{fmt:<8} (decode unsupported)");
            continue;
        };
        let px = first.pixels.len() / 4;
        let blocks = (first.width as usize).div_ceil(4) * (first.height as usize).div_ceil(4);

        const ITERS: u32 = 20;
        // Warm the output allocation path so the first call is not the sample.
        std::hint::black_box(view.decode_rgba8(id).expect("warm").pixels.len());

        let a0 = ALLOCS.load(Relaxed);
        let t = Instant::now();
        for _ in 0..ITERS {
            std::hint::black_box(view.decode_rgba8(id).expect("decode").pixels.len());
        }
        let ms = t.elapsed().as_secs_f64() * 1e3 / ITERS as f64;
        let allocs = (ALLOCS.load(Relaxed) - a0) as f64 / ITERS as f64;

        println!(
            "{:<8} {:>10.1} {:>8.1} {:>10.3} {:>8}  {}",
            fmt,
            px as f64 / (ms / 1e3) / 1e6,
            allocs,
            ms,
            blocks,
            if allocs > 2.0 { "parallel" } else { "SERIAL" }
        );
    }

    // ---- the two new paths, on the largest surface in the pack -------------
    {
        let bytes = std::fs::read(&picks_first).expect("read");
        let view = DdsView::parse(&bytes).expect("parse");
        let id = SubresourceId::mip_layer(0, 0);
        let whole = view.decode_rgba8(id).expect("decode");
        let (w, h) = (whole.width as usize, whole.height as usize);
        const N: u32 = 20;

        let t = Instant::now();
        for _ in 0..N {
            std::hint::black_box(view.decode_rgba8(id).expect("d").pixels.len());
        }
        let alloc_ms = t.elapsed().as_secs_f64() * 1e3 / N as f64;

        let mut buf = Vec::new();
        view.decode_rgba8_into(id, &mut buf).expect("warm");
        let a0 = ALLOCS.load(Relaxed);
        let t = Instant::now();
        for _ in 0..N {
            std::hint::black_box(view.decode_rgba8_into(id, &mut buf).expect("d"));
        }
        let into_ms = t.elapsed().as_secs_f64() * 1e3 / N as f64;
        let into_allocs = (ALLOCS.load(Relaxed) - a0) as f64 / N as f64;

        // Caller-driven parallelism: split the surface across the caller's own
        // threads, with the library spawning none.
        let rows = view.block_rows(id).expect("rows");
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        let chunk = rows.div_ceil(cores as u32).max(1);
        let mut dst = vec![0u8; w * h * 4];
        let t = Instant::now();
        for _ in 0..N {
            let mut rest: &mut [u8] = &mut dst;
            let mut bands: Vec<(std::ops::Range<u32>, &mut [u8])> = Vec::new();
            let mut r0 = 0u32;
            while r0 < rows {
                let r1 = (r0 + chunk).min(rows);
                let px_rows = ((r1 - r0) * 4).min(h as u32 - r0 * 4) as usize;
                let (band, tail) = rest.split_at_mut(px_rows * w * 4);
                rest = tail;
                bands.push((r0..r1, band));
                r0 = r1;
            }
            let v = &view;
            std::thread::scope(|sc| {
                for (range, band) in bands {
                    sc.spawn(move || {
                        v.decode_block_rows_into(id, range, band).expect("band");
                    });
                }
            });
        }
        let rows_ms = t.elapsed().as_secs_f64() * 1e3 / N as f64;

        println!();
        println!("{}x{} {}", w, h, picks_first.file_name().unwrap().to_string_lossy());
        println!("  decode_rgba8            {alloc_ms:.3} ms   (allocates a fresh output)");
        println!("  decode_rgba8_into       {into_ms:.3} ms   {into_allocs:.2} allocs/call");
        println!("  decode_block_rows_into  {rows_ms:.3} ms   caller-driven across {cores} threads");
        println!(
            "  -> into is {:.2}x, caller-parallel is {:.2}x",
            alloc_ms / into_ms.max(1e-9),
            alloc_ms / rows_ms.max(1e-9)
        );
    }

    // What the parallel machinery itself costs: `decode_bc7_parallel` opens a
    // `thread::scope` and spawns one worker per core, per call.
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    const SPAWN_ITERS: u32 = 50;
    let t = Instant::now();
    for _ in 0..SPAWN_ITERS {
        std::thread::scope(|s| {
            for _ in 0..cores {
                s.spawn(|| std::hint::black_box(0u32));
            }
        });
    }
    let spawn_ms = t.elapsed().as_secs_f64() * 1e3 / SPAWN_ITERS as f64;
    println!();
    println!("thread::scope with {cores} no-op workers: {spawn_ms:.3} ms per call");
    println!("  the fixed toll on every parallel decode, before a pixel is touched.");
}
