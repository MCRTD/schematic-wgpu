//! Builds textured triangle meshes from Litematica schematics.

pub mod atlas;
pub mod bake;
mod block_entity;
mod light;
mod tint;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use glam::Vec3;
use litematic::{BlockState, Litematic, Nbt, Region, Vec3i};
use mc_assets::{Assets, Direction, Texture};
pub use mc_assets::Transparency;

pub use atlas::{Atlas, PbrMaps};
use bake::Quad;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vertex {
    /// World position; one block is one unit, +X east, +Y up, +Z south.
    pub position: [f32; 3],
    pub normal: [f32; 3],
    /// UV into the atlas.
    pub uv: [f32; 2],
    /// Tint multiplied with the texture, RGBA8.
    pub color: [u8; 4],
    /// Ambient occlusion brightness from neighbouring blocks, 0..1 (1 = unoccluded).
    /// Kept apart from `color` so renderers and exporters can choose to use it.
    pub ao: f32,
    /// World Y of the block this vertex belongs to, for showing the build
    /// layer by layer. Parts of a model that stick out of its block still
    /// count as that block's layer.
    pub block_y: i32,
    /// Light from torches, lanterns and other light sources reaching this
    /// corner, 0..=15 as in Minecraft; see [`Options::block_light`].
    pub block_light: u8,
}

#[derive(Debug, Clone, Default)]
pub struct MeshLayer {
    pub vertices: Vec<Vertex>,
    /// Triangle list, counter-clockwise front faces.
    pub indices: Vec<u32>,
}

impl MeshLayer {
    fn push_quad(&mut self, corners: [Vertex; 4]) {
        let base = self.vertices.len() as u32;
        // Split along the diagonal with the smoother occlusion gradient, as
        // Minecraft does, so dark corners don't bleed across the quad.
        let ao = corners.map(|v| v.ao);
        let order = if ao[0] + ao[2] < ao[1] + ao[3] { [1, 2, 3, 1, 3, 0] } else { [0, 1, 2, 0, 2, 3] };
        self.vertices.extend(corners);
        self.indices.extend(order.map(|i| base + i));
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

/// Faces are split by how they blend, so renderers and exporters can pick
/// the right alpha mode for each.
#[derive(Debug, Clone, Default)]
pub struct Mesh {
    pub opaque: MeshLayer,
    /// Fully transparent texels are discarded (alpha test).
    pub cutout: MeshLayer,
    /// Alpha blended; draw after the other layers.
    pub translucent: MeshLayer,
    pub atlas: Option<Atlas>,
    /// World-space bounds of all blocks, `[min, max]`.
    pub bounds: [[f32; 3]; 2],
    /// With [`Options::slice_caps`]: top faces hidden only by the block
    /// above, indexed by `Transparency as usize`. A layer view draws them on
    /// the topmost visible layer so cut blocks look solid. Not part of
    /// [`Mesh::layers`], so exporters leave them out.
    pub caps: [MeshLayer; 3],
}

impl Mesh {
    pub fn layers(&self) -> [(Transparency, &MeshLayer); 3] {
        [
            (Transparency::Opaque, &self.opaque),
            (Transparency::Cutout, &self.cutout),
            (Transparency::Translucent, &self.translucent),
        ]
    }

    fn layer_mut(&mut self, t: Transparency) -> &mut MeshLayer {
        match t {
            Transparency::Opaque => &mut self.opaque,
            Transparency::Cutout => &mut self.cutout,
            Transparency::Translucent => &mut self.translucent,
        }
    }

    /// Like [`Mesh::layers`], for [`Mesh::caps`].
    pub fn caps(&self) -> [(Transparency, &MeshLayer); 3] {
        [
            (Transparency::Opaque, &self.caps[Transparency::Opaque as usize]),
            (Transparency::Cutout, &self.caps[Transparency::Cutout as usize]),
            (Transparency::Translucent, &self.caps[Transparency::Translucent as usize]),
        ]
    }

    pub fn triangle_count(&self) -> usize {
        self.layers().iter().map(|(_, l)| l.indices.len() / 3).sum()
    }
}

/// Blocks and textures that could not be drawn.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// Block name → reason and number of blocks affected.
    pub skipped: BTreeMap<String, (String, usize)>,
    pub missing_textures: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Draw blocks that failed to resolve as missing-texture cubes instead
    /// of leaving them out.
    pub placeholder_for_missing: bool,
    /// Multiply tints (grass, leaves, water, redstone) into copies of their
    /// textures instead of using vertex colors. Formats and tools that
    /// ignore vertex colors then still show the right colors.
    pub bake_tint: bool,
    /// Compute [`Vertex::ao`] from neighbouring blocks; otherwise it is always 1.
    pub ambient_occlusion: bool,
    /// Fill [`Mesh::caps`], for viewers that show the build layer by layer.
    pub slice_caps: bool,
    /// Read each texture's LabPBR maps (`<texture>_n` and `_s`) into
    /// [`Atlas::pbr`], for renderers that use them.
    pub pbr_maps: bool,
    /// Spread light from light-emitting blocks into [`Vertex::block_light`];
    /// otherwise it is 0.
    pub block_light: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { placeholder_for_missing: true, bake_tint: false, ambient_occlusion: true, slice_caps: false, pbr_maps: false, block_light: true }
    }
}

/// Brightness for 0..=3 unoccluded neighbours around a vertex.
const AO_LEVELS: [f32; 4] = [0.45, 0.62, 0.8, 1.0];

/// The quads of one weighted variant, each with its atlas sprite.
type Choice = (u32, Vec<(Quad, u32)>);

/// A palette entry baked into quads. Weighted variants have one quad list
/// per choice; `parts` are always all drawn.
#[derive(Debug, Default)]
struct BakedState {
    parts: Vec<Vec<Choice>>,
    /// Whether the block completely hides the face of a neighbour touching
    /// each side, indexed by `Direction as usize`.
    opaque_sides: [bool; 6],
    /// Whether the block fills each side at all, for culling between
    /// identical transparent blocks such as glass.
    full_sides: [bool; 6],
    /// Whether the block is a full opaque cube that casts ambient occlusion.
    occludes: bool,
    tint: [u8; 3],
    /// Liquids hide faces touching the same liquid at any level.
    fluid: Option<&'static str>,
    /// Block light the block gives off, 0..=15.
    emission: u8,
    /// Counted in [`Report::skipped`].
    reported: bool,
    skip: bool,
}

