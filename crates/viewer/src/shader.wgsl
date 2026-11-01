struct Globals {
    view_proj: mat4x4<f32>,
    light_view_proj: mat4x4<f32>,
    // xyz: unit vector towards the sun; w: 1 when shadows are enabled.
    sun: vec4<f32>,
    // Linear RGB.
    sky_top: vec4<f32>,
    sky_bottom: vec4<f32>,
    // xyz: centre of the ground disc; w: its radius, 0 to hide it.
    ground: vec4<f32>,
    // x: ambient occlusion strength, y: shadow strength,
    // z: shadow map texel size in world units, w: texel size in UV.
    params: vec4<f32>,
    // x: world Y of the topmost block layer shown; everything above is cut away.
    slice: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;
@group(1) @binding(0) var shadow_map: texture_depth_2d;
@group(1) @binding(1) var shadow_sampler: sampler_comparison;

// Colour multiplied into surfaces the sun doesn't reach: cool, like skylight.
const SHADOW_TINT: vec3<f32> = vec3<f32>(0.46, 0.52, 0.64);

struct VertexIn {
    @location(0) position: vec3<f32>,
    // xyz: normal; w: 1 for slice caps.
    @location(1) normal: vec4<f32>,
    @location(2) uv: vec2<f32>,
    // rgb: tint; a: ambient occlusion.
    @location(3) color: vec4<f32>,
    // World Y of the block the vertex belongs to.
    @location(4) block_y: f32,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) tint: vec3<f32>,
    @location(2) world: vec3<f32>,
    @location(3) normal: vec3<f32>,
};

// Minecraft's fixed directional shading: top 1.0, bottom 0.5,
// north/south 0.8, east/west 0.6, blended for rotated faces.
fn shade(n: vec3<f32>) -> f32 {
    let up = select(0.5, 1.0, n.y > 0.0);
    return n.x * n.x * 0.6 + n.z * n.z * 0.8 + n.y * n.y * up;
}

// How much sunlight reaches `world`, 0..1, using the shadow map with 3x3 PCF.
// `two_sided` surfaces (plants, rails) are lit from either side.
fn sunlight(world: vec3<f32>, normal: vec3<f32>, two_sided: bool) -> f32 {
    if globals.sun.w == 0.0 {
        return 1.0;
    }
    var n_dot_l = dot(normal, globals.sun.xyz);
    var n = normal;
    if two_sided && n_dot_l < 0.0 {
        n_dot_l = -n_dot_l;
        n = -n;
    }
    let facing = smoothstep(0.0, 0.2, n_dot_l);
    if facing == 0.0 {
        return 0.0;
    }

    // Offset along the normal to keep surfaces from shadowing themselves.
    let p = globals.light_view_proj * vec4<f32>(world + n * globals.params.z * 1.5, 1.0);
    let ndc = p.xyz / p.w;
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    if any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) || ndc.z > 1.0 {
        return facing;
    }

    var lit = 0.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let offset = vec2<f32>(f32(x), f32(y)) * globals.params.w;
            lit += textureSampleCompareLevel(shadow_map, shadow_sampler, uv + offset, ndc.z);
        }
    }
    return facing * lit / 9.0;
}

fn light(sun: f32) -> vec3<f32> {
    let shadow = mix(vec3<f32>(1.0), SHADOW_TINT, globals.params.y);
    return mix(shadow, vec3<f32>(1.0), sun);
}

// Whether a vertex is hidden by the layer view. Slice caps, the tops of blocks
// covered by the block above, only show on the topmost layer.
fn sliced_away(in: VertexIn) -> bool {
    return in.block_y > globals.slice.x || (in.normal.w > 0.5 && in.block_y != globals.slice.x);
}

// Outside the clip volume, so triangles made of these vertices are dropped.
const CULLED: vec4<f32> = vec4<f32>(2.0, 2.0, 2.0, 1.0);

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    return pow(c, vec3<f32>(2.2));
}

