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
    /// Resolves after the browser had a chance to run other tasks.
    #[wasm_bindgen(catch)]
    async fn next_task() -> Result<JsValue, JsValue>;
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

/// Faces meshed and uploaded at a time, about 100 MB of mesh. A wasm page
/// can address at most 4 GB, which large builds exceed as a single mesh.
const CHUNK_QUADS: usize = 500_000;
/// Larger models aren't exported in the browser: the whole mesh and the
/// file would both have to fit in that memory.
const EXPORT_MAX_TRIANGLES: usize = 6_000_000;

/// Whether the viewer's event loop has been torn down, and who is waiting for it.
#[derive(Default)]
struct Shutdown {
    done: bool,
    waiting: Vec<js_sys::Function>,
}

/// A resource pack or mod jar added by the user.
struct Pack {
    name: String,
    source: Arc<ZipSource<Cursor<Vec<u8>>>>,
}

#[derive(Default)]
struct State {
    vanilla: Option<Arc<MemorySource>>,
    /// User resource packs and mod jars, highest priority first.
    packs: Vec<Pack>,
    schematic: Option<Rc<Litematic>>,
    /// Bounds and triangle count of the model shown, once fully meshed.
    shown: Option<([[f32; 3]; 2], usize)>,
    /// Bumped by every rebuild, so one that is still running stops.
    generation: u64,
    /// Called with 0..1 while a model is meshed.
    progress: Option<js_sys::Function>,
}

impl State {
    /// User packs over the vanilla assets, or `None` if neither is loaded.
    fn assets(&self) -> Option<Assets> {
        if self.vanilla.is_none() && self.packs.is_empty() {
            return None;
        }
        let mut stack = PackStack::new();
        for pack in &self.packs {
            stack.push(pack.source.clone());
        }
        if let Some(vanilla) = &self.vanilla {
            stack.push(vanilla.clone());
        }
        Some(Assets::new(stack))
    }
}