const LIQUIDS: [&str; 2] = ["water", "lava"];

/// Converts schematic blocks into a [`Mesh`].
pub struct Mesher<'a> {
    assets: &'a mut Assets,
    options: Options,
    sprites: Vec<Arc<Texture>>,
    /// LabPBR maps of each sprite, with [`Options::pbr_maps`].
    sprite_maps: Vec<atlas::SpriteMaps>,
    /// Keyed by texture and, with [`Options::bake_tint`], the baked tint.
    sprite_ids: HashMap<(Option<String>, Option<[u8; 3]>), u32>,
    missing_sprite: Option<u32>,
    report: Report,
}

impl<'a> Mesher<'a> {
    pub fn new(assets: &'a mut Assets, options: Options) -> Self {
        Self {
            assets,
            options,
            sprites: Vec::new(),
            sprite_maps: Vec::new(),
            sprite_ids: HashMap::new(),
            missing_sprite: None,
            report: Report::default(),
        }
    }

    pub fn build(self, schematic: &Litematic) -> (Mesh, Report) {
        let mut build = self.prepare(schematic);
        let mut mesh = Mesh::default();
        build.fill(schematic, &mut mesh, usize::MAX);
        let (atlas, report) = build.finish();
        mesh.atlas = Some(atlas);
        (mesh, report)
    }

    /// Resolve every block state and texture of `schematic` and pack the
    /// atlas, so the blocks can then be meshed a piece at a time with
    /// [`Build::fill`]. That keeps memory bounded for builds too large to
    /// hold as one [`Mesh`].
    pub fn prepare(mut self, schematic: &Litematic) -> Build {
        let mut baked = Vec::new();
        let mut variants = Vec::new();
        let mut lights = Vec::new();
        for region in &schematic.regions {
            let mut states: Vec<BakedState> = region.palette.iter().map(|s| self.bake_state(s, None)).collect();
            // Blocks whose block entity data changes their look (banner
            // patterns) get extra states after the palette's.
            let mut by_block = HashMap::new();
            let mut by_look = HashMap::new();
            for entity in &region.block_entities {
                let p = entity.pos;
                let inside = (0..region.size.x).contains(&p.x) && (0..region.size.y).contains(&p.y) && (0..region.size.z).contains(&p.z);
                if !inside {
                    continue;
                }
                let i = region.index(p.x, p.y, p.z);
                let palette_index = region.blocks[i];
                let state = &region.palette[palette_index as usize];
                let Some(key) = block_entity::variant_key(state, &entity.nbt) else { continue };
                let index = *by_look.entry((palette_index, key)).or_insert_with(|| {
                    states.push(self.bake_state(state, Some(&entity.nbt)));
                    states.len() - 1
                });
                by_block.insert(i, index);
            }
            let palette_emission: Vec<u8> = states[..region.palette.len()].iter().map(|s| s.emission).collect();
            let block_light = if self.options.block_light && palette_emission.iter().any(|&e| e > 0) {
                light::spread(region, |i| palette_emission[region.blocks[i] as usize], |c| occludes(region, &states, c))
            } else {
                light::LightMap::new()
            };
            baked.push(states);
            variants.push(by_block);
            lights.push(block_light);
        }
        Build {
            atlas: Atlas::pack_with_pbr(&self.sprites, &self.sprite_maps),
            transparency: self.sprites.iter().map(|s| s.transparency).collect(),
            options: self.options,
            baked,
            variants,
            lights,
            report: self.report,
            bounds: [Vec3::MAX, Vec3::MIN],
            region: 0,
            next: 0,
            done: 0,
            total: schematic.regions.iter().map(|r| r.blocks.len()).sum(),
        }
    }

    /// Atlas sprite index for a texture; missing textures share one sprite.
    fn sprite(&mut self, texture: Option<String>) -> u32 {
        let key = (texture, None);
        if let Some(&id) = self.sprite_ids.get(&key) {
            return id;
        }
        let texture = key.0;
        let loaded = match texture.as_deref() {
            Some(t) if t.starts_with('+') => {
                let assets = &mut *self.assets;
                block_entity::compose(&t[1..], |l| assets.texture(l), &mut self.report.missing_textures).map(Arc::new)
            }
            Some(t) => {
                let loaded = self.assets.texture(t);
                if loaded.is_none() {
                    self.report.missing_textures.push(t.to_owned());
                }
                loaded
            }
            None => None,
        };
        let maps = match (&loaded, texture.as_deref()) {
            (Some(_), Some(t)) if self.options.pbr_maps && !t.starts_with('+') => {
                [self.assets.texture(&format!("{t}_n")), self.assets.texture(&format!("{t}_s"))]
            }
            _ => [None, None],
        };
        let id = match loaded {
            Some(loaded) => {
                // Packs often add detailed maps to the vanilla colors; keep
                // the maps' detail by enlarging the color sprite.
                let widest = maps.iter().flatten().map(|m| m.width).max().unwrap_or(0);
                let loaded = if widest > loaded.width && widest % loaded.width == 0 {
                    Arc::new(enlarged(&loaded, widest / loaded.width))
                } else {
                    loaded
                };
                self.push_sprite(loaded, maps)
            }
            None => {
                match self.missing_sprite {
                    Some(id) => id,
                    None => {
                        let id = self.push_sprite(Arc::new(Texture::missing()), [None, None]);
                        self.missing_sprite = Some(id);
                        id
                    }
                }
            }
        };
        self.sprite_ids.insert((texture, None), id);
        id
    }

