//! Export meshes to 3D file formats. Writers return in-memory files so the
//! same code serves the CLI and the browser.

mod fbx;
mod gltf;
mod obj;

use mesher::Mesh;

pub use fbx::fbx;
pub use gltf::{glb, gltf};
pub use obj::obj;

#[derive(Debug, Clone)]
pub struct File {
    pub name: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Single binary glTF file.
    Glb,
    /// glTF JSON with separate `.bin` and `.png`.
    Gltf,
    /// Wavefront OBJ with `.mtl` and `.png`.
    Obj,
    /// Binary FBX 7.4 with the texture embedded (and a `.png` beside it).
    Fbx,
}

impl Format {
    /// Whether this format relies on tints being baked into the texture
    /// (see `mesher::Options::bake_tint`) because vertex colors are not
    /// reliably applied by the tools that read it.
    pub fn wants_baked_tint(self) -> bool {
        matches!(self, Format::Obj | Format::Fbx)
    }
}

impl Format {
    pub fn from_extension(ext: &str) -> Option<Format> {
        match ext.to_ascii_lowercase().as_str() {
            "glb" => Some(Format::Glb),
            "gltf" => Some(Format::Gltf),
            "obj" => Some(Format::Obj),
            "fbx" => Some(Format::Fbx),
            _ => None,
        }
    }
}

/// Write `mesh` in `format`. `name` labels the model inside the file and
/// `stem` names the output files, e.g. `house` → `house.obj`, `house.png`.
pub fn export(mesh: &Mesh, format: Format, name: &str, stem: &str) -> Vec<File> {
    match format {
        Format::Glb => vec![File { name: format!("{stem}.glb"), data: glb(mesh, name) }],
        Format::Gltf => gltf(mesh, name, stem),
        Format::Obj => obj(mesh, name, stem),
        Format::Fbx => fbx(mesh, name, stem),
    }
}

/// The atlas as a PNG at its native size, for formats that can ask readers
/// for nearest filtering (glTF samplers).
fn atlas_png(mesh: &Mesh) -> Vec<u8> {
    mesh.atlas.as_ref().map(encode_png).unwrap_or_default()
}

/// Largest side of an upscaled atlas; 4096 is safe for every engine and GPU.
const MAX_UPSCALED_SIZE: u32 = 4096;
/// Enough that smooth filtering blurs only 1/8 of a texel.
const MAX_UPSCALE: u32 = 8;

/// The atlas as a PNG, upscaled for formats whose readers always filter
/// textures smoothly (Blender imports FBX and OBJ with linear filtering and
/// there is no way to ask for nearest). Blocky upscaling keeps the pixel-art
/// look without any manual setup.
fn crisp_atlas_png(mesh: &Mesh) -> Vec<u8> {
    let Some(atlas) = &mesh.atlas else { return Vec::new() };
    let largest = atlas.width.max(atlas.height).max(1);
    let factor = (MAX_UPSCALED_SIZE / largest).clamp(1, MAX_UPSCALE);
    let factor = 1 << factor.ilog2();
    encode_png(&atlas.upscaled(factor))
}

