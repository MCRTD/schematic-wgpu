//! Loading Minecraft resource packs: blockstates, block models and textures.
//!
//! Everything is read on demand from an [`AssetSource`] and cached, so a
//! vanilla client `.jar` or a resource pack zip can be used directly.

pub mod blockstate;
mod geo;
pub mod model;
pub mod remote_zip;
pub mod source;
pub mod texture;

use std::collections::HashMap;
use std::sync::Arc;

pub use blockstate::{BlockStateDef, Properties, Variant};
pub use model::{Axis, Direction, Element, ElementRotation, Face, Model, Polygon};
pub use source::{AssetSource, DirSource, MemorySource, PackStack, ZipSource};
pub use texture::{Texture, Transparency};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("no `assets` folder found in {0}")]
    NoAssets(String),
    #[error("no blockstate definition for `{0}`")]
    MissingBlockState(String),
    #[error("model `{0}` not found")]
    MissingModel(String),
    #[error("model `{0}` has a cyclic or too deep parent chain")]
    ParentCycle(String),
    #[error("invalid json in {path}: {source}")]
    Json { path: String, source: serde_json::Error },
}

pub type Result<T> = std::result::Result<T, Error>;

/// One model to draw for a block, with its whole-block rotation.
#[derive(Debug, Clone)]
pub struct ModelInstance {
    pub model: Arc<Model>,
    /// Rotation around X then Y in degrees, multiples of 90.
    pub x: i32,
    pub y: i32,
    pub uvlock: bool,
    pub weight: u32,
}

/// A block's appearance: every part is drawn, and each part is a weighted
/// random choice between one or more models.
pub type ResolvedBlock = Vec<Vec<ModelInstance>>;

/// Blocks renamed between versions, as `(older name, newer name)`. When a
/// schematic and a resource pack come from different versions, the other
/// name is tried.
const RENAMES: &[(&str, &str)] = &[
    ("minecraft:grass", "minecraft:short_grass"),
    ("minecraft:grass_path", "minecraft:dirt_path"),
    ("minecraft:chain", "minecraft:iron_chain"),
];

/// Parent models that are drawn by code rather than by elements.
const BUILTIN_PARENTS: &[&str] = &["minecraft:builtin/generated", "minecraft:builtin/entity"];

pub struct Assets {
    source: Box<dyn AssetSource>,
    blockstates: HashMap<String, Option<Arc<BlockStateDef>>>,
    models: HashMap<String, Arc<Model>>,
    textures: HashMap<String, Option<Arc<Texture>>>,
    geo_models: HashMap<String, Option<Arc<Model>>>,
}

impl Assets {
    pub fn new(source: impl AssetSource + 'static) -> Self {
        Self {
            source: Box::new(source),
            blockstates: HashMap::new(),
            models: HashMap::new(),
            textures: HashMap::new(),
            geo_models: HashMap::new(),
        }
    }

    /// `minecraft:block/stone` + (`models`, `.json`) →
    /// `assets/minecraft/models/block/stone.json`
    fn read(&self, location: &str, kind: &str, ext: &str) -> Option<(String, Vec<u8>)> {
        let location = model::qualify(location);
        let (ns, path) = location.split_once(':')?;
        let file = format!("assets/{ns}/{kind}/{path}{ext}");
        let data = self.source.read(&file)?;
        Some((file, data))
    }

    fn parse<T: serde::de::DeserializeOwned>(path: String, data: &[u8]) -> Result<T> {
        serde_json::from_slice(data).map_err(|source| Error::Json { path, source })
    }

    /// The blockstate definition for a block name like `minecraft:oak_stairs`.
    pub fn blockstate(&mut self, name: &str) -> Result<Arc<BlockStateDef>> {
        let name = model::qualify(name);
        if let Some(cached) = self.blockstates.get(&name) {
            return cached.clone().ok_or(Error::MissingBlockState(name));
        }
        let renamed = RENAMES.iter().find_map(|&(old, new)| match name.as_str() {
            n if n == old => Some(new),
            n if n == new => Some(old),
            _ => None,
        });
        let found = std::iter::once(name.as_str())
            .chain(renamed)
            .find_map(|n| self.read(n, "blockstates", ".json"));
        let def = match found {
            Some((path, data)) => Some(Arc::new(Self::parse(path, &data)?)),
            None => None,
        };
        self.blockstates.insert(name.clone(), def.clone());
        def.ok_or(Error::MissingBlockState(name))
    }

