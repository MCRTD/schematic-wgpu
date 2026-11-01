//! Packs block textures into a single atlas image.

use std::sync::Arc;

use mc_assets::Texture;

/// Each sprite is surrounded by texels copied from its edge, half its size
/// wide, so every sprite sits in a cell twice its size. Cells are aligned to
/// their size, so each mip level down to one texel per cell only ever
/// averages a sprite with its own extruded edge, never a neighbour.
fn padding(t: &Texture) -> u32 {
    (t.width.max(t.height) / 2).max(1)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Top-left corner, in 0..1 atlas UV space.
    pub min: [f32; 2],
    pub size: [f32; 2],
}

impl Rect {
    /// Map a UV in 0..1 sprite space into atlas space.
    pub fn map(&self, uv: [f32; 2]) -> [f32; 2] {
        [self.min[0] + uv[0] * self.size[0], self.min[1] + uv[1] * self.size[1]]
    }
}

#[derive(Debug, Clone)]
pub struct Atlas {
    pub width: u32,
    pub height: u32,
    /// RGBA8, row-major.
    pub rgba: Vec<u8>,
    /// UV rectangle of each input sprite, in input order.
    pub rects: Vec<Rect>,
    /// How many mip levels (including the full-size one) can be generated
    /// without sprites bleeding into each other.
    pub mip_levels: u32,
    /// LabPBR maps of the sprites, if any sprite has one.
    pub pbr: Option<PbrMaps>,
}

/// LabPBR maps laid out like the atlas: same size, same sprite positions.
/// Both are data, not colors (no sRGB).
#[derive(Debug, Clone)]
pub struct PbrMaps {
    /// From `_n` textures: normal X and Y, material ambient occlusion,
    /// height. Flat where a sprite has none.
    pub normal: Vec<u8>,
    /// From `_s` textures: smoothness, F0 or metal, porosity, emission.
    /// Zero (rough, not reflective) where a sprite has none.
    pub specular: Vec<u8>,
}

/// The `_n` and `_s` maps of one sprite.
pub type SpriteMaps = [Option<Arc<Texture>>; 2];

/// A flat normal without occlusion, and no specular response.
const DEFAULT_MAPS: [[u8; 4]; 2] = [[128, 128, 255, 255], [0, 0, 0, 0]];

/// Copy `image` into the padded cell at `(px, py)` that holds `sprite`,
/// scaling it to the sprite's size and extruding its edges into the padding.
fn blit(rgba: &mut [u8], width: u32, (px, py): (u32, u32), sprite: &Texture, image: &Texture) {
    let p = padding(sprite);
    for y in 0..sprite.height + 2 * p {
        for x in 0..sprite.width + 2 * p {
            let sx = x.saturating_sub(p).min(sprite.width - 1) * image.width / sprite.width;
            let sy = y.saturating_sub(p).min(sprite.height - 1) * image.height / sprite.height;
            let src = ((sy * image.width + sx) * 4) as usize;
            let dst = (((py + y) * width + px + x) * 4) as usize;
            rgba[dst..dst + 4].copy_from_slice(&image.rgba[src..src + 4]);
        }
    }
}

impl Atlas {
    /// Shelf-pack sprites, tallest first, into a power-of-two sized image.
    pub fn pack(sprites: &[Arc<Texture>]) -> Atlas {
        Self::pack_with_pbr(sprites, &[])
    }

