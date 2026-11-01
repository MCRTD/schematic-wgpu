//! Blocks Minecraft draws with code instead of block models (block entity
//! renderers): banners, chests, shulker boxes, beds, signs and mob heads.
//! Their shapes are the game's entity models: boxes unwrapped onto a texture
//! sheet, placed with the same transforms its renderers use.
//!
//! Newer versions model beds and signs as regular blocks; these shapes are
//! only used when the assets have no geometry for a block.

use std::f32::consts::{FRAC_PI_2, PI};
use std::sync::Arc;

use glam::{Mat4, Vec3};
use litematic::{BlockState, Nbt};
use mc_assets::Texture;

use crate::bake::Quad;

/// A box of an entity model in model pixels, with the top-left corner of its
/// unwrapped faces on the texture sheet.
struct Cube {
    tex: [f32; 2],
    min: [f32; 3],
    size: [f32; 3],
    /// Grown on every side without changing the UVs, e.g. a head's hat layer.
    grow: f32,
}

const fn cube(tex: [f32; 2], min: [f32; 3], size: [f32; 3]) -> Cube {
    Cube { tex, min, size, grow: 0.0 }
}

/// The 16 dye colors as used to tint banner layers, in dye id order.
const DYES: [(&str, u32); 16] = [
    ("white", 0xF9FFFE),
    ("orange", 0xF9801D),
    ("magenta", 0xC74EBD),
    ("light_blue", 0x3AB3DA),
    ("yellow", 0xFED83D),
    ("lime", 0x80C71F),
    ("pink", 0xF38BAA),
    ("gray", 0x474F52),
    ("light_gray", 0x9D9D97),
    ("cyan", 0x169C9C),
    ("purple", 0x8932B8),
    ("blue", 0x3C44AA),
    ("brown", 0x835432),
    ("green", 0x5E7C16),
    ("red", 0xB02E26),
    ("black", 0x1D1D21),
];

/// Banner pattern codes saved before 1.20.5, and their texture names.
const PATTERN_CODES: [(&str, &str); 41] = [
    ("b", "base"),
    ("bl", "square_bottom_left"),
    ("br", "square_bottom_right"),
    ("tl", "square_top_left"),
    ("tr", "square_top_right"),
    ("bs", "stripe_bottom"),
    ("ts", "stripe_top"),
    ("ls", "stripe_left"),
    ("rs", "stripe_right"),
    ("cs", "stripe_center"),
    ("ms", "stripe_middle"),
    ("drs", "stripe_downright"),
    ("dls", "stripe_downleft"),
    ("ss", "small_stripes"),
    ("cr", "cross"),
    ("sc", "straight_cross"),
    ("bt", "triangle_bottom"),
    ("tt", "triangle_top"),
    ("bts", "triangles_bottom"),
    ("tts", "triangles_top"),
    ("ld", "diagonal_left"),
    ("rd", "diagonal_up_right"),
    ("lud", "diagonal_up_left"),
    ("rud", "diagonal_right"),
    ("mc", "circle"),
    ("mr", "rhombus"),
    ("vh", "half_vertical"),
    ("hh", "half_horizontal"),
    ("vhr", "half_vertical_right"),
    ("hhb", "half_horizontal_bottom"),
    ("bo", "border"),
    ("cbo", "curly_border"),
    ("gra", "gradient"),
    ("gru", "gradient_up"),
    ("bri", "bricks"),
    ("glb", "globe"),
    ("cre", "creeper"),
    ("sku", "skull"),
    ("flo", "flower"),
    ("moj", "mojang"),
    ("pig", "piglin"),
];

/// The banner sheet moved into `entity/banner/` in 1.21.5.
const BANNER_BASE: &str = "minecraft:entity/banner/banner_base,minecraft:entity/banner_base";

