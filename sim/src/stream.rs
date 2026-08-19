//! The streaming pool: residency, eviction, and the worker pool that turns
//! requests into uploads.
//!
//! Determinism under threads is the whole trick here. Workers finish in
//! whatever order the scheduler picks, so:
//!
//! * the **batch** for a frame is chosen on the main thread from a sorted
//!   request list against a byte budget — never by whoever finishes first;
//! * the frame is **joined** before it ends, so a frame's uploads are exactly
//!   the batch it issued;
//! * the frame hash **sorts** per-upload hashes before folding
//!   ([`crate::hash::combine_sorted`]).
//!
//! `--workers 0` runs the same jobs inline. The two must produce identical hash
//! streams; `sim verify` asserts it.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::hash::combine_sorted;
use crate::pack::{sub_bytes, Pack};
use crate::provider::{OpenTexture, SimError, SimResult, SubId, TextureProvider};
use crate::renderer::{NullRenderer, Renderer, UploadRec};
use crate::scenario::Tier;

// ----------------------------------------------------------------- job types

struct Job {
    tex: u32,
    path: PathBuf,
    /// A payload buffer from the pool, if one was free. Read into rather than
    /// allocated: 77% of a `fs::read` is the destination buffer, not the file.
    buf: Option<Vec<u8>>,
    /// Moved out of residency for the duration of the job, moved back on merge.
    /// A texture appears in at most one job per frame, so this never races.
    open: Option<Box<dyn OpenTexture>>,
    mips: Vec<u32>,
}

struct JobResult {
    tex: u32,
    open: Option<Box<dyn OpenTexture>>,
    read_ns: u64,
    parse_ns: u64,
    upload_ns: u64,
    bytes_read: u64,
    uploads: Vec<UploadRec>,
    err: Option<String>,
}

fn run_job(
    provider: &dyn TextureProvider,
    renderer: &mut dyn Renderer,
    mut job: Job,
) -> JobResult {
    let mut res = JobResult {
        tex: job.tex,
        open: None,
        read_ns: 0,
        parse_ns: 0,
        upload_ns: 0,
        bytes_read: 0,
        uploads: Vec::with_capacity(job.mips.len()),
        err: None,
    };

    let open = match job.open.take() {
        Some(o) => o,
        None => {
            let t0 = Instant::now();
            // Reuse a pooled buffer when one is free. `clear` keeps the
            // capacity, so the pages stay resident and the read is a copy rather
            // than a copy plus a page fault per 4 KiB.
            let mut bytes = job.buf.take().unwrap_or_default();
            bytes.clear();
            let read = std::fs::File::open(&job.path)
                .and_then(|mut f| std::io::Read::read_to_end(&mut f, &mut bytes));
            if let Err(e) = read {
                res.err = Some(format!("read {}: {e}", job.path.display()));
                return res;
            }
            res.read_ns = t0.elapsed().as_nanos() as u64;
            res.bytes_read = bytes.len() as u64;

            let t1 = Instant::now();
            match provider.open(bytes) {
                Ok(o) => {
                    res.parse_ns = t1.elapsed().as_nanos() as u64;
                    o
                }
                Err(e) => {
                    res.err = Some(format!("open {}: {e}", job.path.display()));
                    return res;
                }
            }
        }
    };

    let t2 = Instant::now();
    for &mip in &job.mips {
        let id = SubId::mip(mip);
        match open.subresource(id) {
            Ok(sub) => res.uploads.push(renderer.upload(job.tex, &sub, id)),
            Err(e) => {
                res.err = Some(format!("subresource {}/{mip}: {e}", job.tex));
                break;
            }
        }
    }
    res.upload_ns = t2.elapsed().as_nanos() as u64;
    res.open = Some(open);
    res
}

// ------------------------------------------------------------------ the pool

enum Pool {
    Inline {
        provider: Arc<dyn TextureProvider>,
        renderer: NullRenderer,
    },
    Threads {
        /// `Option` so `Drop` can close the queue *before* joining. Dropping the
        /// enum's fields happens after `Drop::drop` returns, so a plain `Sender`
        /// here would leave the workers blocked in `recv` and deadlock the join.
        jobs: Option<Sender<Job>>,
        results: Receiver<JobResult>,
        handles: Vec<std::thread::JoinHandle<()>>,
    },
}

