//! Browser front end. Minecraft's own assets are fetched straight from
//! Mojang's servers with HTTP range requests (only the ~5 MB of the client
//! jar needed for blocks), so nothing from Mojang is ever hosted here.
//!
//! Build with `scripts/build-web.sh`; this crate is empty on other targets.
#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::io::Cursor;
use std::rc::Rc;
use std::sync::Arc;

use litematic::Litematic;
use mc_assets::remote_zip::{self, TAIL_LEN};
use mc_assets::{Assets, MemorySource, PackStack, ZipSource};
use mesher::{Mesh, Mesher};
use serde::Deserialize;
use viewer::app::UserEvent;
use wasm_bindgen::prelude::*;
use winit::event_loop::EventLoopProxy;

const MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
/// Oldest release offered: Litematica exists from 1.13, but 1.14 is where
/// the block asset layout settles into its current form.
const OLDEST_VERSION: &str = "1.14";

#[wasm_bindgen(module = "/www/net.js")]
extern "C" {
    /// Bytes `start..end` of `url`, cached in the browser.
    #[wasm_bindgen(catch)]
    async fn fetch_range(url: &str, start: f64, end: f64) -> Result<JsValue, JsValue>;
    /// A text document; `cache` keeps it for later visits.
    #[wasm_bindgen(catch)]
    async fn fetch_text(url: &str, cache: bool) -> Result<JsValue, JsValue>;
}

#[wasm_bindgen(start)]
fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
}

fn js_error(e: JsValue) -> JsError {
    let message = match e.dyn_ref::<js_sys::Error>() {
        Some(error) => String::from(error.message()),
        None => e.as_string().unwrap_or_else(|| format!("{e:?}")),
    };
    JsError::new(&message)
}

async fn text(url: &str, cache: bool) -> Result<String, JsError> {
    fetch_text(url, cache).await.map_err(js_error)?.as_string().ok_or_else(|| JsError::new("expected text"))
}

async fn range(url: &str, r: std::ops::Range<u64>) -> Result<Vec<u8>, JsError> {
    let data = fetch_range(url, r.start as f64, r.end as f64).await.map_err(js_error)?;
    Ok(js_sys::Uint8Array::new(&data).to_vec())
}

#[derive(Deserialize)]
struct Manifest {
    latest: Latest,
    versions: Vec<ManifestVersion>,
}

#[derive(Deserialize)]
struct Latest {
    release: String,
}

#[derive(Deserialize)]
struct ManifestVersion {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    url: String,
}

#[derive(Deserialize)]
struct VersionInfo {
    downloads: Downloads,
}

#[derive(Deserialize)]
struct Downloads {
    client: Download,
}

#[derive(Deserialize)]
struct Download {
    url: String,
    size: u64,
}

async fn manifest() -> Result<Manifest, JsError> {
    serde_json::from_str(&text(MANIFEST_URL, false).await?).map_err(|e| JsError::new(&e.to_string()))
}

/// Release versions, newest first.
#[wasm_bindgen]
pub async fn minecraft_versions() -> Result<Vec<String>, JsError> {
    let manifest = manifest().await?;
    let mut ids = Vec::new();
    for v in manifest.versions.into_iter().filter(|v| v.kind == "release") {
        let oldest = v.id == OLDEST_VERSION;
        ids.push(v.id);
        if oldest {
            break;
        }
    }
    if let Some(i) = ids.iter().position(|id| *id == manifest.latest.release) {
        let latest = ids.remove(i);
        ids.insert(0, latest);
    }
    Ok(ids)
}

/// Block assets of one Minecraft version.
#[wasm_bindgen]
pub struct VanillaAssets {
    version: String,
    source: Arc<MemorySource>,
    downloaded: u64,
}

