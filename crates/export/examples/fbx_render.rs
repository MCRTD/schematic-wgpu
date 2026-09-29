//! Render an FBX as ufbx reads it, to check geometry, UVs and the embedded
//! texture through an independent parser.
//!
//! cargo run --release -p export --example fbx_render -- in.fbx out.png [yaw] [zoom]

use mesher::{Atlas, Mesh, MeshLayer, Vertex, atlas::Rect};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (input, output) = (&args[1], &args[2]);
    let yaw: f32 = args.get(3).map_or(45.0, |a| a.parse().unwrap());
    let zoom: f32 = args.get(4).map_or(1.0, |a| a.parse().unwrap());

    let opts = ufbx::LoadOpts { strict: true, ..Default::default() };
    let scene = ufbx::load_file(input, opts).expect("load fbx");
    let fbx = &scene.meshes[0];

    // Material order in the file is opaque, cutout, translucent (skipping empty ones).
    let layer_of = |name: &str| match name {
        "cutout" => 1,
        "translucent" => 2,
        _ => 0,
    };
    let mut layers = [MeshLayer::default(), MeshLayer::default(), MeshLayer::default()];
    let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
    for (face_index, face) in fbx.faces.iter().enumerate() {
        let material = &fbx.materials[fbx.face_material[face_index] as usize];
        let layer = &mut layers[layer_of(&material.element.name)];
        let base = layer.vertices.len() as u32;
        for i in face.index_begin..face.index_begin + face.num_indices {
            let i = i as usize;
            let p = fbx.vertex_position.values[fbx.vertex_position.indices[i] as usize];
            let n = fbx.vertex_normal.values[fbx.vertex_normal.indices[i] as usize];
            let uv = fbx.vertex_uv.values[fbx.vertex_uv.indices[i] as usize];
            let position = [p.x as f32, p.y as f32, p.z as f32];
            for k in 0..3 {
                min[k] = min[k].min(position[k]);
                max[k] = max[k].max(position[k]);
            }
            layer.vertices.push(Vertex {
                position,
                normal: [n.x as f32, n.y as f32, n.z as f32],
                // Back from FBX's bottom-up v to our top-down atlas.
                uv: [uv.x as f32, 1.0 - uv.y as f32],
                color: [255; 4],
                ao: 1.0,
            });
        }
        for k in 1..face.num_indices - 1 {
            layer.indices.extend([base, base + k, base + k + 1]);
        }
    }

    let texture = fbx.materials[0].fbx.diffuse_color.texture.as_ref().expect("texture");
    let content: &[u8] = texture.content.as_ref();
    let image = image::load_from_memory(content).expect("embedded png").to_rgba8();
    let [opaque, cutout, translucent] = layers;
    let mesh = Mesh {
        opaque,
        cutout,
        translucent,
        atlas: Some(Atlas {
            width: image.width(),
            height: image.height(),
            rgba: image.into_raw(),
            rects: vec![Rect { min: [0.0; 2], size: [1.0; 2] }],
            mip_levels: 1,
        }),
        bounds: [min, max],
    };

    let mut camera = viewer::OrbitCamera { yaw: yaw.to_radians(), ..Default::default() };
    camera.frame(glam::Vec3::from(min), glam::Vec3::from(max));
    camera.distance /= zoom;
    let pixels = pollster::block_on(viewer::headless::render(&mesh, &camera, 1280, 720)).expect("render");
    image::RgbaImage::from_raw(1280, 720, pixels).unwrap().save(output).unwrap();
    println!("rendered {} faces to {output}", fbx.num_faces);
}
