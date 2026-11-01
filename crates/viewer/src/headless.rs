//! Offscreen rendering to an RGBA image, for screenshots and thumbnails.

use mesher::Mesh;

use crate::{Lighting, OrbitCamera, Renderer};

#[derive(Debug)]
pub enum Error {
    NoAdapter(wgpu::RequestAdapterError),
    NoDevice(wgpu::RequestDeviceError),
    Readback(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoAdapter(e) => write!(f, "no GPU adapter: {e}"),
            Error::NoDevice(e) => write!(f, "could not open GPU device: {e}"),
            Error::Readback(e) => write!(f, "could not read back the image: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// Render `mesh` to tightly packed RGBA8 pixels (sRGB), row-major, with the
/// default [`Lighting`].
pub async fn render(mesh: &Mesh, camera: &OrbitCamera, width: u32, height: u32) -> Result<Vec<u8>, Error> {
    render_with_lighting(mesh, camera, width, height, &Lighting::default()).await
}

/// Like [`render`], with custom lighting.
pub async fn render_with_lighting(
    mesh: &Mesh,
    camera: &OrbitCamera,
    width: u32,
    height: u32,
    lighting: &Lighting,
) -> Result<Vec<u8>, Error> {
    render_sliced(mesh, camera, width, height, lighting, None).await
}

/// Like [`render_with_lighting`], showing only the blocks up to world Y
/// `slice`, see [`Renderer::set_slice`].
pub async fn render_sliced(
    mesh: &Mesh,
    camera: &OrbitCamera,
    width: u32,
    height: u32,
    lighting: &Lighting,
    slice: Option<i32>,
) -> Result<Vec<u8>, Error> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions::default())
        .await
        .map_err(Error::NoAdapter)?;
    let (device, queue) = adapter.request_device(&Default::default()).await.map_err(Error::NoDevice)?;

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = Renderer::new(&device, format);
    // Set lighting and slice before the mesh so the shadow map is only drawn once.
    renderer.set_lighting(&device, &queue, lighting.clone());
    renderer.set_slice(&device, &queue, slice);
    renderer.set_mesh(&device, &queue, mesh);

    let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("screenshot"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());

    // Buffer rows must be aligned to 256 bytes.
    let row = width * 4;
    let padded_row = row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (padded_row * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.render(&device, &queue, &mut encoder, &view, (width, height), camera);
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded_row), rows_per_image: None },
        },
        size,
    );
    queue.submit([encoder.finish()]);

    let (tx, rx) = std::sync::mpsc::channel();
    readback.map_async(wgpu::MapMode::Read, .., move |result| {
        let _ = tx.send(result);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| Error::Readback(e.to_string()))?;
    rx.recv()
        .map_err(|e| Error::Readback(e.to_string()))?
        .map_err(|e| Error::Readback(e.to_string()))?;

    let mapped = readback.get_mapped_range(..).map_err(|e| Error::Readback(e.to_string()))?;
    let mut pixels = Vec::with_capacity((row * height) as usize);
    for y in 0..height as usize {
        let start = y * padded_row as usize;
        pixels.extend_from_slice(&mapped[start..start + row as usize]);
    }
    Ok(pixels)
}
