//! Load FBX files with ufbx in strict mode and print what it sees.
//!
//! cargo run -p export --example fbx_check -- file.fbx...

fn main() {
    for path in std::env::args().skip(1) {
        let data = std::fs::read(&path).expect("read file");
        let opts = ufbx::LoadOpts { strict: true, ..Default::default() };
        let scene = match ufbx::load_memory(&data, opts) {
            Ok(scene) => scene,
            Err(e) => {
                println!("{path}: REJECTED: {}", e.description);
                continue;
            }
        };
        let mesh = &scene.meshes[0];
        let quads = mesh.faces.iter().filter(|f| f.num_indices == 4).count();
        let textures = scene.textures.iter().filter(|t| !t.content.is_empty()).count();
        println!(
            "{path}: FBX {}, {} faces ({quads} quads), {} triangles, {} points, {} materials, {} embedded textures, colors: {}, warnings: {}",
            scene.metadata.version,
            mesh.num_faces,
            mesh.num_triangles,
            mesh.num_vertices,
            mesh.materials.len(),
            textures,
            mesh.vertex_color.exists,
            scene.metadata.warnings.len(),
        );
        for w in scene.metadata.warnings.iter() {
            println!("  warning: {}", w.description);
        }
    }
}
