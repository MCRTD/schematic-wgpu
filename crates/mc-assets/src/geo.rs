//! GeckoLib models (`assets/<ns>/geo/<name>.geo.json`, Bedrock geometry).
//!
//! Many mods draw animated blocks with GeckoLib; their block model is then a
//! placeholder, often a cube showing the GeckoLib texture sheet. The
//! geometry is baked the way GeckoLib's `BakedModelFactory` and
//! `GeoRenderer` do it, in its rest pose: X is mirrored, bones and cubes
//! turn around their pivots in Z, Y, X order, and the model stands on the
//! bottom centre of the block.

use std::collections::HashMap;

use glam::{Mat4, Quat, Vec3};
use serde::Deserialize;

use crate::model::Polygon;

#[derive(Deserialize)]
struct GeoFile {
    #[serde(rename = "minecraft:geometry", default)]
    geometry: Vec<Geometry>,
}

#[derive(Deserialize)]
struct Geometry {
    #[serde(default)]
    description: Description,
    #[serde(default)]
    bones: Vec<Bone>,
}

#[derive(Deserialize)]
struct Description {
    #[serde(default = "sixteen")]
    texture_width: f32,
    #[serde(default = "sixteen")]
    texture_height: f32,
}

impl Default for Description {
    fn default() -> Self {
        Description { texture_width: 16.0, texture_height: 16.0 }
    }
}

fn sixteen() -> f32 {
    16.0
}

#[derive(Deserialize)]
struct Bone {
    name: String,
    parent: Option<String>,
    #[serde(default)]
    pivot: [f32; 3],
    #[serde(default)]
    rotation: [f32; 3],
    #[serde(default)]
    mirror: bool,
    inflate: Option<f32>,
    #[serde(rename = "neverRender", default)]
    never_render: bool,
    #[serde(default)]
    cubes: Vec<Cube>,
}

