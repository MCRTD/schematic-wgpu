//! Block models: `assets/<ns>/models/<path>.json`.

use std::collections::HashMap;

use serde::Deserialize;

/// A cube face direction. North is -Z, east is +X, up is +Y.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    #[serde(alias = "bottom")]
    Down,
    Up,
    North,
    South,
    West,
    East,
}

impl Direction {
    pub const ALL: [Direction; 6] = [
        Direction::Down,
        Direction::Up,
        Direction::North,
        Direction::South,
        Direction::West,
        Direction::East,
    ];

    pub fn normal(self) -> [i32; 3] {
        match self {
            Direction::Down => [0, -1, 0],
            Direction::Up => [0, 1, 0],
            Direction::North => [0, 0, -1],
            Direction::South => [0, 0, 1],
            Direction::West => [-1, 0, 0],
            Direction::East => [1, 0, 0],
        }
    }

    /// The direction whose normal is closest to `n`.
    pub fn from_normal(n: [f32; 3]) -> Direction {
        let dot = |d: Direction| {
            let m = d.normal();
            n[0] * m[0] as f32 + n[1] * m[1] as f32 + n[2] * m[2] as f32
        };
        Direction::ALL.into_iter().max_by(|a, b| dot(*a).total_cmp(&dot(*b))).unwrap()
    }