/// Quads for a block drawn by a block entity renderer, or `None` if `state`
/// isn't one. `nbt` is the block entity's data, for banner patterns.
/// Textures may be specs for [`compose`], starting with `+`.
pub(crate) fn quads(state: &BlockState, nbt: Option<&Nbt>) -> Option<Vec<Quad>> {
    let name = state.short_name();
    let props = &state.properties;
    let facing = props.get("facing").map_or("south", String::as_str);
    let rotation = props.get("rotation").and_then(|r| r.parse::<f32>().ok()).unwrap_or(0.0) * 22.5;
    let mut out = Vec::new();

    if let Some(color) = name.strip_suffix("_wall_banner").and_then(dye) {
        banner(&mut out, color, nbt, Some(facing), 0.0);
    } else if let Some(color) = name.strip_suffix("_banner").and_then(dye) {
        banner(&mut out, color, nbt, None, rotation);
    } else if let Some(texture) = chest_texture(name) {
        let kind = props.get("type").map_or("single", String::as_str);
        chest(&mut out, &texture, facing, kind);
    } else if name == "shulker_box" || name.strip_suffix("_shulker_box").is_some_and(|c| dye(c).is_some()) {
        let color = name.strip_suffix("_shulker_box").map_or(String::new(), |c| format!("_{c}"));
        let facing = props.get("facing").map_or("up", String::as_str);
        shulker_box(&mut out, &format!("minecraft:entity/shulker/shulker{color}"), facing);
    } else if let Some(color) = name.strip_suffix("_bed").filter(|c| dye(c).is_some()) {
        let head = props.get("part").is_some_and(|p| p == "head");
        bed(&mut out, &format!("minecraft:entity/bed/{color}"), facing, head);
    } else if name.ends_with("_hanging_sign") {
        return None;
    } else if let Some(wood) = name.strip_suffix("_wall_sign") {
        sign(&mut out, &format!("minecraft:entity/signs/{wood}"), Some(facing), 0.0);
    } else if let Some(wood) = name.strip_suffix("_sign") {
        sign(&mut out, &format!("minecraft:entity/signs/{wood}"), None, rotation);
    } else if let Some(head) = head_kind(name) {
        let wall = name.contains("_wall_");
        skull(&mut out, head, wall.then_some(facing), rotation);
    } else {
        return None;
    }
    Some(out)
}

/// A key that tells apart blocks of the same state whose block entity data
/// changes how they look, i.e. banners with patterns; `None` if it doesn't.
pub(crate) fn variant_key(state: &BlockState, nbt: &Nbt) -> Option<String> {
    let patterns = state.short_name().ends_with("_banner").then(|| banner_patterns(nbt))?;
    (!patterns.is_empty()).then(|| patterns.iter().map(|(p, c)| format!("{p}*{c:06x}|")).collect())
}

fn dye(name: &str) -> Option<u32> {
    DYES.iter().find(|(n, _)| *n == name).map(|&(_, c)| c)
}

fn dye_id(id: i32) -> Option<u32> {
    DYES.get(usize::try_from(id).ok()?).map(|&(_, c)| c)
}

/// Rotation of a horizontal facing around Y, in degrees, as Minecraft's
/// `Direction::toYRot`.
fn y_rot(facing: &str) -> f32 {
    match facing {
        "west" => 90.0,
        "north" => 180.0,
        "east" => 270.0,
        _ => 0.0,
    }
}

fn translate(x: f32, y: f32, z: f32) -> Mat4 {
    Mat4::from_translation(Vec3::new(x, y, z))
}

fn scale(x: f32, y: f32, z: f32) -> Mat4 {
    Mat4::from_scale(Vec3::new(x, y, z))
}

fn rotate_y(degrees: f32) -> Mat4 {
    Mat4::from_rotation_y(degrees.to_radians())
}

/// A model part's pose: offset in pixels, then rotations in radians applied
/// Z, Y, X, as `ModelPart::translateAndRotate`.
fn pose(offset: [f32; 3], [x, y, z]: [f32; 3]) -> Mat4 {
    Mat4::from_translation(Vec3::from(offset) / 16.0)
        * Mat4::from_rotation_z(z)
        * Mat4::from_rotation_y(y)
        * Mat4::from_rotation_x(x)
}

