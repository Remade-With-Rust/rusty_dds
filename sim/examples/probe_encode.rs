//! Allocation and time profile of the Cook path (encode) and BC6H decode.
//!
//! The runtime paths have been measured to death; these two have not. Encode is
//! the bake farm, and BC6H decode returns 16 bytes a pixel — four times the
//! fresh-buffer tax that RGBA8 decode carried.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

use rusty_dds::{Dds, DdsView, DecodeContent, EncodeLayout, SubresourceId};

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

struct C;
// SAFETY: forwards to System with the layout given; counters are plain atomics.
unsafe impl GlobalAlloc for C {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(l.size() as u64, Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(n.saturating_sub(l.size()) as u64, Relaxed);
        unsafe { System.realloc(p, l, n) }
    }
}
#[global_allocator]
static A: C = C;

fn snap() -> (u64, u64) {
    (ALLOCS.load(Relaxed), BYTES.load(Relaxed))
}

fn main() {
    const W: u32 = 512;
    let px: Vec<u8> = (0..(W as usize * W as usize * 4))
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
        .collect();
    let mips = W.trailing_zeros() + 1;

    println!("{W}x{W} source, {mips} mips\n");
    println!("{:<10} {:>10} {:>10} {:>12} {:>10}", "format", "ms", "allocs", "MiB alloc", "out KiB");

    for (name, content, iters) in [
        ("BC1", DecodeContent::Bc1, 10u32),
        ("BC3", DecodeContent::Bc3, 10),
        ("BC5U", DecodeContent::Bc5UNorm, 10),
        ("BC7", DecodeContent::Bc7, 3),
    ] {
        let layout = EncodeLayout::flat_2d(content, W, W).with_mips(mips);
        // Warm any lazily-built tables so the first call is not the sample.
        let warm = Dds::encode_from_rgba8(&px, layout).expect("encode");
        let out_len = warm.data.len();

        let (a0, b0) = snap();
        let t = Instant::now();
        for _ in 0..iters {
            std::hint::black_box(Dds::encode_from_rgba8(&px, layout).expect("encode").data.len());
        }
        let ms = t.elapsed().as_secs_f64() * 1e3 / iters as f64;
        let (a1, b1) = snap();
        println!(
            "{:<10} {:>10.2} {:>10.1} {:>12.2} {:>10}",
            name,
            ms,
            (a1 - a0) as f64 / iters as f64,
            (b1 - b0) as f64 / iters as f64 / (1 << 20) as f64,
            out_len / 1024
        );
    }

    // ---- BC6H decode: 16 bytes a pixel out ---------------------------------
    let hdr: Vec<f32> = (0..(256usize * 256 * 4)).map(|i| (i % 97) as f32 / 97.0).collect();
    if let Ok(bc6) = Dds::encode_bc6h_uf16(&hdr, 256, 256) {
        let mut buf = Vec::new();
        bc6.write(&mut buf).expect("write");
        let view = DdsView::parse(&buf).expect("parse");
        let id = SubresourceId::mip_layer(0, 0);
        const N: u32 = 20;
        std::hint::black_box(view.decode_rgba_f32(id).ok());
        let (a0, b0) = snap();
        let t = Instant::now();
        for _ in 0..N {
            std::hint::black_box(view.decode_rgba_f32(id).expect("dec").pixels.len());
        }
        let ms = t.elapsed().as_secs_f64() * 1e3 / N as f64;
        let (a1, b1) = snap();
        println!(
            "\nBC6H decode_rgba_f32 256x256\n  {ms:.4} ms  {:.1} allocs  {:.2} MiB/call (output is {:.2} MiB)",
            (a1 - a0) as f64 / N as f64,
            (b1 - b0) as f64 / N as f64 / (1 << 20) as f64,
            256.0 * 256.0 * 16.0 / (1 << 20) as f64
        );
        println!("  -> no `_into` twin exists for this path yet");
    }
}