#[wasm_bindgen]
impl VanillaAssets {
    #[wasm_bindgen(getter)]
    pub fn version(&self) -> String {
        self.version.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn files(&self) -> usize {
        self.source.files.len()
    }

    /// Bytes fetched over the network (or from the browser cache).
    #[wasm_bindgen(getter)]
    pub fn downloaded(&self) -> f64 {
        self.downloaded as f64
    }
}

/// Fetch the block assets of `version` from Mojang. `progress` is called
/// with status messages.
#[wasm_bindgen]
pub async fn load_vanilla(version: String, progress: js_sys::Function) -> Result<VanillaAssets, JsError> {
    let report = |msg: String| {
        let _ = progress.call1(&JsValue::NULL, &msg.into());
    };
    let manifest = manifest().await?;
    let entry = manifest
        .versions
        .iter()
        .find(|v| v.id == version)
        .ok_or_else(|| JsError::new(&format!("unknown version {version}")))?;
    // Version files are content addressed, so they can be cached for good.
    let info: VersionInfo =
        serde_json::from_str(&text(&entry.url, true).await?).map_err(|e| JsError::new(&e.to_string()))?;
    let Download { url, size } = info.downloads.client;

    let mut downloaded = 0;
    report(format!("Reading the {version} client jar index…"));
    let tail = size.saturating_sub(TAIL_LEN)..size;
    downloaded += tail.end - tail.start;
    let cd = remote_zip::locate_central_directory(&range(&url, tail).await?, size)?;
    downloaded += cd.end - cd.start;
    let entries = remote_zip::parse_central_directory(&range(&url, cd.clone()).await?)?;

    let spans = remote_zip::plan(&entries, cd.start, 64 * 1024, remote_zip::is_block_asset);
    let total: u64 = spans.iter().map(|s| s.range.end - s.range.start).sum();
    let mut files = Vec::new();
    let mut done = 0;
    for span in &spans {
        report(format!("Downloading {version} block assets… {:.1} / {:.1} MB", done as f64 / 1e6, total as f64 / 1e6));
        let data = range(&url, span.range.clone()).await?;
        files.extend(remote_zip::extract(span, &data)?);
        done += span.range.end - span.range.start;
    }
    downloaded += done;
    Ok(VanillaAssets { version, source: Arc::new(remote_zip::into_source(files)), downloaded })
}

/// Whether the viewer's event loop has been torn down, and who is waiting for it.
#[derive(Default)]
struct Shutdown {
    done: bool,
    waiting: Vec<js_sys::Function>,
}

/// The page's state: which assets are loaded and which schematic is shown.
///
/// Only one `App` can run at a time. Await [`App::destroy`] before creating
/// the next one, e.g. when a component that shows the viewer unmounts.
#[wasm_bindgen]
pub struct App {
    viewer: EventLoopProxy<UserEvent>,
    shutdown: Rc<RefCell<Shutdown>>,
    vanilla: Option<Arc<MemorySource>>,
    /// User resource packs, highest priority first.
    packs: Vec<Arc<ZipSource<Cursor<Vec<u8>>>>>,
    schematic: Option<Litematic>,
    mesh: Option<Mesh>,
}

#[wasm_bindgen]
impl App {
    #[wasm_bindgen(constructor)]
    pub fn new(canvas: web_sys::HtmlCanvasElement) -> Result<App, JsError> {
        let shutdown = Rc::new(RefCell::new(Shutdown::default()));
        let on_exit = {
            let shutdown = shutdown.clone();
            move || {
                let waiting = {
                    let mut shutdown = shutdown.borrow_mut();
                    shutdown.done = true;
                    std::mem::take(&mut shutdown.waiting)
                };
                for resolve in waiting {
                    let _ = resolve.call0(&JsValue::NULL);
                }
            }
        };
        let viewer = viewer::app::spawn(canvas, on_exit).map_err(|e| match e {
            winit::error::EventLoopError::RecreationAttempt => {
                JsError::new("another viewer is still running; await its destroy() first")
            }
            e => JsError::new(&e.to_string()),
        })?;
        Ok(App { viewer, shutdown, vanilla: None, packs: Vec::new(), schematic: None, mesh: None })
    }

    /// Stop the viewer and release its canvas and GPU resources. The promise
    /// resolves once a new `App` can be created. Calling it again is harmless;
    /// `free()` the object afterwards as usual.
    pub fn destroy(&mut self) -> js_sys::Promise {
        self.schematic = None;
        self.mesh = None;
        let shutdown = self.shutdown.clone();
        let promise = js_sys::Promise::new(&mut |resolve, _reject| {
            let mut shutdown = shutdown.borrow_mut();
            if shutdown.done {
                let _ = resolve.call0(&JsValue::NULL);
            } else {
                shutdown.waiting.push(resolve);
            }
        });
        if !self.shutdown.borrow().done {
            let _ = self.viewer.send_event(UserEvent::Exit);
        }
        promise
    }