/// Add the six faces of each cube, as Minecraft's `ModelPart.Cube` lays them
/// out, transformed by `m` from model pixels to block units.
fn push(out: &mut Vec<Quad>, texture: &str, [tw, th]: [f32; 2], m: Mat4, cubes: &[Cube]) {
    for c in cubes {
        let ([x0, y0, z0], [w, h, d], [u, v], g) = (c.min, c.size, c.tex, c.grow);
        let (x1, y1, z1) = (x0 + w + g, y0 + h + g, z0 + d + g);
        let (x0, y0, z0) = (x0 - g, y0 - g, z0 - g);
        let p = [
            [x0, y0, z0],
            [x1, y0, z0],
            [x1, y1, z0],
            [x0, y1, z0],
            [x0, y0, z1],
            [x1, y0, z1],
            [x1, y1, z1],
            [x0, y1, z1],
        ];
        let us = [u, u + d, u + d + w, u + d + w + w, u + d + w + d, u + d + w + d + w];
        let vs = [v, v + d, v + d + h];
        // Corners counter-clockwise from outside, and the UV rectangle.
        let faces = [
            ([5, 4, 0, 1], [us[1], vs[0], us[2], vs[1]]),
            ([2, 3, 7, 6], [us[2], vs[1], us[3], vs[0]]),
            ([0, 4, 7, 3], [us[0], vs[1], us[1], vs[2]]),
            ([1, 0, 3, 2], [us[1], vs[1], us[2], vs[2]]),
            ([5, 1, 2, 6], [us[2], vs[1], us[4], vs[2]]),
            ([4, 5, 6, 7], [us[4], vs[1], us[5], vs[2]]),
        ];
        for (corners, [u1, v1, u2, v2]) in faces {
            // Minecraft gives the corners (u2, v1), (u1, v1), (u1, v2), (u2, v2);
            // quads start at the top left.
            let mut positions = [1, 2, 3, 0].map(|k| m.transform_point3(Vec3::from(p[corners[k]]) / 16.0));
            let mut uvs = [[u1 / tw, v1 / th], [u1 / tw, v2 / th], [u2 / tw, v2 / th], [u2 / tw, v1 / th]];
            if m.determinant() < 0.0 {
                positions.reverse();
                uvs.reverse();
            }
            let normal = (positions[1] - positions[0]).cross(positions[2] - positions[0]).normalize_or_zero();
            if normal == Vec3::ZERO {
                continue;
            }
            out.push(Quad {
                positions,
                uvs,
                normal,
                cullface: None,
                texture: Some(texture.to_owned()),
                tinted: false,
                ambient_occlusion: false,
            });
        }
    }
}

/// `facing` is set for wall banners; `rotation` (degrees) for standing ones.
fn banner(out: &mut Vec<Quad>, color: u32, nbt: Option<&Nbt>, facing: Option<&str>, rotation: f32) {
    let place = match facing {
        Some(facing) => translate(0.5, -1.0 / 6.0, 0.5) * rotate_y(-y_rot(facing)) * translate(0.0, -0.3125, -0.4375),
        None => translate(0.5, 0.5, 0.5) * rotate_y(-rotation),
    };
    let m = place * scale(2.0 / 3.0, -2.0 / 3.0, -2.0 / 3.0);

    // The flag is the sheet, then the base color, then each pattern on top.
    let mut flag = format!("+{BANNER_BASE}|minecraft:entity/banner/base*{color:06x}");
    for (pattern, color) in nbt.map(banner_patterns).unwrap_or_default() {
        flag += &format!("|{pattern}*{color:06x}");
    }
    push(out, &flag, [64.0, 64.0], m * pose([0.0, -32.0, 0.0], [0.0; 3]), &[cube([0.0, 0.0], [-10.0, 0.0, -2.0], [20.0, 40.0, 1.0])]);
    let mut sticks = vec![cube([0.0, 42.0], [-10.0, -32.0, -1.0], [20.0, 2.0, 2.0])];
    if facing.is_none() {
        sticks.push(cube([44.0, 0.0], [-1.0, -30.0, -1.0], [2.0, 42.0, 2.0]));
    }
    push(out, &format!("+{BANNER_BASE}"), [64.0, 64.0], m, &sticks);
}