    /// A copy of a texture's sprite with `tint` multiplied into its colors.
    fn tinted_sprite(&mut self, texture: Option<String>, tint: [u8; 3]) -> u32 {
        let key = (texture, Some(tint));
        if let Some(&id) = self.sprite_ids.get(&key) {
            return id;
        }
        let base = self.sprite(key.0.clone());
        let source = &self.sprites[base as usize];
        let mut rgba = source.rgba.clone();
        for pixel in rgba.chunks_exact_mut(4) {
            for (c, t) in pixel.iter_mut().zip(tint) {
                *c = (*c as u16 * t as u16 / 255) as u8;
            }
        }
        let tinted = Texture::from_rgba(source.width, source.height, rgba);
        let maps = self.sprite_maps[base as usize].clone();
        let id = self.push_sprite(Arc::new(tinted), maps);
        self.sprite_ids.insert(key, id);
        id
    }

    fn push_sprite(&mut self, texture: Arc<Texture>, maps: atlas::SpriteMaps) -> u32 {
        self.sprites.push(texture);
        self.sprite_maps.push(maps);
        self.sprites.len() as u32 - 1
    }

    fn report_skip(&mut self, state: &BlockState, reason: String) {
        self.report.skipped.entry(state.name.clone()).or_insert((reason, 0));
    }

    /// `nbt` is the block entity data of a block that [`block_entity::variant_key`] picked out.
    fn bake_state(&mut self, state: &BlockState, nbt: Option<&Nbt>) -> BakedState {
        if state.is_air() {
            return BakedState { skip: true, ..Default::default() };
        }
        let name = state.short_name();
        let mut reported = false;
        let quads: Vec<Vec<(u32, Vec<Quad>)>> = if LIQUIDS.contains(&name) {
            vec![vec![(1, liquid_quads(name, &state.properties))]]
        } else if let Some(nbt) = nbt
            && let Some(quads) = block_entity::quads(state, Some(nbt))
        {
            vec![vec![(1, quads)]]
        } else {
            let resolved = self.assets.resolve(&state.name, &state.properties).map(|parts| {
                parts
                    .iter()
                    .map(|choices| choices.iter().map(|m| (m.weight, bake::bake(m))).collect::<Vec<_>>())
                    .collect::<Vec<_>>()
            });
            // Blocks drawn by code have no model geometry; packs may still
            // give them some, which then wins.
            let geometry = matches!(&resolved, Ok(parts) if parts.iter().flatten().any(|(_, q)| !q.is_empty()));
            let entity = if geometry { None } else { block_entity::quads(state, None) };
            match (entity, resolved) {
                (Some(quads), _) => vec![vec![(1, quads)]],
                (None, Ok(parts)) => parts,
                (None, Err(e)) if self.options.placeholder_for_missing => {
                    reported = true;
                    self.report_skip(state, e.to_string());
                    vec![vec![(1, placeholder_cube())]]
                }
                (None, Err(e)) => {
                    self.report_skip(state, e.to_string());
                    return BakedState { skip: true, reported: true, ..Default::default() };
                }
            }
        };

        if quads.iter().flatten().all(|(_, quads)| quads.is_empty()) {
            reported = true;
            self.report_skip(state, "no model geometry (drawn by a block entity renderer)".into());
        }

        // Culling data comes from the first choice of each part; weighted
        // variants of one block share their shape.
        let mut opaque_sides = [false; 6];
        let mut full_sides = [false; 6];
        for first in quads.iter().filter_map(|p| p.first()).map(|(_, q)| q) {
            for quad in first {
                let Some(dir) = quad.cullface else { continue };
                if !quad.covers_face(dir) {
                    continue;
                }
                full_sides[dir as usize] = true;
                let sprite = self.sprite(quad.texture.clone());
                if self.sprites[sprite as usize].transparency == Transparency::Opaque {
                    opaque_sides[dir as usize] = true;
                }
            }
        }
        let fluid = LIQUIDS.into_iter().find(|&l| l == name);
        let tint = tint::color(name, &state.properties);
        let mut with_sprite = |quad: Quad| {
            let sprite = if quad.tinted && self.options.bake_tint {
                self.tinted_sprite(quad.texture.clone(), tint)
            } else {
                self.sprite(quad.texture.clone())
            };
            (quad, sprite)
        };
        let parts = quads
            .into_iter()
            .map(|choices| choices.into_iter().map(|(w, quads)| (w, quads.into_iter().map(&mut with_sprite).collect())).collect())
            .collect();
        BakedState {
            parts,
            opaque_sides,
            full_sides,
            occludes: fluid.is_none() && opaque_sides.iter().all(|&o| o),
            tint,
            fluid,
            emission: light::emission(state),
            reported,
            skip: false,
        }
    }
}

/// A schematic being meshed piece by piece, from [`Mesher::prepare`].
pub struct Build {
    atlas: Atlas,
    /// By sprite index.
    transparency: Vec<Transparency>,
    options: Options,
    /// By region, then palette index; then variants of palette entries.
    baked: Vec<Vec<BakedState>>,
    /// By region: block index → index in `baked` of its variant.
    variants: Vec<HashMap<usize, usize>>,
    /// By region: block light levels.
    lights: Vec<light::LightMap>,
    report: Report,
    bounds: [Vec3; 2],
    /// Next block to mesh: region and index into its blocks.
    region: usize,
    next: usize,
    /// Blocks visited so far, and in all regions.
    done: usize,
    total: usize,
}

impl Build {
    /// The texture atlas every piece of the mesh uses.
    pub fn atlas(&self) -> &Atlas {
        &self.atlas
    }

