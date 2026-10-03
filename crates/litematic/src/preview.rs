//! Compact, already decoded form of a [`Litematic`] for previews.
//!
//! A server can decode a schematic once and hand viewers just what the mesher
//! needs: metadata, and each region's palette and block indices. Indices are
//! stored at the narrowest width that fits the palette instead of Litematica's
//! bit-packed longs, and the whole payload is gzip-compressed.
//!
//! Layout before compression (little-endian; `str` is a `u32` byte length
//! followed by UTF-8; `vec3` is three `i32`s):
//!
//! ```text
//! "SWPV" u8:format
//! i32:data_version i32:version
//! str:name str:author str:description vec3:enclosing_size
//! i32:region_count i32:total_blocks i32:total_volume i64:time_created i64:time_modified
//! u32:regions, then for each:
//!   str:name vec3:origin vec3:size
//!   u32:palette, then for each: str:name u32:properties, then (str:key str:value)*
//!   u8:index_width (1, 2 or 4) then size.x * size.y * size.z indices
//! ```

use std::collections::BTreeMap;
use std::io::{Read, Write};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;

use crate::{BlockState, Error, Litematic, Metadata, Region, Result, Vec3i};

const MAGIC: &[u8; 4] = b"SWPV";
const FORMAT: u8 = 1;

impl Litematic {
    /// Encode as a gzip-compressed preview payload.
    pub fn to_preview(&self) -> Vec<u8> {
        let mut w = Writer(Vec::new());
        w.0.extend_from_slice(MAGIC);
        w.u8(FORMAT);
        w.i32(self.data_version);
        w.i32(self.version);

        let m = &self.metadata;
        w.str(&m.name);
        w.str(&m.author);
        w.str(&m.description);
        w.vec3(m.enclosing_size);
        w.i32(m.region_count);
        w.i32(m.total_blocks);
        w.i32(m.total_volume);
        w.i64(m.time_created);
        w.i64(m.time_modified);

        w.len(self.regions.len());
        for region in &self.regions {
            w.str(&region.name);
            w.vec3(region.origin);
            w.vec3(region.size);
            w.len(region.palette.len());
            for state in &region.palette {
                w.str(&state.name);
                w.len(state.properties.len());
                for (key, value) in &state.properties {
                    w.str(key);
                    w.str(value);
                }
            }
            let width = index_width(region.palette.len());
            w.u8(width);
            for &index in &region.blocks {
                w.0.extend_from_slice(&index.to_le_bytes()[..width as usize]);
            }
        }

        let mut gz = GzEncoder::new(Vec::new(), Compression::best());
        gz.write_all(&w.0).expect("writing to a Vec cannot fail");
        gz.finish().expect("writing to a Vec cannot fail")
    }

    /// Decode a payload made by [`Litematic::to_preview`].
    pub fn from_preview(bytes: &[u8]) -> Result<Self> {
        let mut data = Vec::new();
        GzDecoder::new(bytes).read_to_end(&mut data)?;
        let mut r = Reader(&data);

        if r.take(4)? != MAGIC {
            return Err(Error::Preview("not a preview payload".into()));
        }
        let format = r.u8()?;
        if format != FORMAT {
            return Err(Error::Preview(format!("unsupported preview format {format}")));
        }
        let data_version = r.i32()?;
        let version = r.i32()?;
        let metadata = Metadata {
            name: r.str()?,
            author: r.str()?,
            description: r.str()?,
            enclosing_size: r.vec3()?,
            region_count: r.i32()?,
            total_blocks: r.i32()?,
            total_volume: r.i32()?,
            time_created: r.i64()?,
            time_modified: r.i64()?,
        };

        let mut regions = Vec::new();
        for _ in 0..r.len()? {
            let name = r.str()?;
            let origin = r.vec3()?;
            let size = r.vec3()?;
            if size.x < 0 || size.y < 0 || size.z < 0 {
                return Err(Error::Preview(format!("region `{name}` has a negative size")));
            }

            let mut palette = Vec::new();
            for _ in 0..r.len()? {
                let state_name = r.str()?;
                let mut properties = BTreeMap::new();
                for _ in 0..r.len()? {
                    properties.insert(r.str()?, r.str()?);
                }
                palette.push(BlockState { name: state_name, properties });
            }

            let width = r.u8()?;
            if !matches!(width, 1 | 2 | 4) {
                return Err(Error::Preview(format!("region `{name}`: bad index width {width}")));
            }
            let bytes = (size.x as usize)
                .checked_mul(size.y as usize)
                .and_then(|v| v.checked_mul(size.z as usize))
                .and_then(|v| v.checked_mul(width as usize))
                .ok_or_else(|| Error::Preview(format!("region `{name}` is too large")))?;
            let raw = r.take(bytes)?;
            let blocks: Vec<u32> = raw
                .chunks_exact(width as usize)
                .map(|c| {
                    let mut b = [0u8; 4];
                    b[..c.len()].copy_from_slice(c);
                    u32::from_le_bytes(b)
                })
                .collect();
            if let Some(&index) = blocks.iter().find(|&&i| i as usize >= palette.len()) {
                return Err(Error::PaletteIndex { region: name, index, len: palette.len() });
            }
            regions.push(Region { name, origin, size, palette, blocks });
        }
        if !r.0.is_empty() {
            return Err(Error::Preview("trailing data after the last region".into()));
        }

        Ok(Litematic { data_version, version, metadata, regions })
    }
}

