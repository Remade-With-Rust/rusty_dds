//! Deterministic replay: tiers, world layout, camera traces, and the request
//! generator.
//!
//! Nothing here reads the clock. Every decision is a pure function of
//! `(frame index, tier, world)`, which is what lets two arms be compared at all.
//!
//! Phase 0 deviation from the plan: scenarios are defined in code rather than
//! loaded from `scenarios/*.json`. A JSON loader buys nothing until traces are
//! *recorded* rather than generated, and it would be a parser dependency in a
//! harness whose whole job is to be above suspicion. The shapes below are the
//! ones the plan names.

use crate::hash::{mix, Rng, FNV_OFFSET};
use rusty_dds::{DecodeContent, Rdo};

// --------------------------------------------------------------------- tiers

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    Ultra,
    High,
    Medium,
}

/// What a pack texture holds.
///
/// The crate's `DecodeContent` covers LDR formats only — and that type being
/// LDR-only is precisely how HDR stayed invisible to this harness. Anything
/// that describes pack content has to name both domains or the gap reopens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    Ldr(DecodeContent),
    /// BC6H UF16 — sky, reflection probes, lightmaps.
    Hdr,
}

impl Content {
    pub fn name(self) -> &'static str {
        match self {
            Content::Ldr(c) => c.name(),
            Content::Hdr => "bc6h",
        }
    }

    pub fn parse(s: &str) -> Option<Content> {
        if s == "bc6h" {
            return Some(Content::Hdr);
        }
        DecodeContent::ALL_LDR
            .iter()
            .copied()
            .find(|c| c.name() == s)
            .map(Content::Ldr)
    }

    pub fn is_hdr(self) -> bool {
        matches!(self, Content::Hdr)
    }
}

impl Tier {
    pub fn parse(s: &str) -> Option<Tier> {
        match s {
            "ultra" => Some(Tier::Ultra),
            "high" => Some(Tier::High),
            "medium" => Some(Tier::Medium),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Tier::Ultra => "ultra",
            Tier::High => "high",
            Tier::Medium => "medium",
        }
    }

    /// Resolution cap. Overridable at cook time so Phase 0 can iterate without
    /// baking 4K BC7.
    pub fn default_size(self) -> u32 {
        match self {
            Tier::Ultra => 2048,
            Tier::High => 1024,
            Tier::Medium => 512,
        }
    }

    /// Format mix, cycled across the pack. Ultra is BC7-heavy; medium drops to
    /// BC1/BC3 as the plan's tier table specifies, and the top two tiers carry
    /// HDR sky and reflection probes.
    pub fn content_for(self, index: u32) -> Content {
        // One texture in sixteen is HDR: the sky and the reflection probes. That
        // small fraction is exactly why BC6H went unprofiled for five rounds of
        // optimisation — it is easy to forget a format the harness never cooks.
        // Medium ships an LDR sky, as a medium-tier game would.
        if matches!(self, Tier::Ultra | Tier::High) && index % 16 == 8 {
            return Content::Hdr;
        }
        Content::Ldr(self.ldr_content_for(index))
    }

    fn ldr_content_for(self, index: u32) -> DecodeContent {
        let albedo = match self {
            Tier::Ultra => DecodeContent::Bc7,
            Tier::High => {
                if index % 8 < 4 {
                    DecodeContent::Bc7
                } else {
                    DecodeContent::Bc1
                }
            }
            Tier::Medium => {
                if index % 8 < 4 {
                    DecodeContent::Bc1
                } else {
                    DecodeContent::Bc3
                }
            }
        };
        match index % 4 {
            0 | 1 => albedo,
            2 => DecodeContent::Bc5UNorm, // normal
            _ => DecodeContent::Bc4UNorm, // mask
        }
    }

    pub fn rdo(self) -> Rdo {
        match self {
            Tier::Ultra => Rdo::Off,
            Tier::High => Rdo::lambda(4.0),
            Tier::Medium => Rdo::lambda(10.0),
        }
    }

    pub fn mip_bias(self) -> f32 {
        match self {
            Tier::Ultra | Tier::High => 0.0,
            Tier::Medium => 1.0,
        }
    }

    /// How much of the scenario's own peak demand the pool is allowed to hold.
    ///
    /// This replaces a fraction-of-pack budget, which could not work: demand and
    /// pack size scale together, so *no* constant fraction of the pack reliably
    /// binds — the first two attempts (0.25, then 0.10) both equilibrated just
    /// under budget with zero uploads in the steady state. Sizing against
    /// measured peak demand is self-calibrating for any pack, any texture count
    /// and any scenario, and it states the pressure directly: at 0.5 the pool
    /// holds half of what the frame wants, so half of it is always in flight.
    pub fn pool_pressure(self) -> f64 {
        match self {
            Tier::Ultra => 0.80,
            Tier::High => 0.65,
            Tier::Medium => 0.50,
        }
    }

