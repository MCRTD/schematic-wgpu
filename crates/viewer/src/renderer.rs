//! Draws a [`Mesh`] with wgpu. Independent of windows, so it works for the
//! native viewer, headless screenshots and the browser alike.

use bytemuck::{Pod, Zeroable};
use mesher::{Mesh, Transparency};
use wgpu::util::DeviceExt;

use crate::camera::OrbitCamera;

pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
pub const SAMPLE_COUNT: u32 = 4;
const CLEAR: wgpu::Color = wgpu::Color { r: 0.009, g: 0.011, b: 0.015, a: 1.0 };

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuVertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    color: [u8; 4],
}

struct GpuLayer {
    transparency: Transparency,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
}

/// Render targets that depend on the output size.
struct Targets {
    size: (u32, u32),
    msaa: wgpu::TextureView,
    depth: wgpu::TextureView,
}

pub struct Renderer {
    format: wgpu::TextureFormat,
    pipelines: [wgpu::RenderPipeline; 3],
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    globals: wgpu::Buffer,
    bind_group: Option<wgpu::BindGroup>,
    layers: Vec<GpuLayer>,
    targets: Option<Targets>,
}

impl Renderer {
    /// `format` is the color format of the views passed to [`Renderer::render`].
    /// It should be an sRGB format so output is gamma encoded.
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("shader.wgsl"));
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(64),
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
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("schematic"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let attributes = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Unorm8x4];
        let vertex_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: size_of::<GpuVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &attributes,
        })];
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
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(depth_write),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState { count: SAMPLE_COUNT, ..Default::default() },
                multiview_mask: None,
                cache: None,
            })
        };
        let pipelines = [
            pipeline(Transparency::Opaque),
            pipeline(Transparency::Cutout),
            pipeline(Transparency::Translucent),
        ];

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
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self { format, pipelines, bind_group_layout, sampler, globals, bind_group: None, layers: Vec::new(), targets: None }
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Upload a mesh, replacing the previous one.
    pub fn set_mesh(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, mesh: &Mesh) {
        self.layers.clear();
        self.bind_group = None;
        let Some(atlas) = &mesh.atlas else { return };

        for (transparency, layer) in mesh.layers() {
            if layer.is_empty() {
                continue;
            }
            let vertices: Vec<GpuVertex> = layer
                .vertices
                .iter()
                .map(|v| GpuVertex { position: v.position, normal: v.normal, uv: v.uv, color: v.color })
                .collect();
            self.layers.push(GpuLayer {
                transparency,
                vertices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("vertices"),
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                }),
                indices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("indices"),
                    contents: bytemuck::cast_slice(&layer.indices),
                    usage: wgpu::BufferUsages::INDEX,
                }),
                count: layer.indices.len() as u32,
            });
        }

        let size = wgpu::Extent3d { width: atlas.width, height: atlas.height, depth_or_array_layers: 1 };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("atlas"),
            size,
            mip_level_count: atlas.mip_levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let levels = std::iter::once((atlas.width, atlas.height, atlas.rgba.clone())).chain(atlas.mip_chain());
        for (level, (width, height, rgba)) in levels.enumerate() {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &rgba,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width * 4), rows_per_image: None },
                wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            );
        }
        let view = texture.create_view(&Default::default());
        self.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.globals.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        }));
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
        queue.write_buffer(&self.globals, 0, bytemuck::cast_slice(&view_proj.to_cols_array()));

        let targets = self.targets(device, width, height);
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

        let Some(bind_group) = &self.bind_group else { return };
        pass.set_bind_group(0, bind_group, &[]);
        // Layers are stored opaque, cutout, translucent: the order they must draw in.
        for layer in &self.layers {
            pass.set_pipeline(&self.pipelines[layer.transparency as usize]);
            pass.set_vertex_buffer(0, layer.vertices.slice(..));
            pass.set_index_buffer(layer.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..layer.count, 0, 0..1);
        }
    }
}
