struct Globals {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) tint: vec3<f32>,
};

// Minecraft's fixed directional shading: top 1.0, bottom 0.5,
// north/south 0.8, east/west 0.6, blended for rotated faces.
fn shade(n: vec3<f32>) -> f32 {
    let up = select(0.5, 1.0, n.y > 0.0);
    return n.x * n.x * 0.6 + n.z * n.z * 0.8 + n.y * n.y * up;
}

@vertex
fn vs_main(in: VertexIn) -> VertexOut {
    var out: VertexOut;
    out.clip = globals.view_proj * vec4<f32>(in.position, 1.0);
    out.uv = in.uv;
    // Minecraft multiplies tint and shade in sRGB space. Because
    // (a * b)^2.2 == a^2.2 * b^2.2, converting both to linear here gives the
    // same result once the sRGB target encodes the output again.
    let s = shade(normalize(in.normal));
    out.tint = pow(in.color.rgb * s, vec3<f32>(2.2));
    return out;
}

@fragment
fn fs_opaque(in: VertexOut) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas, atlas_sampler, in.uv);
    return vec4<f32>(texel.rgb * in.tint, 1.0);
}

@fragment
fn fs_cutout(in: VertexOut) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas, atlas_sampler, in.uv);
    if texel.a < 0.5 {
        discard;
    }
    return vec4<f32>(texel.rgb * in.tint, 1.0);
}

@fragment
fn fs_translucent(in: VertexOut) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas, atlas_sampler, in.uv);
    return vec4<f32>(texel.rgb * in.tint, texel.a);
}
