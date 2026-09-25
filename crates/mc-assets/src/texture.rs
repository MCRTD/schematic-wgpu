//! Block textures: `assets/<ns>/textures/<path>.png`.

/// How a texture's alpha channel should be rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Transparency {
    /// Every pixel is fully opaque.
    Opaque,
    /// Pixels are either fully opaque or fully transparent (leaves, glass).
    Cutout,
    /// Some pixels are partially transparent (stained glass, water, ice).
    Translucent,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Texture {
    pub width: u32,
    pub height: u32,
    /// RGBA8, row-major, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
    pub transparency: Transparency,
}

impl Texture {
    /// Decode a PNG. Animated textures are stored as a vertical strip of
    /// square frames; only the first frame is kept.
    pub fn from_png(bytes: &[u8]) -> image::ImageResult<Self> {
        let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)?.to_rgba8();
        let width = image.width();
        let height = image.height().min(width);
        let mut rgba = image.into_raw();
        rgba.truncate((width * height * 4) as usize);
        Ok(Self::from_rgba(width, height, rgba))
    }

    pub fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        let transparency = classify(&rgba);
        Self { width, height, rgba, transparency }
    }

    /// The magenta and black checkerboard Minecraft shows for missing textures.
    pub fn missing() -> Self {
        let rgba = (0..16 * 16)
            .flat_map(|i| {
                let (x, y) = (i % 16, i / 16);
                if (x < 8) == (y < 8) { [248, 0, 248, 255] } else { [0, 0, 0, 255] }
            })
            .collect();
        Self::from_rgba(16, 16, rgba)
    }
}

fn classify(rgba: &[u8]) -> Transparency {
    let mut result = Transparency::Opaque;
    for a in rgba.chunks_exact(4).map(|p| p[3]) {
        match a {
            255 => {}
            0 => result = result.max(Transparency::Cutout),
            _ => return Transparency::Translucent,
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(width, height, |x, y| image::Rgba(pixel(x, y)));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn classifies_alpha() {
        let opaque = Texture::from_png(&png(4, 4, |_, _| [1, 2, 3, 255])).unwrap();
        let cutout = Texture::from_png(&png(4, 4, |x, _| [1, 2, 3, if x == 0 { 0 } else { 255 }])).unwrap();
        let translucent = Texture::from_png(&png(4, 4, |_, _| [1, 2, 3, 128])).unwrap();
        assert_eq!(opaque.transparency, Transparency::Opaque);
        assert_eq!(cutout.transparency, Transparency::Cutout);
        assert_eq!(translucent.transparency, Transparency::Translucent);
        assert_eq!(Texture::missing().transparency, Transparency::Opaque);
    }

    #[test]
    fn animated_strip_keeps_first_frame() {
        // Two 4x4 frames: red on top, translucent below.
        let t = Texture::from_png(&png(4, 8, |_, y| if y < 4 { [255, 0, 0, 255] } else { [0, 0, 0, 7] })).unwrap();
        assert_eq!((t.width, t.height, t.rgba.len()), (4, 4, 64));
        assert_eq!(t.transparency, Transparency::Opaque);
        assert_eq!(&t.rgba[..4], &[255, 0, 0, 255]);
    }
}
