//! Binary FBX 7.4 output: one mesh with a material per render layer and the
//! atlas embedded in the file (and written next to it for tools that want
//! the image separately).
//!
//! The binary layout follows Blender's `encode_bin.py`, the most widely
//! tested open-source FBX writer.

use std::collections::HashMap;
use std::io::Write;

use mesher::{Mesh, Transparency};

use crate::{File, crisp_atlas_png};

const VERSION: u32 = 7400;
const HEADER_MAGIC: &[u8] = b"Kaydara FBX Binary  \x00\x1a\x00";
/// Fixed file id, creation time and footer id. Readers check these against
/// each other, and Blender writes the same constants.
const FILE_ID: &[u8] = b"\x28\xb3\x2a\xeb\xb6\x24\xcc\xc2\xbf\xc8\xb0\x2a\xa9\x2b\xfc\xf1";
const TIME_ID: &str = "1970-01-01 10:00:00:000";
const FOOTER_ID: &[u8] = b"\xfa\xbc\xab\x09\xd0\xc8\xd4\x66\xb1\x76\xfb\x83\x1c\xf7\x26\x7e";
const FOOTER_MAGIC: &[u8] = b"\xf8\x5a\x8c\x6a\xde\xf5\xd9\x7e\xec\xe9\x0c\xe3\x75\x8f\x29\x0b";
/// End-of-children marker: an all-zero node record (3 x u32 + name length).
const SENTINEL: [u8; 13] = [0; 13];

enum Prop {
    I32(i32),
    I64(i64),
    F64(f64),
    Str(String),
    Raw(Vec<u8>),
    I32s(Vec<i32>),
    F64s(Vec<f64>),
}

impl From<i32> for Prop {
    fn from(v: i32) -> Self {
        Prop::I32(v)
    }
}
impl From<i64> for Prop {
    fn from(v: i64) -> Self {
        Prop::I64(v)
    }
}
impl From<f64> for Prop {
    fn from(v: f64) -> Self {
        Prop::F64(v)
    }
}
impl From<&str> for Prop {
    fn from(v: &str) -> Self {
        Prop::Str(v.to_owned())
    }
}
impl From<String> for Prop {
    fn from(v: String) -> Self {
        Prop::Str(v)
    }
}
impl From<Vec<i32>> for Prop {
    fn from(v: Vec<i32>) -> Self {
        Prop::I32s(v)
    }
}
impl From<Vec<f64>> for Prop {
    fn from(v: Vec<f64>) -> Self {
        Prop::F64s(v)
    }
}

struct Node {
    name: &'static str,
    props: Vec<Prop>,
    children: Vec<Node>,
}

impl Node {
    fn new(name: &'static str) -> Self {
        Node { name, props: Vec::new(), children: Vec::new() }
    }

    /// A node holding a single value, like `Version: 100`.
    fn value(name: &'static str, value: impl Into<Prop>) -> Self {
        Node::new(name).prop(value)
    }

    fn prop(mut self, p: impl Into<Prop>) -> Self {
        self.props.push(p.into());
        self
    }

    fn child(mut self, n: Node) -> Self {
        self.children.push(n);
        self
    }

    fn children(mut self, n: impl IntoIterator<Item = Node>) -> Self {
        self.children.extend(n);
        self
    }
}

/// `name` + `\0\x01` + `class`, how binary FBX spells `Class::name`.
fn name_class(name: &str, class: &str) -> String {
    format!("{name}\0\x01{class}")
}

/// A `P` entry of a `Properties70` block.
fn p(name: &str, kind: &str, label: &str, flags: &str, values: impl IntoIterator<Item = Prop>) -> Node {
    let mut node = Node::new("P").prop(name).prop(kind).prop(label).prop(flags);
    node.props.extend(values);
    node
}

fn write_array<T: Copy>(out: &mut Vec<u8>, code: u8, values: &[T], bytes: impl Fn(T) -> Vec<u8>) {
    let raw: Vec<u8> = values.iter().flat_map(|&v| bytes(v)).collect();
    out.push(code);
    out.extend((values.len() as u32).to_le_bytes());
    // Like Blender: compress anything larger than 128 bytes.
    if raw.len() > 128 {
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&raw).expect("in-memory compression");
        let compressed = encoder.finish().expect("in-memory compression");
        out.extend(1u32.to_le_bytes());
        out.extend((compressed.len() as u32).to_le_bytes());
        out.extend(compressed);
    } else {
        out.extend(0u32.to_le_bytes());
        out.extend((raw.len() as u32).to_le_bytes());
        out.extend(raw);
    }
}

