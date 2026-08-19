//! The **API swap point**. Phase 0 ships `NullRenderer`; Phase 1 adds D3D11 and
//! Phase 3 Vulkan behind the same trait.
//!
//! `NullRenderer` is not a stub that discards work. It performs the staging
//! copy a real upload performs — row by row, honouring `bytes_per_row` — so the
//! memory traffic the harness measures in Phase 0 has the same shape as the
//! traffic the D3D11 `Map(DISCARD)` / Vulkan staging-ring path will perform.
//! It also hashes what it copied, which is the credibility gate of the whole
//! demo (§4 of the plan).

use crate::hash::{bulk_hash, fnv1a_seed, FNV_OFFSET};
use crate::provider::{SubId, SubresourceBytes};

/// What one subresource upload cost and what it uploaded.
#[derive(Clone, Copy, Debug)]
pub struct UploadRec {
    pub id: SubId,
    pub bytes: u64,
    /// Hash of the bytes handed to the GPU, salted by (texture, mip) so the
    /// frame hash is sensitive to *which* subresource carried them.
    pub hash: u64,
}

pub trait Renderer: Send {
    fn name(&self) -> &'static str;

    /// Copy one subresource into staging and record it.
    fn upload(&mut self, texture: u32, sub: &SubresourceBytes<'_>, id: SubId) -> UploadRec;

    /// End-of-frame GPU work. Returns GPU milliseconds; `0.0` when there is no
    /// GPU (Phase 0), which the board renders as "n/a" rather than a zero.
    fn frame(&mut self) -> f64 {
        0.0
    }
}

pub struct NullRenderer {
    staging: Vec<u8>,
}

impl NullRenderer {
    /// `capacity` should be the pack's largest subresource, so the steady state
    /// never measures `Vec` growth — and so a small dev pack does not reserve
    /// a 4K-sized staging buffer per worker (the first Phase 0 probe reported
    /// 66 MiB peak live against a 7 MiB working set for exactly that reason).
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            staging: Vec::with_capacity(capacity),
        }
    }
}

impl Renderer for NullRenderer {
    fn name(&self) -> &'static str {
        "null"
    }

    fn upload(&mut self, texture: u32, sub: &SubresourceBytes<'_>, id: SubId) -> UploadRec {
        let row = sub.bytes_per_row as usize;
        let rows = sub.rows_per_image as usize;
        let want = row.saturating_mul(rows);

        self.staging.clear();
        self.staging.reserve(want);

        // Row-at-a-time, exactly as a buffer→image copy walks the source. A
        // single bulk memcpy would measure a different access pattern than the
        // real backends will.
        let mut hash = fnv1a_seed(FNV_OFFSET, &texture.to_le_bytes());
        hash = fnv1a_seed(hash, &id.mip.to_le_bytes());
        for r in 0..rows {
            let start = r * row;
            let end = (start + row).min(sub.bytes.len());
            if start >= end {
                break;
            }
            let chunk = &sub.bytes[start..end];
            self.staging.extend_from_slice(chunk);
            hash = bulk_hash(hash, chunk);
        }

        UploadRec {
            id,
            bytes: self.staging.len() as u64,
            hash,
        }
    }
}
