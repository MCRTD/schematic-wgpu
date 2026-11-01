//! Parser for Litematica `.litematic` files.
//!
//! A `.litematic` file is a gzip-compressed NBT compound. Each region stores a
//! block-state palette plus a tightly packed `long[]` of palette indices.

mod preview;

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("nbt error: {0}")]
    Nbt(#[from] fastnbt::error::Error),
    #[error("region `{region}`: palette index {index} out of range (palette has {len} entries)")]
    PaletteIndex { region: String, index: u32, len: usize },
    #[error("region `{region}`: block state array too short ({got} longs, need {need})")]
    ShortBlockStates { region: String, got: usize, need: usize },
    #[error("region `{region}` is too large to load ({x}×{y}×{z} blocks)")]
    TooLarge { region: String, x: i32, y: i32, z: i32 },
    #[error("invalid preview: {0}")]
    Preview(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vec3i {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// A block state, e.g. `minecraft:oak_stairs[facing=north,half=bottom]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlockState {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Properties", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, String>,
}

impl BlockState {
    /// Name without the `minecraft:` namespace.
    pub fn short_name(&self) -> &str {
        self.name.strip_prefix("minecraft:").unwrap_or(&self.name)
    }

    pub fn is_air(&self) -> bool {
        matches!(self.short_name(), "air" | "cave_air" | "void_air")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Metadata {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
    pub enclosing_size: Vec3i,
    #[serde(default)]
    pub region_count: i32,
    #[serde(default)]
    pub total_blocks: i32,
    #[serde(default)]
    pub total_volume: i32,
    #[serde(default)]
    pub time_created: i64,
    #[serde(default)]
    pub time_modified: i64,
}

/// Raw on-disk region layout.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawRegion {
    position: Vec3i,
    size: Vec3i,
    block_state_palette: Vec<BlockState>,
    block_states: fastnbt::LongArray,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawLitematic {
    #[serde(default)]
    minecraft_data_version: i32,
    #[serde(default)]
    version: i32,
    metadata: Metadata,
    regions: BTreeMap<String, RawRegion>,
}

/// A decoded region. Blocks are stored as palette indices in
/// `(y * size.z + z) * size.x + x` order, where `size` is the absolute size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Region {
    pub name: String,
    /// Minimum corner of the region, relative to the schematic origin.
    pub origin: Vec3i,
    /// Absolute size along each axis.
    pub size: Vec3i,
    pub palette: Vec<BlockState>,
    pub blocks: Vec<u32>,
}

impl Region {
    fn decode(name: String, raw: RawRegion) -> Result<Self> {
        let size = Vec3i {
            x: raw.size.x.abs(),
            y: raw.size.y.abs(),
            z: raw.size.z.abs(),
        };
        // A negative size means the region extends from `position` towards
        // negative coordinates, so the minimum corner is `position + size + 1`.
        let min = |p: i32, s: i32| if s < 0 { p + s + 1 } else { p };
        let origin = Vec3i {
            x: min(raw.position.x, raw.size.x),
            y: min(raw.position.y, raw.size.y),
            z: min(raw.position.z, raw.size.z),
        };

        let bits = bits_per_entry(raw.block_state_palette.len());
        // On 32-bit targets (wasm) a huge region would overflow these sizes
        // or the block array; report it instead.
        let volume = (size.x as usize)
            .checked_mul(size.y as usize)
            .and_then(|v| v.checked_mul(size.z as usize))
            .filter(|v| v.checked_mul(size_of::<u32>()).is_some_and(|b| b <= isize::MAX as usize))
            .and_then(|v| Some((v, v.checked_mul(bits as usize)?)));
        let Some((volume, volume_bits)) = volume else {
            return Err(Error::TooLarge { region: name, x: size.x, y: size.y, z: size.z });
        };
        let longs: &[i64] = &raw.block_states;
        let need = volume_bits.div_ceil(64);
        if longs.len() < need {
            return Err(Error::ShortBlockStates { region: name, got: longs.len(), need });
        }

        let blocks = unpack(longs, bits, volume);
        if let Some(&index) = blocks.iter().find(|&&i| i as usize >= raw.block_state_palette.len()) {
            return Err(Error::PaletteIndex {
                region: name,
                index,
                len: raw.block_state_palette.len(),
            });
        }

        Ok(Region { name, origin, size, palette: raw.block_state_palette, blocks })
    }

    pub fn index(&self, x: i32, y: i32, z: i32) -> usize {
        ((y * self.size.z + z) * self.size.x + x) as usize
    }

    /// Block at local coordinates, or `None` if out of bounds.
    pub fn get(&self, x: i32, y: i32, z: i32) -> Option<&BlockState> {
        let in_bounds = (0..self.size.x).contains(&x)
            && (0..self.size.y).contains(&y)
            && (0..self.size.z).contains(&z);
        in_bounds.then(|| &self.palette[self.blocks[self.index(x, y, z)] as usize])
    }

    /// Local position of `blocks[i]`; the inverse of [`Region::index`].
    pub fn position(&self, i: usize) -> Vec3i {
        let (sx, sz) = (self.size.x as usize, self.size.z as usize);
        Vec3i { x: (i % sx) as i32, z: ((i / sx) % sz) as i32, y: (i / (sx * sz)) as i32 }
    }

    /// Iterate over all non-air blocks as `(local position, state)`.
    pub fn iter_blocks(&self) -> impl Iterator<Item = (Vec3i, &BlockState)> {
        self.blocks.iter().enumerate().filter_map(move |(i, &p)| {
            let state = &self.palette[p as usize];
            (!state.is_air()).then(|| (self.position(i), state))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Litematic {
    pub data_version: i32,
    pub version: i32,
    pub metadata: Metadata,
    pub regions: Vec<Region>,
}

impl Litematic {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_gzip_bytes(&std::fs::read(path)?)
    }

    /// Parse a gzip-compressed `.litematic` file.
    pub fn from_gzip_bytes(bytes: &[u8]) -> Result<Self> {
        let mut nbt = Vec::new();
        GzDecoder::new(bytes).read_to_end(&mut nbt)?;
        Self::from_nbt_bytes(&nbt)
    }

    /// Parse an uncompressed NBT payload.
    pub fn from_nbt_bytes(nbt: &[u8]) -> Result<Self> {
        let raw: RawLitematic = fastnbt::from_bytes(nbt)?;
        let regions = raw
            .regions
            .into_iter()
            .map(|(name, region)| Region::decode(name, region))
            .collect::<Result<_>>()?;
        Ok(Litematic {
            data_version: raw.minecraft_data_version,
            version: raw.version,
            metadata: raw.metadata,
            regions,
        })
    }
}

/// Litematica uses at least 2 bits per entry.
pub fn bits_per_entry(palette_len: usize) -> u32 {
    let needed = usize::BITS - palette_len.saturating_sub(1).leading_zeros();
    needed.max(2)
}

/// Unpack `count` entries of `bits` width from a little-endian bit stream.
/// Unlike chunk sections since 1.16, entries may span two longs.
pub fn unpack(longs: &[i64], bits: u32, count: usize) -> Vec<u32> {
    let mask = (1u64 << bits) - 1;
    (0..count)
        .map(|i| {
            let start = i * bits as usize;
            let (word, offset) = (start / 64, (start % 64) as u32);
            let mut value = (longs[word] as u64) >> offset;
            if offset + bits > 64 {
                value |= (longs[word + 1] as u64) << (64 - offset);
            }
            (value & mask) as u32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(values: &[u32], bits: u32) -> Vec<i64> {
        let mut longs = vec![0u64; (values.len() * bits as usize).div_ceil(64)];
        for (i, &v) in values.iter().enumerate() {
            let start = i * bits as usize;
            let (word, offset) = (start / 64, (start % 64) as u32);
            longs[word] |= (v as u64) << offset;
            if offset + bits > 64 {
                longs[word + 1] |= (v as u64) >> (64 - offset);
            }
        }
        longs.into_iter().map(|l| l as i64).collect()
    }

    #[test]
    fn bits_minimum_is_two() {
        assert_eq!(bits_per_entry(1), 2);
        assert_eq!(bits_per_entry(4), 2);
        assert_eq!(bits_per_entry(5), 3);
        assert_eq!(bits_per_entry(30), 5);
        assert_eq!(bits_per_entry(256), 8);
        assert_eq!(bits_per_entry(257), 9);
    }

    #[test]
    fn unpack_spans_long_boundaries() {
        // 5 bits per entry: entry 12 straddles the first and second long.
        let values: Vec<u32> = (0..100).map(|i| (i * 7) % 30).collect();
        assert_eq!(unpack(&pack(&values, 5), 5, values.len()), values);
    }

    #[test]
    fn unpack_matches_litematica_docs_example() {
        // 5-bit entries from a 30-entry palette.
        let longs = [0x1084_4210_8410_8400u64 as i64, 0x4210_8420_8400_0842u64 as i64];
        assert_eq!(unpack(&longs, 5, 12), [0, 0, 1, 1, 1, 2, 2, 2, 2, 2, 1, 1]);
    }

    #[derive(Serialize)]
    #[serde(rename_all = "PascalCase")]
    struct TestRegion {
        position: Vec3i,
        size: Vec3i,
        block_state_palette: Vec<BlockState>,
        block_states: fastnbt::LongArray,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "PascalCase")]
    struct TestFile {
        minecraft_data_version: i32,
        version: i32,
        metadata: Metadata,
        regions: BTreeMap<String, TestRegion>,
    }

    fn state(name: &str, props: &[(&str, &str)]) -> BlockState {
        BlockState {
            name: name.into(),
            properties: props.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    #[test]
    fn roundtrip_negative_size_region() {
        let palette = vec![
            state("minecraft:air", &[]),
            state("minecraft:stone", &[]),
            state("minecraft:oak_stairs", &[("facing", "east"), ("half", "bottom")]),
        ];
        let size = Vec3i { x: 3, y: 2, z: 2 };
        let values: Vec<u32> = (0..12).map(|i| i % 3).collect();
        let file = TestFile {
            minecraft_data_version: 3465,
            version: 6,
            metadata: Metadata {
                name: "test".into(),
                author: "me".into(),
                description: String::new(),
                enclosing_size: size,
                region_count: 1,
                total_blocks: 8,
                total_volume: 12,
                time_created: 0,
                time_modified: 0,
            },
            regions: BTreeMap::from([(
                "Unnamed".to_string(),
                TestRegion {
                    position: Vec3i { x: 5, y: 0, z: 0 },
                    size: Vec3i { x: -3, y: 2, z: 2 },
                    block_state_palette: palette.clone(),
                    block_states: fastnbt::LongArray::new(pack(&values, 2)),
                },
            )]),
        };

        let nbt = fastnbt::to_bytes(&file).unwrap();
        let lit = Litematic::from_nbt_bytes(&nbt).unwrap();
        let region = &lit.regions[0];

        assert_eq!(lit.metadata.enclosing_size, size);
        assert_eq!(region.size, size);
        assert_eq!(region.origin, Vec3i { x: 3, y: 0, z: 0 });
        assert_eq!(region.blocks, values);
        assert_eq!(region.get(2, 0, 0), Some(&palette[2]));
        assert_eq!(region.get(0, 1, 0), Some(&palette[0]));
        assert_eq!(region.get(3, 0, 0), None);
        assert_eq!(region.iter_blocks().count(), 8);
        assert!(region.iter_blocks().all(|(p, s)| region.get(p.x, p.y, p.z) == Some(s)));
    }

    #[test]
    fn rejects_out_of_range_palette_index() {
        let region = RawRegion {
            position: Vec3i { x: 0, y: 0, z: 0 },
            size: Vec3i { x: 2, y: 1, z: 1 },
            block_state_palette: vec![state("minecraft:air", &[])],
            block_states: fastnbt::LongArray::new(pack(&[0, 3], 2)),
        };
        assert!(matches!(
            Region::decode("r".into(), region),
            Err(Error::PaletteIndex { index: 3, .. })
        ));
    }
}