    /// Like [`Atlas::pack`], also laying out each sprite's LabPBR maps,
    /// given in the same order (missing entries count as none).
    pub fn pack_with_pbr(sprites: &[Arc<Texture>], maps: &[SpriteMaps]) -> Atlas {
        let padded = |t: &Texture| (t.width + 2 * padding(t), t.height + 2 * padding(t));
        let area: u32 = sprites.iter().map(|t| padded(t).0 * padded(t).1).sum();
        let widest = sprites.iter().map(|t| padded(t).0).max().unwrap_or(1);
        let mut width = ((area as f64).sqrt().ceil() as u32).max(widest).next_power_of_two();

        // Largest first: with power-of-two sizes every cell then starts at a
        // multiple of its own size.
        let mut order: Vec<usize> = (0..sprites.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse((padded(&sprites[i]).1, padded(&sprites[i]).0)));

        // Place sprites; if the result is much taller than wide, widen.
        let (positions, height) = loop {
            let mut positions = vec![(0, 0); sprites.len()];
            let (mut x, mut y, mut shelf) = (0, 0, 0);
            for &i in &order {
                let (w, h) = padded(&sprites[i]);
                if x + w > width {
                    (x, y, shelf) = (0, y + shelf, 0);
                }
                positions[i] = (x, y);
                x += w;
                shelf = shelf.max(h);
            }
            let height = (y + shelf).max(1).next_power_of_two();
            if height <= width * 2 {
                break (positions, height);
            }
            width *= 2;
        };

        let mut rgba = vec![0u8; (width * height * 4) as usize];
        let rects = sprites
            .iter()
            .zip(&positions)
            .map(|(t, &position)| {
                blit(&mut rgba, width, position, t, t);
                let (px, py) = position;
                let p = padding(t);
                Rect {
                    min: [(px + p) as f32 / width as f32, (py + p) as f32 / height as f32],
                    size: [t.width as f32 / width as f32, t.height as f32 / height as f32],
                }
            })
            .collect();

        // A cell supports as many halvings as its position and size allow.
        let mip_levels = sprites
            .iter()
            .zip(&positions)
            .map(|(t, &(x, y))| {
                let (w, h) = padded(t);
                [x | w, y | h].iter().map(|&v| if v == 0 { u32::MAX } else { v.trailing_zeros() }).min().unwrap()
            })
            .min()
            .unwrap_or(0)
            .min(width.min(height).trailing_zeros())
            + 1;

        let pbr = maps.iter().any(|m| m.iter().any(Option::is_some)).then(|| {
            let [normal, specular] = [0, 1].map(|k| {
                let mut layer = DEFAULT_MAPS[k].repeat((width * height) as usize);
                for (i, (t, &position)) in sprites.iter().zip(&positions).enumerate() {
                    if let Some(image) = maps.get(i).and_then(|m| m[k].as_ref()) {
                        blit(&mut layer, width, position, t, image);
                    }
                }
                layer
            });
            PbrMaps { normal, specular }
        });

        Atlas { width, height, rgba, rects, mip_levels, pbr }
    }

    /// The atlas with every texel repeated `factor` times in each direction.
    /// UVs are normalized, so they stay valid unchanged. Used for formats
    /// whose readers can't be told to use nearest filtering: with smooth
    /// filtering, the blur then only spans 1/`factor` of a texel.
    pub fn upscaled(&self, factor: u32) -> Atlas {
        let factor = factor.max(1);
        let (width, height) = (self.width * factor, self.height * factor);
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for row in self.rgba.chunks_exact((self.width * 4) as usize) {
            let wide: Vec<u8> = row.chunks_exact(4).flat_map(|px| px.repeat(factor as usize)).collect();
            for _ in 0..factor {
                rgba.extend_from_slice(&wide);
            }
        }
        Atlas {
            width,
            height,
            rgba,
            rects: self.rects.clone(),
            mip_levels: self.mip_levels + factor.ilog2(),
            // Only used by the viewer, which doesn't upscale.
            pbr: None,
        }
    }