/// The page's state: which assets are loaded and which schematic is shown.
///
/// Methods that change what is shown return a promise of the mesh report
/// (or `null` while assets are missing, or if a later change superseded
/// it). Large builds are meshed and uploaded in pieces, so they take a while.
///
/// Only one `App` can run at a time. Await [`App::destroy`] before creating
/// the next one, e.g. when a component that shows the viewer unmounts.
#[wasm_bindgen]
pub struct App {
    viewer: EventLoopProxy<UserEvent>,
    shutdown: Rc<RefCell<Shutdown>>,
    state: Rc<RefCell<State>>,
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
        Ok(App { viewer, shutdown, state: Rc::default() })
    }

    /// Stop the viewer and release its canvas and GPU resources. The promise
    /// resolves once a new `App` can be created. Calling it again is harmless;
    /// `free()` the object afterwards as usual.
    pub fn destroy(&mut self) -> js_sys::Promise {
        {
            let mut state = self.state.borrow_mut();
            state.schematic = None;
            state.shown = None;
            state.generation += 1;
        }
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

    /// Use these Mojang assets as the base pack.
    pub fn set_vanilla(&self, assets: &VanillaAssets) -> js_sys::Promise {
        self.state.borrow_mut().vanilla = Some(assets.source.clone());
        self.rebuild()
    }

    /// Add a resource pack zip, or a mod jar for its blocks, on top of the
    /// others. `name` is only for [`App::packs`].
    pub fn add_pack(&self, name: String, zip: Vec<u8>) -> Result<js_sys::Promise, JsError> {
        let source = Arc::new(ZipSource::from_bytes(zip)?);
        self.state.borrow_mut().packs.insert(0, Pack { name, source });
        Ok(self.rebuild())
    }

    /// Names of the added packs, highest priority first: where several have
    /// the same model or texture, the first one's is used.
    #[wasm_bindgen(getter)]
    pub fn packs(&self) -> Vec<String> {
        self.state.borrow().packs.iter().map(|p| p.name.clone()).collect()
    }

    /// Move the pack at `from` to position `to` in [`App::packs`].
    pub fn move_pack(&self, from: usize, to: usize) -> Result<js_sys::Promise, JsError> {
        {
            let mut state = self.state.borrow_mut();
            if from >= state.packs.len() || to >= state.packs.len() {
                return Err(JsError::new("no such pack"));
            }
            let pack = state.packs.remove(from);
            state.packs.insert(to, pack);
        }
        Ok(self.rebuild())
    }

    pub fn remove_pack(&self, index: usize) -> Result<js_sys::Promise, JsError> {
        {
            let mut state = self.state.borrow_mut();
            if index >= state.packs.len() {
                return Err(JsError::new("no such pack"));
            }
            state.packs.remove(index);
        }
        Ok(self.rebuild())
    }

    pub fn clear_packs(&self) -> js_sys::Promise {
        self.state.borrow_mut().packs.clear();
        self.rebuild()
    }

    /// Show a `.litematic` file.
    pub fn load_schematic(&self, bytes: &[u8]) -> Result<js_sys::Promise, JsError> {
        self.state.borrow_mut().schematic = Some(Rc::new(Litematic::from_gzip_bytes(bytes)?));
        Ok(self.rebuild())
    }

    /// Show an already decoded schematic, as made by `Litematic::to_preview`
    /// (e.g. served by a blueprint site).
    pub fn load_preview(&self, bytes: &[u8]) -> Result<js_sys::Promise, JsError> {
        self.state.borrow_mut().schematic = Some(Rc::new(Litematic::from_preview(bytes)?));
        Ok(self.rebuild())
    }

    /// Called with the fraction done, 0..1, while a model is meshed.
    pub fn set_progress(&self, callback: Option<js_sys::Function>) {
        self.state.borrow_mut().progress = callback;
    }

    #[wasm_bindgen(getter)]
    pub fn schematic_name(&self) -> Option<String> {
        self.state.borrow().schematic.as_ref().map(|s| s.metadata.name.clone())
    }

    /// How many block layers the current model has, 0 if none is shown.
    #[wasm_bindgen(getter)]
    pub fn layer_count(&self) -> u32 {
        self.state.borrow().shown.map_or(0, |([min, max], _)| (max[1] - min[1]).max(0.0) as u32)
    }

    /// Show the model layer by layer, like a 3D printing slicer: only layers
    /// `1..=layer` are drawn, counted from the bottom. `undefined` shows all.
    pub fn set_layer(&self, layer: Option<u32>) {
        let bottom = self.state.borrow().shown.map_or(0, |([min, _], _)| min[1] as i32);
        let _ = self.viewer.send_event(UserEvent::SetSlice(layer.map(|n| bottom + n as i32 - 1)));
    }

    /// Light the scene as at `time`: `day`, `sunset` or `night`. At night
    /// torches, lanterns and other light sources light up their surroundings.
    pub fn set_time(&self, time: &str) -> Result<(), JsError> {
        let time = time.parse().map_err(|e: String| JsError::new(&e))?;
        let _ = self.viewer.send_event(UserEvent::SetTime(time));
        Ok(())
    }

    /// The current model as a binary glTF file.
    pub fn export_glb(&self) -> Result<Vec<u8>, JsError> {
        let (lit, mut assets) = self.exportable()?;
        let (mesh, _) = Mesher::new(&mut assets, Default::default()).build(&lit);
        Ok(export::glb(&mesh, &lit.metadata.name))
    }

    /// The current model as a binary FBX with the texture embedded. Tints
    /// are baked into the texture because FBX readers ignore vertex colors.
    pub fn export_fbx(&self) -> Result<Vec<u8>, JsError> {
        let (lit, mut assets) = self.exportable()?;
        let options = mesher::Options { bake_tint: true, ..Default::default() };
        let (mesh, _) = Mesher::new(&mut assets, options).build(&lit);
        let name = &lit.metadata.name;
        // The first file is the .fbx; the texture is embedded in it.
        Ok(export::fbx(&mesh, name, name).swap_remove(0).data)
    }

    /// The schematic and assets to export, if it's shown and small enough.
    fn exportable(&self) -> Result<(Rc<Litematic>, Assets), JsError> {
        let state = self.state.borrow();
        let (Some(lit), Some(assets), Some((_, triangles))) = (&state.schematic, state.assets(), state.shown) else {
            return Err(JsError::new("nothing to export yet"));
        };
        if triangles > EXPORT_MAX_TRIANGLES {
            return Err(JsError::new(&format!(
                "{triangles} triangles is too large to export in the browser; use the `schematic export` command instead"
            )));
        }
        Ok((lit.clone(), assets))
    }

    fn rebuild(&self) -> js_sys::Promise {
        let (state, viewer, shutdown) = (self.state.clone(), self.viewer.clone(), self.shutdown.clone());
        wasm_bindgen_futures::future_to_promise(async move {
            match rebuild(state, viewer, shutdown).await {
                Ok(report) => Ok(report.map_or(JsValue::NULL, JsValue::from)),
                Err(e) => Err(e.into()),
            }
        })
    }
}

