//! Per-frame records, the allocation-counting shim, and the statistics the
//! board is allowed to report.
//!
//! Rule inherited from the campaign: report the median for central tendency and
//! p99/p99.9 for stability, and never quote a difference narrower than the null
//! band. The stats here compute the numbers; `board.rs` enforces the band.

use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};

// ---------------------------------------------------------------- allocator

static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);
static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_LIVE: AtomicUsize = AtomicUsize::new(0);

/// Counting wrapper over whichever allocator is underneath.
///
/// Generic in the inner allocator so **both** allocator arms are instrumented by
/// exactly the same code: `sim` wraps `std::alloc::System`, `sim-ra` wraps
/// `rusty_alloc`. If the shim differed between arms it would be measuring
/// itself. The shim is not free — build without `alloc-counters` to measure its
/// own tax before trusting any allocation-side number.
pub struct CountingAlloc<A>(pub A);

// SAFETY: every method forwards to the inner allocator with the same layout it
// was given; the counters are plain atomics and do not allocate.
unsafe impl<A: GlobalAlloc> GlobalAlloc for CountingAlloc<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = self.0.alloc(layout);
        if !p.is_null() {
            note_alloc(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size(), Relaxed);
        self.0.dealloc(ptr, layout);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = self.0.realloc(ptr, layout, new_size);
        if !p.is_null() {
            ALLOC_COUNT.fetch_add(1, Relaxed);
            if new_size >= layout.size() {
                let grew = new_size - layout.size();
                ALLOC_BYTES.fetch_add(grew as u64, Relaxed);
                bump_live(LIVE_BYTES.fetch_add(grew, Relaxed) + grew);
            } else {
                LIVE_BYTES.fetch_sub(layout.size() - new_size, Relaxed);
            }
        }
        p
    }
}

fn note_alloc(size: usize) {
    ALLOC_COUNT.fetch_add(1, Relaxed);
    ALLOC_BYTES.fetch_add(size as u64, Relaxed);
    bump_live(LIVE_BYTES.fetch_add(size, Relaxed) + size);
}

fn bump_live(now: usize) {
    // Relaxed max-update: a lost race understates the peak by one allocation,
    // which is inside the noise this number is used at.
    if now > PEAK_LIVE.load(Relaxed) {
        PEAK_LIVE.store(now, Relaxed);
    }
}

#[derive(Clone, Copy, Default)]
pub struct AllocSnapshot {
    pub count: u64,
    pub bytes: u64,
    pub live: u64,
    pub peak_live: u64,
}

/// Name of the global allocator this binary linked, set once at start-up by the
/// binary that owns the `#[global_allocator]` and written into every run
/// manifest. A board that mixes allocator arms must be able to say so.
static ALLOCATOR_PTR: std::sync::atomic::AtomicPtr<u8> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
static ALLOCATOR_LEN: AtomicUsize = AtomicUsize::new(0);

pub fn set_allocator_name(name: &'static str) {
    ALLOCATOR_PTR.store(name.as_ptr() as *mut u8, Relaxed);
    ALLOCATOR_LEN.store(name.len(), Relaxed);
}

pub fn allocator_name() -> &'static str {
    let p = ALLOCATOR_PTR.load(Relaxed);
    let len = ALLOCATOR_LEN.load(Relaxed);
    if p.is_null() || len == 0 {
        return "unknown";
    }
    // SAFETY: only ever set from a `&'static str`, together with its own length.
    unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(p, len)) }
}

pub fn alloc_snapshot() -> AllocSnapshot {
    AllocSnapshot {
        count: ALLOC_COUNT.load(Relaxed),
        bytes: ALLOC_BYTES.load(Relaxed),
        live: LIVE_BYTES.load(Relaxed) as u64,
        peak_live: PEAK_LIVE.load(Relaxed) as u64,
    }
}

// ------------------------------------------------------------- frame record

