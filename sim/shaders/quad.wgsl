// The scene shader: one textured quad, sampled and drawn.
//
// Written in WGSL and compiled to SPIR-V by naga at build time (pure Rust — no
// Vulkan SDK, no glslc, no dxc). The D3D11 viewport still compiles its own HLSL
// through D3DCompile; the two are deliberately identical in behaviour, and
// unifying them onto this one source via naga's `hlsl-out` is the remaining
// step towards the plan's "one shader source, both APIs" parity requirement.

struct PushConstants {
    mvp: mat4x4<f32>,
}

var<push_constant> pc: PushConstants;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@location(0) p: vec3<f32>, @location(1) uv: vec2<f32>) -> VsOut {
    var o: VsOut;
    o.pos = pc.mvp * vec4<f32>(p, 1.0);
    o.uv = uv;
    return o;
}

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Opaque: the quads are sorted far-to-near and composite without blending,
    // exactly as the D3D11 pixel shader does.
    return vec4<f32>(textureSample(tex, smp, in.uv).rgb, 1.0);
}