impl Pool {
    fn new(provider: Arc<dyn TextureProvider>, workers: usize, staging: usize) -> Pool {
        if workers == 0 {
            return Pool::Inline {
                provider,
                renderer: NullRenderer::with_capacity(staging),
            };
        }
        let (job_tx, job_rx) = std::sync::mpsc::channel::<Job>();
        let (res_tx, res_rx) = std::sync::mpsc::channel::<JobResult>();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let job_rx = Arc::clone(&job_rx);
            let res_tx = res_tx.clone();
            let provider = Arc::clone(&provider);
            handles.push(std::thread::spawn(move || {
                let mut renderer = NullRenderer::with_capacity(staging);
                loop {
                    // Hold the lock only long enough to take one job.
                    let job = {
                        let rx = job_rx.lock().expect("job queue poisoned");
                        rx.recv()
                    };
                    let Ok(job) = job else { break };
                    let out = run_job(provider.as_ref(), &mut renderer, job);
                    if res_tx.send(out).is_err() {
                        break;
                    }
                }
            }));
        }
        Pool::Threads {
            jobs: Some(job_tx),
            results: res_rx,
            handles,
        }
    }

    /// Issue a batch and wait for all of it. Returns results in completion
    /// order — callers must not depend on that order.
    fn run_batch(&mut self, batch: Vec<Job>) -> SimResult<Vec<JobResult>> {
        match self {
            Pool::Inline { provider, renderer } => Ok(batch
                .into_iter()
                .map(|j| run_job(provider.as_ref(), renderer, j))
                .collect()),
            Pool::Threads { jobs, results, .. } => {
                let n = batch.len();
                let tx = jobs
                    .as_ref()
                    .ok_or_else(|| SimError("streaming pool already shut down".into()))?;
                for job in batch {
                    tx.send(job)
                        .map_err(|_| SimError("streaming workers died".into()))?;
                }
                let mut out = Vec::with_capacity(n);
                for _ in 0..n {
                    out.push(
                        results
                            .recv()
                            .map_err(|_| SimError("streaming worker dropped a result".into()))?,
                    );
                }
                Ok(out)
            }
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        if let Pool::Threads { jobs, handles, .. } = self {
            // Close the queue first so `recv` returns Err and the workers exit,
            // then join so their time lands inside the measured process.
            drop(jobs.take());
            for h in handles.drain(..) {
                let _ = h.join();
            }
        }
    }
}

// ----------------------------------------------------------------- residency

const MAX_MIPS: usize = 32;

/// Default payload buffers held for reuse. Each is one texture's payload, so
/// this caps the pool's own memory rather than letting eviction bursts pin the
/// pack. Overridable with `--pool-buffers`, and `0` disables reuse entirely —
/// which is how the reuse win is A/B'd against itself.
pub const DEFAULT_POOLED_BUFFERS: usize = 32;

struct TexState {
    open: Option<Box<dyn OpenTexture>>,
    /// Bit `m` set = mip `m` resident. BCn packs top out well under 32 mips.
    resident: u32,
    /// Bytes actually uploaded per mip. Recorded on admission rather than
    /// re-derived at eviction, so the pool can never drift from what it charged.
    mip_bytes: [u32; MAX_MIPS],
    last_frame: u32,
}

/// What one simulated frame did.
#[derive(Default, Clone, Copy)]
pub struct FrameWork {
    pub requests: u32,
    pub bytes_read: u64,
    pub bytes_uploaded: u64,
    pub read_ms: f64,
    pub parse_ms: f64,
    pub upload_ms: f64,
    pub resident_pct: f32,
    pub upload_hash: u64,
    pub evicted: u32,
}

