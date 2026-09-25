//! Turns block models into quads in block-local space (0..1 on each axis).

use glam::{Quat, Vec3};
use mc_assets::{Axis, Direction, Element, Face, ModelInstance};

/// One textured quad of a block, before placement in the world.
#[derive(Debug, Clone, PartialEq)]
pub struct Quad {
    /// Corners in counter-clockwise order seen from the front:
    /// top-left, bottom-left, bottom-right, top-right of the texture.
    pub positions: [Vec3; 4],
    /// UVs in 0..1 sprite space.
    pub uvs: [[f32; 2]; 4],
    pub normal: Vec3,
    /// Skip this quad when the neighbour in this direction hides it.
    pub cullface: Option<Direction>,
    /// Texture location, or `None` if the model's reference was unresolved.
    pub texture: Option<String>,
    pub tinted: bool,
}

impl Quad {
    /// Whether this quad covers the entire block face on side `dir`.
    pub fn covers_face(&self, dir: Direction) -> bool {
        let n = dir.normal();
        let axis = n.iter().position(|&c| c != 0).unwrap();
        let plane = if n[axis] > 0 { 1.0 } else { 0.0 };
        let (a, b) = ((axis + 1) % 3, (axis + 2) % 3);
        let close = |x: f32, y: f32| (x - y).abs() < 1e-4;
        let on_plane = self.positions.iter().all(|p| close(p[axis], plane));
        let (mut min, mut max) = ([f32::MAX; 2], [f32::MIN; 2]);
        for p in &self.positions {
            for (k, &c) in [a, b].iter().enumerate() {
                min[k] = min[k].min(p[c]);
                max[k] = max[k].max(p[c]);
            }
        }
        on_plane && min.iter().all(|&m| close(m, 0.0)) && max.iter().all(|&m| close(m, 1.0))
    }
}

/// Face corners in 0..16 model space, ordered top-left, bottom-left,
/// bottom-right, top-right as the texture appears on the face.
fn corners(dir: Direction, f: [f32; 3], t: [f32; 3]) -> [Vec3; 4] {
    let v = Vec3::new;
    match dir {
        Direction::Down => [v(f[0], f[1], t[2]), v(f[0], f[1], f[2]), v(t[0], f[1], f[2]), v(t[0], f[1], t[2])],
        Direction::Up => [v(f[0], t[1], f[2]), v(f[0], t[1], t[2]), v(t[0], t[1], t[2]), v(t[0], t[1], f[2])],
        Direction::North => [v(t[0], t[1], f[2]), v(t[0], f[1], f[2]), v(f[0], f[1], f[2]), v(f[0], t[1], f[2])],
        Direction::South => [v(f[0], t[1], t[2]), v(f[0], f[1], t[2]), v(t[0], f[1], t[2]), v(t[0], t[1], t[2])],
        Direction::West => [v(f[0], t[1], f[2]), v(f[0], f[1], f[2]), v(f[0], f[1], t[2]), v(f[0], t[1], t[2])],
        Direction::East => [v(t[0], t[1], t[2]), v(t[0], f[1], t[2]), v(t[0], f[1], f[2]), v(t[0], t[1], f[2])],
    }
}

/// UV of a point in 0..16 space projected onto a face, matching the UVs
/// Minecraft derives when a face has no explicit `uv`.
fn project_uv(dir: Direction, p: Vec3) -> [f32; 2] {
    match dir {
        Direction::Down => [p.x, 16.0 - p.z],
        Direction::Up => [p.x, p.z],
        Direction::North => [16.0 - p.x, 16.0 - p.y],
        Direction::South => [p.x, 16.0 - p.y],
        Direction::West => [p.z, 16.0 - p.y],
        Direction::East => [16.0 - p.z, 16.0 - p.y],
    }
}

fn default_uv(dir: Direction, from: [f32; 3], to: [f32; 3]) -> [f32; 4] {
    let c = corners(dir, from, to);
    let [u0, v0] = project_uv(dir, c[0]);
    let [u1, v1] = project_uv(dir, c[2]);
    [u0, v0, u1, v1]
}