    /// Superseded by [`Tier::pool_pressure`]; kept only for the record of what
    /// was tried. See the note there.
    #[allow(dead_code)]
    /// Streaming pool as a fraction of the cooked pack — sized so the pool holds
    /// roughly **half** of what the scenario demands, which is what makes this a
    /// streaming benchmark rather than a load-once benchmark.
    ///
    /// The derivation matters, because the first values here (0.75/0.45/0.25)
    /// never bound and the failure was scale-invariant: with `N` textures of `S`
    /// bytes, the pool grows as `frac * N * S` while peak demand grows as
    /// `visible_share * tail_share * N * S` ~= 0.59 * 0.33 * N * S ~= 0.19 * N * S.
    /// Any fraction above ~0.19 holds the entire demanded set forever, so nothing
    /// is ever evicted, nothing is ever re-streamed, and the harness measures an
    /// idle loop no matter how many textures are added. Halving that gives ~0.10;
    /// the tiers scale up from there because a higher tier is defined partly by
    /// being allowed to keep more resident.
    pub fn pool_fraction(self) -> f64 {
        match self {
            Tier::Ultra => 0.16,
            Tier::High => 0.13,
            Tier::Medium => 0.10,
        }
    }

    /// Per-frame upload budget in bytes. ~8 MB at 60 Hz ≈ 480 MB/s, an
    /// NVMe-shaped streaming rate.
    pub fn upload_budget_bytes(self) -> u64 {
        match self {
            Tier::Ultra => 12 << 20,
            Tier::High => 8 << 20,
            Tier::Medium => 6 << 20,
        }
    }
}

// ----------------------------------------------------------------- scenarios

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScenarioKind {
    Traverse,
    Arrival,
    Hub,
    Soak,
}

#[derive(Clone, Copy, Debug)]
pub struct Scenario {
    pub name: &'static str,
    pub kind: ScenarioKind,
    pub frames: u32,
    /// Fixed timestep. The simulation never reads the wall clock, so a frame
    /// that misses its budget changes timing but never changes *work*.
    pub dt: f32,
    /// Frames discarded before recording (shader/page cache, allocator warm-up).
    pub warmup: u32,
}

pub const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "traverse",
        kind: ScenarioKind::Traverse,
        frames: 10_800, // 3 min at 60 Hz
        dt: 1.0 / 60.0,
        warmup: 300,
    },
    Scenario {
        name: "arrival",
        kind: ScenarioKind::Arrival,
        frames: 12_000, // 10 x 20 s
        dt: 1.0 / 60.0,
        warmup: 300,
    },
    Scenario {
        name: "hub",
        kind: ScenarioKind::Hub,
        frames: 18_000, // 5 min
        dt: 1.0 / 60.0,
        warmup: 300,
    },
    Scenario {
        name: "soak",
        kind: ScenarioKind::Soak,
        frames: 108_000, // 30 min
        dt: 1.0 / 60.0,
        warmup: 600,
    },
    // Short scenario for harness development; not a reportable arm.
    Scenario {
        name: "smoke",
        kind: ScenarioKind::Traverse,
        frames: 600,
        dt: 1.0 / 60.0,
        warmup: 60,
    },
];

pub fn scenario_by_name(name: &str) -> Option<Scenario> {
    SCENARIOS.iter().copied().find(|s| s.name == name)
}

// --------------------------------------------------------------------- world

/// Half-extent of the world box. Sized *against* `CULL_DIST`: at 300 the
/// visible disc covers ~59% of the box, so a large fraction of the pack is
/// in flight at once and the pool budget actually binds. The first Phase 0
/// probe used 600 and measured an empty workload — 5 requests per frame, zero
/// uploads after warm-up, `resident_pct` pinned at 1.0. A harness that
/// measures nothing is deterministic too.
pub const WORLD_EXTENT: f32 = 300.0;
/// Distance at which mip 0 is the right choice.
///
/// This is the knob that decides whether the pool binds. At 12 the visible set
/// sat at mips 2-5, resident bytes stayed far under budget, nothing was ever
/// evicted and the steady state uploaded nothing at all. At 40 most of the
/// visible set wants mip 0-1, the working set exceeds the pool, and the camera's
/// motion produces continuous eviction and re-streaming — which is the workload
/// the demo is about.
const NEAR_DIST: f32 = 40.0;
/// Beyond this the texture is not requested at all.
const CULL_DIST: f32 = 260.0;

/// Fixed placement of every texture in the pack, derived from the pack index so
/// it never has to be stored or transported.
pub struct World {
    pub positions: Vec<[f32; 3]>,
    pub mips: Vec<u32>,
}

impl World {
    pub fn new(mips_per_texture: &[u32], seed: u64) -> World {
        let mut rng = Rng::new(seed);
        let positions = (0..mips_per_texture.len())
            .map(|_| {
                [
                    rng.range_f32(-WORLD_EXTENT, WORLD_EXTENT),
                    rng.range_f32(-6.0, 6.0),
                    rng.range_f32(-WORLD_EXTENT, WORLD_EXTENT),
                ]
            })
            .collect();
        World {
            positions,
            mips: mips_per_texture.to_vec(),
        }
    }

    pub fn len(&self) -> usize {
        self.positions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }
}

// -------------------------------------------------------------------- camera