pub struct Streamer {
    pack: Pack,
    pool: Pool,
    state: Vec<TexState>,
    pool_budget: u64,
    upload_budget: u64,
    resident_bytes: u64,
    /// Scratch, reused so the steady state does not measure Vec growth.
    scratch_hashes: Vec<u64>,
    /// Payload buffers from evicted textures, waiting to be filled again.
    /// Bounded so a burst of evictions cannot pin the whole pack in memory.
    buffers: Vec<Vec<u8>>,
    max_buffers: usize,
    /// What became resident this frame, and what was closed — the live viewport
    /// needs both to keep its GPU resources in step with the pool. Recorded
    /// rather than returned so `FrameWork` stays `Copy` and the headless path
    /// pays nothing for the viewport's needs.
    newly_resident: Vec<(u32, u32)>,
    closed: Vec<u32>,
}

impl Streamer {
    pub fn new(
        pack: Pack,
        tier: Tier,
        provider: Arc<dyn TextureProvider>,
        workers: usize,
        pool_budget: u64,
        max_buffers: usize,
    ) -> Streamer {
        // Largest single subresource in the pack: mip 0 of the biggest format.
        let staging = pack
            .textures
            .iter()
            .map(|t| sub_bytes(t.content, pack.size, 0) as usize)
            .max()
            .unwrap_or(1 << 20);
        let state = (0..pack.textures.len())
            .map(|_| TexState {
                open: None,
                resident: 0,
                mip_bytes: [0; MAX_MIPS],
                last_frame: 0,
            })
            .collect();
        Streamer {
            pool: Pool::new(provider, workers, staging),
            state,
            pool_budget,
            upload_budget: tier.upload_budget_bytes(),
            resident_bytes: 0,
            scratch_hashes: Vec::with_capacity(256),
            buffers: Vec::new(),
            max_buffers,
            newly_resident: Vec::with_capacity(64),
            closed: Vec::with_capacity(16),
            pack,
        }
    }

    pub fn pool_budget(&self) -> u64 {
        self.pool_budget
    }

    pub fn resident_bytes(&self) -> u64 {
        self.resident_bytes
    }

    /// `(texture, mip)` pairs that became resident during the last `step`.
    pub fn newly_resident(&self) -> &[(u32, u32)] {
        &self.newly_resident
    }

    /// Textures whose last mip was evicted during the last `step`.
    pub fn closed_textures(&self) -> &[u32] {
        &self.closed
    }

    pub fn open_texture(&self, tex: u32) -> Option<&dyn OpenTexture> {
        self.state
            .get(tex as usize)
            .and_then(|s| s.open.as_deref())
    }

    /// Finest mip currently resident — what the viewport clamps sampling to, so
    /// the picture never shows detail the streamer has not delivered.
    pub fn min_resident_mip(&self, tex: u32) -> Option<u32> {
        let st = self.state.get(tex as usize)?;
        if st.resident == 0 {
            None
        } else {
            Some(st.resident.trailing_zeros())
        }
    }

    /// Every texture with at least one resident mip.
    pub fn resident_textures(&self, out: &mut Vec<u32>) {
        out.clear();
        for (i, st) in self.state.iter().enumerate() {
            if st.resident != 0 {
                out.push(i as u32);
            }
        }
    }

