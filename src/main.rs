use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use litematic::Litematic;

#[derive(Parser)]
#[command(name = "schematic", version, about = "Convert Litematica schematics to 3D models")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Decode a .litematic file to JSON
    Decode {
        litematic: PathBuf,
        /// Output file (defaults to <input>.json)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Print a summary of a .litematic file
    Info { litematic: PathBuf },
    /// Build the 3D mesh and report statistics and unsupported blocks
    Mesh {
        litematic: PathBuf,
        /// Resource packs (folder, .zip or client .jar), highest priority first
        #[arg(short, long = "pack", required = true)]
        packs: Vec<PathBuf>,
    },
    /// Export a .litematic file as .glb, .gltf, .obj or .fbx
    Export {
        litematic: PathBuf,
        /// Resource packs (folder, .zip or client .jar), highest priority first
        #[arg(short, long = "pack", required = true)]
        packs: Vec<PathBuf>,
        /// Output file; the format follows the extension (defaults to <input>.glb)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Leave out blocks that can't be resolved instead of drawing placeholder cubes
        #[arg(long)]
        no_placeholders: bool,
    },
    /// Open an interactive 3D viewer window
    View {
        litematic: PathBuf,
        /// Resource packs (folder, .zip or client .jar), highest priority first
        #[arg(short, long = "pack", required = true)]
        packs: Vec<PathBuf>,
    },
    /// Render a .litematic file to a PNG image on the GPU
    Render {
        litematic: PathBuf,
        /// Resource packs (folder, .zip or client .jar), highest priority first
        #[arg(short, long = "pack", required = true)]
        packs: Vec<PathBuf>,
        /// Output image (defaults to <input>.png)
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long, default_value_t = 1280)]
        width: u32,
        #[arg(long, default_value_t = 720)]
        height: u32,
        /// Camera angle around the vertical axis, in degrees
        #[arg(long, default_value_t = 45.0)]
        yaw: f32,
        /// Camera angle above the horizon, in degrees
        #[arg(long, default_value_t = 30.0)]
        pitch: f32,
        /// Zoom factor; 2 is twice as close as the default framing
        #[arg(long, default_value_t = 1.0)]
        zoom: f32,
    },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Decode { litematic, output } => {
            let lit = open(&litematic)?;
            let output = output.unwrap_or_else(|| litematic.with_extension("json"));
            let file = std::fs::File::create(&output)
                .with_context(|| format!("failed to create {}", output.display()))?;
            serde_json::to_writer_pretty(std::io::BufWriter::new(file), &lit)?;
            println!("wrote {}", output.display());
        }
        Command::Info { litematic } => {
            let lit = open(&litematic)?;
            let m = &lit.metadata;
            println!("name:         {}", m.name);
            println!("author:       {}", m.author);
            println!("data version: {}", lit.data_version);
            println!("size:         {}x{}x{}", m.enclosing_size.x, m.enclosing_size.y, m.enclosing_size.z);
            for r in &lit.regions {
                println!(
                    "region `{}`: origin ({}, {}, {}), size {}x{}x{}, {} palette entries, {} non-air blocks",
                    r.name, r.origin.x, r.origin.y, r.origin.z, r.size.x, r.size.y, r.size.z,
                    r.palette.len(),
                    r.iter_blocks().count(),
                );
            }
        }
        Command::Mesh { litematic, packs } => {
            let lit = open(&litematic)?;
            let mut assets = load_packs(&packs)?;
            let start = std::time::Instant::now();
            let (mesh, report) = mesher::Mesher::new(&mut assets, Default::default()).build(&lit);
            println!("meshed in {:.2?}", start.elapsed());
            for (kind, layer) in mesh.layers() {
                println!("{kind:?}: {} triangles, {} vertices", layer.indices.len() / 3, layer.vertices.len());
            }
            if let Some(atlas) = &mesh.atlas {
                println!("atlas: {}x{}, {} sprites", atlas.width, atlas.height, atlas.rects.len());
            }
            println!("bounds: {:?} .. {:?}", mesh.bounds[0], mesh.bounds[1]);
            print_report(&report);
        }
        Command::Export { litematic, packs, output, no_placeholders } => {
            let output = output.unwrap_or_else(|| litematic.with_extension("glb"));
            let ext = output.extension().and_then(|e| e.to_str()).unwrap_or_default();
            let format = export::Format::from_extension(ext)
                .with_context(|| format!("unsupported output format `.{ext}` (use .glb, .gltf, .obj or .fbx)"))?;
            let stem = output.file_stem().and_then(|s| s.to_str()).context("output needs a file name")?;
            let dir = output.parent().unwrap_or(std::path::Path::new(""));

            let lit = open(&litematic)?;
            let mut assets = load_packs(&packs)?;
            let options = mesher::Options {
                placeholder_for_missing: !no_placeholders,
                bake_tint: format.wants_baked_tint(),
            };
            let (mesh, report) = mesher::Mesher::new(&mut assets, options).build(&lit);
            print_report(&report);

            let name = if lit.metadata.name.is_empty() { stem } else { &lit.metadata.name };
            for file in export::export(&mesh, format, name, stem) {
                let path = dir.join(&file.name);
                std::fs::write(&path, &file.data).with_context(|| format!("failed to write {}", path.display()))?;
                println!("wrote {} ({:.1} MB)", path.display(), file.data.len() as f64 / 1e6);
            }
        }
        Command::View { litematic, packs } => {
            let lit = open(&litematic)?;
            let mut assets = load_packs(&packs)?;
            let (mesh, report) = mesher::Mesher::new(&mut assets, Default::default()).build(&lit);
            print_report(&report);
            let name = if lit.metadata.name.is_empty() { "schematic" } else { &lit.metadata.name };
            viewer::app::run(mesh, &format!("{name} - schematic viewer"))?;
        }
        Command::Render { litematic, packs, output, width, height, yaw, pitch, zoom } => {
            render(litematic, packs, output, (width, height), (yaw, pitch, zoom))?;
        }
    }
    Ok(())
}

