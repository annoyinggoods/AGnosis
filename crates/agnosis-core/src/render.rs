//! The drawing back ends' shared piece: the fullscreen-quad shader.
//!
//! WHY the shader lives in the core: the native renderer and the web
//! renderer both blit the core's frame through the **same** WGSL, and
//! two copies of it cannot drift-check each other — a change in one
//! would silently change the picture of one front end only (audit
//! 2026-10-21 §E9).
//!
//! The **texture format** is not shared: the native renderer prefers an
//! sRGB surface (and uploads an `Rgba8UnormSrgb` texture), while the
//! browser's canvas format is not sRGB (and uploads an `Rgba8Unorm`
//! one). The shader is the same either way.

/// The WGSL the fullscreen textured quad uses.
///
/// One fullscreen triangle, the frame's texture, and a sampler.
pub const BLIT_SHADER: &str = r#"
struct VertexOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VertexOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var uvs = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );
    var out: VertexOut;
    out.pos = vec4<f32>(positions[idx], 0.0, 1.0);
    out.uv = uvs[idx];
    return out;
}

@group(0) @binding(0) var frame_tex: texture_2d<f32>;
@group(0) @binding(1) var frame_smp: sampler;

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return textureSample(frame_tex, frame_smp, in.uv);
}
"#;