    pub fn opposite(self) -> Direction {
        match self {
            Direction::Down => Direction::Up,
            Direction::Up => Direction::Down,
            Direction::North => Direction::South,
            Direction::South => Direction::North,
            Direction::West => Direction::East,
            Direction::East => Direction::West,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    X,
    Y,
    Z,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ElementRotation {
    pub origin: [f32; 3],
    pub axis: Axis,
    pub angle: f32,
    #[serde(default)]
    pub rescale: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct RawFace {
    uv: Option<[f32; 4]>,
    texture: String,
    cullface: Option<Direction>,
    #[serde(default)]
    rotation: i32,
    tintindex: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawElement {
    from: [f32; 3],
    to: [f32; 3],
    rotation: Option<ElementRotation>,
    #[serde(default = "yes")]
    shade: bool,
    #[serde(default)]
    faces: HashMap<Direction, RawFace>,
}

fn yes() -> bool {
    true
}

/// Texture map values are usually strings; newer versions also allow
/// `{"sprite": "...", ...}` objects.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum RawTextureRef {
    Name(String),
    Sprite { sprite: String },
}

impl RawTextureRef {
    fn as_str(&self) -> &str {
        match self {
            RawTextureRef::Name(s) | RawTextureRef::Sprite { sprite: s } => s,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RawModel {
    pub parent: Option<String>,
    #[serde(rename = "ambientocclusion")]
    pub ambient_occlusion: Option<bool>,
    #[serde(default)]
    textures: HashMap<String, RawTextureRef>,
    elements: Option<Vec<RawElement>>,
    /// A (Neo)Forge model loader, e.g. `neoforge:obj` or `forge:composite`.
    pub loader: Option<String>,
    /// The `.obj` file of an OBJ model, e.g. `create:models/block/x.obj`.
    pub model: Option<String>,
    /// OBJ models: flip texture V, as Blender exports need.
    pub flip_v: Option<bool>,
    /// Composite models: named sub-models, each drawn in full.
    #[serde(alias = "parts")]
    pub children: Option<HashMap<String, RawModel>>,
}

/// A face of an OBJ model, before its texture reference is resolved.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawPolygon {
    positions: [[f32; 3]; 4],
    uvs: [[f32; 2]; 4],
    /// Texture reference from the material, usually `#name`.
    texture: String,
}

/// A free-form face of an OBJ model. Triangles repeat their last corner.
#[derive(Debug, Clone, PartialEq)]
pub struct Polygon {
    /// Corners in 0..16 model space, counter-clockwise seen from the front.
    pub positions: [[f32; 3]; 4],
    /// UVs in 0..16 texel space.
    pub uvs: [[f32; 2]; 4],
    pub texture: Option<String>,
}

/// Parse a Wavefront `.obj` file in block units, as (Neo)Forge's OBJ loader
/// reads it. `mtl` returns the text of a material library the file names;
/// each material's `map_Kd` is the face's texture reference.
pub(crate) fn parse_obj(obj: &str, mtl: impl Fn(&str) -> Option<String>, flip_v: bool) -> Vec<RawPolygon> {
    let mut materials: HashMap<String, String> = HashMap::new();
    let (mut positions, mut uvs) = (Vec::<[f32; 3]>::new(), Vec::<[f32; 2]>::new());
    // Faces before any `usemtl` have no material, e.g. helper boxes left in
    // an export; (Neo)Forge doesn't draw them.
    let mut texture: Option<String> = None;
    let mut polygons = Vec::new();
    let floats = |rest: std::str::SplitWhitespace| rest.filter_map(|t| t.parse::<f32>().ok()).collect::<Vec<_>>();
    for line in obj.lines() {
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some("mtllib") => {
                let name = parts.collect::<Vec<_>>().join(" ");
                let Some(text) = mtl(&name) else { continue };
                let mut current = None;
                for line in text.lines() {
                    let mut parts = line.split_whitespace();
                    match parts.next() {
                        Some("newmtl") => current = parts.next().map(str::to_owned),
                        Some("map_Kd") => {
                            if let (Some(name), Some(map)) = (&current, parts.next()) {
                                materials.insert(name.clone(), map.to_owned());
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("usemtl") => {
                let name = parts.next().unwrap_or_default();
                texture = Some(materials.get(name).cloned().unwrap_or_else(|| format!("#{name}")));
            }
            Some("v") => {
                if let [x, y, z, ..] = floats(parts)[..] {
                    positions.push([x * 16.0, y * 16.0, z * 16.0]);
                }
            }
            Some("vt") => {
                if let [u, v, ..] = floats(parts)[..] {
                    let v = if flip_v { 1.0 - v } else { v };
                    uvs.push([u * 16.0, v * 16.0]);
                }
            }
            Some("f") => {
                let Some(texture) = &texture else { continue };
                // `v`, `v/vt`, `v/vt/vn` or `v//vn`; indices are 1-based or negative.
                let index = |i: Option<&str>, len: usize| -> Option<usize> {
                    let i: i64 = i?.parse().ok()?;
                    let i = if i < 0 { len as i64 + i } else { i - 1 };
                    usize::try_from(i).ok().filter(|&i| i < len)
                };
                let corners: Option<Vec<([f32; 3], [f32; 2])>> = parts
                    .map(|c| {
                        let mut ids = c.split('/');
                        let p = positions[index(ids.next(), positions.len())?];
                        let uv = index(ids.next(), uvs.len()).map_or([0.0; 2], |i| uvs[i]);
                        Some((p, uv))
                    })
                    .collect();
                let Some(corners) = corners.filter(|c| c.len() >= 3) else { continue };
                // Quads stay whole; other polygons become a fan of triangles.
                let mut push = |c: [usize; 4]| {
                    polygons.push(RawPolygon {
                        positions: c.map(|i| corners[i].0),
                        uvs: c.map(|i| corners[i].1),
                        texture: texture.clone(),
                    })
                };
                if corners.len() == 4 {
                    push([0, 1, 2, 3]);
                } else {
                    for i in 1..corners.len() - 1 {
                        push([0, i, i + 1, i + 1]);
                    }
                }
            }
            _ => {}
        }
    }
    polygons
}

#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    pub direction: Direction,
    /// `[u0, v0, u1, v1]` in 0..16 texel space; `None` means derive it
    /// from the element bounds.
    pub uv: Option<[f32; 4]>,
    /// Fully qualified texture location such as `minecraft:block/stone`,
    /// or `None` if the `#reference` could not be resolved.
    pub texture: Option<String>,
    pub cullface: Option<Direction>,
    /// Texture rotation in degrees: 0, 90, 180 or 270.
    pub rotation: i32,
    pub tintindex: Option<i32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    pub from: [f32; 3],
    pub to: [f32; 3],
    pub rotation: Option<ElementRotation>,
    pub shade: bool,
    pub faces: Vec<Face>,
}

impl Element {
    /// Whether this element covers the whole 16³ block without rotation.
    pub fn is_full_cube(&self) -> bool {
        self.rotation.as_ref().is_none_or(|r| r.angle == 0.0)
            && self.from == [0.0; 3]
            && self.to == [16.0; 3]
    }
}

/// A block model with its parent chain and texture variables resolved.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Model {
    pub ambient_occlusion: bool,
    pub elements: Vec<Element>,
    /// Faces of OBJ models.
    pub polygons: Vec<Polygon>,
}

/// Normalize `stone`, `block/stone` or `minecraft:block/stone` to
/// `namespace:path`.
pub fn qualify(location: &str) -> String {
    if location.contains(':') {
        location.to_owned()
    } else {
        format!("minecraft:{location}")
    }
}

/// Flatten a chain of models, child first, into a resolved [`Model`].
pub(crate) fn flatten(chain: &[RawModel], polygons: &[RawPolygon]) -> Model {
    // Children override parents, so apply texture maps from the root down.
    let mut textures: HashMap<&str, &str> = HashMap::new();
    for m in chain.iter().rev() {
        for (k, v) in &m.textures {
            textures.insert(k, v.as_str());
        }
    }
    let resolve = |name: &str| -> Option<String> {
        // Follow `#var` indirections, guarding against cycles.
        let mut var = name.strip_prefix('#');
        let Some(mut var_name) = var else { return Some(qualify(name)) };
        for _ in 0..16 {
            let value = *textures.get(var_name)?;
            var = value.strip_prefix('#');
            match var {
                Some(next) => var_name = next,
                None => return Some(qualify(value)),
            }
        }
        None
    };

    let ambient_occlusion = chain.iter().find_map(|m| m.ambient_occlusion).unwrap_or(true);
    let raw_elements = chain.iter().find_map(|m| m.elements.as_ref());
    let elements = raw_elements
        .into_iter()
        .flatten()
        .map(|e| {
            let mut faces: Vec<Face> = e
                .faces
                .iter()
                .map(|(&direction, f)| Face {
                    direction,
                    uv: f.uv,
                    texture: resolve(&f.texture),
                    cullface: f.cullface,
                    rotation: f.rotation.rem_euclid(360),
                    tintindex: f.tintindex,
                })
                .collect();
            // HashMap order is random; keep output deterministic.
            faces.sort_by_key(|f| f.direction as u8);
            Element { from: e.from, to: e.to, rotation: e.rotation.clone(), shade: e.shade, faces }
        })
        .collect();
    let polygons = polygons
        .iter()
        .map(|p| Polygon { positions: p.positions, uvs: p.uvs, texture: resolve(&p.texture) })
        .collect();
    Model { ambient_occlusion, elements, polygons }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(json: &str) -> RawModel {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn flatten_resolves_parent_textures_and_elements() {
        let cube = raw(
            r##"{"elements": [{"from": [0,0,0], "to": [16,16,16], "faces": {
                "up": {"texture": "#up", "cullface": "up"},
                "north": {"texture": "#side", "uv": [0,0,16,8], "rotation": 90, "tintindex": 0}
            }}]}"##,
        );
        let column = raw(r##"{"parent": "block/cube", "textures": {"up": "#end", "side": "#end"}}"##);
        let log = raw(r#"{"parent": "block/cube_column", "textures": {"end": "block/oak_log_top", "side": "block/oak_log"}}"#);

        let model = flatten(&[log, column, cube], &[]);
        assert!(model.ambient_occlusion);
        let e = &model.elements[0];
        assert!(e.is_full_cube());
        assert_eq!(e.faces.len(), 2);
        let up = e.faces.iter().find(|f| f.direction == Direction::Up).unwrap();
        assert_eq!(up.texture.as_deref(), Some("minecraft:block/oak_log_top"));
        assert_eq!(up.cullface, Some(Direction::Up));
        let north = e.faces.iter().find(|f| f.direction == Direction::North).unwrap();
        // The child's `side` overrides the parent's `#end` indirection.
        assert_eq!(north.texture.as_deref(), Some("minecraft:block/oak_log"));
        assert_eq!((north.uv, north.rotation, north.tintindex), (Some([0.0, 0.0, 16.0, 8.0]), 90, Some(0)));
    }

    #[test]
    fn unresolved_and_cyclic_references_are_none() {
        let m = raw(
            r##"{"textures": {"a": "#b", "b": "#a", "s": {"sprite": "foo:block/x"}},
                "elements": [{"from": [0,0,0], "to": [16,1,16], "faces": {
                    "up": {"texture": "#a"}, "down": {"texture": "#missing"}, "east": {"texture": "#s"}
                }}]}"##,
        );
        let model = flatten(&[m], &[]);
        let textures: Vec<_> = model.elements[0].faces.iter().map(|f| f.texture.clone()).collect();
        assert_eq!(textures, [None, None, Some("foo:block/x".into())]);
        assert!(!model.elements[0].is_full_cube());
    }

    #[test]
    fn obj_faces_use_material_textures() {
        let obj = "mtllib wheel.mtl\nv 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nv 0.5 2 0\nvt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\n\
                   o Helper\nf 1 2 3\nusemtl metal\nf 1/1 2/2 3/3 4/4\nusemtl unknown\nf 4/4 3/3 -1\n";
        let mtl = |name: &str| (name == "wheel.mtl").then(|| "newmtl metal\nmap_Kd #plates\n".to_string());
        let polygons = parse_obj(obj, mtl, true);
        assert_eq!(polygons.len(), 2);
        assert_eq!(polygons[0].positions[2], [16.0, 16.0, 0.0]);
        // Flipped V: the third corner had v = 1.
        assert_eq!(polygons[0].uvs[2], [16.0, 0.0]);
        // A triangle repeats its last corner.
        assert_eq!(polygons[1].positions[3], [8.0, 32.0, 0.0]);

        let model = raw(r##"{"textures": {"plates": "create:block/plates"}}"##);
        let model = flatten(&[model], &polygons);
        assert_eq!(model.polygons[0].texture.as_deref(), Some("create:block/plates"));
        assert_eq!(model.polygons[1].texture, None);
    }
}