    /// Mesh blocks into `mesh` until it gained at least `max_quads` quads
    /// (counting [`Mesh::caps`]) or all blocks are done; returns whether
    /// blocks remain. `schematic` must be the one given to [`Mesher::prepare`].
    /// `mesh.bounds` is set to the bounds of all blocks meshed so far, and
    /// `mesh.atlas` is left alone.
    pub fn fill(&mut self, schematic: &Litematic, mesh: &mut Mesh, max_quads: usize) -> bool {
        let mut added = 0;
        while let Some(region) = schematic.regions.get(self.region) {
            let baked = &self.baked[self.region];
            let origin = Vec3::new(region.origin.x as f32, region.origin.y as f32, region.origin.z as f32);
            while self.next < region.blocks.len() && added < max_quads {
                let i = self.next;
                self.next += 1;
                self.done += 1;
                let index = region.blocks[i] as usize;
                let state = &baked[self.variants[self.region].get(&i).copied().unwrap_or(index)];
                if state.reported {
                    let name = &region.palette[index].name;
                    if let Some(entry) = self.report.skipped.get_mut(name) {
                        entry.1 += 1;
                    }
                }
                if state.skip {
                    continue;
                }
                let pos = region.position(i);
                let block_pos = origin + Vec3::new(pos.x as f32, pos.y as f32, pos.z as f32);
                self.bounds = [self.bounds[0].min(block_pos), self.bounds[1].max(block_pos + Vec3::ONE)];

                for part in &state.parts {
                    let Some(quads) = pick_weighted(part, pos) else { continue };
                    for (quad, sprite) in quads {
                        let mut cap = false;
                        if let Some(dir) = quad.cullface
                            && is_hidden(region, baked, pos, index, dir)
                        {
                            if dir != Direction::Up || !self.options.slice_caps {
                                continue;
                            }
                            cap = true;
                        }
                        // Baked tints are already in the sprite.
                        let [r, g, b] = if quad.tinted && !self.options.bake_tint { state.tint } else { [255; 3] };
                        let transparency = self.transparency[*sprite as usize];
                        let rect = self.atlas.rects[*sprite as usize];
                        // A cap is seen with everything above it cut away.
                        let ao = if self.options.ambient_occlusion && quad.ambient_occlusion && !cap {
                            vertex_ao(region, baked, pos, quad)
                        } else {
                            [1.0; 4]
                        };
                        let light = if self.options.block_light {
                            vertex_light(region, baked, &self.lights[self.region], pos, quad, state.emission)
                        } else {
                            [0; 4]
                        };
                        let corners = std::array::from_fn(|i| Vertex {
                            position: (block_pos + quad.positions[i]).into(),
                            normal: quad.normal.into(),
                            uv: rect.map(quad.uvs[i]),
                            color: [r, g, b, 255],
                            ao: ao[i],
                            block_y: region.origin.y + pos.y,
                            block_light: light[i],
                        });
                        let layer = if cap { &mut mesh.caps[transparency as usize] } else { mesh.layer_mut(transparency) };
                        layer.push_quad(corners);
                        added += 1;
                    }
                }
            }
            if self.next < region.blocks.len() {
                break;
            }
            self.region += 1;
            self.next = 0;
        }
        mesh.bounds = self.bounds();
        self.region < schematic.regions.len()
    }

    /// Fraction of the blocks meshed so far, 0..=1.
    pub fn progress(&self) -> f32 {
        if self.total == 0 { 1.0 } else { self.done as f32 / self.total as f32 }
    }

    /// World-space bounds of the blocks meshed so far, `[min, max]`.
    pub fn bounds(&self) -> [[f32; 3]; 2] {
        let [min, max] = self.bounds;
        if min.x <= max.x { [min.into(), max.into()] } else { [[0.0; 3]; 2] }
    }

    pub fn finish(mut self) -> (Atlas, Report) {
        self.report.missing_textures.sort();
        self.report.missing_textures.dedup();
        (self.atlas, self.report)
    }
}

/// `texture` with every texel repeated `factor` times in each direction.
fn enlarged(texture: &Texture, factor: u32) -> Texture {
    let (width, height) = (texture.width * factor, texture.height * factor);
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let i = (((y / factor) * texture.width + x / factor) * 4) as usize;
            rgba.extend_from_slice(&texture.rgba[i..i + 4]);
        }
    }
    Texture::from_rgba(width, height, rgba)
}

/// Deterministically choose a weighted variant based on block position.
fn pick_weighted<T>(choices: &[(u32, Vec<T>)], pos: Vec3i) -> Option<&Vec<T>> {
    let total: u32 = choices.iter().map(|(w, _)| *w).sum();
    if choices.len() <= 1 || total == 0 {
        return choices.first().map(|(_, q)| q);
    }
    let hash = (pos.x as u32).wrapping_mul(3_129_871)
        ^ (pos.z as u32).wrapping_mul(116_129_781)
        ^ (pos.y as u32).wrapping_mul(0x9E37_79B9);
    let mut roll = hash.wrapping_mul(0x85EB_CA6B).rotate_right(13) % total;
    choices.iter().find_map(|(w, q)| {
        if roll < *w {
            Some(q)
        } else {
            roll -= w;
            None
        }
    })
}

/// Whether the face of the block at `pos` on side `dir` is hidden by its neighbour.
fn is_hidden(region: &Region, baked: &[BakedState], pos: Vec3i, index: usize, dir: Direction) -> bool {
    let [dx, dy, dz] = dir.normal();
    let (x, y, z) = (pos.x + dx, pos.y + dy, pos.z + dz);
    let inside = (0..region.size.x).contains(&x) && (0..region.size.y).contains(&y) && (0..region.size.z).contains(&z);
    if !inside {
        return false;
    }
    let neighbour_index = region.blocks[region.index(x, y, z)] as usize;
    let neighbour = &baked[neighbour_index];
    let side = dir.opposite() as usize;
    neighbour.opaque_sides[side]
        || (neighbour_index == index && neighbour.full_sides[side])
        || (neighbour.fluid.is_some() && neighbour.fluid == baked[index].fluid)
}

/// Whether the block at `pos` (region-local) is a full opaque cube.
fn occludes(region: &Region, baked: &[BakedState], [x, y, z]: [i32; 3]) -> bool {
    let inside = (0..region.size.x).contains(&x) && (0..region.size.y).contains(&y) && (0..region.size.z).contains(&z);
    inside && baked[region.blocks[region.index(x, y, z)] as usize].occludes
}