fn write_prop(out: &mut Vec<u8>, prop: &Prop) {
    match prop {
        Prop::I32(v) => {
            out.push(b'I');
            out.extend(v.to_le_bytes());
        }
        Prop::I64(v) => {
            out.push(b'L');
            out.extend(v.to_le_bytes());
        }
        Prop::F64(v) => {
            out.push(b'D');
            out.extend(v.to_le_bytes());
        }
        Prop::Str(s) => {
            out.push(b'S');
            out.extend((s.len() as u32).to_le_bytes());
            out.extend(s.as_bytes());
        }
        Prop::Raw(b) => {
            out.push(b'R');
            out.extend((b.len() as u32).to_le_bytes());
            out.extend(b);
        }
        Prop::I32s(v) => write_array(out, b'i', v, |x| x.to_le_bytes().to_vec()),
        Prop::F64s(v) => write_array(out, b'd', v, |x| x.to_le_bytes().to_vec()),
    }
}

/// Write a node record. Offsets are absolute, so `out` must hold the whole
/// file written so far.
fn write_node(out: &mut Vec<u8>, node: &Node, is_last: bool) {
    let start = out.len();
    out.extend([0u8; 12]);
    out.push(node.name.len() as u8);
    out.extend(node.name.as_bytes());
    let props_start = out.len();
    for prop in &node.props {
        write_prop(out, prop);
    }
    let props_len = out.len() - props_start;

    write_children(out, &node.children);
    // An empty node that isn't last in its list also needs a sentinel.
    if node.children.is_empty() && node.props.is_empty() && !is_last {
        out.extend(SENTINEL);
    }

    let end = out.len() as u32;
    out[start..start + 4].copy_from_slice(&end.to_le_bytes());
    out[start + 4..start + 8].copy_from_slice(&(node.props.len() as u32).to_le_bytes());
    out[start + 8..start + 12].copy_from_slice(&(props_len as u32).to_le_bytes());
}

fn write_children(out: &mut Vec<u8>, children: &[Node]) {
    if children.is_empty() {
        return;
    }
    for (i, child) in children.iter().enumerate() {
        write_node(out, child, i + 1 == children.len());
    }
    out.extend(SENTINEL);
}

fn encode(nodes: &[Node]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(HEADER_MAGIC);
    out.extend(VERSION.to_le_bytes());
    write_children(&mut out, nodes);
    out.extend(FOOTER_ID);
    out.extend([0u8; 4]);
    let pad = match out.len() % 16 {
        0 => 16,
        r => 16 - r,
    };
    out.extend(std::iter::repeat_n(0u8, pad));
    out.extend(VERSION.to_le_bytes());
    out.extend([0u8; 120]);
    out.extend(FOOTER_MAGIC);
    out
}

/// Mesh data in FBX terms: shared control points, polygons, and per
/// polygon-vertex attributes.
struct Geometry {
    points: Vec<f64>,
    /// Control point indices; the last index of each polygon is stored as
    /// `!index` (that is, `-index - 1`).
    polygon_vertex_index: Vec<i32>,
    normals: Vec<f64>,
    uvs: Vec<f64>,
    uv_index: Vec<i32>,
    colors: Vec<f64>,
    color_index: Vec<i32>,
    materials: Vec<i32>,
    material_layers: Vec<Transparency>,
}