#[derive(Deserialize)]
struct Cube {
    origin: [f32; 3],
    size: [f32; 3],
    #[serde(default)]
    pivot: [f32; 3],
    #[serde(default)]
    rotation: [f32; 3],
    inflate: Option<f32>,
    mirror: Option<bool>,
    #[serde(default)]
    uv: Uv,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Uv {
    Box([f32; 2]),
    Faces(HashMap<String, FaceUv>),
}

impl Default for Uv {
    fn default() -> Self {
        Uv::Box([0.0; 2])
    }
}

#[derive(Deserialize)]
struct FaceUv {
    uv: [f32; 2],
    #[serde(default)]
    uv_size: [f32; 2],
    #[serde(default)]
    uv_rotation: Option<i32>,
}

/// Bedrock rotation in degrees as GeckoLib applies it: X and Y negated,
/// then rotated around Z, Y, X.
fn rotation(r: [f32; 3]) -> Quat {
    Quat::from_rotation_z(r[2].to_radians())
        * Quat::from_rotation_y((-r[1]).to_radians())
        * Quat::from_rotation_x((-r[0]).to_radians())
}

fn about(pivot: [f32; 3], rot: [f32; 3]) -> Mat4 {
    if rot == [0.0; 3] {
        return Mat4::IDENTITY;
    }
    let p = Vec3::new(-pivot[0], pivot[1], pivot[2]) / 16.0;
    Mat4::from_translation(p) * Mat4::from_quat(rotation(rot)) * Mat4::from_translation(-p)
}

/// Parse a `.geo.json` into faces in 0..16 block model space, all with
/// `texture`. Returns `None` if the file isn't Bedrock geometry.
pub(crate) fn parse(data: &[u8], texture: &str) -> Option<Vec<Polygon>> {
    let file: GeoFile = serde_json::from_slice(data).ok()?;
    let geometry = file.geometry.into_iter().next()?;
    let (tw, th) = (geometry.description.texture_width, geometry.description.texture_height);
    let bones: HashMap<&str, &Bone> = geometry.bones.iter().map(|b| (b.name.as_str(), b)).collect();

    // A bone's transform is its parents' then its own; guard against cycles.
    let transform = |bone: &Bone| -> Mat4 {
        let mut chain = vec![bone];
        while let Some(parent) = chain.last().and_then(|b| b.parent.as_deref()).and_then(|p| bones.get(p)) {
            if chain.len() > 64 {
                break;
            }
            chain.push(parent);
        }
        chain.iter().rev().fold(Mat4::IDENTITY, |m, b| m * about(b.pivot, b.rotation))
    };
    // Model space has its origin on the bottom centre of the block.
    let to_block = Mat4::from_translation(Vec3::new(0.5, 0.0, 0.5));

    let mut polygons = Vec::new();
    for bone in &geometry.bones {
        if bone.never_render {
            continue;
        }
        let bone_m = to_block * transform(bone);
        for cube in &bone.cubes {
            let m = bone_m * about(cube.pivot, cube.rotation);
            let mirror = cube.mirror.unwrap_or(bone.mirror);
            let inflate = cube.inflate.or(bone.inflate).unwrap_or(0.0) / 16.0;
            let size = Vec3::from(cube.size) / 16.0;
            let min = Vec3::new(-(cube.origin[0] + cube.size[0]), cube.origin[1], cube.origin[2]) / 16.0 - inflate;
            let max = min + size + 2.0 * inflate;
            let v = |x: bool, y: bool, z: bool| {
                Vec3::new(if x { max.x } else { min.x }, if y { max.y } else { min.y }, if z { max.z } else { min.z })
            };
            // GeckoLib's `VertexSet`: "left/right" is Z, "back/front" is X.
            let (blb, brb, tlb, trb) = (v(false, false, false), v(false, false, true), v(false, true, false), v(false, true, true));
            let (tlf, trf, blf, brf) = (v(true, true, false), v(true, true, true), v(true, false, false), v(true, false, true));
            let west = [trb, tlb, blb, brb];
            let east = [tlf, trf, brf, blf];
            let up = [trb, trf, tlf, tlb];
            let down = [blb, blf, brf, brb];
            let box_uv = matches!(cube.uv, Uv::Box(_));
            let faces = [
                ("west", if mirror { east } else { west }),
                ("east", if mirror { west } else { east }),
                ("north", [tlb, tlf, blf, blb]),
                ("south", [trf, trb, brb, brf]),
                ("up", if mirror && !box_uv { down } else { up }),
                ("down", if mirror && !box_uv { up } else { down }),
            ];
            for (name, corners) in faces {
                let (uv, uv_size, turn) = match &cube.uv {
                    Uv::Faces(map) => match map.get(name) {
                        Some(f) => (f.uv, f.uv_size, f.uv_rotation.unwrap_or(0)),
                        None => continue,
                    },
                    Uv::Box([u, w]) => {
                        let [sx, sy, sz] = cube.size.map(f32::floor);
                        let (at, size) = match name {
                            "west" => ([u + sz + sx, w + sz], [sz, sy]),
                            "east" => ([*u, w + sz], [sz, sy]),
                            "north" => ([u + sz, w + sz], [sx, sy]),
                            "south" => ([u + sz + sx + sz, w + sz], [sx, sy]),
                            "up" => ([u + sz, *w], [sx, sz]),
                            _ => ([u + sz + sx, w + sz], [sx, -sz]),
                        };
                        (at, size, 0)
                    }
                };
                // `GeoQuad.build`: U is flipped unless mirrored.
                let (mut u0, mut u1) = (uv[0] / tw, (uv[0] + uv_size[0]) / tw);
                let (v0, v1) = (uv[1] / th, (uv[1] + uv_size[1]) / th);
                if !mirror {
                    std::mem::swap(&mut u0, &mut u1);
                }
                let uvs = match turn.rem_euclid(360) {
                    90 => [[u1, v0], [u1, v1], [u0, v1], [u0, v0]],
                    180 => [[u1, v1], [u0, v1], [u0, v0], [u1, v0]],
                    270 => [[u0, v1], [u0, v0], [u1, v0], [u1, v1]],
                    _ => [[u0, v0], [u1, v0], [u1, v1], [u0, v1]],
                };
                polygons.push(Polygon {
                    positions: corners.map(|c| (m.transform_point3(c) * 16.0).into()),
                    uvs: uvs.map(|[u, v]| [u * 16.0, v * 16.0]),
                    texture: Some(texture.to_owned()),
                });
            }
        }
    }
    Some(polygons)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_stands_on_the_block_centre_with_x_mirrored() {
        let geo = br#"{"format_version": "1.12.0", "minecraft:geometry": [{
            "description": {"texture_width": 32, "texture_height": 32},
            "bones": [{"name": "root", "pivot": [0, 0, 0], "cubes": [
                {"origin": [2, 0, -3], "size": [4, 6, 6], "uv": {"north": {"uv": [0, 0], "uv_size": [4, 6]}}}
            ]}]}]}"#;
        let polygons = parse(geo, "mod:block/thing").unwrap();
        assert_eq!(polygons.len(), 1);
        let p = &polygons[0];
        let xs: Vec<f32> = p.positions.iter().map(|c| c[0]).collect();
        // Bedrock x 2..6 becomes -6..-2, i.e. 2..6 px left of centre (8).
        assert!(xs.iter().all(|&x| (2.0..=6.0).contains(&x)), "{xs:?}");
        // The north face sits on z = -3 px from the centre.
        assert!(p.positions.iter().all(|c| (c[2] - 5.0).abs() < 1e-4));
        // UVs scale from the 32 px sheet to 0..16, with U flipped.
        assert_eq!(p.uvs[0], [2.0, 0.0]);
        assert_eq!(p.uvs[2], [0.0, 3.0]);
        assert_eq!(p.texture.as_deref(), Some("mod:block/thing"));
    }

    #[test]
    fn child_bones_turn_with_their_parent() {
        let geo = br#"{"minecraft:geometry": [{"bones": [
            {"name": "a", "pivot": [0, 8, 0], "rotation": [0, 0, 90]},
            {"name": "b", "parent": "a", "pivot": [0, 8, 0], "cubes": [{"origin": [0, 8, 0], "size": [1, 1, 1], "uv": [0, 0]}]}
        ]}]}"#;
        let polygons = parse(geo, "t").unwrap();
        assert_eq!(polygons.len(), 6);
        // Turned 90° around Z at y = 8: the cube now hangs below the pivot.
        let ys: Vec<f32> = polygons.iter().flat_map(|p| p.positions.map(|c| c[1])).collect();
        assert!(ys.iter().all(|&y| y <= 8.0 + 1e-4), "{ys:?}");
    }
}