fn render(
    litematic: PathBuf,
    packs: Vec<PathBuf>,
    output: Option<PathBuf>,
    (width, height): (u32, u32),
    (yaw, pitch, zoom): (f32, f32, f32),
) -> anyhow::Result<()> {
    let lit = open(&litematic)?;
    let mut assets = load_packs(&packs)?;
    let (mesh, report) = mesher::Mesher::new(&mut assets, Default::default()).build(&lit);
    print_report(&report);

    let mut camera = viewer::OrbitCamera {
        yaw: yaw.to_radians(),
        pitch: pitch.to_radians(),
        ..Default::default()
    };
    camera.frame(glam::Vec3::from(mesh.bounds[0]), glam::Vec3::from(mesh.bounds[1]));
    camera.distance /= zoom;

    let pixels = pollster::block_on(viewer::headless::render(&mesh, &camera, width, height))?;
    let output = output.unwrap_or_else(|| litematic.with_extension("png"));
    image::RgbaImage::from_raw(width, height, pixels)
        .context("rendered image has the wrong size")?
        .save(&output)
        .with_context(|| format!("failed to write {}", output.display()))?;
    println!("wrote {}", output.display());
    Ok(())
}

fn print_report(report: &mesher::Report) {
    for (name, (reason, count)) in &report.skipped {
        eprintln!("warning: {name} x{count}: {reason}");
    }
    for texture in &report.missing_textures {
        eprintln!("warning: missing texture {texture}");
    }
}

fn load_packs(packs: &[PathBuf]) -> anyhow::Result<mc_assets::Assets> {
    let mut stack = mc_assets::PackStack::new();
    for pack in packs {
        stack
            .push_path(pack)
            .with_context(|| format!("failed to open resource pack {}", pack.display()))?;
    }
    Ok(mc_assets::Assets::new(stack))
}

fn open(path: &PathBuf) -> anyhow::Result<Litematic> {
    Litematic::open(path).with_context(|| format!("failed to read {}", path.display()))
}