fn geometry(mesh: &Mesh) -> Geometry {
    let mut g = Geometry {
        points: Vec::new(),
        polygon_vertex_index: Vec::new(),
        normals: Vec::new(),
        uvs: Vec::new(),
        uv_index: Vec::new(),
        colors: Vec::new(),
        color_index: Vec::new(),
        materials: Vec::new(),
        material_layers: Vec::new(),
    };
    let mut point_ids: HashMap<[u32; 3], i32> = HashMap::new();
    let mut uv_ids: HashMap<[u32; 2], i32> = HashMap::new();
    let mut color_ids: HashMap<[u8; 4], i32> = HashMap::new();
    let tinted = mesh.layers().iter().any(|(_, l)| l.vertices.iter().any(|v| v.color != [255; 4]));

    for (transparency, layer) in mesh.layers() {
        if layer.is_empty() {
            continue;
        }
        let material = g.material_layers.len() as i32;
        g.material_layers.push(transparency);

        // The mesher emits quads as [b, b+1, b+2, b, b+2, b+3]; keep those
        // as quads and anything else as triangles.
        let mut polygons: Vec<Vec<u32>> = Vec::new();
        for six in layer.indices.chunks(6) {
            let b = six[0];
            if six == [b, b + 1, b + 2, b, b + 2, b + 3] {
                polygons.push(vec![b, b + 1, b + 2, b + 3]);
            } else {
                polygons.extend(six.chunks_exact(3).map(|t| t.to_vec()));
            }
        }

        for polygon in polygons {
            for (k, &vi) in polygon.iter().enumerate() {
                let v = layer.vertices[vi as usize];
                let key = v.position.map(f32::to_bits);
                let next = point_ids.len() as i32;
                let point = *point_ids.entry(key).or_insert_with(|| {
                    g.points.extend(v.position.map(f64::from));
                    next
                });
                g.polygon_vertex_index.push(if k + 1 == polygon.len() { !point } else { point });
                g.normals.extend(v.normal.map(f64::from));

                // FBX puts v = 0 at the bottom of the image.
                let uv = [v.uv[0], 1.0 - v.uv[1]];
                let next = uv_ids.len() as i32;
                g.uv_index.push(*uv_ids.entry(uv.map(f32::to_bits)).or_insert_with(|| {
                    g.uvs.extend(uv.map(f64::from));
                    next
                }));

                if tinted {
                    let next = color_ids.len() as i32;
                    g.color_index.push(*color_ids.entry(v.color).or_insert_with(|| {
                        g.colors.extend(v.color.map(|c| c as f64 / 255.0));
                        next
                    }));
                }
            }
            g.materials.push(material);
        }
    }
    g
}

/// Reference from a `Layer` to the first layer element of a type.
fn layer_element(kind: &'static str) -> Node {
    Node::new("LayerElement").child(Node::value("Type", kind)).child(Node::value("TypedIndex", 0))
}

fn geometry_node(id: i64, name: &str, g: Geometry) -> Node {
    let has_colors = !g.color_index.is_empty();
    let mut node = Node::new("Geometry")
        .prop(id)
        .prop(name_class(name, "Geometry"))
        .prop("Mesh")
        .child(Node::value("GeometryVersion", 124))
        .child(Node::value("Vertices", g.points))
        .child(Node::value("PolygonVertexIndex", g.polygon_vertex_index))
        .child(
            Node::value("LayerElementNormal", 0)
                .child(Node::value("Version", 101))
                .child(Node::value("Name", ""))
                .child(Node::value("MappingInformationType", "ByPolygonVertex"))
                .child(Node::value("ReferenceInformationType", "Direct"))
                .child(Node::value("Normals", g.normals)),
        );
    if has_colors {
        node = node.child(
            Node::value("LayerElementColor", 0)
                .child(Node::value("Version", 101))
                .child(Node::value("Name", "Tint"))
                .child(Node::value("MappingInformationType", "ByPolygonVertex"))
                .child(Node::value("ReferenceInformationType", "IndexToDirect"))
                .child(Node::value("Colors", g.colors))
                .child(Node::value("ColorIndex", g.color_index)),
        );
    }
    node = node
        .child(
            Node::value("LayerElementUV", 0)
                .child(Node::value("Version", 101))
                .child(Node::value("Name", "UVMap"))
                .child(Node::value("MappingInformationType", "ByPolygonVertex"))
                .child(Node::value("ReferenceInformationType", "IndexToDirect"))
                .child(Node::value("UV", g.uvs))
                .child(Node::value("UVIndex", g.uv_index)),
        )
        .child(
            Node::value("LayerElementMaterial", 0)
                .child(Node::value("Version", 101))
                .child(Node::value("Name", ""))
                .child(Node::value("MappingInformationType", "ByPolygon"))
                .child(Node::value("ReferenceInformationType", "IndexToDirect"))
                .child(Node::value("Materials", g.materials)),
        );

    let mut layer = Node::value("Layer", 0).child(Node::value("Version", 100));
    layer = layer.child(layer_element("LayerElementNormal"));
    if has_colors {
        layer = layer.child(layer_element("LayerElementColor"));
    }
    layer = layer.child(layer_element("LayerElementUV")).child(layer_element("LayerElementMaterial"));
    node.child(layer)
}