    /// Downsampled copies of a [`PbrMaps`] layer for levels `1..mip_levels`:
    /// plain averages, as the channels are data rather than colors.
    pub fn data_mip_chain(&self, layer: &[u8]) -> Vec<(u32, u32, Vec<u8>)> {
        let mut levels: Vec<(u32, u32, Vec<u8>)> = Vec::new();
        let (mut w, mut h) = (self.width, self.height);
        for _ in 1..self.mip_levels {
            let prev = levels.last().map_or(layer, |(_, _, l)| l);
            let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
            let mut next = vec![0u8; (nw * nh * 4) as usize];
            for y in 0..nh {
                for x in 0..nw {
                    let mut sum = [0u32; 4];
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let i = (((y * 2 + dy).min(h - 1) * w + (x * 2 + dx).min(w - 1)) * 4) as usize;
                        for k in 0..4 {
                            sum[k] += prev[i + k] as u32;
                        }
                    }
                    let o = ((y * nw + x) * 4) as usize;
                    for k in 0..4 {
                        next[o + k] = ((sum[k] + 2) / 4) as u8;
                    }
                }
            }
            levels.push((nw, nh, next));
            (w, h) = (nw, nh);
        }
        levels
    }

    /// Downsampled copies of the atlas for levels `1..mip_levels`, each half
    /// the size of the previous. Colors are averaged in linear space and
    /// weighted by alpha so transparent texels don't darken edges.
    pub fn mip_chain(&self) -> Vec<(u32, u32, Vec<u8>)> {
        let to_linear: Vec<f32> = (0..256).map(|v| (v as f32 / 255.0).powf(2.2)).collect();
        let to_srgb = |v: f32| (v.max(0.0).powf(1.0 / 2.2) * 255.0 + 0.5).min(255.0) as u8;

        let mut levels = Vec::new();
        let (mut w, mut h, mut prev) = (self.width, self.height, self.rgba.clone());
        for _ in 1..self.mip_levels {
            let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
            let mut next = vec![0u8; (nw * nh * 4) as usize];
            for y in 0..nh {
                for x in 0..nw {
                    let (mut rgb, mut alpha) = ([0.0f32; 3], 0.0f32);
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let i = (((y * 2 + dy).min(h - 1) * w + (x * 2 + dx).min(w - 1)) * 4) as usize;
                        let a = prev[i + 3] as f32 / 255.0;
                        for k in 0..3 {
                            rgb[k] += to_linear[prev[i + k] as usize] * a;
                        }
                        alpha += a;
                    }
                    let o = ((y * nw + x) * 4) as usize;
                    for k in 0..3 {
                        next[o + k] = if alpha > 0.0 { to_srgb(rgb[k] / alpha) } else { 0 };
                    }
                    next[o + 3] = (alpha / 4.0 * 255.0 + 0.5) as u8;
                }
            }
            levels.push((nw, nh, next.clone()));
            (w, h, prev) = (nw, nh, next);
        }
        levels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(size: u32, color: [u8; 4]) -> Arc<Texture> {
        Arc::new(Texture::from_rgba(size, size, color.repeat((size * size) as usize)))
    }

    fn pixel(atlas: &Atlas, uv: [f32; 2]) -> [u8; 4] {
        let x = (uv[0] * atlas.width as f32) as u32;
        let y = (uv[1] * atlas.height as f32) as u32;
        let i = ((y * atlas.width + x) * 4) as usize;
        atlas.rgba[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn sprites_do_not_overlap_and_keep_their_pixels() {
        let colors: Vec<[u8; 4]> = (0..40u8).map(|i| [i, 255 - i, i / 2, 255]).collect();
        let sprites: Vec<_> = colors
            .iter()
            .enumerate()
            .map(|(i, &c)| solid(if i % 5 == 0 { 32 } else { 16 }, c))
            .collect();
        let atlas = Atlas::pack(&sprites);
        assert!(atlas.width.is_power_of_two() && atlas.height.is_power_of_two());

        for (rect, &color) in atlas.rects.iter().zip(&colors) {
            for uv in [[0.5, 0.5], [0.001, 0.001], [0.999, 0.999]] {
                assert_eq!(pixel(&atlas, rect.map(uv)), color);
            }
        }
        // 16px sprites in 32px cells and 32px sprites in 64px cells: 6 levels
        // (32 -> 1 texel per cell).
        assert_eq!(atlas.mip_levels, 6);
        let texel = [1.0 / atlas.width as f32, 1.0 / atlas.height as f32];
        for (i, a) in atlas.rects.iter().enumerate() {
            for b in &atlas.rects[i + 1..] {
                let texel = [texel[0] * 8.0, texel[1] * 8.0];
                let apart = (0..2).any(|k| {
                    a.min[k] + a.size[k] + texel[k] <= b.min[k] - texel[k] + 1e-6
                        || b.min[k] + b.size[k] + texel[k] <= a.min[k] - texel[k] + 1e-6
                });
                assert!(apart, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn mips_stay_inside_their_cells() {
        let red = solid(16, [255, 0, 0, 255]);
        let blue = solid(16, [0, 0, 255, 255]);
        let atlas = Atlas::pack(&[red, blue]);
        let chain = atlas.mip_chain();
        assert_eq!(chain.len() as u32, atlas.mip_levels - 1);
        for (w, h, rgba) in &chain {
            for (rect, color) in atlas.rects.iter().zip([[255, 0, 0, 255], [0, 0, 255, 255]]) {
                let [u, v] = rect.map([0.5, 0.5]);
                let (x, y) = (((u * *w as f32) as u32).min(w - 1), ((v * *h as f32) as u32).min(h - 1));
                let i = ((y * w + x) * 4) as usize;
                assert_eq!(&rgba[i..i + 4], &color, "level {w}x{h}");
            }
        }
    }

    #[test]
    fn pbr_maps_line_up_with_their_sprites() {
        let sprites = [solid(16, [255, 0, 0, 255]), solid(16, [0, 0, 255, 255])];
        assert!(Atlas::pack(&sprites).pbr.is_none());
        // The first sprite has a 32px normal map (scaled to fit), the second none.
        let maps = [[Some(solid(32, [10, 20, 30, 40])), None], [None, None]];
        let atlas = Atlas::pack_with_pbr(&sprites, &maps);
        let pbr = atlas.pbr.as_ref().unwrap();
        let at = |layer: &[u8], uv: [f32; 2]| {
            let (x, y) = ((uv[0] * atlas.width as f32) as u32, (uv[1] * atlas.height as f32) as u32);
            layer[((y * atlas.width + x) * 4) as usize..][..4].to_vec()
        };
        for uv in [[0.5, 0.5], [0.001, 0.999]] {
            assert_eq!(at(&pbr.normal, atlas.rects[0].map(uv)), [10, 20, 30, 40]);
            assert_eq!(at(&pbr.normal, atlas.rects[1].map(uv)), DEFAULT_MAPS[0]);
            assert_eq!(at(&pbr.specular, atlas.rects[0].map(uv)), DEFAULT_MAPS[1]);
        }
        let chain = atlas.data_mip_chain(&pbr.normal);
        assert_eq!(chain.len() as u32, atlas.mip_levels - 1);
        let (w, h, last) = chain.last().unwrap();
        let [u, v] = atlas.rects[0].map([0.5, 0.5]);
        let i = (((v * *h as f32) as u32 * w + (u * *w as f32) as u32) * 4) as usize;
        assert_eq!(&last[i..i + 4], &[10, 20, 30, 40]);
    }

    #[test]
    fn upscaling_repeats_texels_and_keeps_uvs() {
        let atlas = Atlas::pack(&[solid(16, [255, 0, 0, 255]), solid(16, [0, 0, 255, 255])]);
        let big = atlas.upscaled(4);
        assert_eq!((big.width, big.height), (atlas.width * 4, atlas.height * 4));
        assert_eq!(big.rects, atlas.rects);
        for (rect, color) in big.rects.iter().zip([[255, 0, 0, 255], [0, 0, 255, 255]]) {
            let uv = rect.map([0.0, 0.0]);
            assert_eq!(pixel(&big, [uv[0] + 1e-4, uv[1] + 1e-4]), color);
        }
        let at = |a: &Atlas, x: u32, y: u32| a.rgba[((y * a.width + x) * 4) as usize..][..4].to_vec();
        for (x, y) in [(3, 5), (10, 1)] {
            for (dx, dy) in [(0, 0), (3, 0), (0, 3), (3, 3)] {
                assert_eq!(at(&big, x * 4 + dx, y * 4 + dy), at(&atlas, x, y));
            }
        }
    }
}
