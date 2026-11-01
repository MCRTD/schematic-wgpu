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
    // xyz: camera position; w: 1 to use the LabPBR maps.
    camera: vec4<f32>,
    // Linear colors of direct sun- or moonlight, of the sky's light in
    // shadow, and of block light at level 15.
    sunlight: vec4<f32>,
    skylight: vec4<f32>,
    block_light: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;
// LabPBR `_n` and `_s` maps laid out like the atlas: flat and plain where a
// texture has none.
@group(0) @binding(3) var normal_map: texture_2d<f32>;
@group(0) @binding(4) var specular_map: texture_2d<f32>;
@group(1) @binding(0) var shadow_map: texture_depth_2d;
@group(1) @binding(1) var shadow_sampler: sampler_comparison;

struct VertexIn {
    @location(0) position: vec3<f32>,
    // xyz: normal; w: flags, see `unpack_flags`.
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
    // Brightness from block light, 0..1.
    @location(4) block_light: f32,
};

// Minecraft's fixed directional shading: top 1.0, bottom 0.5,
// north/south 0.8, east/west 0.6, blended for rotated faces.
fn shade(n: vec3<f32>) -> f32 {
    let up = select(0.5, 1.0, n.y > 0.0);
    return n.x * n.x * 0.6 + n.z * n.z * 0.8 + n.y * n.y * up;
}