/// Minecraft-style ambient occlusion for each corner of an axis-aligned quad:
/// every corner looks at the two edge neighbours and the diagonal neighbour in
/// the layer of blocks the face looks into. Other quads are left unoccluded.
fn vertex_ao(region: &Region, baked: &[BakedState], pos: Vec3i, quad: &Quad) -> [f32; 4] {
    let n = quad.normal;
    let Some(axis) = (0..3).find(|&a| n[a].abs() > 0.999) else { return [1.0; 4] };
    let sign = n[axis].signum() as i32;
    let (t1, t2) = ((axis + 1) % 3, (axis + 2) % 3);

    // Faces on the block's boundary look into the neighbouring layer; inset
    // faces (slab tops, stair steps) look into the block's own layer.
    let plane = quad.positions[0][axis];
    let on_boundary = if sign > 0 { plane > 1.0 - 1e-4 } else { plane < 1e-4 };
    let mut front = [pos.x, pos.y, pos.z];
    if on_boundary {
        front[axis] += sign;
    }

    quad.positions.map(|p| {
        let d1 = if p[t1] > 0.5 { 1 } else { -1 };
        let d2 = if p[t2] > 0.5 { 1 } else { -1 };
        let offset = |a: i32, b: i32| {
            let mut c = front;
            c[t1] += a;
            c[t2] += b;
            occludes(region, baked, c)
        };
        let (side1, side2, corner) = (offset(d1, 0), offset(0, d2), offset(d1, d2));
        let level = if side1 && side2 { 0 } else { 3 - (side1 as usize + side2 as usize + corner as usize) };
        AO_LEVELS[level]
    })
}

/// Block light at each corner of a quad. Like [`vertex_ao`], axis-aligned
/// quads average the cells around each corner in the layer they face, for
/// smooth lighting; other quads take their own cell's level. A light source
/// is at least as bright as its own `emission`.
fn vertex_light(region: &Region, baked: &[BakedState], light: &light::LightMap, pos: Vec3i, quad: &Quad, emission: u8) -> [u8; 4] {
    if light.is_empty() {
        return [emission; 4];
    }
    let level = |c: [i32; 3]| light.get(&c).copied().unwrap_or(0);
    let own = [pos.x, pos.y, pos.z];
    let n = quad.normal;
    let Some(axis) = (0..3).find(|&a| n[a].abs() > 0.999) else { return [level(own).max(emission); 4] };
    let sign = n[axis].signum() as i32;
    let (t1, t2) = ((axis + 1) % 3, (axis + 2) % 3);
    let plane = quad.positions[0][axis];
    let on_boundary = if sign > 0 { plane > 1.0 - 1e-4 } else { plane < 1e-4 };
    let mut front = own;
    if on_boundary {
        front[axis] += sign;
    }

    quad.positions.map(|p| {
        let d1 = if p[t1] > 0.5 { 1 } else { -1 };
        let d2 = if p[t2] > 0.5 { 1 } else { -1 };
        let (mut sum, mut count) = (level(front) as u32, 1);
        // Cells inside solid blocks are dark and don't count.
        for (a, b) in [(d1, 0), (0, d2), (d1, d2)] {
            let mut c = front;
            c[t1] += a;
            c[t2] += b;
            if !occludes(region, baked, c) {
                sum += level(c) as u32;
                count += 1;
            }
        }
        (((sum + count / 2) / count) as u8).max(emission)
    })
}

fn placeholder_cube() -> Vec<Quad> {
    cube_quads(1.0, None)
}

/// A unit cube, `height` tall, with every face culling against its side.
fn cube_quads(height: f32, texture: Option<&str>) -> Vec<Quad> {
    Direction::ALL
        .iter()
        .map(|&dir| {
            let element = mc_assets::Element {
                from: [0.0; 3],
                to: [16.0, height * 16.0, 16.0],
                rotation: None,
                shade: true,
                faces: vec![mc_assets::Face {
                    direction: dir,
                    uv: None,
                    texture: texture.map(str::to_owned),
                    cullface: Some(dir),
                    rotation: 0,
                    tintindex: None,
                }],
            };
            let instance = mc_assets::ModelInstance {
                model: Arc::new(mc_assets::Model { ambient_occlusion: true, elements: vec![element] }),
                x: 0,
                y: 0,
                uvlock: false,
                weight: 1,
            };
            bake::bake(&instance).remove(0)
        })
        .collect()
}