fn element_transform(element: &Element) -> impl Fn(Vec3) -> Vec3 {
    let rotation = element.rotation.as_ref().map(|r| {
        let axis = match r.axis {
            Axis::X => Vec3::X,
            Axis::Y => Vec3::Y,
            Axis::Z => Vec3::Z,
        };
        let angle = r.angle.to_radians();
        let scale = if r.rescale {
            // Stretch the two other axes so a 45° plane still spans the block.
            (Vec3::ONE - axis) * (1.0 / angle.cos() - 1.0) + Vec3::ONE
        } else {
            Vec3::ONE
        };
        (Quat::from_axis_angle(axis, angle), Vec3::from(r.origin), scale)
    });
    move |p| match rotation {
        Some((q, origin, scale)) => (q * (p - origin)) * scale + origin,
        None => p,
    }
}

pub fn bake(instance: &ModelInstance) -> Vec<Quad> {
    // Minecraft's ModelRotation: rotate around X first, then Y, both clockwise.
    let block_rotation = Quat::from_rotation_y((-instance.y as f32).to_radians())
        * Quat::from_rotation_x((-instance.x as f32).to_radians());
    let rotated = instance.x.rem_euclid(360) != 0 || instance.y.rem_euclid(360) != 0;
    let center = Vec3::splat(8.0);
    let rotate_dir = |d: Direction| {
        let n = d.normal();
        let v = block_rotation * Vec3::new(n[0] as f32, n[1] as f32, n[2] as f32);
        Direction::from_normal(v.into())
    };

    let mut quads = Vec::new();
    for element in &instance.model.elements {
        let transform = element_transform(element);
        let element_rotation = element
            .rotation
            .as_ref()
            .map(|r| {
                let axis = match r.axis {
                    Axis::X => Vec3::X,
                    Axis::Y => Vec3::Y,
                    Axis::Z => Vec3::Z,
                };
                Quat::from_axis_angle(axis, r.angle.to_radians())
            })
            .unwrap_or(Quat::IDENTITY);

        for face in &element.faces {
            let Face { direction, uv, texture, cullface, rotation, tintindex } = face;
            let positions = corners(*direction, element.from, element.to)
                .map(|p| block_rotation * (transform(p) - center) + center);

            let world_dir = rotate_dir(*direction);
            let uvs = if instance.uvlock && rotated {
                // Keep the texture aligned to the world instead of the model.
                positions.map(|p| project_uv(world_dir, p))
            } else {
                let [u0, v0, u1, v1] = uv.unwrap_or_else(|| default_uv(*direction, element.from, element.to));
                let corner_uvs = [[u0, v0], [u0, v1], [u1, v1], [u1, v0]];
                let shift = (rotation / 90) as usize;
                std::array::from_fn(|i| corner_uvs[(i + shift) % 4])
            };

            let n = direction.normal();
            let normal = block_rotation * element_rotation * Vec3::new(n[0] as f32, n[1] as f32, n[2] as f32);
            quads.push(Quad {
                positions: positions.map(|p| p / 16.0),
                uvs: uvs.map(|[u, v]| [u / 16.0, v / 16.0]),
                normal: normal.normalize(),
                cullface: cullface.map(rotate_dir),
                texture: texture.clone(),
                tinted: tintindex.is_some(),
            });
        }
    }
    quads
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_assets::{ElementRotation, Model};
    use std::sync::Arc;

    fn face(direction: Direction, cull: bool) -> Face {
        Face {
            direction,
            uv: None,
            texture: Some(format!("minecraft:block/{direction:?}")),
            cullface: cull.then_some(direction),
            rotation: 0,
            tintindex: None,
        }
    }

    fn instance(elements: Vec<Element>, x: i32, y: i32, uvlock: bool) -> ModelInstance {
        ModelInstance {
            model: Arc::new(Model { ambient_occlusion: true, elements }),
            x,
            y,
            uvlock,
            weight: 1,
        }
    }

    fn cube(from: [f32; 3], to: [f32; 3]) -> Element {
        Element {
            from,
            to,
            rotation: None,
            shade: true,
            faces: Direction::ALL.iter().map(|&d| face(d, true)).collect(),
        }
    }

    fn quad_facing(quads: &[Quad], dir: Direction) -> &Quad {
        let n = dir.normal();
        let n = Vec3::new(n[0] as f32, n[1] as f32, n[2] as f32);
        quads.iter().find(|q| q.normal.dot(n) > 0.99).unwrap()
    }

    #[test]
    fn full_cube_faces_point_outwards_and_wind_ccw() {
        let quads = bake(&instance(vec![cube([0.0; 3], [16.0; 3])], 0, 0, false));
        assert_eq!(quads.len(), 6);
        for q in &quads {
            let [a, b, c, _] = q.positions;
            let winding = (b - a).cross(c - a).normalize();
            assert!(winding.dot(q.normal) > 0.99, "{q:?}");
            assert!(q.positions.iter().all(|p| p.cmpge(Vec3::ZERO).all() && p.cmple(Vec3::ONE).all()));
            let dir = Direction::from_normal(q.normal.into());
            assert!(q.covers_face(dir));
            assert_eq!(q.cullface, Some(dir));
            assert_eq!(q.uvs, [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]]);
        }
    }

    #[test]
    fn y_rotation_turns_east_to_south() {
        // A slab on the east half, like the top step of an east-facing stair.
        let quads = bake(&instance(vec![cube([8.0, 0.0, 0.0], [16.0, 8.0, 16.0])], 0, 90, false));
        let min = quads.iter().flat_map(|q| q.positions).fold(Vec3::MAX, Vec3::min);
        let max = quads.iter().flat_map(|q| q.positions).fold(Vec3::MIN, Vec3::max);
        assert!(min.abs_diff_eq(Vec3::new(0.0, 0.0, 0.5), 1e-5), "{min}");
        assert!(max.abs_diff_eq(Vec3::new(1.0, 0.5, 1.0), 1e-5), "{max}");
        let south = quad_facing(&quads, Direction::South);
        assert_eq!(south.texture.as_deref(), Some("minecraft:block/East"));
        assert_eq!(south.cullface, Some(Direction::South));
    }

    #[test]
    fn x_rotation_turns_up_to_north() {
        let quads = bake(&instance(vec![cube([0.0; 3], [16.0; 3])], 90, 0, false));
        let north = quad_facing(&quads, Direction::North);
        assert_eq!(north.texture.as_deref(), Some("minecraft:block/Up"));
    }

    #[test]
    fn face_rotation_and_explicit_uv() {
        let mut e = cube([0.0; 3], [16.0; 3]);
        e.faces = vec![Face { uv: Some([0.0, 0.0, 16.0, 8.0]), rotation: 90, ..face(Direction::Up, false) }];
        let quads = bake(&instance(vec![e], 0, 0, false));
        // Rotating 90° clockwise shows the texture's bottom-left at the top-left corner.
        assert_eq!(quads[0].uvs, [[0.0, 0.5], [1.0, 0.5], [1.0, 0.0], [0.0, 0.0]]);
    }

    #[test]
    fn uvlock_keeps_top_texture_world_aligned() {
        let plain = bake(&instance(vec![cube([0.0; 3], [16.0; 3])], 0, 0, false));
        let locked = bake(&instance(vec![cube([0.0; 3], [16.0; 3])], 0, 90, true));
        let (a, b) = (quad_facing(&plain, Direction::Up), quad_facing(&locked, Direction::Up));
        for (p, uv) in b.positions.iter().zip(b.uvs) {
            let i = a.positions.iter().position(|q| q.abs_diff_eq(*p, 1e-5)).unwrap();
            assert!((a.uvs[i][0] - uv[0]).abs() < 1e-5 && (a.uvs[i][1] - uv[1]).abs() < 1e-5);
        }
    }

    #[test]
    fn rescaled_45_degree_cross_spans_the_block() {
        // Like the `cross` model used by flowers and saplings.
        let e = Element {
            from: [0.8, 0.0, 8.0],
            to: [15.2, 16.0, 8.0],
            rotation: Some(ElementRotation { origin: [8.0, 8.0, 8.0], axis: Axis::Y, angle: 45.0, rescale: true }),
            shade: false,
            faces: vec![face(Direction::North, false)],
        };
        let q = &bake(&instance(vec![e], 0, 0, false))[0];
        let xs: Vec<f32> = q.positions.iter().map(|p| p.x).collect();
        let (min, max) = xs.iter().fold((f32::MAX, f32::MIN), |(a, b), &x| (a.min(x), b.max(x)));
        assert!((min - 0.05).abs() < 0.01 && (max - 0.95).abs() < 0.01, "{xs:?}");
        assert!((q.normal.length() - 1.0).abs() < 1e-5 && q.normal.y.abs() < 1e-5);
    }
}
