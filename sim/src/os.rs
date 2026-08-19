//! Process-level OS counters: CPU time and working set.
//!
//! Wall time alone is not a verdict on a loaded box — the encoder campaign saw
//! wall swing 2-3x while CPU held steady. Every run therefore records both.
//!
//! Windows is the only implemented target (the demo is a D3D11/Vulkan-on-PC
//! story); elsewhere these return `None` and the board omits the columns.

/// Total process CPU time (kernel + user), in seconds.
pub fn process_cpu_secs() -> Option<f64> {
    imp::process_cpu_secs()
}

/// `(working_set_bytes, peak_working_set_bytes)`.
pub fn working_set() -> Option<(u64, u64)> {
    imp::working_set()
}

pub fn os_name() -> String {
    imp::os_name()
}

/// Pin this process to `mask` and raise its priority class.
///
/// Applied by the process to *itself* rather than by the bench driver, so a run
/// is pinned however it was launched — from `sim bench`, by hand, or from the
/// cockpit's detached button. Returns false where unsupported.
///
/// The default mask mirrors the encoder campaign's harness (cores 2-5): leaving
/// core 0 to the OS and keeping the arm on a fixed set of physical cores is what
/// stops the scheduler from being the thing under test.
pub fn pin_process(mask: usize, high_priority: bool) -> bool {
    let ok = imp::pin_process(mask, high_priority);
    if ok {
        APPLIED_MASK.store(mask, std::sync::atomic::Ordering::Relaxed);
    }
    ok
}

static APPLIED_MASK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The affinity mask this process actually applied; `0` when unpinned.
///
/// Recorded in every run manifest, so the board can refuse to compare a pinned
/// run against an unpinned one — the single easiest way to manufacture a fake
/// difference.
pub fn applied_affinity_mask() -> usize {
    APPLIED_MASK.load(std::sync::atomic::Ordering::Relaxed)
}

/// `0b111100` — cores 2..5, the campaign harness's mask.
pub const DEFAULT_AFFINITY_MASK: usize = 60;

#[cfg(windows)]
mod imp {
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    impl FileTime {
        /// FILETIME counts 100-nanosecond intervals.
        fn secs(self) -> f64 {
            (((self.high as u64) << 32) | self.low as u64) as f64 * 1e-7
        }
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool: usize,
        quota_paged_pool: usize,
        quota_peak_nonpaged_pool: usize,
        quota_nonpaged_pool: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn GetProcessTimes(
            process: isize,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
        fn K32GetProcessMemoryInfo(
            process: isize,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
        fn SetProcessAffinityMask(process: isize, mask: usize) -> i32;
        fn SetPriorityClass(process: isize, class: u32) -> i32;
    }

    const HIGH_PRIORITY_CLASS: u32 = 0x0000_0080;

    pub fn pin_process(mask: usize, high_priority: bool) -> bool {
        if mask == 0 {
            return false;
        }
        // SAFETY: both take the current-process pseudo-handle, which needs no
        // close, plus plain scalars. Failure is reported, never assumed away.
        unsafe {
            let h = GetCurrentProcess();
            let a = SetProcessAffinityMask(h, mask);
            let p = if high_priority {
                SetPriorityClass(h, HIGH_PRIORITY_CLASS)
            } else {
                1
            };
            a != 0 && p != 0
        }
    }

    pub fn process_cpu_secs() -> Option<f64> {
        let (mut c, mut e, mut k, mut u) = (
            FileTime::default(),
            FileTime::default(),
            FileTime::default(),
            FileTime::default(),
        );
        // SAFETY: four out-params, all valid and correctly sized for the ABI;
        // the handle from GetCurrentProcess is a pseudo-handle needing no close.
        let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
        if ok == 0 {
            return None;
        }
        Some(k.secs() + u.secs())
    }

    pub fn working_set() -> Option<(u64, u64)> {
        let mut pmc = ProcessMemoryCounters {
            cb: core::mem::size_of::<ProcessMemoryCounters>() as u32,
            ..Default::default()
        };
        // SAFETY: `pmc` is a correctly sized PROCESS_MEMORY_COUNTERS with `cb`
        // set as the API requires; the pseudo-handle needs no close.
        let ok = unsafe {
            K32GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut pmc,
                core::mem::size_of::<ProcessMemoryCounters>() as u32,
            )
        };
        if ok == 0 {
            return None;
        }
        Some((
            pmc.working_set_size as u64,
            pmc.peak_working_set_size as u64,
        ))
    }

    pub fn os_name() -> String {
        "windows".to_string()
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn process_cpu_secs() -> Option<f64> {
        None
    }
    pub fn working_set() -> Option<(u64, u64)> {
        None
    }
    pub fn os_name() -> String {
        std::env::consts::OS.to_string()
    }

    pub fn pin_process(_mask: usize, _high_priority: bool) -> bool {
        false
    }
}