@vertex
fn vs_main(in: VertexIn) -> VertexOut {
    var out: VertexOut;
    out.clip = globals.view_proj * vec4<f32>(in.position, 1.0);
    if sliced_away(in) {
        out.clip = CULLED;
    }
    out.uv = in.uv;
    out.world = in.position;
    out.normal = normalize(in.normal.xyz);
    // Minecraft multiplies tint, shade and ambient occlusion in sRGB space.
    // Because (a * b)^2.2 == a^2.2 * b^2.2, converting to linear here gives
    // the same result once the sRGB target encodes the output again.
    let ao = mix(1.0, in.color.a, globals.params.x);
    out.tint = srgb_to_linear(in.color.rgb * shade(out.normal) * ao);
    return out;
}

@fragment
fn fs_opaque(in: VertexOut) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas, atlas_sampler, in.uv);
    let sun = sunlight(in.world, in.normal, false);
    return vec4<f32>(texel.rgb * in.tint * light(sun), 1.0);
}

@fragment
fn fs_cutout(in: VertexOut) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas, atlas_sampler, in.uv);
    if texel.a < 0.5 {
        discard;
    }
    let sun = sunlight(in.world, in.normal, true);
    return vec4<f32>(texel.rgb * in.tint * light(sun), 1.0);
}

@fragment
fn fs_translucent(in: VertexOut) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas, atlas_sampler, in.uv);
    let sun = sunlight(in.world, in.normal, true);
    return vec4<f32>(texel.rgb * in.tint * light(sun), texel.a);
}

// Shadow map pass: depth only, seen from the sun.

struct ShadowOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_shadow(in: VertexIn) -> ShadowOut {
    var out: ShadowOut;
    out.clip = globals.light_view_proj * vec4<f32>(in.position, 1.0);
    if sliced_away(in) {
        out.clip = CULLED;
    }
    out.uv = in.uv;
    return out;
}

@fragment
fn fs_shadow_cutout(in: ShadowOut) {
    if textureSample(atlas, atlas_sampler, in.uv).a < 0.5 {
        discard;
    }
}

// Sky: a full-screen triangle with a vertical gradient, drawn first.

struct SkyOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) height: f32,
};

@vertex
fn vs_sky(@builtin(vertex_index) i: u32) -> SkyOut {
    let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u)) * 2.0 - 1.0;
    var out: SkyOut;
    out.clip = vec4<f32>(xy, 0.0, 1.0);
    out.height = xy.y * 0.5 + 0.5;
    return out;
}

@fragment
fn fs_sky(in: SkyOut) -> @location(0) vec4<f32> {
    let t = smoothstep(0.0, 1.0, in.height);
    return vec4<f32>(mix(globals.sky_bottom.rgb, globals.sky_top.rgb, t), 1.0);
}

// Ground: an invisible disc under the build that only shows the shadow cast on
// it, fading out towards its edge, so it never draws a visible horizon.

struct GroundOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
};

@vertex
fn vs_ground(@builtin(vertex_index) i: u32) -> GroundOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(-1.0, 1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(1.0, -1.0),
    );
    let c = corners[i] * globals.ground.w;
    let world = globals.ground.xyz + vec3<f32>(c.x, 0.0, c.y);
    var out: GroundOut;
    out.clip = globals.view_proj * vec4<f32>(world, 1.0);
    out.world = world;
    return out;
}

@fragment
fn fs_ground(in: GroundOut) -> @location(0) vec4<f32> {
    let d = length(in.world.xz - globals.ground.xz) / globals.ground.w;
    let fade = 1.0 - smoothstep(0.45, 1.0, d);
    let shadow = 1.0 - sunlight(in.world, vec3<f32>(0.0, 1.0, 0.0), false);
    let alpha = fade * shadow * globals.params.y * 0.45;
    if alpha <= 0.0 {
        discard;
    }
    return vec4<f32>(globals.sky_bottom.rgb * SHADOW_TINT * 0.6, alpha);
}