/// One CSV row. Columns match §5.1 of docs/plans/simulator-demo.md; the ones
/// that need a real swapchain (`gpu_frame_ms`, `present_ms`, `vram_*`) are
/// carried as zero in Phase 0 rather than dropped, so the schema does not move
/// when the D3D11 backend lands.
#[derive(Default, Clone)]
pub struct FrameRecord {
    pub frame: u32,
    pub sim_time: f32,
    pub cpu_frame_ms: f64,
    pub gpu_frame_ms: f64,
    pub present_ms: f64,
    pub stream_cpu_ms: f64,
    pub parse_ms: f64,
    pub decode_ms: f64,
    pub upload_ms: f64,
    pub bytes_read: u64,
    pub bytes_uploaded: u64,
    pub requests: u32,
    pub resident_pct: f32,
    pub alloc_count: u64,
    pub alloc_bytes: u64,
    pub peak_rss_mb: f64,
    pub vram_used_mb: f64,
    pub vram_budget_mb: f64,
    pub pool_used_mb: f64,
    pub pool_budget_mb: f64,
    pub evicted: u32,
    pub request_hash: u64,
    pub upload_hash: u64,
    pub hitch: u8,
}

pub const CSV_HEADER: &str = "frame,sim_time,cpu_frame_ms,gpu_frame_ms,present_ms,\
stream_cpu_ms,parse_ms,decode_ms,upload_ms,bytes_read,bytes_uploaded,requests,resident_pct,\
alloc_count,alloc_bytes,peak_rss_mb,vram_used_mb,vram_budget_mb,pool_used_mb,pool_budget_mb,evicted,request_hash,upload_hash,hitch";

impl FrameRecord {
    pub fn to_csv(&self) -> String {
        format!(
            "{},{:.4},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{},{},{},{:.4},{},{},{:.3},{:.3},{:.3},{:.3},{:.3},{},{:016x},{:016x},{}",
            self.frame,
            self.sim_time,
            self.cpu_frame_ms,
            self.gpu_frame_ms,
            self.present_ms,
            self.stream_cpu_ms,
            self.parse_ms,
            self.decode_ms,
            self.upload_ms,
            self.bytes_read,
            self.bytes_uploaded,
            self.requests,
            self.resident_pct,
            self.alloc_count,
            self.alloc_bytes,
            self.peak_rss_mb,
            self.vram_used_mb,
            self.vram_budget_mb,
            self.pool_used_mb,
            self.pool_budget_mb,
            self.evicted,
            self.request_hash,
            self.upload_hash,
            self.hitch,
        )
    }
}

// ------------------------------------------------------------------- hitches

/// A frame costs more than this on the streaming path => hitch.
///
/// Phase 0 deliberately uses an **absolute** threshold rather than a multiple of
/// a rolling median. There is no swapchain yet, so the median frame is an *idle*
/// frame costing ~0.01 ms; twice that is still timer quantisation, and the
/// rolling rule duly flagged a quarter of all frames as "hitches" purely for
/// doing any streaming work at all. 1 ms is ~6% of a 60 Hz budget — a stall
/// worth naming, on any machine.
///
/// Phase 1 replaces this with the definition studios actually use: a frame that
/// missed its present deadline. That needs a present, which needs the D3D11
/// backend.
pub const HITCH_MS: f64 = 1.0;

pub fn is_hitch(cpu_frame_ms: f64) -> bool {
    cpu_frame_ms > HITCH_MS
}

// -------------------------------------------------------------- statistics

/// Percentile of an already-sorted slice, nearest-rank.
pub fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

pub fn median(sorted: &[f64]) -> f64 {
    pct(sorted, 50.0)
}