fn material_node(id: i64, transparency: Transparency) -> Node {
    let name = match transparency {
        Transparency::Opaque => "opaque",
        Transparency::Cutout => "cutout",
        Transparency::Translucent => "translucent",
    };
    Node::new("Material")
        .prop(id)
        .prop(name_class(name, "Material"))
        .prop("")
        .child(Node::value("Version", 102))
        .child(Node::value("ShadingModel", "lambert"))
        .child(Node::value("MultiLayer", 0))
        .child(Node::new("Properties70").children([
            p("DiffuseColor", "Color", "", "A", [1.0.into(), 1.0.into(), 1.0.into()]),
            p("DiffuseFactor", "Number", "", "A", [1.0.into()]),
            p("AmbientColor", "Color", "", "A", [0.0.into(), 0.0.into(), 0.0.into()]),
            p("SpecularFactor", "Number", "", "A", [0.0.into()]),
        ]))
}

/// `<stem>.fbx` with the atlas embedded, plus `<stem>.png`.
pub fn fbx(mesh: &Mesh, name: &str, stem: &str) -> Vec<File> {
    const MODEL: i64 = 1_000_001;
    const GEOMETRY: i64 = 1_000_002;
    const TEXTURE: i64 = 1_000_003;
    const VIDEO: i64 = 1_000_004;
    const MATERIAL_BASE: i64 = 1_000_100;

    let png = crisp_atlas_png(mesh);
    let image = format!("{stem}.png");
    let g = geometry(mesh);
    let layers = g.material_layers.clone();
    let has_mesh = !layers.is_empty();

    let mut objects = Node::new("Objects");
    let mut connections = Node::new("Connections");
    let connect = |kind: &str, child: i64, parent: i64| Node::new("C").prop(kind).prop(child).prop(parent);

    objects = objects.child(
        Node::new("Model")
            .prop(MODEL)
            .prop(name_class(name, "Model"))
            .prop("Mesh")
            .child(Node::value("Version", 232))
            .child(Node::new("Properties70").child(p("DefaultAttributeIndex", "int", "Integer", "", [0.into()])))
            .child(Node::value("Culling", "CullingOff")),
    );
    connections = connections.child(connect("OO", MODEL, 0));

    if has_mesh {
        objects = objects.child(geometry_node(GEOMETRY, name, g));
        connections = connections.child(connect("OO", GEOMETRY, MODEL));
        for (i, &t) in layers.iter().enumerate() {
            let id = MATERIAL_BASE + i as i64;
            objects = objects.child(material_node(id, t));
            // Material indices follow the order materials connect to the model.
            connections = connections.child(connect("OO", id, MODEL));
            // Readers take alpha from the diffuse texture. `TransparentColor` is
            // deliberately not used: in Maya's convention white there means
            // fully transparent, which would make bright blocks see-through.
            // FBX has no way to say alpha-test vs. blend; the material names do.
            connections = connections.child(connect("OP", TEXTURE, id).prop("DiffuseColor"));
        }
        objects = objects
            .child(
                Node::new("Texture")
                    .prop(TEXTURE)
                    .prop(name_class("atlas", "Texture"))
                    .prop("")
                    .child(Node::value("Type", "TextureVideoClip"))
                    .child(Node::value("Version", 202))
                    .child(Node::value("TextureName", name_class("atlas", "Texture")))
                    .child(Node::value("Media", name_class("atlas", "Video")))
                    .child(Node::value("FileName", image.as_str()))
                    .child(Node::value("RelativeFilename", image.as_str()))
                    .child(Node::value("ModelUVTranslation", 0.0).prop(0.0))
                    .child(Node::value("ModelUVScaling", 1.0).prop(1.0))
                    .child(Node::value("Texture_Alpha_Source", "None"))
                    .child(Node::value("Cropping", 0).prop(0).prop(0).prop(0)),
            )
            .child(
                Node::new("Video")
                    .prop(VIDEO)
                    .prop(name_class("atlas", "Video"))
                    .prop("Clip")
                    .child(Node::value("Type", "Clip"))
                    .child(Node::new("Properties70").child(p("Path", "KString", "XRefUrl", "", [image.as_str().into()])))
                    .child(Node::value("UseMipMap", 0))
                    .child(Node::value("Filename", image.as_str()))
                    .child(Node::value("RelativeFilename", image.as_str()))
                    .child(Node::new("Content").prop(Prop::Raw(png.clone()))),
            );
        connections = connections.child(connect("OO", VIDEO, TEXTURE));
    }

    let object_types: Vec<(&str, i32)> = [
        ("GlobalSettings", 1),
        ("Model", 1),
        ("Geometry", has_mesh as i32),
        ("Material", layers.len() as i32),
        ("Texture", has_mesh as i32),
        ("Video", has_mesh as i32),
    ]
    .into_iter()
    .filter(|&(_, n)| n > 0)
    .collect();

    let creator = concat!("schematic-wgpu ", env!("CARGO_PKG_VERSION"));
    let axis = |name: &str, value: i32| p(name, "int", "Integer", "", [value.into()]);
    let nodes = vec![
        Node::new("FBXHeaderExtension")
            .child(Node::value("FBXHeaderVersion", 1003))
            .child(Node::value("FBXVersion", VERSION as i32))
            .child(Node::value("EncryptionType", 0))
            .child(Node::new("CreationTimeStamp").children([
                Node::value("Version", 1000),
                Node::value("Year", 1970),
                Node::value("Month", 1),
                Node::value("Day", 1),
                Node::value("Hour", 10),
                Node::value("Minute", 0),
                Node::value("Second", 0),
                Node::value("Millisecond", 0),
            ]))
            .child(Node::value("Creator", creator)),
        Node::new("FileId").prop(Prop::Raw(FILE_ID.to_vec())),
        Node::value("CreationTime", TIME_ID),
        Node::value("Creator", creator),
        // Y up, Z forward, X right: the same right-handed axes as the mesh.
        // UnitScaleFactor is centimetres per unit, so one block is 1 m.
        Node::new("GlobalSettings").child(Node::value("Version", 1000)).child(Node::new("Properties70").children([
            axis("UpAxis", 1),
            axis("UpAxisSign", 1),
            axis("FrontAxis", 2),
            axis("FrontAxisSign", 1),
            axis("CoordAxis", 0),
            axis("CoordAxisSign", 1),
            axis("OriginalUpAxis", 1),
            axis("OriginalUpAxisSign", 1),
            p("UnitScaleFactor", "double", "Number", "", [100.0.into()]),
            p("OriginalUnitScaleFactor", "double", "Number", "", [100.0.into()]),
        ])),
        Node::new("Documents").child(Node::value("Count", 1)).child(
            Node::new("Document")
                .prop(1_000_000i64)
                .prop("Scene")
                .prop("Scene")
                .child(Node::value("RootNode", 0i64)),
        ),
        Node::new("References"),
        Node::new("Definitions")
            .child(Node::value("Version", 100))
            .child(Node::value("Count", object_types.iter().map(|(_, n)| n).sum::<i32>()))
            .children(object_types.iter().map(|&(kind, n)| Node::value("ObjectType", kind).child(Node::value("Count", n)))),
        objects,
        connections,
    ];

    let mut files = vec![File { name: format!("{stem}.fbx"), data: encode(&nodes) }];
    if has_mesh {
        files.push(File { name: image, data: png });
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_offsets_and_sentinels_match_blender_layout() {
        // Two top-level nodes: one with a property, one empty but last.
        let nodes = [Node::value("A", 7), Node::new("B")];
        let out = encode(&nodes);
        let base = HEADER_MAGIC.len() + 4;
        assert_eq!(&out[..base - 4], HEADER_MAGIC);
        // A: 13-byte record header + 1-byte name + 5-byte int property.
        let a_end = u32::from_le_bytes(out[base..base + 4].try_into().unwrap()) as usize;
        assert_eq!(a_end, base + 13 + 1 + 5);
        assert_eq!(&out[base + 4..base + 12], &[1, 0, 0, 0, 5, 0, 0, 0]);
        // B is empty and last: no sentinel of its own, then the list's sentinel.
        let b_end = u32::from_le_bytes(out[a_end..a_end + 4].try_into().unwrap()) as usize;
        assert_eq!(b_end, a_end + 13 + 1);
        assert_eq!(&out[b_end..b_end + 13], &SENTINEL);
        assert_eq!(&out[b_end + 13..b_end + 29], FOOTER_ID);
        assert_eq!(&out[out.len() - 16..], FOOTER_MAGIC);
        assert_eq!((out.len() - 16 - 120 - 4) % 16, 0);
    }

    #[test]
    fn small_arrays_are_raw_and_large_ones_compressed() {
        let mut small = Vec::new();
        write_prop(&mut small, &Prop::I32s(vec![1, 2, 3]));
        assert_eq!(small[0], b'i');
        assert_eq!(&small[1..13], &[3, 0, 0, 0, 0, 0, 0, 0, 12, 0, 0, 0]);

        let mut large = Vec::new();
        write_prop(&mut large, &Prop::F64s(vec![0.5; 1000]));
        assert_eq!(u32::from_le_bytes(large[5..9].try_into().unwrap()), 1);
        assert!(large.len() < 1000);
    }

    fn test_mesh() -> Mesh {
        use mesher::{Atlas, MeshLayer, Vertex, atlas::Rect};
        let quad = |y: f32, color: [u8; 4]| {
            let v = |x: f32, z: f32, u: f32, w: f32| Vertex { position: [x, y, z], normal: [0.0, 1.0, 0.0], uv: [u, w], color, ao: 1.0, block_y: 0 };
            MeshLayer {
                vertices: vec![v(0.0, 0.0, 0.0, 0.0), v(0.0, 1.0, 0.0, 0.5), v(1.0, 1.0, 0.5, 0.5), v(1.0, 0.0, 0.5, 0.0)],
                indices: vec![0, 1, 2, 0, 2, 3],
            }
        };
        // Two quads sharing an edge in the opaque layer, one tinted quad above.
        let mut opaque = quad(0.0, [255; 4]);
        let second = quad(0.0, [255; 4]);
        opaque.vertices.extend(second.vertices.iter().map(|v| Vertex { position: [v.position[0] + 1.0, 0.0, v.position[2]], ..*v }));
        opaque.indices.extend([4, 5, 6, 4, 6, 7]);
        Mesh {
            opaque,
            cutout: MeshLayer::default(),
            translucent: quad(1.0, [63, 118, 228, 255]),
            atlas: Some(Atlas {
                width: 2,
                height: 2,
                rgba: [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [9, 9, 9, 100]].concat(),
                rects: vec![Rect { min: [0.0; 2], size: [0.5; 2] }],
                mip_levels: 1,
            }),
            bounds: [[0.0; 3], [2.0, 1.0, 1.0]],
            caps: Default::default(),
        }
    }

    #[test]
    fn ufbx_reads_it_back_in_strict_mode() {
        let files = fbx(&test_mesh(), "test", "model");
        assert_eq!(files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["model.fbx", "model.png"]);
        let opts = ufbx::LoadOpts { strict: true, ..Default::default() };
        let scene = ufbx::load_memory(&files[0].data, opts).expect("ufbx rejected the file");

        assert_eq!(scene.metadata.version, 7400);
        assert!(scene.metadata.warnings.is_empty(), "{:?}", scene.metadata.warnings.iter().map(|w| w.description.to_string()).collect::<Vec<_>>());
        assert!((scene.settings.unit_meters - 0.01 * 100.0).abs() < 1e-9);

        assert_eq!(scene.meshes.len(), 1);
        let mesh = &scene.meshes[0];
        // Three quads; the two opaque ones share an edge, so 4 + 2 + 4 points.
        assert_eq!(mesh.num_faces, 3);
        assert!(mesh.faces.iter().all(|f| f.num_indices == 4));
        assert_eq!(mesh.num_vertices, 10);
        assert_eq!(mesh.face_material.iter().copied().collect::<Vec<_>>(), [0, 0, 1]);
        assert_eq!(mesh.materials.len(), 2);

        let first_uv = mesh.vertex_uv.values[mesh.vertex_uv.indices[0] as usize];
        assert!((first_uv.x - 0.0).abs() < 1e-6 && (first_uv.y - 1.0).abs() < 1e-6);
        let normal = mesh.vertex_normal.values[mesh.vertex_normal.indices[0] as usize];
        assert!((normal.y - 1.0).abs() < 1e-6);
        assert!(mesh.vertex_color.exists);
        let tint = mesh.vertex_color.values[mesh.vertex_color.indices[mesh.num_indices - 1] as usize];
        assert!((tint.x - 63.0 / 255.0).abs() < 1e-6 && (tint.w - 1.0).abs() < 1e-6);

        for material in mesh.materials.iter() {
            let texture = material.fbx.diffuse_color.texture.as_ref().expect("diffuse texture");
            assert_eq!(texture.relative_filename.as_ref() as &str, "model.png");
            let content: &[u8] = texture.content.as_ref();
            assert_eq!(content, files[1].data.as_slice());
        }
        assert!(mesh.materials.iter().all(|m| m.fbx.transparency_color.texture.is_none()));
        let names: Vec<String> = mesh.materials.iter().map(|m| m.element.name.to_string()).collect();
        assert_eq!(names, ["opaque", "translucent"]);
    }
}
