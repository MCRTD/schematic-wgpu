//! Builds textured triangle meshes from Litematica schematics.

pub mod atlas;
pub mod bake;
mod tint;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use glam::Vec3;
use litematic::{BlockState, Litematic, Region, Vec3i};
use mc_assets::{Assets, Direction, Texture};
pub use mc_assets::Transparency;

pub use atlas::Atlas;
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
}

impl Default for Options {
    fn default() -> Self {
        Self { placeholder_for_missing: true, bake_tint: false, ambient_occlusion: true }
    }
}

/// Brightness for 0..=3 unoccluded neighbours around a vertex.
const AO_LEVELS: [f32; 4] = [0.45, 0.62, 0.8, 1.0];

/// A palette entry baked into quads. Weighted variants have one quad list
/// per choice; `parts` are always all drawn.
#[derive(Debug, Default)]
struct BakedState {
    parts: Vec<Vec<(u32, Vec<Quad>)>>,
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
            sprite_ids: HashMap::new(),
            missing_sprite: None,
            report: Report::default(),
        }
    }

    pub fn build(mut self, schematic: &Litematic) -> (Mesh, Report) {
        let mut mesh = Mesh::default();
        let mut bounds = [Vec3::MAX, Vec3::MIN];
        // Sprite index per face, resolved into atlas UVs once all sprites are known.
        let mut pending: Vec<(Transparency, [Vertex; 4], u32)> = Vec::new();

        for region in &schematic.regions {
            let baked: Vec<BakedState> = region.palette.iter().map(|s| self.bake_state(s)).collect();
            let origin = Vec3::new(region.origin.x as f32, region.origin.y as f32, region.origin.z as f32);

            for (pos, _) in region.iter_blocks() {
                let index = region.blocks[region.index(pos.x, pos.y, pos.z)] as usize;
                let state = &baked[index];
                if state.reported {
                    let name = &region.palette[index].name;
                    if let Some(entry) = self.report.skipped.get_mut(name) {
                        entry.1 += 1;
                    }
                }
                if state.skip {
                    continue;
                }
                let block_pos = origin + Vec3::new(pos.x as f32, pos.y as f32, pos.z as f32);
                bounds = [bounds[0].min(block_pos), bounds[1].max(block_pos + Vec3::ONE)];

                for part in &state.parts {
                    let Some(quads) = pick_weighted(part, pos) else { continue };
                    for quad in quads {
                        if let Some(dir) = quad.cullface
                            && is_hidden(region, &baked, pos, index, dir)
                        {
                            continue;
                        }
                        let tint = quad.tinted.then_some(state.tint);
                        let (sprite, [r, g, b]) = match tint {
                            Some(tint) if self.options.bake_tint => (self.tinted_sprite(quad.texture.clone(), tint), [255; 3]),
                            _ => (self.sprite(quad.texture.clone()), tint.unwrap_or([255; 3])),
                        };
                        let transparency = self.sprites[sprite as usize].transparency;
                        let ao = if self.options.ambient_occlusion && quad.ambient_occlusion {
                            vertex_ao(region, &baked, pos, quad)
                        } else {
                            [1.0; 4]
                        };
                        let corners = std::array::from_fn(|i| Vertex {
                            position: (block_pos + quad.positions[i]).into(),
                            normal: quad.normal.into(),
                            uv: quad.uvs[i],
                            color: [r, g, b, 255],
                            ao: ao[i],
                        });
                        pending.push((transparency, corners, sprite));
                    }
                }
            }
        }

        let atlas = Atlas::pack(&self.sprites);
        for (transparency, mut corners, sprite) in pending {
            let rect = atlas.rects[sprite as usize];
            for v in &mut corners {
                v.uv = rect.map(v.uv);
            }
            mesh.layer_mut(transparency).push_quad(corners);
        }
        mesh.atlas = Some(atlas);
        mesh.bounds = if bounds[0].x <= bounds[1].x { [bounds[0].into(), bounds[1].into()] } else { [[0.0; 3]; 2] };
        self.report.missing_textures.sort();
        (mesh, self.report)
    }

    /// Atlas sprite index for a texture; missing textures share one sprite.
    fn sprite(&mut self, texture: Option<String>) -> u32 {
        let key = (texture, None);
        if let Some(&id) = self.sprite_ids.get(&key) {
            return id;
        }
        let texture = key.0;
        let id = match texture.as_deref().and_then(|t| self.assets.texture(t)) {
            Some(loaded) => self.push_sprite(loaded),
            None => {
                if let Some(t) = &texture {
                    self.report.missing_textures.push(t.clone());
                }
                match self.missing_sprite {
                    Some(id) => id,
                    None => {
                        let id = self.push_sprite(Arc::new(Texture::missing()));
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
        let id = self.push_sprite(Arc::new(tinted));
        self.sprite_ids.insert(key, id);
        id
    }

    fn push_sprite(&mut self, texture: Arc<Texture>) -> u32 {
        self.sprites.push(texture);
        self.sprites.len() as u32 - 1
    }

    fn report_skip(&mut self, state: &BlockState, reason: String) {
        self.report.skipped.entry(state.name.clone()).or_insert((reason, 0));
    }

    fn bake_state(&mut self, state: &BlockState) -> BakedState {
        if state.is_air() {
            return BakedState { skip: true, ..Default::default() };
        }
        let name = state.short_name();
        let mut reported = false;
        let parts: Vec<Vec<(u32, Vec<Quad>)>> = if LIQUIDS.contains(&name) {
            vec![vec![(1, liquid_quads(name, &state.properties))]]
        } else {
            match self.assets.resolve(&state.name, &state.properties) {
                Ok(parts) => parts
                    .iter()
                    .map(|choices| choices.iter().map(|m| (m.weight, bake::bake(m))).collect())
                    .collect(),
                Err(e) if self.options.placeholder_for_missing => {
                    reported = true;
                    self.report_skip(state, e.to_string());
                    vec![vec![(1, placeholder_cube())]]
                }
                Err(e) => {
                    self.report_skip(state, e.to_string());
                    return BakedState { skip: true, reported: true, ..Default::default() };
                }
            }
        };

        if parts.iter().flatten().all(|(_, quads)| quads.is_empty()) {
            reported = true;
            self.report_skip(state, "no model geometry (drawn by a block entity renderer)".into());
        }

        // Culling data comes from the first choice of each part; weighted
        // variants of one block share their shape.
        let mut opaque_sides = [false; 6];
        let mut full_sides = [false; 6];
        for quads in parts.iter().filter_map(|p| p.first()).map(|(_, q)| q) {
            for quad in quads {
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
        BakedState {
            parts,
            opaque_sides,
            full_sides,
            occludes: fluid.is_none() && opaque_sides.iter().all(|&o| o),
            tint: tint::color(name, &state.properties),
            fluid,
            reported,
            skip: false,
        }
    }
}

/// Deterministically choose a weighted variant based on block position.
fn pick_weighted(choices: &[(u32, Vec<Quad>)], pos: Vec3i) -> Option<&Vec<Quad>> {
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
        Assets::new(src)
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
            regions: vec![Region { name: "r".into(), origin: Vec3i { x: 10, y: 0, z: 0 }, size, palette, blocks }],
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