/// Sort ascending, NaN-safe.
///
/// `partial_cmp().unwrap()` is a landmine here: a single missing metric (an OS
/// counter this platform does not expose, say) becomes a NaN, and the whole
/// board panics instead of reporting "no data" for one row.
pub fn sorted(values: &[f64]) -> Vec<f64> {
    let mut v = values.to_vec();
    v.sort_unstable_by(f64::total_cmp);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_are_nearest_rank() {
        let v = sorted(&[5.0, 1.0, 4.0, 2.0, 3.0]);
        assert_eq!(median(&v), 3.0);
        assert_eq!(pct(&v, 100.0), 5.0);
        assert_eq!(pct(&v, 20.0), 1.0);
    }

    #[test]
    fn hitches_are_absolute_not_relative() {
        // The rule this replaced fired on a 2x multiple of an idle frame, which
        // flagged a quarter of all frames in the first Phase 0 board.
        assert!(!is_hitch(0.0002));
        assert!(!is_hitch(0.01));
        assert!(!is_hitch(HITCH_MS));
        assert!(is_hitch(HITCH_MS + 0.001));
        assert!(is_hitch(34.0));
    }
}

// ------------------------------------------------------------- histogram

/// Fixed log-spaced histogram: O(1) insert, O(buckets) percentile.
///
/// The live cockpit needs p99/p99.9 continuously over a run that can reach
/// 108,000 frames. Keeping every sample and re-sorting would either cost the UI
/// thread milliseconds per refresh or cost the *sim* thread time it is supposed
/// to be measuring. A histogram costs neither, at a resolution (~1.2% per
/// bucket) far finer than the null band any of these numbers is read against.
pub struct LogHistogram {
    counts: Vec<u32>,
    total: u64,
    min: f64,
    max: f64,
}

const HIST_BUCKETS: usize = 600;
/// 1 ns .. ~1 s, log-spaced.
const HIST_LO_MS: f64 = 1e-6;
const HIST_HI_MS: f64 = 1e3;

impl Default for LogHistogram {
    fn default() -> Self {
        Self::new()
    }
}

impl LogHistogram {
    pub fn new() -> Self {
        Self {
            counts: vec![0; HIST_BUCKETS],
            total: 0,
            min: f64::INFINITY,
            max: 0.0,
        }
    }

    fn bucket(ms: f64) -> usize {
        if ms <= HIST_LO_MS || ms.is_nan() {
            return 0;
        }
        let t = (ms / HIST_LO_MS).ln() / (HIST_HI_MS / HIST_LO_MS).ln();
        ((t * HIST_BUCKETS as f64) as usize).min(HIST_BUCKETS - 1)
    }

    fn bucket_value(i: usize) -> f64 {
        let t = (i as f64 + 0.5) / HIST_BUCKETS as f64;
        HIST_LO_MS * (t * (HIST_HI_MS / HIST_LO_MS).ln()).exp()
    }

    pub fn record(&mut self, ms: f64) {
        self.counts[Self::bucket(ms)] += 1;
        self.total += 1;
        self.min = self.min.min(ms);
        self.max = self.max.max(ms);
    }

    pub fn len(&self) -> u64 {
        self.total
    }

    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    pub fn max(&self) -> f64 {
        if self.total == 0 {
            f64::NAN
        } else {
            self.max
        }
    }

    pub fn percentile(&self, p: f64) -> f64 {
        if self.total == 0 {
            return f64::NAN;
        }
        let want = ((p / 100.0) * self.total as f64).ceil().max(1.0) as u64;
        let mut seen = 0u64;
        for (i, &c) in self.counts.iter().enumerate() {
            seen += c as u64;
            if seen >= want {
                return Self::bucket_value(i);
            }
        }
        self.max
    }
}

#[cfg(test)]
mod hist_tests {
    use super::*;

    #[test]
    fn percentiles_land_within_bucket_resolution() {
        let mut h = LogHistogram::new();
        for i in 1..=1000 {
            h.record(i as f64 * 0.01); // 0.01 .. 10.0 ms
        }
        // p50 ~ 5.0 ms, p99 ~ 9.9 ms; buckets are ~1.2% wide so allow 3%.
        assert!((h.percentile(50.0) - 5.0).abs() / 5.0 < 0.03, "{}", h.percentile(50.0));
        assert!((h.percentile(99.0) - 9.9).abs() / 9.9 < 0.03, "{}", h.percentile(99.0));
        assert_eq!(h.len(), 1000);
    }
}
