//! Draws a [`Mesh`] with wgpu. Independent of windows, so it works for the
//! native viewer, headless screenshots and the browser alike.

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use std::ops::Range;

use mesher::{Atlas, Mesh, MeshLayer, Transparency};
use wgpu::util::DeviceExt;

use crate::camera::OrbitCamera;

pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
pub const SAMPLE_COUNT: u32 = 4;
const SHADOW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const CLEAR: wgpu::Color = wgpu::Color { r: 0.009, g: 0.011, b: 0.015, a: 1.0 };

/// Times of day with a [`Lighting`] preset each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeOfDay {
    Day,
    Sunset,
    /// Moonlight, with light sources standing out.
    Night,
}

impl TimeOfDay {
    pub const ALL: [TimeOfDay; 3] = [TimeOfDay::Day, TimeOfDay::Sunset, TimeOfDay::Night];

    pub fn name(self) -> &'static str {
        match self {
            TimeOfDay::Day => "day",
            TimeOfDay::Sunset => "sunset",
            TimeOfDay::Night => "night",
        }
    }
}

impl std::str::FromStr for TimeOfDay {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|t| t.name().eq_ignore_ascii_case(s))
            .ok_or_else(|| format!("unknown time of day `{s}` (use day, sunset or night)"))
    }
}

/// How the scene is lit and what surrounds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Lighting {
    /// Direction towards the sun (or moon), +Y up; normalised when used.
    pub sun_direction: [f32; 3],
    /// Color of direct sun- or moonlight, linear, 1 = full brightness.
    pub sunlight: [f32; 3],
    /// Color of the sky's light in shadow, linear.
    pub skylight: [f32; 3],
    /// Color of light from torches and other light sources at full
    /// strength ([`mesher::Vertex::block_light`] 15), linear. It adds to
    /// the other light, so it shows mostly at night.
    pub block_light: [f32; 3],
    /// Cast shadows from the sun.
    pub shadows: bool,
    /// Width and height of the shadow map in texels.
    pub shadow_map_size: u32,
    /// How dark fully shadowed surfaces get, 0..1.
    pub shadow_strength: f32,
    /// How strongly [`mesher::Vertex::ao`] darkens corners, 0..1.
    pub ambient_occlusion: f32,
    /// Show the shadow the build casts on the ground beneath it.
    pub ground: bool,
    /// Background gradient, sRGB.
    pub sky_top: [f32; 3],
    pub sky_bottom: [f32; 3],
    /// Use the LabPBR normal and specular maps of resource packs, when the
    /// mesh has them (see [`mesher::Options::pbr_maps`]).
    pub pbr: bool,
}

impl Default for Lighting {
    /// Daytime.
    fn default() -> Self {
        Self {
            sun_direction: [-0.32, 1.0, 0.65],
            sunlight: [1.0, 1.0, 1.0],
            skylight: [0.46, 0.52, 0.64],
            block_light: [1.0, 0.82, 0.6],
            shadows: true,
            shadow_map_size: 2048,
            shadow_strength: 1.0,
            ambient_occlusion: 1.0,
            ground: true,
            sky_top: [0.47, 0.65, 0.88],
            sky_bottom: [0.86, 0.9, 0.95],
            pbr: true,
        }
    }
}

impl Lighting {
    pub fn at(time: TimeOfDay) -> Self {
        let mut lighting = Self::default();
        lighting.set_time(time);
        lighting
    }

    /// Change the sun, sky and light colors to `time`'s, keeping the rest.
    pub fn set_time(&mut self, time: TimeOfDay) {
        let day = Self::default();
        (self.sun_direction, self.sunlight, self.skylight, self.sky_top, self.sky_bottom) = match time {
            TimeOfDay::Day => (day.sun_direction, day.sunlight, day.skylight, day.sky_top, day.sky_bottom),
            TimeOfDay::Sunset => ([-1.0, 0.22, 0.3], [1.0, 0.66, 0.42], [0.32, 0.3, 0.42], [0.3, 0.4, 0.66], [0.98, 0.7, 0.48]),
            TimeOfDay::Night => ([0.35, 1.0, -0.45], [0.13, 0.16, 0.26], [0.035, 0.045, 0.08], [0.015, 0.02, 0.06], [0.06, 0.08, 0.16]),
        };
    }
}