    /// Advance one frame against a **sorted** request list.
    pub fn step(&mut self, frame: u32, reqs: &[(u32, u32)]) -> SimResult<FrameWork> {
        let mut work = FrameWork {
            requests: reqs.len() as u32,
            ..Default::default()
        };

        // 1. Touch everything requested (drives LRU), and collect what is missing.
        //    `reqs` is sorted by (tex, mip), so runs of the same texture are
        //    contiguous and the batch order is a pure function of the request list.
        let mut batch: Vec<Job> = Vec::new();
        let mut budget_left = self.upload_budget;
        let mut resident_now = 0u32;
        let mut i = 0;
        while i < reqs.len() {
            let tex = reqs[i].0;
            let mut j = i;
            let mut missing: Vec<u32> = Vec::new();
            while j < reqs.len() && reqs[j].0 == tex {
                let mip = reqs[j].1;
                let st = &self.state[tex as usize];
                if st.resident & (1 << mip) != 0 {
                    resident_now += 1;
                } else if budget_left > 0 {
                    let need = sub_bytes(
                        self.pack.textures[tex as usize].content,
                        self.pack.size,
                        mip,
                    );
                    // A mip that does not fit is simply skipped this frame; the
                    // scan order is fixed, so which ones fit is deterministic.
                    if need <= budget_left {
                        budget_left -= need;
                        missing.push(mip);
                    }
                }
                j += 1;
            }
            self.state[tex as usize].last_frame = frame;
            if !missing.is_empty() {
                let st = &mut self.state[tex as usize];
                let buf = if st.open.is_none() {
                    self.buffers.pop()
                } else {
                    None
                };
                let st = &mut self.state[tex as usize];
                batch.push(Job {
                    tex,
                    path: self.pack.path(&self.pack.textures[tex as usize]),
                    open: st.open.take(),
                    buf,
                    mips: missing,
                });
            }
            i = j;
        }

        // 2. Issue and join. Everything the frame uploads, it uploads this frame.
        let results = self.pool.run_batch(batch)?;

        self.scratch_hashes.clear();
        self.newly_resident.clear();
        self.closed.clear();
        for r in results {
            if let Some(err) = r.err {
                return Err(SimError(err));
            }
            let st = &mut self.state[r.tex as usize];
            st.open = r.open;
            for u in &r.uploads {
                if u.id.mip as usize >= MAX_MIPS {
                    return Err(SimError(format!("mip {} exceeds MAX_MIPS", u.id.mip)));
                }
                if st.resident & (1 << u.id.mip) == 0 {
                    st.resident |= 1 << u.id.mip;
                    st.mip_bytes[u.id.mip as usize] = u.bytes as u32;
                    self.resident_bytes += u.bytes;
                    resident_now += 1;
                    self.newly_resident.push((r.tex, u.id.mip));
                }
                work.bytes_uploaded += u.bytes;
                self.scratch_hashes.push(u.hash);
            }
            work.bytes_read += r.bytes_read;
            work.read_ms += r.read_ns as f64 / 1e6;
            work.parse_ms += r.parse_ns as f64 / 1e6;
            work.upload_ms += r.upload_ns as f64 / 1e6;
        }
        work.upload_hash = combine_sorted(&mut self.scratch_hashes);

        // 3. Evict to budget. Oldest first, and within a texture the top (largest)
        //    mip first — never anything requested this frame.
        work.evicted = self.evict(frame);

        work.resident_pct = if reqs.is_empty() {
            1.0
        } else {
            resident_now as f32 / reqs.len() as f32
        };
        Ok(work)
    }

    fn evict(&mut self, frame: u32) -> u32 {
        if self.resident_bytes <= self.pool_budget {
            return 0;
        }
        // (last_frame, tex, mip) ascending: least-recently-used texture first,
        // finest mip first inside it. Deterministic, no clock, no hash order.
        let mut cands: Vec<(u32, u32, u32)> = Vec::new();
        for (tex, st) in self.state.iter().enumerate() {
            if st.last_frame == frame || st.resident == 0 {
                continue;
            }
            for mip in 0..MAX_MIPS as u32 {
                if st.resident & (1 << mip) != 0 {
                    cands.push((st.last_frame, tex as u32, mip));
                }
            }
        }
        cands.sort_unstable();

        let mut evicted = 0;
        for (_, tex, mip) in cands {
            if self.resident_bytes <= self.pool_budget {
                break;
            }
            let st = &mut self.state[tex as usize];
            st.resident &= !(1 << mip);
            let bytes = std::mem::take(&mut st.mip_bytes[mip as usize]) as u64;
            self.resident_bytes = self.resident_bytes.saturating_sub(bytes);
            evicted += 1;
            if st.resident == 0 {
                // Nothing left resident: close the file, exactly as a streamer
                // drops a texture. The next request re-reads and re-parses it,
                // which is where parse cost belongs.
                if let Some(open) = st.open.take() {
                    // Keep the payload buffer, not the texture. Capped so a
                    // burst of evictions cannot hold the whole pack resident.
                    if self.buffers.len() < self.max_buffers {
                        if let Some(buf) = open.reclaim() {
                            self.buffers.push(buf);
                        }
                    }
                }
                self.closed.push(tex);
            }
        }
        evicted
    }
}
