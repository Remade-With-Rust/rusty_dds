//! Where the runtime cost of `rusty_dds` actually goes, counted rather than guessed.
//!
//! The simulator showed rusty_dds behind DirectXTex's borrowing loader on the
//! streaming path, and showed `rusty_alloc` recovering most of the gap — which
//! says the path is allocation-bound. This example counts the allocations and
//! times the two calls a streaming engine makes per texture:
//!
//!   1. `Dds::read`                — parse the container
//!   2. `upload_plan_compressed`   — one per resident mip
//!
//! Run: `cargo run --release --example profile_rusty_dds -- pack/high192`

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

use rusty_dds::{Dds, DdsView, SubresourceId};

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

struct Counting;

// SAFETY: forwards every call to `System` with the layout it was given; the
// counters are plain atomics and never allocate.
unsafe impl GlobalAlloc for Counting {
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
static A: Counting = Counting;

fn snapshot() -> (u64, u64) {
    (ALLOCS.load(Relaxed), BYTES.load(Relaxed))
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "pack/high192".into());
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {dir}: {e}"))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|e| e == "dds").unwrap_or(false))
        .collect();
    files.sort();
    let file = files.first().unwrap_or_else(|| panic!("no .dds in {dir}"));
    let bytes = std::fs::read(file).expect("read file");
    println!(
        "file: {}  ({:.2} MiB)\n",
        file.display(),
        bytes.len() as f64 / (1 << 20) as f64
    );

    const ITERS: u32 = 200;

    // ---- 1. Dds::read -------------------------------------------------------
    let (a0, b0) = snapshot();
    let t0 = Instant::now();
    let mut mips = 0;
    for _ in 0..ITERS {
        let dds = Dds::read(std::io::Cursor::new(&bytes[..])).expect("parse");
        mips = dds.get_num_mipmap_levels();
        std::hint::black_box(&dds);
    }
    let read_ms = t0.elapsed().as_secs_f64() * 1e3 / ITERS as f64;
    let (a1, b1) = snapshot();
    println!("Dds::read");
    println!("  {read_ms:.4} ms/call");
    println!("  {:.1} allocations/call", (a1 - a0) as f64 / ITERS as f64);
    println!(
        "  {:.2} MiB allocated/call  (payload is {:.2} MiB)",
        (b1 - b0) as f64 / ITERS as f64 / (1 << 20) as f64,
        bytes.len() as f64 / (1 << 20) as f64
    );
    println!(
        "  -> allocated/payload ratio {:.2}x\n",
        ((b1 - b0) as f64 / ITERS as f64) / bytes.len() as f64
    );