    /// A resolved model like `minecraft:block/oak_stairs`.
    pub fn model(&mut self, location: &str) -> Result<Arc<Model>> {
        let location = model::qualify(location);
        if let Some(cached) = self.models.get(&location) {
            return Ok(cached.clone());
        }
        let chain = self.chain(&location)?;
        let model = Arc::new(self.build(&chain, chain.len(), 0)?);
        self.models.insert(location, model.clone());
        Ok(model)
    }

    /// A model and its parents, child first.
    fn chain(&self, location: &str) -> Result<Vec<model::RawModel>> {
        let mut chain = Vec::new();
        let mut next = Some(model::qualify(location));
        while let Some(current) = next.take() {
            if BUILTIN_PARENTS.contains(&current.as_str()) {
                break;
            }
            if chain.len() >= 32 {
                return Err(Error::ParentCycle(location.to_owned()));
            }
            let (path, data) = self
                .read(&current, "models", ".json")
                .ok_or_else(|| Error::MissingModel(current.clone()))?;
            let raw: model::RawModel = Self::parse(path, &data)?;
            next = raw.parent.as_deref().map(model::qualify);
            chain.push(raw);
        }
        Ok(chain)
    }

    /// Resolve a model chain. Only its first `own` models belong to this
    /// model; the rest give textures to a composite model's child.
    fn build(&self, chain: &[model::RawModel], own: usize, depth: u32) -> Result<Model> {
        let mine = &chain[..own];
        let loader = mine.iter().find_map(|m| m.loader.as_deref()).unwrap_or_default();
        if loader.ends_with(":composite") && depth < 4 {
            let ambient_occlusion = chain.iter().find_map(|m| m.ambient_occlusion).unwrap_or(true);
            let mut model = Model { ambient_occlusion, ..Default::default() };
            let Some(children) = mine.iter().find_map(|m| m.children.as_ref()) else { return Ok(model) };
            let mut names: Vec<&String> = children.keys().collect();
            names.sort();
            for name in names {
                let child = &children[name];
                let mut sub = vec![child.clone()];
                if let Some(parent) = &child.parent {
                    sub.extend(self.chain(parent)?);
                }
                let own = sub.len();
                sub.extend(chain.iter().cloned());
                let part = self.build(&sub, own, depth + 1)?;
                model.elements.extend(part.elements);
                model.polygons.extend(part.polygons);
            }
            return Ok(model);
        }
        let polygons = match mine.iter().find_map(|m| m.model.as_deref()) {
            Some(obj) if loader.ends_with(":obj") => {
                let flip_v = mine.iter().find_map(|m| m.flip_v).unwrap_or(false);
                self.obj(obj, flip_v)?
            }
            _ => Vec::new(),
        };
        Ok(model::flatten(chain, &polygons))
    }

    /// Faces of an OBJ model file like `create:models/block/wheel.obj`.
    fn obj(&self, location: &str, flip_v: bool) -> Result<Vec<model::RawPolygon>> {
        let location = model::qualify(location);
        let (ns, path) = location.split_once(':').unwrap_or(("minecraft", &location));
        let file = format!("assets/{ns}/{path}");
        let data = self.source.read(&file).ok_or_else(|| Error::MissingModel(location.clone()))?;
        let dir = file.rsplit_once('/').map_or("", |(d, _)| d);
        let mtl = |name: &str| {
            let data = self.source.read(&format!("{dir}/{name}"))?;
            Some(String::from_utf8_lossy(&data).into_owned())
        };
        Ok(model::parse_obj(&String::from_utf8_lossy(&data), mtl, flip_v))
    }

    /// The GeckoLib model of a block like `ars_nouveau:enchanting_apparatus`,
    /// from `geo/<name>.geo.json` or `geo/block/<name>.geo.json`, drawn with
    /// `texture`; `None` if the block has none.
    pub fn geo_model(&mut self, block: &str, texture: &str) -> Option<Arc<Model>> {
        let block = model::qualify(block);
        let key = format!("{block}|{texture}");
        if let Some(cached) = self.geo_models.get(&key) {
            return cached.clone();
        }
        let (ns, name) = block.split_once(':')?;
        let model = [format!("assets/{ns}/geo/{name}.geo.json"), format!("assets/{ns}/geo/block/{name}.geo.json")]
            .iter()
            .find_map(|path| self.source.read(path))
            .and_then(|data| geo::parse(&data, texture))
            .filter(|polygons| !polygons.is_empty())
            .map(|polygons| Arc::new(Model { ambient_occlusion: true, elements: Vec::new(), polygons }));
        self.geo_models.insert(key, model.clone());
        model
    }