/// Packed to 28 bytes: large builds put hundreds of megabytes of these on the GPU.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuVertex {
    position: [f32; 3],
    /// xyz: normal; w: block light * 2, plus 1 for slice caps (see
    /// `unpack_flags` in shader.wgsl).
    normal: [i8; 4],
    /// Atlas UV in 1/65535 steps.
    uv: [u16; 2],
    /// rgb: tint; a: ambient occlusion.
    color: [u8; 4],
    block_y: f32,
}

const _: () = assert!(size_of::<GpuVertex>() == 28);

impl GpuVertex {
    fn new(v: &mesher::Vertex, cap: bool) -> Self {
        let snorm = |c: f32| (c.clamp(-1.0, 1.0) * 127.0).round() as i8;
        let unorm16 = |c: f32| (c.clamp(0.0, 1.0) * 65535.0).round() as u16;
        let [r, g, b, _] = v.color;
        // Block light and the cap flag share the spare normal byte.
        let flags = (v.block_light.min(15) * 2 + cap as u8) as i8;
        GpuVertex {
            position: v.position,
            normal: [snorm(v.normal[0]), snorm(v.normal[1]), snorm(v.normal[2]), flags],
            uv: [unorm16(v.uv[0]), unorm16(v.uv[1])],
            color: [r, g, b, (v.ao.clamp(0.0, 1.0) * 255.0).round() as u8],
            block_y: v.block_y as f32,
        }
    }
}

/// Mirrors `Globals` in shader.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    view_proj: [f32; 16],
    light_view_proj: [f32; 16],
    sun: [f32; 4],
    sky_top: [f32; 4],
    sky_bottom: [f32; 4],
    ground: [f32; 4],
    params: [f32; 4],
    slice: [f32; 4],
    camera: [f32; 4],
    sunlight: [f32; 4],
    skylight: [f32; 4],
    block_light: [f32; 4],
}

struct GpuLayer {
    transparency: Transparency,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
}

/// A 2D texture with the given mip levels (largest first), as a view.
fn upload(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    format: wgpu::TextureFormat,
    levels: impl Iterator<Item = (u32, u32, Vec<u8>)>,
) -> wgpu::TextureView {
    let levels: Vec<_> = levels.collect();
    let (width, height, _) = levels[0];
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: levels.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (level, (width, height, rgba)) in levels.iter().enumerate() {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: level as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width * 4), rows_per_image: None },
            wgpu::Extent3d { width: *width, height: *height, depth_or_array_layers: 1 },
        );
    }
    texture.create_view(&Default::default())
}

/// Split a layer into runs of whole triangles whose vertex and index buffers
/// each fit in `max_bytes`, as `(vertices, indices)` ranges. Large builds
/// exceed the 256 MB a single buffer may hold on the web. Meshes from the
/// mesher keep each quad's vertices together, so the runs stay compact.
fn chunks(layer: &MeshLayer, max_bytes: u64) -> Vec<(Range<usize>, Range<usize>)> {
    let max_vertices = (max_bytes / size_of::<GpuVertex>() as u64).max(3) as usize;
    let max_indices = (max_bytes / size_of::<u32>() as u64).max(3) as usize;
    let mut runs = Vec::new();
    let (mut start, mut min, mut max) = (0, usize::MAX, 0);
    for (i, triangle) in layer.indices.chunks_exact(3).enumerate() {
        let (lo, hi) = triangle.iter().fold((min, max), |(lo, hi), &v| (lo.min(v as usize), hi.max(v as usize)));
        let end = i * 3;
        if end > start && (hi - lo >= max_vertices || end + 3 - start > max_indices) {
            runs.push((min..max + 1, start..end));
            start = end;
            (min, max) = triangle.iter().fold((usize::MAX, 0), |(lo, hi), &v| (lo.min(v as usize), hi.max(v as usize)));
        } else {
            (min, max) = (lo, hi);
        }
    }
    if start < layer.indices.len() {
        runs.push((min..max + 1, start..layer.indices.len()));
    }
    runs
}

/// Render targets that depend on the output size.
struct Targets {
    size: (u32, u32),
    msaa: wgpu::TextureView,
    depth: wgpu::TextureView,
}

struct ShadowMap {
    size: u32,
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
}