fn encode_png(atlas: &mesher::Atlas) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_raw(atlas.width, atlas.height, atlas.rgba.clone())
        .expect("atlas size matches its pixels")
        .write_to(&mut out, image::ImageFormat::Png)
        .expect("png encoding into memory");
    out.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesher::{Atlas, MeshLayer, Vertex, atlas::Rect};

    fn quad(y: f32, color: [u8; 4]) -> MeshLayer {
        let v = |x: f32, z: f32, u: f32, w: f32| Vertex { position: [x, y, z], normal: [0.0, 1.0, 0.0], uv: [u, w], color, ao: 1.0 };
        MeshLayer {
            vertices: vec![v(0.0, 0.0, 0.0, 0.0), v(0.0, 1.0, 0.0, 0.5), v(1.0, 1.0, 0.5, 0.5), v(1.0, 0.0, 0.5, 0.0)],
            indices: vec![0, 1, 2, 0, 2, 3],
        }
    }

    fn mesh() -> Mesh {
        Mesh {
            opaque: quad(0.0, [255; 4]),
            cutout: MeshLayer::default(),
            translucent: quad(1.0, [63, 118, 228, 255]),
            atlas: Some(Atlas {
                width: 2,
                height: 2,
                rgba: [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [9, 9, 9, 100]].concat(),
                rects: vec![Rect { min: [0.0; 2], size: [0.5; 2] }],
                mip_levels: 1,
            }),
            bounds: [[0.0; 3], [1.0, 1.0, 1.0]],
        }
    }

    fn check_document(doc: &::gltf::Document, buffers: &[::gltf::buffer::Data], images: &[::gltf::image::Data]) {
        let prims: Vec<_> = doc.meshes().next().unwrap().primitives().collect();
        assert_eq!(prims.len(), 2, "empty cutout layer is left out");
        let modes: Vec<_> = prims.iter().map(|p| p.material().alpha_mode()).collect();
        assert_eq!(modes, [::gltf::material::AlphaMode::Opaque, ::gltf::material::AlphaMode::Blend]);

        let reader = prims[1].reader(|b| Some(&buffers[b.index()]));
        let positions: Vec<[f32; 3]> = reader.read_positions().unwrap().collect();
        assert_eq!(positions[2], [1.0, 1.0, 1.0]);
        let indices: Vec<u32> = reader.read_indices().unwrap().into_u32().collect();
        assert_eq!(indices, [0, 1, 2, 0, 2, 3]);
        let colors: Vec<[u8; 4]> = reader.read_colors(0).unwrap().into_rgba_u8().collect();
        assert_eq!(colors[0], [63, 118, 228, 255]);
        assert!(prims[0].reader(|b| Some(&buffers[b.index()])).read_colors(0).is_none());

        assert_eq!((images[0].width, images[0].height), (2, 2));
        assert_eq!(&images[0].pixels[12..16], &[9, 9, 9, 100]);
    }

    #[test]
    fn glb_round_trips() {
        let data = glb(&mesh(), "test");
        assert_eq!(&data[..4], b"glTF");
        assert_eq!(u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize, data.len());
        let (doc, buffers, images) = ::gltf::import_slice(&data).unwrap();
        check_document(&doc, &buffers, &images);
    }

    #[test]
    fn gltf_with_external_files_round_trips() {
        let dir = std::env::temp_dir().join(format!("export-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let files = gltf(&mesh(), "test", "model");
        let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["model.gltf", "model.bin", "model.png"]);
        for f in &files {
            std::fs::write(dir.join(&f.name), &f.data).unwrap();
        }
        let result = ::gltf::import(dir.join("model.gltf"));
        std::fs::remove_dir_all(&dir).unwrap();
        let (doc, buffers, images) = result.unwrap();
        check_document(&doc, &buffers, &images);
    }

    #[test]
    fn empty_mesh_is_still_valid() {
        let data = glb(&Mesh::default(), "empty");
        let (doc, _, _) = ::gltf::import_slice(&data).unwrap();
        assert_eq!(doc.meshes().count(), 0);
        assert_eq!(doc.nodes().count(), 1);
    }

    #[test]
    fn obj_indices_span_layers_and_flip_v() {
        let files = obj(&mesh(), "test", "model");
        let text = String::from_utf8(files[0].data.clone()).unwrap();
        let lines = |prefix: &'static str| text.lines().filter(move |l| l.starts_with(prefix));
        assert_eq!(lines("v ").count(), 8);
        assert_eq!(lines("vt ").next(), Some("vt 0 1"));
        // The second layer's faces index its own vertices, 5..=8.
        assert_eq!(lines("f ").nth(2), Some("f 5/5/5 6/6/6 7/7/7"));
        assert_eq!(lines("usemtl ").collect::<Vec<_>>(), ["usemtl opaque", "usemtl translucent"]);
        assert_eq!(lines("v ").next(), Some("v 0 0 0"));
        assert_eq!(lines("v ").nth(4), Some("v 0 1 0 0.2471 0.4627 0.8941"));
        let mtl = String::from_utf8(files[1].data.clone()).unwrap();
        assert!(mtl.contains("newmtl translucent") && mtl.contains("map_d model.png"));
        assert_eq!(&files[2].data[1..4], b"PNG");
    }
}