    /// Use these Mojang assets as the base pack. Returns a mesh report if a
    /// schematic is loaded.
    pub fn set_vanilla(&mut self, assets: &VanillaAssets) -> Result<Option<String>, JsError> {
        self.vanilla = Some(assets.source.clone());
        self.rebuild()
    }

    /// Add a resource pack zip on top of the others.
    pub fn add_pack(&mut self, zip: Vec<u8>) -> Result<Option<String>, JsError> {
        self.packs.insert(0, Arc::new(ZipSource::from_bytes(zip)?));
        self.rebuild()
    }

    pub fn clear_packs(&mut self) -> Result<Option<String>, JsError> {
        self.packs.clear();
        self.rebuild()
    }

    /// Show a `.litematic` file. Returns a report once assets are available.
    pub fn load_schematic(&mut self, bytes: &[u8]) -> Result<Option<String>, JsError> {
        self.schematic = Some(Litematic::from_gzip_bytes(bytes)?);
        self.rebuild()
    }

    /// Show an already decoded schematic, as made by `Litematic::to_preview`
    /// (e.g. served by a blueprint site). Returns a report once assets are available.
    pub fn load_preview(&mut self, bytes: &[u8]) -> Result<Option<String>, JsError> {
        self.schematic = Some(Litematic::from_preview(bytes)?);
        self.rebuild()
    }

    #[wasm_bindgen(getter)]
    pub fn schematic_name(&self) -> Option<String> {
        self.schematic.as_ref().map(|s| s.metadata.name.clone())
    }

    /// The current model as a binary glTF file.
    pub fn export_glb(&self) -> Result<Vec<u8>, JsError> {
        let (Some(mesh), Some(lit)) = (&self.mesh, &self.schematic) else {
            return Err(JsError::new("nothing to export yet"));
        };
        Ok(export::glb(mesh, &lit.metadata.name))
    }

    /// The current model as a binary FBX with the texture embedded. Tints
    /// are baked into the texture because FBX readers ignore vertex colors.
    pub fn export_fbx(&self) -> Result<Vec<u8>, JsError> {
        let (Some(lit), Some(mut assets)) = (&self.schematic, self.assets()) else {
            return Err(JsError::new("nothing to export yet"));
        };
        let options = mesher::Options { bake_tint: true, ..Default::default() };
        let (mesh, _) = Mesher::new(&mut assets, options).build(lit);
        let name = &lit.metadata.name;
        // The first file is the .fbx; the texture is embedded in it.
        Ok(export::fbx(&mesh, name, name).swap_remove(0).data)
    }

    /// User packs over the vanilla assets, or `None` if neither is loaded.
    fn assets(&self) -> Option<Assets> {
        if self.vanilla.is_none() && self.packs.is_empty() {
            return None;
        }
        let mut stack = PackStack::new();
        for pack in &self.packs {
            stack.push(pack.clone());
        }
        if let Some(vanilla) = &self.vanilla {
            stack.push(vanilla.clone());
        }
        Some(Assets::new(stack))
    }

    fn rebuild(&mut self) -> Result<Option<String>, JsError> {
        let (Some(lit), Some(mut assets)) = (&self.schematic, self.assets()) else { return Ok(None) };
        let (mesh, report) = Mesher::new(&mut assets, Default::default()).build(lit);

        let size = lit.metadata.enclosing_size;
        let blocks: usize = lit.regions.iter().map(|r| r.iter_blocks().count()).sum();
        let mut summary = format!(
            "{} — {}×{}×{}, {blocks} blocks, {} triangles",
            lit.metadata.name,
            size.x,
            size.y,
            size.z,
            mesh.triangle_count()
        );
        for (name, (reason, count)) in &report.skipped {
            summary += &format!("\n⚠ {name} ×{count}: {reason}");
        }
        for texture in &report.missing_textures {
            summary += &format!("\n⚠ missing texture {texture}");
        }

        let _ = self.viewer.send_event(UserEvent::SetMesh(Box::new(mesh.clone())));
        self.mesh = Some(mesh);
        Ok(Some(summary))
    }
}

/// Freeing the object without `destroy()` still stops the viewer.
impl Drop for App {
    fn drop(&mut self) {
        if !self.shutdown.borrow().done {
            let _ = self.viewer.send_event(UserEvent::Exit);
        }
    }
}
