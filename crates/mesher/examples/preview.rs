//! Render an isometric preview PNG on the CPU, for checking meshes without a GPU.
//!
//! cargo run --release -p mesher --example preview -- <file.litematic> <out.png> <pack>... [--yaw 45] [--size 1024] [--xmax N]

use glam::{Mat3, Vec2, Vec3};
use schematic_wgpu_mesher::{Mesh, MeshLayer, Mesher};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut flag = |name: &str, default: f32| -> f32 {
        match args.iter().position(|a| a == name) {
            Some(i) => {
                let value = args.remove(i + 1).parse().expect("number");
                args.remove(i);
                value
            }
            None => default,
        }
    };
    let yaw = flag("--yaw", 45.0);
    let size = flag("--size", 1024.0) as u32;
    let xmax = flag("--xmax", f32::INFINITY);
    let [input, output, packs @ ..] = args.as_slice() else {
        eprintln!("usage: preview <file.litematic> <out.png> <pack>... [--yaw deg] [--size px]");
        std::process::exit(2);
    };

    let lit = litematic::Litematic::open(input).expect("read litematic");
    let mut stack = mc_assets::PackStack::new();
    for pack in packs {
        stack.push_path(pack).expect("open pack");
    }
    let mut assets = mc_assets::Assets::new(stack);
    let (mut mesh, _) = Mesher::new(&mut assets, Default::default()).build(&lit);
    if xmax.is_finite() {
        crop(&mut mesh, xmax);
    }
    render(&mesh, yaw, size).save(output).expect("write png");
}

/// Drop triangles east of `xmax` so a part of a long build can be inspected.
fn crop(mesh: &mut Mesh, xmax: f32) {
    let mut max = Vec3::MIN;
    for layer in [&mut mesh.opaque, &mut mesh.cutout, &mut mesh.translucent] {
        let vertices = &layer.vertices;
        let kept: Vec<u32> = layer
            .indices
            .chunks_exact(3)
            .filter(|t| t.iter().all(|&i| vertices[i as usize].position[0] <= xmax))
            .flatten()
            .copied()
            .collect();
        for &i in &kept {
            max = max.max(Vec3::from(vertices[i as usize].position));
        }
        layer.indices = kept;
    }
    mesh.bounds[1] = max.into();
}

fn render(mesh: &Mesh, yaw: f32, size: u32) -> image::RgbaImage {
    // Look down 30° from the given yaw, orthographic.
    let view = Mat3::from_rotation_x(30f32.to_radians()) * Mat3::from_rotation_y(yaw.to_radians());
    let [min, max] = mesh.bounds.map(Vec3::from);
    let center = (min + max) / 2.0;
    let project = |p: [f32; 3]| view * (Vec3::from(p) - center);

    let extent = (0..8)
        .map(|i| {
            let corner = Vec3::new(
                if i & 1 == 0 { min.x } else { max.x },
                if i & 2 == 0 { min.y } else { max.y },
                if i & 4 == 0 { min.z } else { max.z },
            );
            let p = project(corner.into());
            p.x.abs().max(p.y.abs())
        })
        .fold(1e-3, f32::max);
    let scale = size as f32 * 0.48 / extent;

    let mut target = Target {
        size,
        color: vec![[24, 26, 30, 255]; (size * size) as usize],
        depth: vec![f32::MAX; (size * size) as usize],
    };
    let atlas = mesh.atlas.as_ref().expect("atlas");
    // The camera looks down -Z, so distance from the viewer is -z.
    let to_screen = |p: Vec3| Vec3::new(size as f32 / 2.0 + p.x * scale, size as f32 / 2.0 - p.y * scale, -p.z);

    let mut draw = |layer: &MeshLayer, blend: bool| {
        let mut tris: Vec<[u32; 3]> = layer.indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect();
        if blend {
            // Painter's algorithm: far triangles first.
            let depth = |t: &[u32; 3]| t.iter().map(|&i| -project(layer.vertices[i as usize].position).z).sum::<f32>();
            tris.sort_by(|a, b| depth(b).total_cmp(&depth(a)));
        }
        for tri in tris {
            let v = tri.map(|i| layer.vertices[i as usize]);
            let s = v.map(|v| to_screen(project(v.position)));
            let n = Vec3::from(v[0].normal);
            // Minecraft's fixed directional shading.
            let shade = if n.y > 0.5 { 1.0 } else if n.y < -0.5 { 0.5 } else if n.z.abs() > n.x.abs() { 0.8 } else { 0.6 };
            target.triangle(s, |w| {
                let uv = Vec2::from(v[0].uv) * w[0] + Vec2::from(v[1].uv) * w[1] + Vec2::from(v[2].uv) * w[2];
                let x = ((uv.x * atlas.width as f32) as u32).min(atlas.width - 1);
                let y = ((uv.y * atlas.height as f32) as u32).min(atlas.height - 1);
                let i = ((y * atlas.width + x) * 4) as usize;
                let t = &atlas.rgba[i..i + 4];
                let c = v[0].color;
                let rgb = [0, 1, 2].map(|k| t[k] as f32 * c[k] as f32 / 255.0 * shade);
                [rgb[0], rgb[1], rgb[2], t[3] as f32 / 255.0]
            }, blend);
        }
    };
    draw(&mesh.opaque, false);
    draw(&mesh.cutout, false);
    draw(&mesh.translucent, true);

    image::RgbaImage::from_fn(size, size, |x, y| image::Rgba(target.color[(y * size + x) as usize]))
}

struct Target {
    size: u32,
    color: Vec<[u8; 4]>,
    depth: Vec<f32>,
}

impl Target {
    /// Rasterize a screen-space triangle; `shade` gets barycentric weights
    /// and returns linear RGB (0..255) plus alpha.
    fn triangle(&mut self, s: [Vec3; 3], shade: impl Fn([f32; 3]) -> [f32; 4], blend: bool) {
        let area = (s[1].x - s[0].x) * (s[2].y - s[0].y) - (s[2].x - s[0].x) * (s[1].y - s[0].y);
        if area.abs() < 1e-9 {
            return;
        }
        let lo = s.iter().fold(Vec3::MAX, |a, &b| a.min(b)).max(Vec3::ZERO);
        let hi = s.iter().fold(Vec3::MIN, |a, &b| a.max(b)).min(Vec3::splat(self.size as f32 - 1.0));
        for py in lo.y as u32..=hi.y.max(0.0) as u32 {
            for px in lo.x as u32..=hi.x.max(0.0) as u32 {
                let p = Vec2::new(px as f32 + 0.5, py as f32 + 0.5);
                let edge = |a: Vec3, b: Vec3| (b.x - a.x) * (p.y - a.y) - (p.x - a.x) * (b.y - a.y);
                let w = [edge(s[1], s[2]) / area, edge(s[2], s[0]) / area, edge(s[0], s[1]) / area];
                if w.iter().any(|&x| x < 0.0) {
                    continue;
                }
                let z = w[0] * s[0].z + w[1] * s[1].z + w[2] * s[2].z;
                let i = (py * self.size + px) as usize;
                if z >= self.depth[i] {
                    continue;
                }
                let [r, g, b, a] = shade(w);
                if a < 0.1 {
                    continue;
                }
                let dst = self.color[i];
                let mix = |src: f32, dst: u8| if blend { src * a + dst as f32 * (1.0 - a) } else { src };
                self.color[i] = [mix(r, dst[0]) as u8, mix(g, dst[1]) as u8, mix(b, dst[2]) as u8, 255];
                if !blend {
                    self.depth[i] = z;
                }
            }
        }
    }
}