/// Pattern textures and colors of a banner, bottom layer first. Reads both
/// the format since 1.20.5 (`patterns`, names) and the older one
/// (`Patterns`, codes and dye ids).
fn banner_patterns(nbt: &Nbt) -> Vec<(String, u32)> {
    let Nbt::Compound(c) = nbt else { return Vec::new() };
    let Some(Nbt::List(layers)) = c.get("patterns").or_else(|| c.get("Patterns")) else { return Vec::new() };
    let string = |v: Option<&Nbt>| match v {
        Some(Nbt::String(s)) => Some(s.clone()),
        _ => None,
    };
    layers
        .iter()
        .filter_map(|layer| {
            let Nbt::Compound(layer) = layer else { return None };
            let pattern = match layer.get("pattern").or_else(|| layer.get("Pattern"))? {
                Nbt::String(s) => s.clone(),
                // An inline pattern definition.
                Nbt::Compound(def) => string(def.get("asset_id"))?,
                _ => return None,
            };
            let texture = match PATTERN_CODES.iter().find(|(code, _)| *code == pattern) {
                Some((_, name)) => format!("minecraft:entity/banner/{name}"),
                None => {
                    let (ns, path) = pattern.split_once(':').unwrap_or(("minecraft", &pattern));
                    format!("{ns}:entity/banner/{path}")
                }
            };
            let color = match layer.get("color").or_else(|| layer.get("Color"))? {
                Nbt::String(name) => dye(name)?,
                Nbt::Int(id) => dye_id(*id)?,
                Nbt::Byte(id) => dye_id(*id as i32)?,
                _ => return None,
            };
            Some((texture, color))
        })
        .take(16)
        .collect()
}

fn chest_texture(name: &str) -> Option<String> {
    let texture = match name.strip_prefix("waxed_").unwrap_or(name) {
        "chest" => "normal",
        "trapped_chest" => "trapped",
        "ender_chest" => "ender",
        "copper_chest" => "copper",
        "exposed_copper_chest" => "copper_exposed",
        "weathered_copper_chest" => "copper_weathered",
        "oxidized_copper_chest" => "copper_oxidized",
        _ => return None,
    };
    Some(format!("minecraft:entity/chest/{texture}"))
}

/// `kind` is the chest's `type`: `single`, or the `left` or `right` half of a
/// double chest, which use their own textures.
fn chest(out: &mut Vec<Quad>, texture: &str, facing: &str, kind: &str) {
    let (texture, [bottom, lid, lock]) = match kind {
        "left" => (
            format!("{texture}_left"),
            [
                cube([0.0, 19.0], [0.0, 0.0, 1.0], [15.0, 10.0, 14.0]),
                cube([0.0, 0.0], [0.0, 0.0, 0.0], [15.0, 5.0, 14.0]),
                cube([0.0, 0.0], [0.0, -1.0, 15.0], [1.0, 4.0, 1.0]),
            ],
        ),
        "right" => (
            format!("{texture}_right"),
            [
                cube([0.0, 19.0], [1.0, 0.0, 1.0], [15.0, 10.0, 14.0]),
                cube([0.0, 0.0], [1.0, 0.0, 0.0], [15.0, 5.0, 14.0]),
                cube([0.0, 0.0], [15.0, -1.0, 15.0], [1.0, 4.0, 1.0]),
            ],
        ),
        _ => (
            texture.to_owned(),
            [
                cube([0.0, 19.0], [1.0, 0.0, 1.0], [14.0, 10.0, 14.0]),
                cube([0.0, 0.0], [1.0, 0.0, 0.0], [14.0, 5.0, 14.0]),
                cube([0.0, 0.0], [7.0, -1.0, 15.0], [2.0, 4.0, 1.0]),
            ],
        ),
    };
    let m = translate(0.5, 0.5, 0.5) * rotate_y(-y_rot(facing)) * translate(-0.5, -0.5, -0.5);
    let size = [64.0, 64.0];
    push(out, &texture, size, m, &[bottom]);
    push(out, &texture, size, m * pose([0.0, 9.0, 1.0], [0.0; 3]), &[lid]);
    push(out, &texture, size, m * pose([0.0, 8.0, 0.0], [0.0; 3]), &[lock]);
}