// How much sunlight reaches `world`, 0..1, using the shadow map with 3x3 PCF.
// `normal` is the geometry's and `shading` the normal-mapped one.
// `two_sided` surfaces (plants, rails) are lit from either side.
fn sunlight(world: vec3<f32>, normal: vec3<f32>, shading: vec3<f32>, two_sided: bool) -> f32 {
    if globals.sun.w == 0.0 {
        return 1.0;
    }
    var n_dot_l = dot(shading, globals.sun.xyz);
    var n = normal;
    if two_sided && dot(normal, globals.sun.xyz) < 0.0 {
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

// Light reaching a surface: sunlight where `sun` is 1, the sky's light in
// shadow, and light from light sources, at most full brightness.
fn light(sun: f32, block: f32) -> vec3<f32> {
    let shadow = mix(globals.sunlight.rgb, globals.skylight.rgb, globals.params.y);
    return min(mix(shadow, globals.sunlight.rgb, sun) + globals.block_light.rgb * block, vec3<f32>(1.0));
}

// The normal's spare byte: block light level (0..15) times 2, plus 1 for
// slice caps. Returns (level, cap).
fn unpack_flags(w: f32) -> vec2<f32> {
    let packed = round(w * 127.0);
    let cap = packed - 2.0 * floor(packed / 2.0);
    return vec2<f32>((packed - cap) / 2.0, cap);
}

// Minecraft's brightness curve for light levels.
fn brightness(level: f32) -> f32 {
    let f = level / 15.0;
    return f / (4.0 - 3.0 * f);
}

// Whether a vertex is hidden by the layer view. Slice caps, the tops of blocks
// covered by the block above, only show on the topmost layer.
fn sliced_away(in: VertexIn) -> bool {
    let cap = unpack_flags(in.normal.w).y > 0.5;
    return in.block_y > globals.slice.x || (cap && in.block_y != globals.slice.x);
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
    // Because (a * b)^2.2 == a^2.2 * b^2.2, converting each to linear gives
    // the same result once the sRGB target encodes the output again. Shade
    // is applied per fragment, where normal maps change the normal.
    let ao = mix(1.0, in.color.a, globals.params.x);
    out.tint = srgb_to_linear(in.color.rgb * ao);
    out.block_light = brightness(unpack_flags(in.normal.w).x);
    return out;
}

// A LabPBR material: https://shaderlabs.org/wiki/LabPBR_Material_Standard
struct Surface {
    // The normal with the normal map applied.
    normal: vec3<f32>,
    smoothness: f32,
    // Reflectance at normal incidence of non-metals.
    f0: f32,
    // 1 for metals, which reflect in their own colour.
    metal: f32,
    emission: f32,
    // Material ambient occlusion.
    ao: f32,
};

// LabPBR normal maps are DirectX style: green points down the texture,
// along atlas V.
const NORMAL_MAP_Y: f32 = 1.0;
// Brightness of the sun's highlight relative to its diffuse light.
const SUN_SPECULAR: f32 = 3.14159;

// Read the LabPBR maps at the fragment. Call it before any `discard`: the
// tangent frame comes from screen-space derivatives.
fn surface(in: VertexOut) -> Surface {
    let dp1 = dpdx(in.world);
    let dp2 = dpdy(in.world);
    let duv1 = dpdx(in.uv);
    let duv2 = dpdy(in.uv);
    let packed_normal = textureSample(normal_map, atlas_sampler, in.uv);
    let specular = textureSample(specular_map, atlas_sampler, in.uv);

    var s = Surface(in.normal, 0.0, 0.0, 0.0, 0.0, 1.0);
    if globals.camera.w == 0.0 {
        return s;
    }
    // Tangent and bitangent along atlas U and V, from how both change
    // across the screen ("Followup: Normal Mapping Without Precomputed
    // Tangents", Christian Schüler). That derivation has window Y pointing
    // up; here it points down, which would flip both, so the cross products
    // are swapped.
    let n = in.normal;
    let dp2perp = cross(n, dp2);
    let dp1perp = cross(dp1, n);
    let t = dp2perp * duv1.x + dp1perp * duv2.x;
    let b = dp2perp * duv1.y + dp1perp * duv2.y;
    let len = max(dot(t, t), dot(b, b));
    if len > 1e-20 {
        let xy = packed_normal.xy * 2.0 - 1.0;
        let z = sqrt(max(1.0 - dot(xy, xy), 0.0));
        let scale = inverseSqrt(len);
        s.normal = normalize(t * scale * xy.x + b * scale * xy.y * NORMAL_MAP_Y + n * z);
    }
    s.ao = packed_normal.b;
    s.smoothness = specular.r;
    // Green: F0 up to 229, predefined metals above.
    let green = specular.g * 255.0;
    s.metal = select(0.0, 1.0, green > 229.5);
    s.f0 = select(specular.g, 0.0, green > 229.5);
    // Alpha: emission up to 254; 255 means none.
    let alpha = specular.a * 255.0;
    s.emission = select(alpha / 254.0, 0.0, alpha > 254.5);
    return s;
}

// The lit colour of a fragment with base colour `albedo` (linear, tinted).
fn shaded(in: VertexOut, albedo: vec3<f32>, s: Surface, two_sided: bool) -> vec3<f32> {
    let sun = sunlight(in.world, in.normal, s.normal, two_sided);
    var color = albedo * srgb_to_linear(vec3<f32>(shade(s.normal))) * s.ao * light(sun, in.block_light);
    if globals.camera.w == 0.0 {
        return color;
    }

    let v = normalize(globals.camera.xyz - in.world);
    var n = s.normal;
    if two_sided && dot(in.normal, v) < 0.0 {
        n = -n;
    }
    let l = globals.sun.xyz;
    let f0 = mix(vec3<f32>(s.f0), albedo, s.metal);
    // Metals have little diffuse light of their own.
    color *= 1.0 - 0.75 * s.metal;

    // Sun highlight: GGX distribution, Smith-Schlick visibility, Schlick
    // Fresnel. LabPBR roughness is (1 - smoothness)^2.
    let alpha = max((1.0 - s.smoothness) * (1.0 - s.smoothness), 0.02);
    let a2 = alpha * alpha;
    let h = normalize(l + v);
    let n_l = max(dot(n, l), 0.0);
    let n_v = max(dot(n, v), 1e-3);
    let n_h = max(dot(n, h), 0.0);
    let d = a2 / (3.14159 * pow(n_h * n_h * (a2 - 1.0) + 1.0, 2.0));
    let k = alpha * 0.5;
    let visibility = 0.25 / ((n_l * (1.0 - k) + k) * (n_v * (1.0 - k) + k));
    let fresnel = f0 + (1.0 - f0) * pow(1.0 - max(dot(v, h), 0.0), 5.0);
    let sun_light = select(1.0, sun, globals.sun.w != 0.0);
    color += d * visibility * fresnel * n_l * sun_light * SUN_SPECULAR * globals.sunlight.rgb;

    // The sky reflected in smooth surfaces.
    let r = reflect(-v, n);
    let sky = mix(globals.sky_bottom.rgb, globals.sky_top.rgb, smoothstep(-0.2, 1.0, r.y));
    let sky_fresnel = f0 + (1.0 - f0) * pow(1.0 - n_v, 5.0);
    color += sky * sky_fresnel * s.smoothness * s.smoothness * s.ao;

    return color + albedo * s.emission;
}

@fragment
fn fs_opaque(in: VertexOut) -> @location(0) vec4<f32> {
    let s = surface(in);
    let texel = textureSample(atlas, atlas_sampler, in.uv);
    return vec4<f32>(shaded(in, texel.rgb * in.tint, s, false), 1.0);
}

@fragment
fn fs_cutout(in: VertexOut) -> @location(0) vec4<f32> {
    let s = surface(in);
    let texel = textureSample(atlas, atlas_sampler, in.uv);
    if texel.a < 0.5 {
        discard;
    }
    return vec4<f32>(shaded(in, texel.rgb * in.tint, s, true), 1.0);
}

@fragment
fn fs_translucent(in: VertexOut) -> @location(0) vec4<f32> {
    let s = surface(in);
    let texel = textureSample(atlas, atlas_sampler, in.uv);
    return vec4<f32>(shaded(in, texel.rgb * in.tint, s, true), texel.a);
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
    let up = vec3<f32>(0.0, 1.0, 0.0);
    let shadow = 1.0 - sunlight(in.world, up, up, false);
    let alpha = fade * shadow * globals.params.y * 0.45;
    if alpha <= 0.0 {
        discard;
    }
    return vec4<f32>(globals.sky_bottom.rgb * globals.skylight.rgb * 0.6, alpha);
}