/// Camera position for a frame. Pure function of `(kind, frame)`.
pub fn camera(kind: ScenarioKind, frame: u32, dt: f32) -> [f32; 3] {
    let t = frame as f32 * dt;
    match kind {
        ScenarioKind::Traverse | ScenarioKind::Soak => {
            // Lissajous loop at ~84 units/s — fast enough that textures cross
            // several mip levels per second, which is what makes the pool
            // churn. The two frequencies are deliberately incommensurate so
            // the residency set never exactly repeats.
            [
                WORLD_EXTENT * 0.8 * (t * 0.35).sin(),
                0.0,
                WORLD_EXTENT * 0.8 * (t * 0.2149).cos(),
            ]
        }
        ScenarioKind::Arrival => {
            // Teleport every 20 s, then drift. The burst after each jump is the
            // cold-residency measurement the plan calls the money shot.
            let leg = frame / 1200;
            let local = (frame % 1200) as f32 * dt;
            let mut rng = Rng::new(0xA221_1AA0 ^ leg as u64);
            let base = [
                rng.range_f32(-WORLD_EXTENT, WORLD_EXTENT),
                0.0,
                rng.range_f32(-WORLD_EXTENT, WORLD_EXTENT),
            ];
            [
                base[0] + local * 14.0,
                base[1],
                base[2] + local * 5.0,
            ]
        }
        ScenarioKind::Hub => {
            // Tight orbit: few textures, many of them at mip 0, heavy churn as
            // the camera swings past each one.
            let r = 70.0;
            [r * (t * 0.6).cos(), 0.0, r * (t * 0.6).sin()]
        }
    }
}

// ----------------------------------------------------------- request generator

/// Requested `(texture, mip)` pairs for a frame, **sorted**.
///
/// Residency is modelled as a mip tail: to be resident at mip `m`, a texture
/// needs `m..last`. That is what shipping streamers do, and it means the coarse
/// mips stay pinned while the expensive top mip is the thing that churns.
pub fn requests(world: &World, cam: [f32; 3], mip_bias: f32, out: &mut Vec<(u32, u32)>) {
    out.clear();
    for (i, pos) in world.positions.iter().enumerate() {
        let dx = pos[0] - cam[0];
        let dy = pos[1] - cam[1];
        let dz = pos[2] - cam[2];
        let d = (dx * dx + dy * dy + dz * dz).sqrt();
        if d > CULL_DIST {
            continue;
        }
        let mips = world.mips[i];
        let lod = ((d / NEAR_DIST).max(1.0).log2() + mip_bias).max(0.0);
        let top = (lod.round() as u32).min(mips.saturating_sub(1));
        for m in top..mips {
            out.push((i as u32, m));
        }
    }
    out.sort_unstable();
}

/// Hash of a frame's request set — the work-count parity gate. Two arms whose
/// request-hash streams differ are rejected, not compared.
pub fn request_hash(reqs: &[(u32, u32)]) -> u64 {
    let mut h = FNV_OFFSET;
    for &(t, m) in reqs {
        h = mix(h, ((t as u64) << 32) | m as u64);
    }
    h
}

/// Peak byte demand of a scenario: the largest per-frame sum of requested
/// subresource sizes over the frames that will actually be run.
///
/// Pure function of `(pack, world, scenario, frames)` — no IO, no clock — so
/// every arm derives exactly the same pool budget, and the budget is a property
/// of the workload rather than a hand-fitted constant.
pub fn peak_demand_bytes(
    world: &World,
    contents: &[&'static str],
    size: u32,
    kind: ScenarioKind,
    dt: f32,
    frames: u32,
    mip_bias: f32,
) -> u64 {
    let mut reqs: Vec<(u32, u32)> = Vec::with_capacity(4096);
    let mut peak = 0u64;
    for frame in 0..frames {
        let cam = camera(kind, frame, dt);
        requests(world, cam, mip_bias, &mut reqs);
        let sum: u64 = reqs
            .iter()
            .map(|&(t, m)| crate::pack::sub_bytes(contents[t as usize], size, m))
            .sum();
        peak = peak.max(sum);
    }
    peak
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> World {
        World::new(&[8u32; 32], 0x5EED)
    }

    #[test]
    fn requests_are_deterministic_and_sorted() {
        let w = world();
        let mut a = Vec::new();
        let mut b = Vec::new();
        for f in [0u32, 37, 991] {
            let cam = camera(ScenarioKind::Traverse, f, 1.0 / 60.0);
            requests(&w, cam, 0.0, &mut a);
            requests(&w, cam, 0.0, &mut b);
            assert_eq!(a, b);
            assert!(a.windows(2).all(|p| p[0] <= p[1]));
            assert_eq!(request_hash(&a), request_hash(&b));
        }
    }

    #[test]
    fn mip_bias_never_requests_a_finer_mip() {
        let w = world();
        let cam = camera(ScenarioKind::Traverse, 120, 1.0 / 60.0);
        let (mut sharp, mut coarse) = (Vec::new(), Vec::new());
        requests(&w, cam, 0.0, &mut sharp);
        requests(&w, cam, 1.0, &mut coarse);
        assert!(coarse.len() <= sharp.len());
    }
}