pub struct Renderer {
    format: wgpu::TextureFormat,
    pipelines: [wgpu::RenderPipeline; 3],
    /// Depth-only pipelines for opaque and cutout geometry seen from the sun.
    shadow_pipelines: [wgpu::RenderPipeline; 2],
    sky_pipeline: wgpu::RenderPipeline,
    ground_pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    shadow_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    shadow_sampler: wgpu::Sampler,
    globals: wgpu::Buffer,
    bind_group: Option<wgpu::BindGroup>,
    shadow: Option<ShadowMap>,
    layers: Vec<GpuLayer>,
    targets: Option<Targets>,
    lighting: Lighting,
    /// World-space bounds of the current mesh.
    bounds: Option<[Vec3; 2]>,
    /// Sun view-projection and shadow map texel size in world units.
    light: (Mat4, f32),
    /// Topmost block layer shown, see [`Renderer::set_slice`].
    slice: Option<i32>,
    /// Whether the current atlas has LabPBR maps.
    pbr_maps: bool,
}

impl Renderer {
    /// `format` is the color format of the views passed to [`Renderer::render`].
    /// It should be an sRGB format so output is gamma encoded.
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("shader.wgsl"));
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                multisampled: false,
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            count: None,
        };
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(size_of::<Globals>() as u64),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // LabPBR normal and specular maps.
                texture_entry(3),
                texture_entry(4),
            ],
        });
        let shadow_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow map"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("schematic"),
            bind_group_layouts: &[Some(&bind_group_layout), Some(&shadow_layout)],
            immediate_size: 0,
        });
        let shadow_pass_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow pass"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let attributes =
            wgpu::vertex_attr_array![0 => Float32x3, 1 => Snorm8x4, 2 => Unorm16x2, 3 => Unorm8x4, 4 => Float32];
        let vertex_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: size_of::<GpuVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &attributes,
        })];
        let depth = |write: bool, compare: wgpu::CompareFunction| wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(write),
            depth_compare: Some(compare),
            stencil: Default::default(),
            bias: Default::default(),
        };
        let multisample = wgpu::MultisampleState { count: SAMPLE_COUNT, ..Default::default() };

        let pipeline = |transparency: Transparency| {
            let (entry, blend, cull, depth_write) = match transparency {
                Transparency::Opaque => ("fs_opaque", None, Some(wgpu::Face::Back), true),
                // Plants, rails and redstone are single planes seen from both sides.
                Transparency::Cutout => ("fs_cutout", None, None, true),
                Transparency::Translucent => ("fs_translucent", Some(wgpu::BlendState::ALPHA_BLENDING), None, false),
            };
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &vertex_buffers,
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
                }),
                primitive: wgpu::PrimitiveState { cull_mode: cull, ..Default::default() },
                depth_stencil: Some(depth(depth_write, wgpu::CompareFunction::LessEqual)),
                multisample,
                multiview_mask: None,
                cache: None,
            })
        };
        let pipelines = [
            pipeline(Transparency::Opaque),
            pipeline(Transparency::Cutout),
            pipeline(Transparency::Translucent),
        ];

        let shadow_pipeline = |fragment: Option<&str>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("shadow"),
                layout: Some(&shadow_pass_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_shadow"),
                    compilation_options: Default::default(),
                    buffers: &vertex_buffers,
                },
                fragment: fragment.map(|entry| wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[],
                }),
                // Many block models are open shells, so both sides must cast shadows.
                primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: SHADOW_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: Default::default(),
                    bias: wgpu::DepthBiasState { constant: 2, slope_scale: 2.0, clamp: 0.0 },
                }),
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let shadow_pipelines = [shadow_pipeline(None), shadow_pipeline(Some("fs_shadow_cutout"))];

        let fullscreen = |label, vertex, fragment, blend, depth_state| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vertex),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fragment),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
                }),
                primitive: Default::default(),
                depth_stencil: Some(depth_state),
                multisample,
                multiview_mask: None,
                cache: None,
            })
        };
        let sky_pipeline = fullscreen("sky", "vs_sky", "fs_sky", None, depth(false, wgpu::CompareFunction::Always));
        let ground_pipeline = fullscreen(
            "ground",
            "vs_ground",
            "fs_ground",
            Some(wgpu::BlendState::ALPHA_BLENDING),
            depth(false, wgpu::CompareFunction::LessEqual),
        );

        // Nearest filtering keeps the pixel-art look.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            // Blending between mip levels avoids shimmering on distant surfaces.
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        // Linear comparison filtering gives each PCF tap a soft 2x2 edge.
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shadow"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            format,
            pipelines,
            shadow_pipelines,
            sky_pipeline,
            ground_pipeline,
            bind_group_layout,
            shadow_layout,
            sampler,
            shadow_sampler,
            globals,
            bind_group: None,
            shadow: None,
            layers: Vec::new(),
            targets: None,
            lighting: Lighting::default(),
            bounds: None,
            light: (Mat4::IDENTITY, 0.0),
            slice: None,
            pbr_maps: false,
        }
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    pub fn lighting(&self) -> &Lighting {
        &self.lighting
    }

    /// Change the lighting; the shadow map is redrawn if needed.
    pub fn set_lighting(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, lighting: Lighting) {
        if lighting != self.lighting {
            self.lighting = lighting;
            self.update_shadow_map(device, queue);
        }
    }

    pub fn slice(&self) -> Option<i32> {
        self.slice
    }

    /// Show the build layer by layer, like a 3D printing slicer: blocks
    /// above world Y `top` are hidden; `None` shows everything. Meshes built
    /// with [`mesher::Options::slice_caps`] keep the cut layer looking solid.
    pub fn set_slice(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, top: Option<i32>) {
        if top != self.slice {
            self.slice = top;
            self.update_shadow_map(device, queue);
        }
    }

    /// Upload a mesh, replacing the previous one.
    pub fn set_mesh(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, mesh: &Mesh) {
        let Some(atlas) = &mesh.atlas else {
            self.layers.clear();
            self.bind_group = None;
            self.bounds = None;
            return;
        };
        self.begin_mesh(device, queue, atlas);
        self.add_mesh(device, mesh);
        self.finish_mesh(device, queue, mesh.bounds);
    }

    /// Start replacing the mesh with one that arrives in pieces, for builds
    /// too large to hold in memory at once (see [`mesher::Build`]). Add the
    /// pieces with [`Renderer::add_mesh`], then call [`Renderer::finish_mesh`].
    /// Pieces already added are drawn in the meantime, without shadows.
    pub fn begin_mesh(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, atlas: &Atlas) {
        self.layers.clear();
        self.bounds = None;

        let base = (atlas.width, atlas.height, atlas.rgba.clone());
        let view = upload(device, queue, "atlas", wgpu::TextureFormat::Rgba8UnormSrgb, std::iter::once(base).chain(atlas.mip_chain()));
        // Without maps, 1x1 stand-ins keep the bind group valid.
        let [normal, specular] = match &atlas.pbr {
            Some(pbr) => [("normal map", &pbr.normal), ("specular map", &pbr.specular)].map(|(label, layer)| {
                let base = (atlas.width, atlas.height, layer.clone());
                upload(device, queue, label, wgpu::TextureFormat::Rgba8Unorm, std::iter::once(base).chain(atlas.data_mip_chain(layer)))
            }),
            None => [("normal map", [128, 128, 255, 255]), ("specular map", [0; 4])].map(|(label, texel)| {
                upload(device, queue, label, wgpu::TextureFormat::Rgba8Unorm, std::iter::once((1, 1, texel.to_vec())))
            }),
        };
        self.pbr_maps = atlas.pbr.is_some();
        self.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.globals.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&normal) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&specular) },
            ],
        }));
        // Without bounds this only makes sure a (placeholder) shadow map exists.
        self.update_shadow_map(device, queue);
    }

    /// Upload the geometry of one piece of the mesh; its `atlas` and
    /// `bounds` are ignored. The piece can be dropped afterwards.
    pub fn add_mesh(&mut self, device: &wgpu::Device, mesh: &Mesh) {
        let max_bytes = device.limits().max_buffer_size;
        let caps = mesh.caps().map(|(t, layer)| (t, layer, true));
        for (transparency, layer, cap) in mesh.layers().map(|(t, layer)| (t, layer, false)).into_iter().chain(caps) {
            for (vertex_range, index_range) in chunks(layer, max_bytes) {
                let base = vertex_range.start as u32;
                let vertices: Vec<GpuVertex> = layer.vertices[vertex_range].iter().map(|v| GpuVertex::new(v, cap)).collect();
                let indices: Vec<u32> = layer.indices[index_range].iter().map(|i| i - base).collect();
                self.layers.push(GpuLayer {
                    transparency,
                    vertices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("vertices"),
                        contents: bytemuck::cast_slice(&vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    }),
                    indices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("indices"),
                        contents: bytemuck::cast_slice(&indices),
                        usage: wgpu::BufferUsages::INDEX,
                    }),
                    count: indices.len() as u32,
                });
            }
        }
    }

    /// All pieces are in: `bounds` (`[min, max]`) of the whole mesh. Draws
    /// the shadow map.
    pub fn finish_mesh(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, bounds: [[f32; 3]; 2]) {
        self.bounds = (!self.layers.is_empty()).then(|| [Vec3::from(bounds[0]), Vec3::from(bounds[1])]);
        self.update_shadow_map(device, queue);
    }

    fn shadows_enabled(&self) -> bool {
        self.lighting.shadows && self.bounds.is_some() && self.bind_group.is_some()
    }

    /// Fit an orthographic sun camera around the mesh and redraw the shadow map.
    /// The sun is fixed in world space, so this only runs when the mesh or
    /// lighting changes, not per frame.
    fn update_shadow_map(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let enabled = self.shadows_enabled();
        // A 1x1 map keeps the bind group valid when shadows are off.
        let size = if enabled { self.lighting.shadow_map_size.clamp(1, device.limits().max_texture_dimension_2d) } else { 1 };
        if self.shadow.as_ref().is_none_or(|s| s.size != size) {
            let view = device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("shadow map"),
                    size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: SHADOW_FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&Default::default());
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("shadow map"),
                layout: &self.shadow_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.shadow_sampler) },
                ],
            });
            self.shadow = Some(ShadowMap { size, view, bind_group });
        }
        let (Some([min, max]), Some(bind_group), Some(shadow)) = (self.bounds, &self.bind_group, &self.shadow) else {
            return;
        };
        if !enabled {
            return;
        }

        let center = (min + max) / 2.0;
        let radius = ((max - min).length() / 2.0).max(1.0);
        let sun = self.sun();
        let up = if sun.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
        let eye = center + sun * radius * 2.0;
        let view = glam::camera::rh::view::look_at_mat4(eye, center, up);
        // Deep enough to reach the ground shadow even with a low sun.
        let proj = glam::camera::rh::proj::directx::orthographic(-radius, radius, -radius, radius, 0.0, radius * 8.0);
        self.light = (proj * view, 2.0 * radius / size as f32);
        queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&self.globals_data(Mat4::IDENTITY, Vec3::ZERO)));

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("shadow map") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shadow map"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &shadow.view,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, bind_group, &[]);
            // Translucent blocks (glass, water) let the light through.
            for layer in &self.layers {
                let pipeline = match layer.transparency {
                    Transparency::Opaque => &self.shadow_pipelines[0],
                    Transparency::Cutout => &self.shadow_pipelines[1],
                    Transparency::Translucent => continue,
                };
                pass.set_pipeline(pipeline);
                pass.set_vertex_buffer(0, layer.vertices.slice(..));
                pass.set_index_buffer(layer.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..layer.count, 0, 0..1);
            }
        }
        queue.submit([encoder.finish()]);
    }

    fn sun(&self) -> Vec3 {
        Vec3::from(self.lighting.sun_direction).try_normalize().unwrap_or(Vec3::Y)
    }

    fn globals_data(&self, view_proj: Mat4, eye: Vec3) -> Globals {
        let l = &self.lighting;
        let linear = |c: [f32; 3]| [c[0].powf(2.2), c[1].powf(2.2), c[2].powf(2.2), 1.0];
        let sun = self.sun();
        let ground = match self.bounds {
            Some([min, max]) if l.ground => {
                let center = (min + max) / 2.0;
                let radius = (max.x - min.x).max(max.z - min.z) * 0.9 + 4.0;
                [center.x, min.y, center.z, radius]
            }
            _ => [0.0; 4],
        };
        let shadow_size = self.shadow.as_ref().map_or(1, |s| s.size) as f32;
        Globals {
            view_proj: view_proj.to_cols_array(),
            light_view_proj: self.light.0.to_cols_array(),
            sun: [sun.x, sun.y, sun.z, if self.shadows_enabled() { 1.0 } else { 0.0 }],
            sky_top: linear(l.sky_top),
            sky_bottom: linear(l.sky_bottom),
            ground,
            params: [l.ambient_occlusion, l.shadow_strength, self.light.1, 1.0 / shadow_size],
            slice: [self.slice.map_or(f32::MAX, |y| y as f32), 0.0, 0.0, 0.0],
            camera: [eye.x, eye.y, eye.z, if l.pbr && self.pbr_maps { 1.0 } else { 0.0 }],
            sunlight: [l.sunlight[0], l.sunlight[1], l.sunlight[2], 0.0],
            skylight: [l.skylight[0], l.skylight[1], l.skylight[2], 0.0],
            block_light: [l.block_light[0], l.block_light[1], l.block_light[2], 0.0],
        }
    }

    fn targets(&mut self, device: &wgpu::Device, width: u32, height: u32) -> &Targets {
        if self.targets.as_ref().is_none_or(|t| t.size != (width, height)) {
            let texture = |label, format| {
                device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some(label),
                        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                        mip_level_count: 1,
                        sample_count: SAMPLE_COUNT,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                        view_formats: &[],
                    })
                    .create_view(&Default::default())
            };
            self.targets = Some(Targets {
                size: (width, height),
                msaa: texture("msaa", self.format),
                depth: texture("depth", DEPTH_FORMAT),
            });
        }
        self.targets.as_ref().unwrap()
    }

    /// Draw into `target`, a `width` x `height` view in [`Renderer::format`].
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        (width, height): (u32, u32),
        camera: &OrbitCamera,
    ) {
        let view_proj = camera.view_proj(width as f32 / height.max(1) as f32);
        queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&self.globals_data(view_proj, camera.eye())));

        self.targets(device, width, height);
        let targets = self.targets.as_ref().unwrap();
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("schematic"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &targets.msaa,
                depth_slice: None,
                resolve_target: Some(target),
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(CLEAR), store: wgpu::StoreOp::Discard },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &targets.depth,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        let (Some(bind_group), Some(shadow)) = (&self.bind_group, &self.shadow) else { return };
        pass.set_bind_group(0, bind_group, &[]);
        pass.set_bind_group(1, &shadow.bind_group, &[]);

        pass.set_pipeline(&self.sky_pipeline);
        pass.draw(0..3, 0..1);

        let draw = |pass: &mut wgpu::RenderPass, layer: &GpuLayer| {
            pass.set_pipeline(&self.pipelines[layer.transparency as usize]);
            pass.set_vertex_buffer(0, layer.vertices.slice(..));
            pass.set_index_buffer(layer.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..layer.count, 0, 0..1);
        };
        // Opaque and cutout first, then the ground so it blends over the sky
        // but stays hidden behind blocks, then translucent layers last.
        for layer in self.layers.iter().filter(|l| l.transparency != Transparency::Translucent) {
            draw(&mut pass, layer);
        }
        if self.lighting.ground && self.bounds.is_some() {
            pass.set_pipeline(&self.ground_pipeline);
            pass.draw(0..6, 0..1);
        }
        for layer in self.layers.iter().filter(|l| l.transparency == Transparency::Translucent) {
            draw(&mut pass, layer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesher::Vertex;

    fn quads(n: u32) -> MeshLayer {
        let v = Vertex { position: [0.0; 3], normal: [0.0; 3], uv: [0.0; 2], color: [255; 4], ao: 1.0, block_y: 0, block_light: 0 };
        let indices = (0..n).flat_map(|q| [0, 1, 2, 0, 2, 3].map(|i| q * 4 + i)).collect();
        MeshLayer { vertices: vec![v; n as usize * 4], indices }
    }

    #[test]
    fn small_layers_are_one_chunk() {
        assert_eq!(chunks(&quads(10), 1 << 28), [(0..40, 0..60)]);
        assert!(chunks(&MeshLayer::default(), 1 << 28).is_empty());
    }

    #[test]
    fn large_layers_split_between_quads_within_the_limit() {
        // Room for 10 vertices: two quads per chunk.
        let max_bytes = 10 * size_of::<GpuVertex>() as u64;
        let runs = chunks(&quads(5), max_bytes);
        assert_eq!(runs, [(0..8, 0..12), (8..16, 12..24), (16..20, 24..30)]);
    }
}