/// Mesh the schematic and send it to the viewer a piece at a time, so only
/// one piece is ever in memory. Returns the report, or `None` if there is
/// nothing to show yet or a later rebuild took over.
async fn rebuild(
    state: Rc<RefCell<State>>,
    viewer: EventLoopProxy<UserEvent>,
    shutdown: Rc<RefCell<Shutdown>>,
) -> Result<Option<String>, JsError> {
    let (lit, mut assets, generation) = {
        let mut s = state.borrow_mut();
        s.generation += 1;
        let (Some(lit), Some(assets)) = (s.schematic.clone(), s.assets()) else { return Ok(None) };
        s.shown = None;
        (lit, assets, s.generation)
    };
    let superseded = || state.borrow().generation != generation || shutdown.borrow().done;

    let options = mesher::Options { slice_caps: true, pbr_maps: true, ..Default::default() };
    let mut build = Mesher::new(&mut assets, options).prepare(&lit);
    // Every texture is in the atlas now; the packs' caches can go.
    drop(assets);
    let _ = viewer.send_event(UserEvent::BeginMesh(Box::new(build.atlas().clone())));

    let mut triangles = 0;
    loop {
        let mut piece = Mesh::default();
        let more = build.fill(&lit, &mut piece, CHUNK_QUADS);
        triangles += piece.triangle_count();
        let piece = Arc::new(piece);
        let uploaded = Arc::downgrade(&piece);
        let _ = viewer.send_event(UserEvent::AddMesh(piece));
        let progress = state.borrow().progress.clone();
        if let Some(progress) = progress {
            let _ = progress.call1(&JsValue::NULL, &build.progress().into());
        }
        // Let the viewer upload and drop the piece before making the next;
        // this also keeps the page responsive.
        loop {
            next_task().await.map_err(js_error)?;
            if superseded() {
                return Ok(None);
            }
            if uploaded.strong_count() == 0 {
                break;
            }
        }
        if !more {
            break;
        }
    }

    let bounds = build.bounds();
    let (_, report) = build.finish();
    let _ = viewer.send_event(UserEvent::FinishMesh(bounds));
    state.borrow_mut().shown = Some((bounds, triangles));

    let size = lit.metadata.enclosing_size;
    let blocks: usize = lit.regions.iter().map(|r| r.iter_blocks().count()).sum();
    let mut summary =
        format!("{} — {}×{}×{}, {blocks} blocks, {triangles} triangles", lit.metadata.name, size.x, size.y, size.z);
    for (name, (reason, count)) in &report.skipped {
        summary += &format!("\n⚠ {name} ×{count}: {reason}");
    }
    for texture in &report.missing_textures {
        summary += &format!("\n⚠ missing texture {texture}");
    }
    Ok(Some(summary))
}

/// Freeing the object without `destroy()` still stops the viewer.
impl Drop for App {
    fn drop(&mut self) {
        self.state.borrow_mut().generation += 1;
        if !self.shutdown.borrow().done {
            let _ = self.viewer.send_event(UserEvent::Exit);
        }
    }
}
