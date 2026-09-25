//! glTF 2.0 output, as a single binary `.glb` or a `.gltf` with separate
//! `.bin` and `.png` files.

use mesher::{Mesh, MeshLayer, Transparency};
use serde_json::{Value, json};

use crate::{File, atlas_png};

const ARRAY_BUFFER: u32 = 34962;
const ELEMENT_ARRAY_BUFFER: u32 = 34963;
const FLOAT: u32 = 5126;
const UNSIGNED_BYTE: u32 = 5121;
const UNSIGNED_INT: u32 = 5125;
const NEAREST: u32 = 9728;
const CLAMP_TO_EDGE: u32 = 33071;

/// Binary data plus the JSON describing it.
struct Builder {
    bin: Vec<u8>,
    buffer_views: Vec<Value>,
    accessors: Vec<Value>,
}

impl Builder {
    fn view(&mut self, bytes: &[u8], target: Option<u32>) -> usize {
        // Every view starts 4-byte aligned, which satisfies all accessor types.
        while !self.bin.len().is_multiple_of(4) {
            self.bin.push(0);
        }
        let mut view = json!({ "buffer": 0, "byteOffset": self.bin.len(), "byteLength": bytes.len() });
        if let Some(target) = target {
            view["target"] = target.into();
        }
        self.bin.extend_from_slice(bytes);
        self.buffer_views.push(view);
        self.buffer_views.len() - 1
    }

    fn accessor(&mut self, bytes: &[u8], target: u32, component: u32, kind: &str, count: usize, extra: Value) -> usize {
        let view = self.view(bytes, Some(target));
        let mut accessor = json!({ "bufferView": view, "componentType": component, "count": count, "type": kind });
        accessor.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
        self.accessors.push(accessor);
        self.accessors.len() - 1
    }

    fn primitive(&mut self, layer: &MeshLayer, material: usize) -> Value {
        let v = &layer.vertices;
        let floats = |f: &dyn Fn(&mesher::Vertex) -> Vec<f32>| -> Vec<u8> {
            v.iter().flat_map(f).flat_map(f32::to_le_bytes).collect()
        };

        let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
        for p in v.iter().map(|v| v.position) {
            for k in 0..3 {
                min[k] = min[k].min(p[k]);
                max[k] = max[k].max(p[k]);
            }
        }
        let n = v.len();
        let position = self.accessor(&floats(&|v| v.position.to_vec()), ARRAY_BUFFER, FLOAT, "VEC3", n, json!({ "min": min, "max": max }));
        let normal = self.accessor(&floats(&|v| v.normal.to_vec()), ARRAY_BUFFER, FLOAT, "VEC3", n, json!({}));
        let uv = self.accessor(&floats(&|v| v.uv.to_vec()), ARRAY_BUFFER, FLOAT, "VEC2", n, json!({}));
        let mut attributes = json!({ "POSITION": position, "NORMAL": normal, "TEXCOORD_0": uv });

        if v.iter().any(|v| v.color != [255; 4]) {
            let colors: Vec<u8> = v.iter().flat_map(|v| v.color).collect();
            attributes["COLOR_0"] = self
                .accessor(&colors, ARRAY_BUFFER, UNSIGNED_BYTE, "VEC4", n, json!({ "normalized": true }))
                .into();
        }

        let indices: Vec<u8> = layer.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
        let indices = self.accessor(&indices, ELEMENT_ARRAY_BUFFER, UNSIGNED_INT, "SCALAR", layer.indices.len(), json!({}));
        json!({ "attributes": attributes, "indices": indices, "material": material })
    }
}

fn material(transparency: Transparency) -> Value {
    let (name, alpha) = match transparency {
        Transparency::Opaque => ("opaque", json!({ "alphaMode": "OPAQUE" })),
        Transparency::Cutout => ("cutout", json!({ "alphaMode": "MASK", "alphaCutoff": 0.5, "doubleSided": true })),
        Transparency::Translucent => ("translucent", json!({ "alphaMode": "BLEND", "doubleSided": true })),
    };
    let mut m = json!({
        "name": name,
        "pbrMetallicRoughness": {
            "baseColorTexture": { "index": 0 },
            "metallicFactor": 0.0,
            "roughnessFactor": 1.0,
        },
    });
    m.as_object_mut().unwrap().extend(alpha.as_object().cloned().unwrap());
    m
}

