//! Read selected files from a zip that can only be fetched in byte ranges,
//! such as a Minecraft client jar served over HTTP.
//!
//! This module does no I/O. The caller fetches:
//! 1. the last [`TAIL_LEN`] bytes, passed to [`locate_central_directory`];
//! 2. that range, passed to [`parse_central_directory`];
//! 3. each range from [`plan`], passed to [`extract`].
//!
//! Entries stored next to each other are merged into one range, so a few
//! requests cover thousands of small files.

use std::io::Read;
use std::ops::Range;

use crate::source::MemorySource;

/// Enough to contain the end-of-central-directory record and a maximal comment.
pub const TAIL_LEN: u64 = 22 + 65535;

#[derive(Debug, thiserror::Error)]
pub enum ZipError {
    #[error("end of central directory not found")]
    NoEndRecord,
    #[error("zip64 archives are not supported")]
    Zip64,
    #[error("corrupt zip: {0}")]
    Corrupt(&'static str),
    #[error("unsupported compression method {method} for {name}")]
    Method { name: String, method: u16 },
    #[error("failed to decompress {name}: {source}")]
    Inflate { name: String, source: std::io::Error },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub method: u16,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    /// Offset of the entry's local header in the archive.
    pub header_offset: u64,
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

/// Find the central directory from the last bytes of an archive of `file_len` bytes.
pub fn locate_central_directory(tail: &[u8], file_len: u64) -> Result<Range<u64>, ZipError> {
    let pos = (0..=tail.len().saturating_sub(22))
        .rev()
        .find(|&i| u32_at(tail, i) == Some(0x0605_4b50))
        .ok_or(ZipError::NoEndRecord)?;
    let size = u32_at(tail, pos + 12).ok_or(ZipError::NoEndRecord)?;
    let offset = u32_at(tail, pos + 16).ok_or(ZipError::NoEndRecord)?;
    if size == u32::MAX || offset == u32::MAX {
        return Err(ZipError::Zip64);
    }
    let range = offset as u64..offset as u64 + size as u64;
    if range.end > file_len {
        return Err(ZipError::Corrupt("central directory past end of file"));
    }
    Ok(range)
}

pub fn parse_central_directory(bytes: &[u8]) -> Result<Vec<Entry>, ZipError> {
    let mut entries = Vec::new();
    let mut i = 0;
    while i + 46 <= bytes.len() {
        if u32_at(bytes, i) != Some(0x0201_4b50) {
            return Err(ZipError::Corrupt("bad central directory signature"));
        }
        let field = |off| u16_at(bytes, i + off).ok_or(ZipError::Corrupt("truncated entry"));
        let word = |off| u32_at(bytes, i + off).ok_or(ZipError::Corrupt("truncated entry"));
        let (name_len, extra_len, comment_len) = (field(28)? as usize, field(30)? as usize, field(32)? as usize);
        let name = bytes.get(i + 46..i + 46 + name_len).ok_or(ZipError::Corrupt("truncated name"))?;
        entries.push(Entry {
            name: String::from_utf8_lossy(name).into_owned(),
            method: field(10)?,
            compressed_size: word(20)? as u64,
            uncompressed_size: word(24)? as u64,
            header_offset: word(42)? as u64,
        });
        i += 46 + name_len + extra_len + comment_len;
    }
    Ok(entries)
}

/// One byte range to fetch and the entries it contains.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub range: Range<u64>,
    pub entries: Vec<Entry>,
}

/// Group the entries accepted by `wanted` into ranges, merging neighbours
/// separated by less than `max_gap` bytes. `data_end` is where entry data
/// stops (the central directory's offset).
pub fn plan(entries: &[Entry], data_end: u64, max_gap: u64, wanted: impl Fn(&str) -> bool) -> Vec<Span> {
    let mut sorted: Vec<&Entry> = entries.iter().collect();
    sorted.sort_by_key(|e| e.header_offset);

    let mut spans: Vec<Span> = Vec::new();
    for (k, entry) in sorted.iter().enumerate() {
        if !wanted(&entry.name) || entry.name.ends_with('/') {
            continue;
        }
        // The local header's extra field can differ from the central one,
        // so read up to where the next entry starts.
        let end = sorted.get(k + 1).map_or(data_end, |next| next.header_offset);
        match spans.last_mut() {
            Some(span) if entry.header_offset.saturating_sub(span.range.end) < max_gap => {
                span.range.end = end;
                span.entries.push((*entry).clone());
            }
            _ => spans.push(Span { range: entry.header_offset..end, entries: vec![(*entry).clone()] }),
        }
    }
    spans
}

/// Decompress every entry of a fetched span.
pub fn extract(span: &Span, data: &[u8]) -> Result<Vec<(String, Vec<u8>)>, ZipError> {
    span.entries
        .iter()
        .map(|e| {
            let start = (e.header_offset - span.range.start) as usize;
            if u32_at(data, start) != Some(0x0403_4b50) {
                return Err(ZipError::Corrupt("bad local header signature"));
            }
            let name_len = u16_at(data, start + 26).ok_or(ZipError::Corrupt("truncated header"))? as usize;
            let extra_len = u16_at(data, start + 28).ok_or(ZipError::Corrupt("truncated header"))? as usize;
            let body = start + 30 + name_len + extra_len;
            let compressed = data
                .get(body..body + e.compressed_size as usize)
                .ok_or(ZipError::Corrupt("entry data past end of span"))?;
            let bytes = match e.method {
                0 => compressed.to_vec(),
                8 => {
                    let mut out = Vec::with_capacity(e.uncompressed_size as usize);
                    flate2::read::DeflateDecoder::new(compressed)
                        .read_to_end(&mut out)
                        .map_err(|source| ZipError::Inflate { name: e.name.clone(), source })?;
                    out
                }
                method => return Err(ZipError::Method { name: e.name.clone(), method }),
            };
            Ok((e.name.clone(), bytes))
        })
        .collect()
}

/// Entity textures of blocks drawn by block entity renderers (chests,
/// banners, beds, signs, heads, shulker boxes).
const BLOCK_ENTITY_TEXTURES: &[&str] = &[
    "banner/",
    "banner_base.png",
    "bed/",
    "chest/",
    "creeper/creeper.png",
    "piglin/piglin.png",
    "player/wide/steve.png",
    "shulker/",
    "signs/",
    "skeleton/skeleton.png",
    "skeleton/wither_skeleton.png",
    "steve.png",
    "zombie/zombie.png",
];

/// The parts of a resource pack needed to build block meshes.
pub fn is_block_asset(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("assets/") else { return false };
    let Some((_, rest)) = rest.split_once('/') else { return false };
    let texture = rest.ends_with(".png") || rest.ends_with(".mcmeta");
    rest.starts_with("blockstates/")
        || rest.starts_with("models/block/")
        || (texture && rest.starts_with("textures/block/"))
        || (texture && rest.strip_prefix("textures/entity/").is_some_and(|e| BLOCK_ENTITY_TEXTURES.iter().any(|p| e.starts_with(p))))
}

pub fn into_source(files: impl IntoIterator<Item = (String, Vec<u8>)>) -> MemorySource {
    let mut source = MemorySource::default();
    for (name, data) in files {
        source.insert(name, data);
    }
    source
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn build_zip(files: &[(&str, Vec<u8>, bool)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, data, compress) in files {
            let method = if *compress { zip::CompressionMethod::Deflated } else { zip::CompressionMethod::Stored };
            let options = zip::write::SimpleFileOptions::default().compression_method(method);
            writer.start_file(*name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        let _ = writer.set_comment("a comment before the end record");
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn fetches_only_wanted_entries_in_few_ranges() {
        let noise: Vec<u8> = (0..200_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        let files = [
            ("assets/minecraft/blockstates/stone.json", br#"{"variants":{}}"#.to_vec(), true),
            ("assets/minecraft/models/block/stone.json", b"{}".repeat(500), true),
            ("assets/minecraft/textures/block/stone.png", vec![7u8; 300], false),
            ("assets/minecraft/sounds/big.ogg", noise, false),
            ("assets/minecraft/textures/block/dirt.png", vec![9u8; 40], true),
            ("assets/minecraft/textures/item/apple.png", vec![1u8; 40], true),
        ];
        let zip = build_zip(&files);
        let len = zip.len() as u64;
        let fetch = |r: &Range<u64>| &zip[r.start as usize..r.end as usize];

        let tail_start = len.saturating_sub(TAIL_LEN);
        let cd = locate_central_directory(fetch(&(tail_start..len)), len).unwrap();
        let entries = parse_central_directory(fetch(&cd)).unwrap();
        assert_eq!(entries.len(), files.len());

        let spans = plan(&entries, cd.start, 64 * 1024, is_block_asset);
        // The 200 KB sound file splits the wanted files into two ranges.
        assert_eq!(spans.len(), 2);
        let fetched: u64 = spans.iter().map(|s| s.range.end - s.range.start).sum();
        assert!(fetched < 10_000, "fetched {fetched} bytes");

        let mut got: Vec<(String, Vec<u8>)> = spans.iter().flat_map(|s| extract(s, fetch(&s.range)).unwrap()).collect();
        got.sort();
        let mut expected: Vec<(String, Vec<u8>)> = files
            .iter()
            .filter(|(n, _, _)| is_block_asset(n))
            .map(|(n, d, _)| (n.to_string(), d.clone()))
            .collect();
        expected.sort();
        assert_eq!(got, expected);
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(locate_central_directory(&[0u8; 100], 100), Err(ZipError::NoEndRecord)));
        assert!(parse_central_directory(&[1u8; 60]).is_err());
    }

    #[test]
    fn block_asset_filter() {
        assert!(is_block_asset("assets/minecraft/blockstates/stone.json"));
        assert!(is_block_asset("assets/create/models/block/cog.json"));
        assert!(is_block_asset("assets/minecraft/textures/block/water_still.png.mcmeta"));
        assert!(!is_block_asset("assets/minecraft/textures/item/apple.png"));
        assert!(is_block_asset("assets/minecraft/textures/entity/chest/normal_left.png"));
        assert!(is_block_asset("assets/minecraft/textures/entity/player/wide/steve.png"));
        assert!(!is_block_asset("assets/minecraft/textures/entity/player/slim/alex.png"));
        assert!(!is_block_asset("assets/minecraft/textures/entity/zombie/drowned.png"));
        assert!(!is_block_asset("assets/minecraft/textures/block/readme.txt"));
        assert!(!is_block_asset("data/minecraft/recipes/stone.json"));
    }
}