/// Bytes per block index for a palette of `len` entries.
fn index_width(len: usize) -> u8 {
    match len {
        0..=0x100 => 1,
        0x101..=0x1_0000 => 2,
        _ => 4,
    }
}

struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }

    fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn i64(&mut self, v: i64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn len(&mut self, v: usize) {
        let v = u32::try_from(v).expect("more than u32::MAX entries");
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn str(&mut self, s: &str) {
        self.len(s.len());
        self.0.extend_from_slice(s.as_bytes());
    }

    fn vec3(&mut self, v: Vec3i) {
        self.i32(v.x);
        self.i32(v.y);
        self.i32(v.z);
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.0.len() < n {
            return Err(Error::Preview("payload is truncated".into()));
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into().expect("took exactly N bytes"))
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    fn len(&mut self) -> Result<usize> {
        Ok(u32::from_le_bytes(self.array()?) as usize)
    }

    fn str(&mut self) -> Result<String> {
        let len = self.len()?;
        String::from_utf8(self.take(len)?.to_vec()).map_err(|_| Error::Preview("string is not UTF-8".into()))
    }

    fn vec3(&mut self) -> Result<Vec3i> {
        Ok(Vec3i { x: self.i32()?, y: self.i32()?, z: self.i32()? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(palette_len: usize) -> Litematic {
        let size = Vec3i { x: 4, y: 3, z: 5 };
        let palette: Vec<BlockState> = (0..palette_len)
            .map(|i| BlockState {
                name: if i == 0 { "minecraft:air".into() } else { format!("minecraft:block_{i}") },
                properties: if i % 2 == 1 {
                    BTreeMap::from([("facing".into(), "north".into()), ("half".into(), "top".into())])
                } else {
                    BTreeMap::new()
                },
            })
            .collect();
        let blocks = (0..60).map(|i| ((i * 7919) % palette_len) as u32).collect();
        Litematic {
            data_version: 3953,
            version: 7,
            metadata: Metadata {
                name: "預覽 test".into(),
                author: "me".into(),
                description: "line one\nline two".into(),
                enclosing_size: size,
                region_count: 1,
                total_blocks: 42,
                total_volume: 60,
                time_created: 1_700_000_000_000,
                time_modified: -1,
            },
            regions: vec![Region { name: "main".into(), origin: Vec3i { x: -2, y: 0, z: 7 }, size, palette, blocks }],
        }
    }

    #[test]
    fn roundtrip_every_index_width() {
        for palette_len in [1, 3, 256, 257, 70_000] {
            let lit = sample(palette_len);
            let bytes = lit.to_preview();
            assert_eq!(Litematic::from_preview(&bytes).unwrap(), lit);
        }
    }

    #[test]
    fn index_width_boundaries() {
        assert_eq!(index_width(256), 1);
        assert_eq!(index_width(257), 2);
        assert_eq!(index_width(65_536), 2);
        assert_eq!(index_width(65_537), 4);
    }

    #[test]
    fn rejects_truncated_payload() {
        let mut gz = GzEncoder::new(Vec::new(), Compression::fast());
        let mut raw = Vec::new();
        GzDecoder::new(&sample(3).to_preview()[..]).read_to_end(&mut raw).unwrap();
        gz.write_all(&raw[..raw.len() - 1]).unwrap();
        let err = Litematic::from_preview(&gz.finish().unwrap()).unwrap_err();
        assert!(matches!(err, Error::Preview(_)), "{err}");
    }

    #[test]
    fn rejects_other_files() {
        let mut gz = GzEncoder::new(Vec::new(), Compression::fast());
        gz.write_all(b"\x0a\x00\x00").unwrap();
        assert!(matches!(Litematic::from_preview(&gz.finish().unwrap()), Err(Error::Preview(_))));
    }
}
