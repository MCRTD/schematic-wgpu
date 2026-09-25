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
    ambient_occlusion: Option<bool>,
    #[serde(default)]
    textures: HashMap<String, RawTextureRef>,
    elements: Option<Vec<RawElement>>,
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
pub(crate) fn flatten(chain: &[RawModel]) -> Model {
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
    Model { ambient_occlusion, elements }
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

        let model = flatten(&[log, column, cube]);
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
        let model = flatten(&[m]);
        let textures: Vec<_> = model.elements[0].faces.iter().map(|f| f.texture.clone()).collect();
        assert_eq!(textures, [None, None, Some("foo:block/x".into())]);
        assert!(!model.elements[0].is_full_cube());
    }
}