/// Build the glTF JSON and binary buffer. The atlas PNG is either placed in
/// the buffer (`embed_image`) or referenced as `<stem>.png`.
fn build(mesh: &Mesh, name: &str, stem: &str, embed_image: bool) -> (Value, Vec<u8>) {
    let mut b = Builder { bin: Vec::new(), buffer_views: Vec::new(), accessors: Vec::new() };
    let mut materials = Vec::new();
    let mut primitives = Vec::new();
    for (transparency, layer) in mesh.layers() {
        if layer.is_empty() {
            continue;
        }
        materials.push(material(transparency));
        primitives.push(b.primitive(layer, materials.len() - 1));
    }

    let mut root = json!({
        "asset": { "version": "2.0", "generator": concat!("schematic-wgpu ", env!("CARGO_PKG_VERSION")) },
        "scene": 0,
        "scenes": [{ "name": name, "nodes": [0] }],
        "nodes": [{ "name": name }],
    });

    if !primitives.is_empty() {
        let png = atlas_png(mesh);
        let image = if embed_image {
            json!({ "bufferView": b.view(&png, None), "mimeType": "image/png" })
        } else {
            json!({ "uri": format!("{stem}.png") })
        };
        root["nodes"][0]["mesh"] = 0.into();
        root["meshes"] = json!([{ "name": name, "primitives": primitives }]);
        root["materials"] = materials.into();
        // Nearest filtering keeps the pixel-art look.
        root["samplers"] = json!([{ "magFilter": NEAREST, "minFilter": NEAREST, "wrapS": CLAMP_TO_EDGE, "wrapT": CLAMP_TO_EDGE }]);
        root["images"] = json!([image]);
        root["textures"] = json!([{ "sampler": 0, "source": 0 }]);
    }

    if !b.bin.is_empty() {
        while !b.bin.len().is_multiple_of(4) {
            b.bin.push(0);
        }
        let mut buffer = json!({ "byteLength": b.bin.len() });
        if !embed_image {
            buffer["uri"] = format!("{stem}.bin").into();
        }
        root["buffers"] = json!([buffer]);
        root["bufferViews"] = b.buffer_views.into();
        root["accessors"] = b.accessors.into();
    }
    (root, b.bin)
}

/// A self-contained binary glTF.
pub fn glb(mesh: &Mesh, name: &str) -> Vec<u8> {
    let (json, bin) = build(mesh, name, "", true);
    let mut json = serde_json::to_vec(&json).expect("serializable json");
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }

    let mut chunks = Vec::new();
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        chunks.extend((data.len() as u32).to_le_bytes());
        chunks.extend(kind);
        chunks.extend(data);
    };
    chunk(b"JSON", &json);
    if !bin.is_empty() {
        chunk(b"BIN\0", &bin);
    }

    let mut out = Vec::with_capacity(12 + chunks.len());
    out.extend(b"glTF");
    out.extend(2u32.to_le_bytes());
    out.extend(((12 + chunks.len()) as u32).to_le_bytes());
    out.extend(chunks);
    out
}

/// `<stem>.gltf`, `<stem>.bin` and `<stem>.png`.
pub fn gltf(mesh: &Mesh, name: &str, stem: &str) -> Vec<File> {
    let (json, bin) = build(mesh, name, stem, false);
    let mut files = vec![File {
        name: format!("{stem}.gltf"),
        data: serde_json::to_vec_pretty(&json).expect("serializable json"),
    }];
    if !bin.is_empty() {
        files.push(File { name: format!("{stem}.bin"), data: bin });
        files.push(File { name: format!("{stem}.png"), data: atlas_png(mesh) });
    }
    files
}
