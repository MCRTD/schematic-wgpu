//! Vanilla structure files (`.nbt`), as saved by structure blocks and by the
//! Create mod's schematic table.
//!
//! The root compound holds a `size`, a block-state `palette` (or several
//! `palettes`, of which the first is used) and a sparse list of `blocks`, each
//! with a `pos`, a palette index `state` and an optional block entity `nbt`.
//! Positions not listed (structure voids) are loaded as air.

use serde::Deserialize;

use crate::{BlockEntity, BlockState, Error, Litematic, Metadata, Nbt, Region, Result, Vec3i};

#[derive(Deserialize)]
struct RawBlock {
    pos: [i32; 3],
    state: i32,
    nbt: Option<Nbt>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawStructure {
    #[serde(default)]
    data_version: i32,
    #[serde(rename = "size")]
    size: [i32; 3],
    #[serde(rename = "palette", default)]
    palette: Vec<BlockState>,
    #[serde(rename = "palettes", default)]
    palettes: Vec<Vec<BlockState>>,
    #[serde(rename = "blocks", default)]
    blocks: Vec<RawBlock>,
    #[serde(rename = "author", default)]
    author: String,
}

const NAME: &str = "structure";

impl Litematic {
    /// Parse an uncompressed structure NBT payload into a single region.
    pub fn from_structure_nbt_bytes(nbt: &[u8]) -> Result<Self> {
        let raw: RawStructure = fastnbt::from_bytes(nbt)?;
        let [x, y, z] = raw.size;
        let size = Vec3i { x, y, z };
        let too_large = || Error::TooLarge { region: NAME.into(), x, y, z };
        if x < 0 || y < 0 || z < 0 {
            return Err(too_large());
        }
        let volume = (x as usize)
            .checked_mul(y as usize)
            .and_then(|v| v.checked_mul(z as usize))
            .filter(|v| v.checked_mul(size_of::<u32>()).is_some_and(|b| b <= isize::MAX as usize))
            .ok_or_else(too_large)?;

        let mut palette = if raw.palette.is_empty() {
            raw.palettes.into_iter().next().unwrap_or_default()
        } else {
            raw.palette
        };
        let air = match palette.iter().position(BlockState::is_air) {
            Some(i) => i,
            None => {
                palette.push(BlockState { name: "minecraft:air".into(), properties: Default::default() });
                palette.len() - 1
            }
        } as u32;

        let mut region = Region {
            name: NAME.into(),
            origin: Vec3i { x: 0, y: 0, z: 0 },
            size,
            palette,
            blocks: vec![air; volume],
            block_entities: Vec::new(),
        };
        for block in raw.blocks {
            let [bx, by, bz] = block.pos;
            if !((0..x).contains(&bx) && (0..y).contains(&by) && (0..z).contains(&bz)) {
                continue;
            }
            let len = region.palette.len();
            let state = u32::try_from(block.state)
                .ok()
                .filter(|&s| (s as usize) < len)
                .ok_or(Error::PaletteIndex { region: NAME.into(), index: block.state as u32, len })?;
            let index = region.index(bx, by, bz);
            region.blocks[index] = state;
            // Block entities carry their position like in a litematic.
            if let Some(Nbt::Compound(mut nbt)) = block.nbt {
                nbt.insert("x".into(), Nbt::Int(bx));
                nbt.insert("y".into(), Nbt::Int(by));
                nbt.insert("z".into(), Nbt::Int(bz));
                region.block_entities.push(BlockEntity { pos: Vec3i { x: bx, y: by, z: bz }, nbt: Nbt::Compound(nbt) });
            }
        }

        let total_blocks = region.blocks.iter().filter(|&&b| b != air).count() as i32;
        Ok(Litematic {
            data_version: raw.data_version,
            version: 0,
            metadata: Metadata {
                name: String::new(),
                author: raw.author,
                description: String::new(),
                enclosing_size: size,
                region_count: 1,
                total_blocks,
                total_volume: volume as i32,
                time_created: 0,
                time_modified: 0,
            },
            regions: vec![region],
        })
    }
}

/// Whether an uncompressed NBT payload is a structure file rather than a
/// litematic: it has a `size` and `blocks` instead of `Regions`.
pub(crate) fn is_structure(nbt: &[u8]) -> bool {
    #[derive(Deserialize)]
    struct Probe {
        #[serde(rename = "Regions")]
        regions: Option<serde::de::IgnoredAny>,
        size: Option<serde::de::IgnoredAny>,
        blocks: Option<serde::de::IgnoredAny>,
    }
    fastnbt::from_bytes::<Probe>(nbt)
        .is_ok_and(|p| p.regions.is_none() && p.size.is_some() && p.blocks.is_some())
}

#[cfg(test)]
mod tests {
    use serde::Serialize;

    use super::*;

    #[derive(Serialize)]
    struct Block {
        pos: [i32; 3],
        state: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        nbt: Option<Nbt>,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "PascalCase")]
    struct File {
        data_version: i32,
        #[serde(rename = "size")]
        size: [i32; 3],
        #[serde(rename = "palette")]
        palette: Vec<BlockState>,
        #[serde(rename = "blocks")]
        blocks: Vec<Block>,
        #[serde(rename = "entities")]
        entities: Vec<Nbt>,
    }

    fn state(name: &str) -> BlockState {
        BlockState { name: name.into(), properties: Default::default() }
    }

    #[test]
    fn sparse_blocks_fill_with_air_and_keep_block_entities() {
        let chest = Nbt::Compound([("id".into(), Nbt::String("minecraft:chest".into()))].into());
        let file = File {
            data_version: 3955,
            size: [2, 2, 1],
            palette: vec![state("create:andesite_casing"), state("minecraft:chest")],
            blocks: vec![
                Block { pos: [0, 0, 0], state: 0, nbt: None },
                Block { pos: [1, 1, 0], state: 1, nbt: Some(chest) },
            ],
            entities: vec![],
        };
        let nbt = fastnbt::to_bytes(&file).unwrap();
        assert!(is_structure(&nbt));

        let lit = Litematic::from_bytes(&nbt).unwrap();
        let region = &lit.regions[0];
        assert_eq!(lit.data_version, 3955);
        assert_eq!(region.size, Vec3i { x: 2, y: 2, z: 1 });
        assert_eq!(region.get(0, 0, 0).unwrap().name, "create:andesite_casing");
        assert!(region.get(1, 0, 0).unwrap().is_air());
        assert_eq!(region.get(1, 1, 0).unwrap().name, "minecraft:chest");
        assert_eq!(lit.metadata.total_blocks, 2);
        assert_eq!(region.block_entities.len(), 1);
        let entity = &region.block_entities[0];
        assert_eq!(entity.pos, Vec3i { x: 1, y: 1, z: 0 });
        let Nbt::Compound(c) = &entity.nbt else { panic!() };
        assert_eq!(c.get("x"), Some(&Nbt::Int(1)));
    }

    #[test]
    fn rejects_out_of_range_state() {
        let file = File {
            data_version: 0,
            size: [1, 1, 1],
            palette: vec![state("minecraft:stone")],
            blocks: vec![Block { pos: [0, 0, 0], state: 5, nbt: None }],
            entities: vec![],
        };
        let nbt = fastnbt::to_bytes(&file).unwrap();
        assert!(matches!(Litematic::from_bytes(&nbt), Err(Error::PaletteIndex { index: 5, .. })));
    }
}