fn shulker_box(out: &mut Vec<Quad>, texture: &str, facing: &str) {
    // `Direction::getRotation`: turns the model's up towards `facing`.
    let x90 = Mat4::from_rotation_x(FRAC_PI_2);
    let rotation = match facing {
        "down" => Mat4::from_rotation_x(PI),
        "north" => x90 * Mat4::from_rotation_z(PI),
        "south" => x90,
        "west" => x90 * Mat4::from_rotation_z(FRAC_PI_2),
        "east" => x90 * Mat4::from_rotation_z(-FRAC_PI_2),
        _ => Mat4::IDENTITY,
    };
    let m = translate(0.5, 0.5, 0.5)
        * Mat4::from_scale(Vec3::splat(0.9995))
        * rotation
        * scale(1.0, -1.0, -1.0)
        * translate(0.0, -1.0, 0.0)
        * pose([0.0, 24.0, 0.0], [0.0; 3]);
    let cubes = [
        cube([0.0, 28.0], [-8.0, -8.0, -8.0], [16.0, 8.0, 16.0]),
        cube([0.0, 0.0], [-8.0, -16.0, -8.0], [16.0, 12.0, 16.0]),
    ];
    push(out, texture, [64.0, 64.0], m, &cubes);
}

/// One half of a bed; `facing` points from the foot to the head.
fn bed(out: &mut Vec<Quad>, texture: &str, facing: &str, head: bool) {
    let m = translate(0.0, 0.5625, 0.0)
        * Mat4::from_rotation_x(FRAC_PI_2)
        * translate(0.5, 0.5, 0.5)
        * Mat4::from_rotation_z((180.0 + y_rot(facing)).to_radians())
        * translate(-0.5, -0.5, -0.5);
    let size = [64.0, 64.0];
    let leg = |tex, min| [cube(tex, min, [3.0, 3.0, 3.0])];
    if head {
        push(out, texture, size, m, &[cube([0.0, 0.0], [0.0; 3], [16.0, 16.0, 6.0])]);
        push(out, texture, size, m * pose([0.0; 3], [FRAC_PI_2, 0.0, FRAC_PI_2]), &leg([50.0, 6.0], [0.0, 6.0, 0.0]));
        push(out, texture, size, m * pose([0.0; 3], [FRAC_PI_2, 0.0, PI]), &leg([50.0, 18.0], [-16.0, 6.0, 0.0]));
    } else {
        push(out, texture, size, m, &[cube([0.0, 22.0], [0.0; 3], [16.0, 16.0, 6.0])]);
        push(out, texture, size, m * pose([0.0; 3], [FRAC_PI_2, 0.0, 0.0]), &leg([50.0, 0.0], [0.0, 6.0, -16.0]));
        let right = pose([0.0; 3], [FRAC_PI_2, 0.0, 1.5 * PI]);
        push(out, texture, size, m * right, &leg([50.0, 12.0], [-16.0, 6.0, -16.0]));
    }
}

/// `facing` is set for wall signs; `rotation` (degrees) for standing ones.
fn sign(out: &mut Vec<Quad>, texture: &str, facing: Option<&str>, rotation: f32) {
    let place = match facing {
        Some(facing) => translate(0.5, 0.5, 0.5) * rotate_y(-y_rot(facing)) * translate(0.0, -0.3125, -0.4375),
        None => translate(0.5, 0.5, 0.5) * rotate_y(-rotation),
    };
    let m = place * scale(2.0 / 3.0, -2.0 / 3.0, -2.0 / 3.0);
    let mut cubes = vec![cube([0.0, 0.0], [-12.0, -14.0, -1.0], [24.0, 12.0, 2.0])];
    if facing.is_none() {
        cubes.push(cube([0.0, 14.0], [-1.0, -2.0, -1.0], [2.0, 14.0, 2.0]));
    }
    push(out, texture, [64.0, 32.0], m, &cubes);
}