/// Liquids have no block model; draw them as a box with a flat surface.
fn liquid_quads(name: &str, props: &mc_assets::Properties) -> Vec<Quad> {
    let level: u32 = props.get("level").and_then(|l| l.parse().ok()).unwrap_or(0);
    // Sources are 8/9 of a block tall, flowing liquid drops by 1/9 per level
    // and falling liquid (level >= 8) is full height.
    let height = if level >= 8 { 1.0 } else { (8 - level) as f32 / 9.0 };
    let mut quads = cube_quads(height, Some(&format!("minecraft:block/{name}_still")));
    for q in &mut quads {
        q.tinted = name == "water";
    }
    quads
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_assets::MemorySource;
    use std::collections::BTreeMap;

    fn png(color: [u8; 4]) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(16, 16, image::Rgba(color));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn assets() -> Assets {
        Assets::new(base_pack())
    }

    fn base_pack() -> MemorySource {
        let mut src = MemorySource::default();
        let cube_all = r##"{"elements": [{"from": [0,0,0], "to": [16,16,16], "faces": {
            "down": {"texture": "#all", "cullface": "down"}, "up": {"texture": "#all", "cullface": "up"},
            "north": {"texture": "#all", "cullface": "north"}, "south": {"texture": "#all", "cullface": "south"},
            "west": {"texture": "#all", "cullface": "west"}, "east": {"texture": "#all", "cullface": "east"}}}]}"##;
        src.insert("assets/minecraft/models/block/cube_all.json", cube_all);
        for (name, color) in [("stone", [128, 128, 128, 255]), ("glass", [255, 255, 255, 0]), ("red_stained_glass", [255, 0, 0, 128])] {
            src.insert(format!("assets/minecraft/blockstates/{name}.json"), format!(r#"{{"variants": {{"": {{"model": "block/{name}"}}}}}}"#))
                .insert(format!("assets/minecraft/models/block/{name}.json"), format!(r#"{{"parent": "block/cube_all", "textures": {{"all": "block/{name}"}}}}"#))
                .insert(format!("assets/minecraft/textures/block/{name}.png"), png(color));
        }
        src.insert(
            "assets/minecraft/blockstates/stone_slab.json",
            r#"{"variants": {"type=bottom": {"model": "block/stone_slab"}}}"#,
        )
        .insert(
            "assets/minecraft/models/block/stone_slab.json",
            r##"{"textures": {"all": "block/stone"}, "elements": [{"from": [0,0,0], "to": [16,8,16], "faces": {
                "down": {"texture": "#all", "cullface": "down"}, "up": {"texture": "#all"},
                "north": {"texture": "#all", "cullface": "north"}}}]}"##,
        );
        src
    }

    fn state(name: &str, props: &[(&str, &str)]) -> BlockState {
        BlockState {
            name: format!("minecraft:{name}"),
            properties: props.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    /// A single-region schematic from a palette and blocks in index order.
    fn schematic(size: Vec3i, palette: Vec<BlockState>, blocks: Vec<u32>) -> Litematic {
        Litematic {
            data_version: 0,
            version: 6,
            metadata: litematic::Metadata {
                name: "t".into(),
                author: String::new(),
                description: String::new(),
                enclosing_size: size,
                region_count: 1,
                total_blocks: 0,
                total_volume: 0,
                time_created: 0,
                time_modified: 0,
            },
            regions: vec![Region {
                name: "r".into(),
                origin: Vec3i { x: 10, y: 0, z: 0 },
                size,
                palette,
                blocks,
                block_entities: Vec::new(),
            }],
        }
    }

    fn mesh(lit: &Litematic) -> (Mesh, Report) {
        let mut assets = assets();
        Mesher::new(&mut assets, Options::default()).build(lit)
    }

    #[test]
    fn touching_opaque_cubes_hide_shared_faces() {
        let lit = schematic(Vec3i { x: 2, y: 1, z: 1 }, vec![state("stone", &[])], vec![0, 0]);
        let (mesh, report) = mesh(&lit);
        // 2 cubes * 6 faces - 2 hidden faces = 10 quads.
        assert_eq!(mesh.opaque.indices.len() / 6, 10);
        assert!(mesh.cutout.is_empty() && mesh.translucent.is_empty());
        assert!(report.skipped.is_empty() && report.missing_textures.is_empty());
        assert_eq!(mesh.bounds, [[10.0, 0.0, 0.0], [12.0, 1.0, 1.0]]);
        assert!(mesh.opaque.vertices.iter().all(|v| v.uv.iter().all(|&c| (0.0..=1.0).contains(&c))));
    }

    #[test]
    fn transparency_layers_and_same_block_culling() {
        let palette = vec![state("air", &[]), state("glass", &[]), state("red_stained_glass", &[]), state("stone", &[])];
        // glass, glass, red glass, stone in a row.
        let lit = schematic(Vec3i { x: 4, y: 1, z: 1 }, palette, vec![1, 1, 2, 3]);
        let (mesh, _) = mesh(&lit);
        let quads = |l: &MeshLayer| l.indices.len() / 6;
        // Glass hides glass but not red glass: 2*6 - 2 = 10.
        assert_eq!(quads(&mesh.cutout), 10);
        // Red glass keeps all faces except the one against stone.
        assert_eq!(quads(&mesh.translucent), 5);
        // Stone's west face is visible through the red glass.
        assert_eq!(quads(&mesh.opaque), 6);
    }

    #[test]
    fn slab_only_hides_the_side_it_fills() {
        let palette = vec![state("stone", &[]), state("stone_slab", &[("type", "bottom")])];
        let lit = schematic(Vec3i { x: 1, y: 3, z: 1 }, palette, vec![0, 1, 0]);
        let (mesh, _) = mesh(&lit);
        // Bottom stone: 5 faces (top hidden). Slab: down hidden by stone,
        // up visible, north visible = 2. Top stone: all 6 (slab top doesn't
        // cull and has no cullface).
        assert_eq!(mesh.opaque.indices.len() / 6, 5 + 2 + 6);
    }

    #[test]
    fn slice_caps_keep_tops_hidden_by_the_block_above() {
        let lit = schematic(Vec3i { x: 1, y: 2, z: 1 }, vec![state("stone", &[])], vec![0, 0]);
        let (plain, _) = mesh(&lit);
        assert!(plain.caps.iter().all(MeshLayer::is_empty));

        let mut assets = assets();
        let (mesh, _) = Mesher::new(&mut assets, Options { slice_caps: true, ..Default::default() }).build(&lit);
        assert_eq!(mesh.opaque.indices, plain.opaque.indices);
        let caps = &mesh.caps[Transparency::Opaque as usize];
        assert_eq!(caps.indices.len() / 6, 1);
        assert!(caps.vertices.iter().all(|v| v.block_y == 0 && v.position[1] == 1.0 && v.ao == 1.0));
        let top: Vec<i32> = mesh.opaque.vertices.iter().filter(|v| v.position[1] == 2.0).map(|v| v.block_y).collect();
        assert!(!top.is_empty() && top.iter().all(|&y| y == 1));
    }

    #[test]
    fn chunked_fill_matches_a_single_build() {
        let palette = vec![state("air", &[]), state("stone", &[]), state("glass", &[])];
        let lit = schematic(Vec3i { x: 3, y: 2, z: 1 }, palette, vec![1, 0, 2, 1, 1, 2]);
        let (whole, _) = mesh(&lit);

        let mut assets = assets();
        let mut build = Mesher::new(&mut assets, Options::default()).prepare(&lit);
        let mut chunks = Vec::new();
        loop {
            let mut chunk = Mesh::default();
            let more = build.fill(&lit, &mut chunk, 4);
            chunks.push(chunk);
            if !more {
                break;
            }
        }
        assert!(chunks.len() > 1);
        assert_eq!(build.progress(), 1.0);
        assert_eq!(chunks.last().unwrap().bounds, whole.bounds);
        let vertices = |layer: fn(&Mesh) -> &MeshLayer| -> Vec<Vertex> {
            chunks.iter().flat_map(|c| layer(c).vertices.clone()).collect()
        };
        assert_eq!(vertices(|m| &m.opaque), whole.opaque.vertices);
        assert_eq!(vertices(|m| &m.cutout), whole.cutout.vertices);
        assert_eq!(build.finish().0.rects, whole.atlas.unwrap().rects);
    }

    #[test]
    fn mod_blocks_and_pack_priority() {
        // A mod jar's block, built on a vanilla parent model, and a pack
        // above both that retextures stone.
        let mut mod_jar = MemorySource::default();
        mod_jar
            .insert("assets/testmod/blockstates/gear.json", r#"{"variants": {"": {"model": "testmod:block/gear"}}}"#)
            .insert("assets/testmod/models/block/gear.json", r#"{"parent": "minecraft:block/cube_all", "textures": {"all": "testmod:block/gear"}}"#)
            .insert("assets/testmod/textures/block/gear.png", png([0, 255, 0, 255]));
        let mut pack = MemorySource::default();
        pack.insert("assets/minecraft/textures/block/stone.png", png([255, 0, 0, 255]));
        let mut stack = mc_assets::PackStack::new();
        stack.push(pack).push(mod_jar).push(base_pack());
        let mut assets = Assets::new(stack);

        let palette = vec![state("stone", &[]), BlockState { name: "testmod:gear".into(), properties: BTreeMap::new() }];
        let lit = schematic(Vec3i { x: 2, y: 1, z: 1 }, palette, vec![0, 1]);
        let (mesh, report) = Mesher::new(&mut assets, Options::default()).build(&lit);
        assert!(report.skipped.is_empty() && report.missing_textures.is_empty(), "{report:?}");
        let atlas = mesh.atlas.as_ref().unwrap();
        // The texel in the middle of the south face touching `x`.
        let color = |x: f32| {
            let vertices = &mesh.opaque.vertices;
            let i = vertices.iter().position(|v| v.position[0] == x && v.normal == [0.0, 0.0, 1.0]).unwrap();
            let quad = &vertices[i / 4 * 4..][..4];
            let uv = quad.iter().fold([0.0; 2], |a, w| [a[0] + w.uv[0] / 4.0, a[1] + w.uv[1] / 4.0]);
            let (px, py) = ((uv[0] * atlas.width as f32) as u32, (uv[1] * atlas.height as f32) as u32);
            atlas.rgba[((py * atlas.width + px) * 4) as usize..][..3].to_vec()
        };
        // Stone at x 10..11 comes from the top pack, the gear at 11..12 from the mod.
        assert_eq!(color(10.0), [255, 0, 0]);
        assert_eq!(color(12.0), [0, 255, 0]);
    }

    #[test]
    fn block_entities_are_drawn_and_banners_get_their_patterns() {
        let mut pack = base_pack();
        for t in ["entity/chest/normal", "entity/banner/banner_base", "entity/banner/base", "entity/banner/border"] {
            pack.insert(format!("assets/minecraft/textures/{t}.png"), png([255, 255, 255, 255]));
        }
        let mut assets = Assets::new(pack);
        let palette = vec![state("chest", &[("facing", "north")]), state("red_banner", &[("rotation", "0")])];
        let mut lit = schematic(Vec3i { x: 3, y: 1, z: 1 }, palette, vec![0, 1, 1]);
        // The second banner has a pattern, the first none.
        let pattern = Nbt::Compound([("pattern".into(), Nbt::String("minecraft:border".into())), ("color".into(), Nbt::String("blue".into()))].into());
        lit.regions[0].block_entities.push(litematic::BlockEntity {
            pos: Vec3i { x: 2, y: 0, z: 0 },
            nbt: Nbt::Compound([("patterns".into(), Nbt::List(vec![pattern]))].into()),
        });
        let (mesh, report) = Mesher::new(&mut assets, Options::default()).build(&lit);
        assert!(report.skipped.is_empty() && report.missing_textures.is_empty(), "{report:?}");
        // Chest: 3 boxes; each banner: flag, bar and pole.
        assert_eq!(mesh.opaque.indices.len() / 6, 18 + 2 * 18);
        // Plain red flag, flag with a blue border, banner sheet, chest.
        assert_eq!(mesh.atlas.unwrap().rects.len(), 4);
    }

    #[test]
    fn pbr_maps_are_read_and_enlarge_their_sprite() {
        let mut pack = base_pack();
        let big = image::RgbaImage::from_pixel(64, 64, image::Rgba([1, 2, 3, 4]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        big.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        pack.insert("assets/minecraft/textures/block/stone_s.png", bytes.into_inner());
        let lit = schematic(Vec3i { x: 1, y: 1, z: 1 }, vec![state("stone", &[])], vec![0]);
        let pack = Arc::new(pack);

        let (plain, _) = Mesher::new(&mut Assets::new(pack.clone()), Options::default()).build(&lit);
        assert!(plain.atlas.unwrap().pbr.is_none());
        let options = Options { pbr_maps: true, ..Default::default() };
        let (mesh, report) = Mesher::new(&mut Assets::new(pack), options).build(&lit);
        assert!(report.missing_textures.is_empty());
        let atlas = mesh.atlas.unwrap();
        // The 16px stone sprite became 64px to match its map.
        assert_eq!((atlas.rects[0].size[0] * atlas.width as f32) as u32, 64);
        let pbr = atlas.pbr.unwrap();
        let [u, v] = atlas.rects[0].map([0.5, 0.5]);
        let i = (((v * atlas.height as f32) as u32 * atlas.width + (u * atlas.width as f32) as u32) * 4) as usize;
        assert_eq!(&pbr.specular[i..i + 4], &[1, 2, 3, 4]);
        assert_eq!(&atlas.rgba[i..i + 4], &[128, 128, 128, 255]);
    }

    #[test]
    fn torches_light_up_nearby_faces() {
        let mut pack = base_pack();
        pack.insert("assets/minecraft/blockstates/torch.json", r#"{"variants": {"": {"model": "block/stone"}}}"#);
        let mut assets = Assets::new(pack);
        // Stone floor of 5, a light source on the first block.
        let palette = vec![state("air", &[]), state("stone", &[]), state("torch", &[])];
        let lit = schematic(Vec3i { x: 5, y: 2, z: 1 }, palette, vec![1, 1, 1, 1, 1, 2, 0, 0, 0, 0]);
        let (mesh, _) = Mesher::new(&mut assets, Options::default()).build(&lit);
        // Average light on the top of each floor block (the torch's block hides
        // the first one's top).
        let top = |x: f32| {
            let v: Vec<u8> = mesh.opaque.vertices.iter()
                .filter(|v| v.normal == [0.0, 1.0, 0.0] && v.position[1] == 1.0 && v.position[0] >= x && v.position[0] <= x + 1.0)
                .map(|v| v.block_light)
                .collect();
            v.iter().map(|&l| l as f32).sum::<f32>() / v.len() as f32
        };
        let (a, b, c) = (top(11.0), top(12.0), top(14.0));
        assert!(a > b && b > c && c > 0.0, "{a} {b} {c}");
        // The source itself is fully bright.
        assert!(mesh.opaque.vertices.iter().filter(|v| v.position[1] > 1.5).all(|v| v.block_light == 14));

        let options = Options { block_light: false, ..Default::default() };
        let (dark, _) = Mesher::new(&mut Assets::new(base_pack()), options).build(&lit);
        assert!(dark.opaque.vertices.iter().all(|v| v.block_light == 0));
    }

    #[test]
    fn missing_blocks_become_placeholders_and_are_reported() {
        let lit = schematic(Vec3i { x: 1, y: 1, z: 1 }, vec![state("mystery", &[])], vec![0]);
        let (mesh, report) = mesh(&lit);
        assert_eq!(mesh.opaque.indices.len() / 6, 6);
        assert_eq!(report.skipped["minecraft:mystery"].1, 1);

        let mut assets = assets();
        let (mesh, _) = Mesher::new(&mut assets, Options { placeholder_for_missing: false, ..Default::default() }).build(&lit);
        assert_eq!(mesh.triangle_count(), 0);
    }

    #[test]
    fn liquids_get_a_lowered_tinted_surface() {
        let lit = schematic(Vec3i { x: 1, y: 1, z: 1 }, vec![state("water", &[("level", "0")])], vec![0]);
        let (mesh, report) = mesh(&lit);
        // No water texture in the test pack: drawn with the missing texture.
        assert_eq!(report.missing_textures, ["minecraft:block/water_still"]);
        let top = mesh.opaque.vertices.iter().map(|v| v.position[1]).fold(0.0, f32::max);
        assert!((top - 8.0 / 9.0).abs() < 1e-5);
        assert!(mesh.opaque.vertices.iter().all(|v| v.color[..3] == tint::color("water", &BTreeMap::new())));
    }

    #[test]
    fn liquid_levels_cull_each_other() {
        let palette = vec![state("water", &[("level", "0")]), state("water", &[("level", "3")]), state("lava", &[("level", "0")])];
        let lit = schematic(Vec3i { x: 3, y: 1, z: 1 }, palette, vec![0, 1, 2]);
        let (mesh, _) = mesh(&lit);
        // Water-water contact is hidden, water-lava is not: 6*3 - 2 = 16.
        assert_eq!(mesh.triangle_count() / 2, 16);
    }

    #[test]
    fn ambient_occlusion_darkens_corners_next_to_a_wall() {
        // Two floor blocks along X, and a wall block on top of the second one.
        let palette = vec![state("air", &[]), state("stone", &[])];
        let lit = schematic(Vec3i { x: 2, y: 2, z: 1 }, palette, vec![1, 1, 0, 1]);
        let (mesh, _) = mesh(&lit);
        let top_of_first: Vec<&Vertex> = mesh
            .opaque
            .vertices
            .iter()
            .filter(|v| v.normal == [0.0, 1.0, 0.0] && v.position[1] == 1.0 && v.position[0] <= 11.0)
            .collect();
        assert_eq!(top_of_first.len(), 4);
        for v in top_of_first {
            let expected = if v.position[0] == 11.0 { AO_LEVELS[2] } else { 1.0 };
            assert_eq!(v.ao, expected, "{v:?}");
        }

        let mut assets = assets();
        let options = Options { ambient_occlusion: false, ..Default::default() };
        let (mesh, _) = Mesher::new(&mut assets, options).build(&lit);
        assert!(mesh.opaque.vertices.iter().all(|v| v.ao == 1.0));
    }

    #[test]
    fn weighted_pick_is_deterministic_and_respects_weights() {
        let choices: Vec<(u32, Vec<Quad>)> = (0..3).map(|i| (if i == 2 { 0 } else { 1 }, vec![])).collect();
        let pick = |x| {
            let q = pick_weighted(&choices, Vec3i { x, y: 0, z: 0 }).unwrap();
            choices.iter().position(|(_, c)| std::ptr::eq(c, q)).unwrap()
        };
        let picks: Vec<usize> = (0..200).map(pick).collect();
        assert_eq!(picks, (0..200).map(pick).collect::<Vec<_>>());
        assert!(picks.contains(&0) && picks.contains(&1) && !picks.contains(&2));
    }

    #[test]
    fn baked_tint_uses_tinted_sprite_copies() {
        let lit = schematic(Vec3i { x: 1, y: 1, z: 1 }, vec![state("water", &[("level", "0")])], vec![0]);
        let mut assets = assets();
        let options = Options { bake_tint: true, ..Default::default() };
        let (mesh, _) = Mesher::new(&mut assets, options).build(&lit);
        let atlas = mesh.atlas.as_ref().unwrap();
        // Missing texture plus its tinted copy.
        assert_eq!(atlas.rects.len(), 2);
        let v = mesh.opaque.vertices[0];
        assert_eq!(v.color, [255; 4]);
        // Sample the sprite this face uses: magenta (248, 0, 248) or black, times the water tint.
        let (x, y) = ((v.uv[0] * atlas.width as f32) as u32, (v.uv[1] * atlas.height as f32) as u32);
        let texel = &atlas.rgba[((y * atlas.width + x) * 4) as usize..][..3];
        let water = tint::color("water", &BTreeMap::new());
        let expected: Vec<u8> = [248u16, 0, 248].iter().zip(water).map(|(c, t)| (c * t as u16 / 255) as u8).collect();
        assert!(texel == expected.as_slice() || texel == [0, 0, 0], "{texel:?}");
    }
}