    // ---- 1b. DdsView::parse — the borrowing path ---------------------------
    let (ab0, bb0) = snapshot();
    let t0b = Instant::now();
    for _ in 0..ITERS {
        let v = DdsView::parse(&bytes).expect("parse view");
        std::hint::black_box(&v);
    }
    let view_ms = t0b.elapsed().as_secs_f64() * 1e3 / ITERS as f64;
    let (ab1, bb1) = snapshot();
    println!("DdsView::parse (borrowing)");
    println!("  {view_ms:.4} ms/call");
    println!("  {:.1} allocations/call", (ab1 - ab0) as f64 / ITERS as f64);
    println!("  {:.0} bytes allocated/call", (bb1 - bb0) as f64 / ITERS as f64);
    println!("  -> {:.0}x faster than Dds::read
", read_ms / view_ms.max(1e-9));

    // ---- 1c. DdsView::read_into — owning path, recycled buffer -------------
    // For callers that cannot borrow (archive decompressor, network stream).
    // The buffer is hoisted out of the loop, which is the whole point.
    let mut reuse: Vec<u8> = Vec::new();
    let (ac0, bc0) = snapshot();
    let t0c = Instant::now();
    for _ in 0..ITERS {
        let v = DdsView::read_into(std::io::Cursor::new(&bytes[..]), &mut reuse)
            .expect("read_into");
        std::hint::black_box(&v);
    }
    let into_ms = t0c.elapsed().as_secs_f64() * 1e3 / ITERS as f64;
    let (ac1, bc1) = snapshot();
    println!("DdsView::read_into (owning, buffer reused)");
    println!("  {into_ms:.4} ms/call");
    println!("  {:.2} allocations/call", (ac1 - ac0) as f64 / ITERS as f64);
    println!("  {:.0} bytes allocated/call", (bc1 - bc0) as f64 / ITERS as f64);
    println!("  -> {:.1}x faster than Dds::read
", read_ms / into_ms.max(1e-9));

    // ---- 2. upload_plan_compressed, one per mip -----------------------------
    let dds = Dds::read(std::io::Cursor::new(&bytes[..])).expect("parse");
    let (a2, b2) = snapshot();
    let t1 = Instant::now();
    for _ in 0..ITERS {
        for mip in 0..mips {
            let plan = dds
                .upload_plan_compressed(SubresourceId::mip_layer(mip, 0))
                .expect("plan");
            std::hint::black_box(&plan);
        }
    }
    let plan_ms = t1.elapsed().as_secs_f64() * 1e3 / ITERS as f64;
    let (a3, b3) = snapshot();
    let per_call = (a3 - a2) as f64 / (ITERS as f64 * mips as f64);
    println!("upload_plan_compressed ({mips} mips)");
    println!("  {plan_ms:.4} ms per full mip chain");
    println!("  {per_call:.1} allocations per subresource query");
    println!(
        "  {:.0} bytes allocated per subresource query\n",
        (b3 - b2) as f64 / (ITERS as f64 * mips as f64)
    );

    // ---- 3. the same information, with nothing allocated at all -------------
    // `surface()` reports the same dimensions and bytes the plan does; the
    // difference is how much bookkeeping each recomputes.
    let (a4, _) = snapshot();
    let t2 = Instant::now();
    for _ in 0..ITERS {
        for mip in 0..mips {
            let s = dds.surface(SubresourceId::mip_layer(mip, 0)).expect("surface");
            std::hint::black_box(s.data.len());
        }
    }
    let surf_ms = t2.elapsed().as_secs_f64() * 1e3 / ITERS as f64;
    let (a5, _) = snapshot();
    println!("surface() ({mips} mips), for comparison");
    println!("  {surf_ms:.4} ms per full mip chain");
    println!(
        "  {:.1} allocations per subresource query\n",
        (a5 - a4) as f64 / (ITERS as f64 * mips as f64)
    );

    // ---- 3b. decode_rgba8 — the Transcode profile's hot call ---------------
    let view = DdsView::parse(&bytes).expect("view");
    let (ad0, bd0) = snapshot();
    let t3b = Instant::now();
    const DEC_ITERS: u32 = 20;
    for _ in 0..DEC_ITERS {
        let px = view
            .decode_rgba8(SubresourceId::mip_layer(0, 0))
            .expect("decode");
        std::hint::black_box(px.pixels.len());
    }
    let dec_ms = t3b.elapsed().as_secs_f64() * 1e3 / DEC_ITERS as f64;
    let (ad1, bd1) = snapshot();
    let out_bytes = view
        .decode_rgba8(SubresourceId::mip_layer(0, 0))
        .map(|p| p.pixels.len())
        .unwrap_or(0);
    println!("decode_rgba8 (mip 0)");
    println!("  {dec_ms:.4} ms/call");
    println!("  {:.1} allocations/call", (ad1 - ad0) as f64 / DEC_ITERS as f64);
    println!(
        "  {:.2} MiB allocated/call  (output is {:.2} MiB)",
        (bd1 - bd0) as f64 / DEC_ITERS as f64 / (1 << 20) as f64,
        out_bytes as f64 / (1 << 20) as f64
    );
    println!(
        "  -> allocated/output ratio {:.2}x",
        ((bd1 - bd0) as f64 / DEC_ITERS as f64) / out_bytes.max(1) as f64
    );
    println!(
        "  -> {:.1} Mpx/s
",
        (out_bytes / 4) as f64 / (dec_ms / 1e3) / 1e6
    );

    // ---- 3c. decode throughput per mip -------------------------------------
    // BC7 decode goes parallel above 4096 blocks and serial below it, so the
    // curve across mips shows what the thread spawns cost. If the SERIAL mips
    // out-throughput the PARALLEL ones, the spawns are not paying for themselves.
    println!("decode_rgba8 throughput by mip");
    println!("  {:>4}  {:>9}  {:>10}  {:>7}  {:>6}", "mip", "px", "Mpx/s", "allocs", "path");
    for mip in 0..mips.min(8) {
        let id = SubresourceId::mip_layer(mip, 0);
        let Ok(first) = view.decode_rgba8(id) else { continue };
        let px = first.pixels.len() / 4;
        let blocks = (first.width as usize).div_ceil(4) * (first.height as usize).div_ceil(4);
        let iters = if px > 200_000 { 20 } else { 200 };
        let (aa0, _) = snapshot();
        let t = Instant::now();
        for _ in 0..iters {
            std::hint::black_box(view.decode_rgba8(id).expect("decode").pixels.len());
        }
        let ms = t.elapsed().as_secs_f64() * 1e3 / iters as f64;
        let (aa1, _) = snapshot();
        println!(
            "  {:>4}  {:>9}  {:>10.1}  {:>7.1}  {:>6}",
            mip,
            px,
            px as f64 / (ms / 1e3) / 1e6,
            (aa1 - aa0) as f64 / iters as f64,
            if blocks >= 4096 { "par" } else { "ser" }
        );
    }
    println!();

    // ---- 4. what the copy would cost with warm memory ----------------------
    // `Dds::read` allocates a fresh payload buffer every call. A fresh mapping
    // is faulted in page by page on first touch, and the OS zeroes each page
    // before we overwrite it — so the copy pays for memory it never reads.
    // Copying into a buffer that is already resident isolates that cost.
    let payload = &bytes[128..];
    let mut warm = vec![0u8; payload.len()];
    let t3 = Instant::now();
    for _ in 0..ITERS {
        warm.copy_from_slice(payload);
        std::hint::black_box(&warm);
    }
    let warm_ms = t3.elapsed().as_secs_f64() * 1e3 / ITERS as f64;

    let t4 = Instant::now();
    for _ in 0..ITERS {
        let mut fresh = vec![0u8; payload.len()];
        fresh.copy_from_slice(payload);
        std::hint::black_box(&fresh);
    }
    let fresh_ms = t4.elapsed().as_secs_f64() * 1e3 / ITERS as f64;

    println!("the payload copy, isolated");
    println!("  {fresh_ms:.4} ms  into a freshly allocated buffer");
    println!("  {warm_ms:.4} ms  into a buffer that is already resident");
    println!(
        "  -> {:.1}x of the copy is first-touch page cost, not copy bandwidth",
        fresh_ms / warm_ms.max(1e-9)
    );
    println!(
        "  -> warm copy runs at {:.1} GB/s
",
        payload.len() as f64 / (warm_ms / 1e3) / 1e9
    );

    // ---- 5. what the decode OUTPUT buffer costs ----------------------------
    // `decode_rgba8` returns a fresh `vec![0u8; w*h*4]` every call. That is
    // alloc_zeroed: the OS hands over zeroed pages which decode then overwrites
    // — the same first-touch tax as the payload copy, on a buffer 3x larger.
    let out_len = view
        .decode_rgba8(SubresourceId::mip_layer(0, 0))
        .map(|p| p.pixels.len())
        .unwrap_or(0);
    let t5 = Instant::now();
    for _ in 0..DEC_ITERS {
        let v = vec![0u8; out_len];
        std::hint::black_box(v.len());
    }
    let fresh_out_ms = t5.elapsed().as_secs_f64() * 1e3 / DEC_ITERS as f64;
    let mut warm_out = vec![0u8; out_len];
    let t6 = Instant::now();
    for _ in 0..DEC_ITERS {
        warm_out.fill(0);
        std::hint::black_box(warm_out.len());
    }
    let warm_out_ms = t6.elapsed().as_secs_f64() * 1e3 / DEC_ITERS as f64;
    println!("decode output buffer ({:.2} MiB)", out_len as f64 / (1 << 20) as f64);
    println!("  {fresh_out_ms:.4} ms  fresh vec![0u8; n] (alloc + OS zeroing)");
    println!("  {warm_out_ms:.4} ms  refilling a buffer already resident");
    println!(
        "  -> the fresh buffer is {:.0}% of a {dec_ms:.3} ms decode
",
        fresh_out_ms / dec_ms * 100.0
    );

    // ---- 6. the file read itself ------------------------------------------
    // The harness (and any engine) calls `fs::read` per texture open, which
    // allocates a fresh Vec every time. Same page-fault question as everywhere
    // else: how much of a "file read" is actually the buffer?
    const IO_ITERS: u32 = 100;
    let t7 = Instant::now();
    for _ in 0..IO_ITERS {
        let v = std::fs::read(file).expect("read");
        std::hint::black_box(v.len());
    }
    let fs_read_ms = t7.elapsed().as_secs_f64() * 1e3 / IO_ITERS as f64;

    let mut pooled: Vec<u8> = Vec::new();
    let t8 = Instant::now();
    for _ in 0..IO_ITERS {
        pooled.clear();
        let mut f = std::fs::File::open(file).expect("open");
        std::io::Read::read_to_end(&mut f, &mut pooled).expect("read");
        std::hint::black_box(pooled.len());
    }
    let pooled_ms = t8.elapsed().as_secs_f64() * 1e3 / IO_ITERS as f64;

    println!("file read (warm page cache)");
    println!("  {fs_read_ms:.4} ms  std::fs::read -> fresh Vec");
    println!("  {pooled_ms:.4} ms  read_to_end into a recycled Vec");
    println!(
        "  -> {:.2}x; {:.0}% of a \"file read\" is the buffer, not the file
",
        fs_read_ms / pooled_ms.max(1e-9),
        (1.0 - pooled_ms / fs_read_ms) * 100.0
    );

    println!("A streaming engine pays Dds::read once per texture open and");
    println!("upload_plan_compressed once per resident mip, every time a texture");
    println!("is re-requested after eviction.");
}