#[derive(Clone, Copy)]
enum Head {
    /// A plain 8x8x8 head on a 64x32 sheet.
    Mob(&'static str),
    /// A head with a hat layer on a 64x64 sheet.
    Humanoid(&'static str),
    Piglin,
}

fn head_kind(name: &str) -> Option<Head> {
    Some(match name {
        "skeleton_skull" | "skeleton_wall_skull" => Head::Mob("minecraft:entity/skeleton/skeleton"),
        "wither_skeleton_skull" | "wither_skeleton_wall_skull" => Head::Mob("minecraft:entity/skeleton/wither_skeleton"),
        "creeper_head" | "creeper_wall_head" => Head::Mob("minecraft:entity/creeper/creeper"),
        "zombie_head" | "zombie_wall_head" => Head::Humanoid("minecraft:entity/zombie/zombie"),
        // The default skin; custom skins would need Mojang's profile servers.
        "player_head" | "player_wall_head" => Head::Humanoid("+minecraft:entity/player/wide/steve,minecraft:entity/steve"),
        "piglin_head" | "piglin_wall_head" => Head::Piglin,
        _ => return None,
    })
}

/// `facing` is set for wall heads; `rotation` (degrees) for standing ones.
fn skull(out: &mut Vec<Quad>, head: Head, facing: Option<&str>, rotation: f32) {
    let (place, y) = match facing {
        Some(facing) => {
            let (dx, dz) = match facing {
                "north" => (0.0, -1.0),
                "south" => (0.0, 1.0),
                "west" => (-1.0, 0.0),
                _ => (1.0, 0.0),
            };
            // Wall heads look away from the wall.
            (translate(0.5 - dx * 0.25, 0.25, 0.5 - dz * 0.25), y_rot(facing) + 180.0)
        }
        None => (translate(0.5, 0.0, 0.5), rotation),
    };
    let m = place * scale(-1.0, -1.0, 1.0) * pose([0.0; 3], [0.0, y.to_radians(), 0.0]);
    let skull = cube([0.0, 0.0], [-4.0, -8.0, -4.0], [8.0, 8.0, 8.0]);
    match head {
        Head::Mob(texture) => push(out, texture, [64.0, 32.0], m, &[skull]),
        Head::Humanoid(texture) => {
            let hat = Cube { tex: [32.0, 0.0], grow: 0.25, ..skull };
            push(out, texture, [64.0, 64.0], m, &[skull, hat]);
        }
        Head::Piglin => {
            let texture = "minecraft:entity/piglin/piglin";
            let size = [64.0, 64.0];
            let cubes = [
                cube([0.0, 0.0], [-5.0, -8.0, -4.0], [10.0, 8.0, 8.0]),
                cube([31.0, 1.0], [-2.0, -4.0, -5.0], [4.0, 4.0, 1.0]),
                cube([2.0, 4.0], [2.0, -2.0, -5.0], [1.0, 2.0, 1.0]),
                cube([2.0, 0.0], [-3.0, -2.0, -5.0], [1.0, 2.0, 1.0]),
            ];
            push(out, texture, size, m, &cubes);
            // Ears as the head renderer leaves them when still.
            let left = m * pose([4.5, -6.0, 0.0], [0.0, 0.0, -0.7]);
            push(out, texture, size, left, &[cube([51.0, 6.0], [0.0, 0.0, -2.0], [1.0, 5.0, 4.0])]);
            let right = m * pose([-4.5, -6.0, 0.0], [0.0, 0.0, 0.7]);
            push(out, texture, size, right, &[cube([39.0, 6.0], [-1.0, 0.0, -2.0], [1.0, 5.0, 4.0])]);
        }
    }
}

/// Load a texture spec made by [`quads`]: after the `+`, layers separated by
/// `|`. Each layer lists alternative locations separated by `,`, the first
/// that exists being used, and may end in `*rrggbb` to multiply it with a
/// color. Later layers are alpha blended over the first. Missing locations
/// are added to `missing`; without the first layer there is no texture.
pub(crate) fn compose(
    spec: &str,
    mut load: impl FnMut(&str) -> Option<Arc<Texture>>,
    missing: &mut Vec<String>,
) -> Option<Texture> {
    let mut result: Option<(u32, u32, Vec<u8>)> = None;
    for layer in spec.split('|') {
        let (locations, tint) = match layer.split_once('*') {
            Some((l, t)) => (l, u32::from_str_radix(t, 16).ok()),
            None => (layer, None),
        };
        let Some(texture) = locations.split(',').find_map(&mut load) else {
            missing.push(locations.split(',').next().unwrap_or_default().to_owned());
            // Without the first layer there is nothing to draw on.
            result.as_ref()?;
            continue;
        };
        let tint = tint.map_or([255; 3], |c| [(c >> 16) as u8, (c >> 8) as u8, c as u8]);
        let (width, height, rgba) = result.get_or_insert_with(|| (texture.width, texture.height, vec![0; texture.rgba.len()]));
        for y in 0..*height {
            for x in 0..*width {
                // Layers of other sizes (e.g. from HD packs) are scaled to the first.
                let (sx, sy) = (x * texture.width / *width, y * texture.height / *height);
                let src = &texture.rgba[((sy * texture.width + sx) * 4) as usize..][..4];
                let dst = &mut rgba[((y * *width + x) * 4) as usize..][..4];
                let a = src[3] as u32;
                for k in 0..3 {
                    let s = src[k] as u32 * tint[k] as u32 / 255;
                    dst[k] = ((s * a + dst[k] as u32 * (255 - a)) / 255) as u8;
                }
                dst[3] = (a + dst[3] as u32 * (255 - a) / 255) as u8;
            }
        }
    }
    result.map(|(width, height, rgba)| Texture::from_rgba(width, height, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn state(name: &str, props: &[(&str, &str)]) -> BlockState {
        BlockState {
            name: format!("minecraft:{name}"),
            properties: props.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<BTreeMap<_, _>>(),
        }
    }

    fn bounds(quads: &[Quad]) -> (Vec3, Vec3) {
        let points = quads.iter().flat_map(|q| q.positions);
        points.fold((Vec3::MAX, Vec3::MIN), |(lo, hi), p| (lo.min(p), hi.max(p)))
    }

    fn close(a: Vec3, b: [f32; 3]) -> bool {
        (a - Vec3::from(b)).abs().max_element() < 1e-3
    }

    #[test]
    fn chest_fills_its_block_minus_the_rim() {
        let quads = quads(&state("chest", &[("facing", "north"), ("type", "single")]), None).unwrap();
        // Bottom, lid and lock: three boxes of six faces.
        assert_eq!(quads.len(), 18);
        let (lo, hi) = bounds(&quads);
        assert!(close(lo, [1.0 / 16.0, 0.0, 0.0]) && close(hi, [15.0 / 16.0, 14.0 / 16.0, 15.0 / 16.0]), "{lo} {hi}");
        // Facing north, the lock sticks out of the north side.
        let lock = &quads[12..];
        assert!(lock.iter().all(|q| q.positions.iter().all(|p| p.z < 1.5 / 16.0)));
        assert!(quads.iter().all(|q| q.texture.as_deref() == Some("minecraft:entity/chest/normal")));
    }

    #[test]
    fn faces_point_outwards() {
        for s in [
            state("red_bed", &[("facing", "east"), ("part", "head")]),
            state("oak_wall_sign", &[("facing", "west")]),
            state("white_banner", &[("rotation", "5")]),
            state("blue_shulker_box", &[("facing", "down")]),
            state("piglin_wall_head", &[("facing", "south")]),
        ] {
            let quads = quads(&s, None).unwrap();
            for chunk in quads.chunks(6) {
                let centre = chunk.iter().flat_map(|q| q.positions).sum::<Vec3>() / 24.0;
                for q in chunk {
                    let face = q.positions.iter().sum::<Vec3>() / 4.0;
                    assert!(q.normal.dot(face - centre) > 0.0, "{} {q:?}", s.name);
                }
            }
        }
    }

    #[test]
    fn bed_halves_sit_on_their_legs() {
        let foot = quads(&state("red_bed", &[("facing", "south"), ("part", "foot")]), None).unwrap();
        let (lo, hi) = bounds(&foot);
        assert!(close(lo, [0.0; 3]) && close(hi, [1.0, 9.0 / 16.0, 1.0]), "{lo} {hi}");
    }

    #[test]
    fn heads_rest_on_the_floor_or_hang_on_the_wall() {
        let floor = quads(&state("skeleton_skull", &[("rotation", "0")]), None).unwrap();
        let (lo, hi) = bounds(&floor);
        assert!(close(lo, [0.25, 0.0, 0.25]) && close(hi, [0.75, 0.5, 0.75]), "{lo} {hi}");
        // A head on a north-facing wall hangs against the south side.
        let wall = quads(&state("zombie_wall_head", &[("facing", "north")]), None).unwrap();
        let (_, hi) = bounds(&wall);
        assert!(hi.z > 1.0 - 1e-3, "{hi}");
    }

    #[test]
    fn hanging_signs_and_other_blocks_are_left_alone() {
        assert!(quads(&state("oak_hanging_sign", &[]), None).is_none());
        assert!(quads(&state("stone", &[]), None).is_none());
        assert!(quads(&state("dragon_head", &[]), None).is_none());
    }

    #[test]
    fn banner_patterns_in_both_formats() {
        let layer = |pattern: Nbt, color: Nbt, new: bool| {
            let (p, c) = if new { ("pattern", "color") } else { ("Pattern", "Color") };
            Nbt::Compound([(p.into(), pattern), (c.into(), color)].into())
        };
        let new = Nbt::Compound(
            [(
                "patterns".into(),
                Nbt::List(vec![layer(Nbt::String("minecraft:border".into()), Nbt::String("red".into()), true)]),
            )]
            .into(),
        );
        let old = Nbt::Compound(
            [("Patterns".into(), Nbt::List(vec![layer(Nbt::String("bo".into()), Nbt::Int(14), false)]))].into(),
        );
        let expected = [("minecraft:entity/banner/border".to_string(), 0xB02E26)];
        assert_eq!(banner_patterns(&new), expected);
        assert_eq!(banner_patterns(&old), expected);

        let banner = state("blue_wall_banner", &[("facing", "north")]);
        assert!(variant_key(&banner, &new).is_some());
        assert!(variant_key(&banner, &Nbt::Compound(Default::default())).is_none());
        let flag = &quads(&banner, Some(&new)).unwrap()[0];
        assert!(flag.texture.as_deref().unwrap().ends_with("|minecraft:entity/banner/border*b02e26"));
    }

    #[test]
    fn compose_tints_and_blends_layers() {
        let solid = |rgba: [u8; 4]| Arc::new(Texture::from_rgba(2, 2, rgba.repeat(4)));
        let load = |name: &str| match name {
            "a" => Some(solid([255, 255, 255, 255])),
            "half" => Some(solid([255, 255, 255, 128])),
            _ => None,
        };
        let mut missing = Vec::new();
        let t = compose("nope,a*ff0000|half*0000ff|gone", load, &mut missing).unwrap();
        // Red under half-transparent blue.
        assert_eq!(&t.rgba[..4], &[127, 0, 128, 255]);
        assert_eq!(missing, ["gone"]);
        assert!(compose("gone", load, &mut missing).is_none());
    }
}
