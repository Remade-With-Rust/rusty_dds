//! What the viewports draw.
//!
//! Deliberately plain: one textured quad per streamed texture, at the world
//! position the scenario already assigns it. The textures are the subject of the
//! demo, and a complicated renderer would only add variables that have nothing
//! to do with the DDS stack or the allocator (open question 2 in the plan).
//!
//! Because the geometry is derived from the same `World` the request generator
//! uses, what you see is exactly what the streamer was asked for.

use crate::gpu::math::Mat4;
use crate::scenario::{camera, ScenarioKind, World};

pub struct View {
    pub view_proj: Mat4,
    pub eye: [f32; 3],
    /// Camera basis, so quads can be billboarded to face the viewer.
    pub right: [f32; 3],
    pub up: [f32; 3],
    pub forward: [f32; 3],
}

/// One quad to draw: which texture, where, and how big.
#[derive(Debug, Clone, Copy)]
pub struct Quad {
    pub texture: u32,
    pub model: Mat4,
    /// Distance from the eye, for back-to-front ordering.
    pub distance: f32,
}

pub const QUAD_SIZE: f32 = 14.0;
const FOV_Y: f32 = std::f32::consts::FRAC_PI_3; // 60 degrees
const ZNEAR: f32 = 0.5;
const ZFAR: f32 = 400.0;

pub fn view_for(kind: ScenarioKind, frame: u32, dt: f32, aspect: f32) -> View {
    let eye = camera(kind, frame, dt);
    // Look where the camera is heading, so traversal reads as motion rather
    // than as a slideshow. One frame ahead is enough and stays deterministic.
    let ahead = camera(kind, frame + 6, dt);
    let target = if crate::gpu::math::dot(
        crate::gpu::math::sub(ahead, eye),
        crate::gpu::math::sub(ahead, eye),
    ) > 1e-4
    {
        ahead
    } else {
        [eye[0], eye[1], eye[2] + 1.0]
    };

    let proj = Mat4::perspective_lh(FOV_Y, aspect, ZNEAR, ZFAR);
    let view = Mat4::look_at_lh(eye, target, [0.0, 1.0, 0.0]);
    let (right, up, forward) = Mat4::look_at_basis(eye, target, [0.0, 1.0, 0.0]);
    View {
        view_proj: proj.mul(view),
        eye,
        right,
        up,
        forward,
    }
}

/// Quads for everything the streamer currently holds, sorted far-to-near so
/// overlapping quads composite sensibly without a depth pre-pass.
pub fn visible_quads(world: &World, view: &View, resident: &[u32], out: &mut Vec<Quad>) {
    out.clear();
    for &t in resident {
        let Some(pos) = world.positions.get(t as usize) else {
            continue;
        };
        let d = crate::gpu::math::sub(*pos, view.eye);
        let distance = crate::gpu::math::dot(d, d).sqrt();
        // Billboard: the textures ARE the subject of this demo, so every one of
        // them faces the viewer. Axis-aligned quads left most of the pack
        // edge-on — the first live frame showed two visible quads out of ~190
        // resident, one of them a one-pixel sliver.
        let (r, u, f) = (view.right, view.up, view.forward);
        let s = QUAD_SIZE;
        let model = Mat4([
            [r[0] * s, u[0] * s, f[0] * s, pos[0]],
            [r[1] * s, u[1] * s, f[1] * s, pos[1]],
            [r[2] * s, u[2] * s, f[2] * s, pos[2]],
            [0.0, 0.0, 0.0, 1.0],
        ]);
        out.push(Quad {
            texture: t,
            model,
            distance,
        });
    }
    out.sort_by(|a, b| b.distance.total_cmp(&a.distance));
}
