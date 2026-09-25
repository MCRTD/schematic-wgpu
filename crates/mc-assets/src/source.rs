//! Where resource pack files come from: a directory, a zip/jar, or memory.

use std::collections::HashMap;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::{Error, Result};

/// A read-only view of a resource pack. Paths are relative to the pack root,
/// e.g. `assets/minecraft/models/block/stone.json`.
pub trait AssetSource: Send + Sync {
    fn read(&self, path: &str) -> Option<Vec<u8>>;
}

impl<T: AssetSource + ?Sized> AssetSource for std::sync::Arc<T> {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        (**self).read(path)
    }
}

/// An unpacked resource pack on disk.
pub struct DirSource {
    root: PathBuf,
}

impl DirSource {
    /// Accepts the pack root, or a folder whose only subfolder is the pack root.
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if path.join("assets").is_dir() {
            return Ok(Self { root: path.to_owned() });
        }
        let nested = std::fs::read_dir(path)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.join("assets").is_dir());
        nested
            .map(|root| Self { root })
            .ok_or_else(|| Error::NoAssets(path.display().to_string()))
    }
}

impl AssetSource for DirSource {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        std::fs::read(self.root.join(path)).ok()
    }
}

/// A zipped resource pack or a Minecraft client `.jar`.
pub struct ZipSource<R> {
    archive: Mutex<zip::ZipArchive<R>>,
    /// Prefix before `assets/`, for zips that wrap the pack in a folder.
    prefix: String,
}

impl ZipSource<std::io::Cursor<Vec<u8>>> {
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        Self::new(std::io::Cursor::new(bytes))
    }
}

impl ZipSource<std::io::BufReader<std::fs::File>> {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::new(std::io::BufReader::new(std::fs::File::open(path)?))
    }
}

impl<R: Read + Seek> ZipSource<R> {
    pub fn new(reader: R) -> Result<Self> {
        let archive = zip::ZipArchive::new(reader)?;
        let prefix = archive
            .file_names()
            .filter_map(|n| n.find("assets/").map(|i| &n[..i]))
            .min_by_key(|p| p.len())
            .ok_or_else(|| Error::NoAssets("zip archive".into()))?
            .to_owned();
        Ok(Self { archive: Mutex::new(archive), prefix })
    }
}

impl<R: Read + Seek + Send> AssetSource for ZipSource<R> {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        let mut archive = self.archive.lock().ok()?;
        let mut file = archive.by_name(&format!("{}{path}", self.prefix)).ok()?;
        let mut buf = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut buf).ok()?;
        Some(buf)
    }
}

/// An in-memory pack, mostly useful for tests.
#[derive(Default)]
pub struct MemorySource {
    pub files: HashMap<String, Vec<u8>>,
}

impl MemorySource {
    pub fn insert(&mut self, path: impl Into<String>, data: impl Into<Vec<u8>>) -> &mut Self {
        self.files.insert(path.into(), data.into());
        self
    }
}

impl AssetSource for MemorySource {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        self.files.get(path).cloned()
    }
}

/// Several packs layered on top of each other; earlier packs win.
#[derive(Default)]
pub struct PackStack {
    packs: Vec<Box<dyn AssetSource>>,
}

impl PackStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a pack with lower priority than all packs added so far.
    pub fn push(&mut self, pack: impl AssetSource + 'static) -> &mut Self {
        self.packs.push(Box::new(pack));
        self
    }

    /// Open a directory, `.zip` or `.jar` by path.
    pub fn push_path(&mut self, path: impl AsRef<Path>) -> Result<&mut Self> {
        let path = path.as_ref();
        if path.is_dir() {
            Ok(self.push(DirSource::new(path)?))
        } else {
            Ok(self.push(ZipSource::open(path)?))
        }
    }
}

impl AssetSource for PackStack {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        self.packs.iter().find_map(|p| p.read(path))
    }
}