    /// A texture like `minecraft:block/stone`, or `None` if missing or not a
    /// valid PNG.
    pub fn texture(&mut self, location: &str) -> Option<Arc<Texture>> {
        let location = model::qualify(location);
        if let Some(cached) = self.textures.get(&location) {
            return cached.clone();
        }
        let texture = self
            .read(&location, "textures", ".png")
            .and_then(|(_, data)| Texture::from_png(&data).ok())
            .map(Arc::new);
        self.textures.insert(location, texture.clone());
        texture
    }

    /// Every model needed to draw a block in the given state.
    pub fn resolve(&mut self, name: &str, props: &Properties) -> Result<ResolvedBlock> {
        let def = self.blockstate(name)?;
        def.select(props)
            .into_iter()
            .map(|list| {
                list.as_slice()
                    .iter()
                    .map(|v| {
                        Ok(ModelInstance {
                            model: self.model(&v.model)?,
                            x: v.x,
                            y: v.y,
                            uvlock: v.uvlock,
                            weight: v.weight,
                        })
                    })
                    .collect()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack() -> MemorySource {
        let mut src = MemorySource::default();
        src.insert(
            "assets/minecraft/blockstates/dirt_path.json",
            r#"{"variants": {"": {"model": "minecraft:block/dirt_path"}}}"#,
        )
        .insert(
            "assets/minecraft/blockstates/stone.json",
            r#"{"variants": {"": [{"model": "block/stone"}, {"model": "block/stone_mirrored", "y": 180, "weight": 2}]}}"#,
        )
        .insert(
            "assets/minecraft/models/block/cube_all.json",
            r##"{"elements": [{"from": [0,0,0], "to": [16,16,16], "faces": {"up": {"texture": "#all"}}}]}"##,
        )
        .insert("assets/minecraft/models/block/stone.json", r#"{"parent": "block/cube_all", "textures": {"all": "block/stone"}}"#)
        .insert("assets/minecraft/models/block/stone_mirrored.json", r#"{"parent": "block/stone"}"#)
        .insert("assets/minecraft/models/block/dirt_path.json", r#"{"parent": "block/cube_all", "textures": {"all": "block/dirt_path_top"}}"#)
        .insert("assets/minecraft/models/block/loop_a.json", r#"{"parent": "block/loop_b"}"#)
        .insert("assets/minecraft/models/block/loop_b.json", r#"{"parent": "block/loop_a"}"#)
        .insert("assets/minecraft/models/block/chest.json", r#"{"parent": "builtin/entity"}"#);
        src
    }

    #[test]
    fn resolves_weighted_variants() {
        let mut assets = Assets::new(pack());
        let block = assets.resolve("minecraft:stone", &Properties::new()).unwrap();
        assert_eq!(block.len(), 1);
        let choices = &block[0];
        assert_eq!(choices.iter().map(|m| (m.y, m.weight)).collect::<Vec<_>>(), [(0, 1), (180, 2)]);
        let face = &choices[1].model.elements[0].faces[0];
        assert_eq!(face.texture.as_deref(), Some("minecraft:block/stone"));
    }

    #[test]
    fn falls_back_to_renamed_block() {
        let mut assets = Assets::new(pack());
        let block = assets.resolve("grass_path", &Properties::new()).unwrap();
        let face = &block[0][0].model.elements[0].faces[0];
        assert_eq!(face.texture.as_deref(), Some("minecraft:block/dirt_path_top"));
    }

    #[test]
    fn reports_missing_and_broken_assets() {
        let mut assets = Assets::new(pack());
        assert!(matches!(assets.blockstate("minecraft:nope"), Err(Error::MissingBlockState(_))));
        assert!(matches!(assets.model("block/nope"), Err(Error::MissingModel(_))));
        assert!(matches!(assets.model("block/loop_a"), Err(Error::ParentCycle(_))));
        assert!(assets.model("block/chest").unwrap().elements.is_empty());
        assert!(assets.texture("block/stone").is_none());
    }

    #[test]
    fn earlier_packs_override_later_ones() {
        let mut top = MemorySource::default();
        top.insert("assets/minecraft/models/block/stone.json", r#"{"parent": "block/cube_all", "textures": {"all": "custom:stone"}}"#);
        let mut stack = PackStack::new();
        stack.push(top).push(pack());
        let mut assets = Assets::new(stack);
        let face = &assets.model("block/stone").unwrap().elements[0].faces[0];
        assert_eq!(face.texture.as_deref(), Some("custom:stone"));
    }
}
