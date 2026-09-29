//! Draws a [`Mesh`] with wgpu. Independent of windows, so it works for the
//! native viewer, headless screenshots and the browser alike.

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use mesher::{Mesh, Transparency};
use wgpu::util::DeviceExt;

use crate::camera::OrbitCamera;

pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
pub const SAMPLE_COUNT: u32 = 4;
const SHADOW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const CLEAR: wgpu::Color = wgpu::Color { r: 0.009, g: 0.011, b: 0.015, a: 1.0 };

/// How the scene is lit and what surrounds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Lighting {
    /// Direction towards the sun, +Y up; normalised when used.
    pub sun_direction: [f32; 3],
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
}

impl Default for Lighting {
    fn default() -> Self {
        Self {
            sun_direction: [-0.32, 1.0, 0.65],
            shadows: true,
            shadow_map_size: 2048,
            shadow_strength: 1.0,
            ambient_occlusion: 1.0,
            ground: true,
            sky_top: [0.47, 0.65, 0.88],
            sky_bottom: [0.86, 0.9, 0.95],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuVertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    color: [u8; 4],
    ao: f32,
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
            wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Unorm8x4, 4 => Float32];
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

    /// Upload a mesh, replacing the previous one.
    pub fn set_mesh(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, mesh: &Mesh) {
        self.layers.clear();
        self.bind_group = None;
        self.bounds = None;
        let Some(atlas) = &mesh.atlas else { return };
        if mesh.triangle_count() > 0 {
            self.bounds = Some([Vec3::from(mesh.bounds[0]), Vec3::from(mesh.bounds[1])]);
        }

        for (transparency, layer) in mesh.layers() {
            if layer.is_empty() {
                continue;
            }
            let vertices: Vec<GpuVertex> = layer
                .vertices
                .iter()
                .map(|v| GpuVertex { position: v.position, normal: v.normal, uv: v.uv, color: v.color, ao: v.ao })
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
        queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&self.globals_data(Mat4::IDENTITY)));

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

    fn globals_data(&self, view_proj: Mat4) -> Globals {
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
        queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&self.globals_data(view_proj)));

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
