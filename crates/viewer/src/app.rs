//! Interactive window: left drag orbits, right/middle drag (or shift + left)
//! pans, scroll zooms, `R` re-frames the model. Page Down / Page Up (or `[`
//! and `]`) step through the build layer by layer, top down; `Esc` shows
//! every layer again. `T` cycles the time of day. On the web the page drives the layer view instead,
//! through [`UserEvent::SetSlice`], so it can keep its own controls in sync.

use std::sync::Arc;

use glam::Vec3;
use mesher::{Atlas, Mesh};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use crate::{OrbitCamera, Renderer, TimeOfDay};

/// Messages delivered to the event loop from outside it.
pub enum UserEvent {
    /// GPU setup finished (it is asynchronous on the web).
    GpuReady(Box<Gpu>),
    /// Show a different model. Shared, so the sender can keep it (e.g. for
    /// exporting) without a copy; the viewer drops it once uploaded.
    SetMesh(Arc<Mesh>),
    /// Start showing a model that arrives in pieces, for builds too large
    /// to hold in memory at once; see [`Renderer::begin_mesh`].
    BeginMesh(Box<Atlas>),
    /// One piece of the model; dropped once uploaded, so the sender can
    /// wait for that before making the next.
    AddMesh(Arc<Mesh>),
    /// All pieces sent: bounds of the whole model, `[min, max]`.
    FinishMesh([[f32; 3]; 2]),
    /// Hide blocks above this world Y; `None` shows them all.
    /// See [`Renderer::set_slice`].
    SetSlice(Option<i32>),
    /// Light the scene as at this time of day; see [`crate::Lighting::set_time`].
    SetTime(TimeOfDay),
    /// Stop the event loop and release the window and GPU. On the web this
    /// lets a new viewer be spawned afterwards.
    Exit,
}

pub struct Gpu {
    window: Arc<Window>,
    instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
}

/// Native: one instance with every backend the platform supports.
#[cfg(not(target_arch = "wasm32"))]
async fn adapter(
    window: &Arc<Window>,
    display: winit::event_loop::OwnedDisplayHandle,
) -> (wgpu::Instance, wgpu::Surface<'static>, wgpu::Adapter) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(Box::new(display)));
    let surface = instance.create_surface(window.clone()).expect("create surface");
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions { compatible_surface: Some(&surface), ..Default::default() })
        .await
        .expect("no compatible GPU adapter");
    (instance, surface, adapter)
}

/// Web: prefer WebGPU, fall back to WebGL2 when it is missing or fails.
#[cfg(target_arch = "wasm32")]
async fn adapter(window: &Arc<Window>) -> (wgpu::Instance, wgpu::Surface<'static>, wgpu::Adapter) {
    for backends in [wgpu::Backends::BROWSER_WEBGPU, wgpu::Backends::GL] {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = backends;
        let instance = wgpu::Instance::new(desc);
        let Ok(surface) = instance.create_surface(window.clone()) else { continue };
        let options = wgpu::RequestAdapterOptions { compatible_surface: Some(&surface), ..Default::default() };
        if let Ok(adapter) = instance.request_adapter(&options).await {
            log::info!("using {:?}", adapter.get_info().backend);
            return (instance, surface, adapter);
        }
    }
    panic!("neither WebGPU nor WebGL2 is available");
}

impl Gpu {
    async fn new(
        window: Arc<Window>,
        #[cfg(not(target_arch = "wasm32"))] display: winit::event_loop::OwnedDisplayHandle,
    ) -> Gpu {
        #[cfg(not(target_arch = "wasm32"))]
        let (instance, surface, adapter) = adapter(&window, display).await;
        #[cfg(target_arch = "wasm32")]
        let (instance, surface, adapter) = adapter(&window).await;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("viewer"),
                // WebGL2 has lower limits than native backends; stay within the adapter's.
                required_limits: wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
                ..Default::default()
            })
            .await
            .expect("could not open GPU device");

        let format = surface.get_capabilities(&adapter).formats[0];
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            // Render through an sRGB view so shading is gamma correct.
            view_formats: vec![format.add_srgb_suffix()],
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            desired_maximum_frame_latency: 2,
            present_mode: wgpu::PresentMode::AutoVsync,
        };
        surface.configure(&device, &config);
        let renderer = Renderer::new(&device, format.add_srgb_suffix());
        Gpu { window, instance, surface, device, queue, config, renderer }
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    fn render(&mut self, camera: &OrbitCamera) {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            wgpu::CurrentSurfaceTexture::Occluded | wgpu::CurrentSurfaceTexture::Timeout => return,
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                drop(frame);
                self.surface.configure(&self.device, &self.config);
                return;
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.surface = self.instance.create_surface(self.window.clone()).expect("recreate surface");
                self.surface.configure(&self.device, &self.config);
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => panic!("surface validation error"),
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.renderer.format()),
            ..Default::default()
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let size = (self.config.width, self.config.height);
        self.renderer.render(&self.device, &self.queue, &mut encoder, &view, size, camera);
        self.queue.submit([encoder.finish()]);
        self.window.pre_present_notify();
        self.queue.present(frame);
    }
}

#[derive(Default)]
struct Drag {
    orbit: bool,
    pan: bool,
    last: Option<PhysicalPosition<f64>>,
    /// Active touch points (id, position): one finger orbits, two pinch-zoom and pan.
    touches: Vec<(u64, PhysicalPosition<f64>)>,
}

/// Midpoint and spread of the first two touches.
fn pinch(touches: &[(u64, PhysicalPosition<f64>)]) -> Option<(PhysicalPosition<f64>, f64)> {
    let [(_, a), (_, b), ..] = touches else { return None };
    let mid = PhysicalPosition::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
    Some((mid, (a.x - b.x).hypot(a.y - b.y)))
}

pub struct Viewer {
    proxy: EventLoopProxy<UserEvent>,
    title: String,
    #[cfg(target_arch = "wasm32")]
    canvas: web_sys::HtmlCanvasElement,
    gpu: Option<Gpu>,
    /// Events that need the GPU, received before it was ready.
    pending: Vec<UserEvent>,
    bounds: Option<[[f32; 3]; 2]>,
    camera: OrbitCamera,
    drag: Drag,
    modifiers: ModifiersState,
    #[cfg(not(target_arch = "wasm32"))]
    time: TimeOfDay,
    /// Called once the viewer has been torn down.
    on_exit: Option<Box<dyn FnOnce()>>,
}

impl Drop for Viewer {
    fn drop(&mut self) {
        if let Some(on_exit) = self.on_exit.take() {
            on_exit();
        }
    }
}

impl Viewer {
    /// Handle an event that needs the GPU.
    fn render_event(&mut self, event: UserEvent) {
        let Some(gpu) = &mut self.gpu else {
            self.pending.push(event);
            return;
        };
        let (device, queue) = (&gpu.device, &gpu.queue);
        // Only the bounds are kept; the GPU holds the geometry.
        let bounds = match event {
            UserEvent::SetMesh(mesh) => {
                gpu.renderer.set_mesh(device, queue, &mesh);
                Some(mesh.bounds)
            }
            UserEvent::BeginMesh(atlas) => {
                gpu.renderer.begin_mesh(device, queue, &atlas);
                self.bounds = None;
                None
            }
            UserEvent::AddMesh(mesh) => {
                gpu.renderer.add_mesh(device, &mesh);
                None
            }
            UserEvent::FinishMesh(bounds) => {
                gpu.renderer.finish_mesh(device, queue, bounds);
                Some(bounds)
            }
            UserEvent::SetSlice(top) => {
                gpu.renderer.set_slice(device, queue, top);
                None
            }
            UserEvent::SetTime(time) => {
                let mut lighting = gpu.renderer.lighting().clone();
                lighting.set_time(time);
                gpu.renderer.set_lighting(device, queue, lighting);
                None
            }
            UserEvent::GpuReady(_) | UserEvent::Exit => unreachable!("not a render event"),
        };
        if let Some([min, max]) = bounds {
            self.bounds = Some([min, max]);
            self.camera.frame(Vec3::from(min), Vec3::from(max));
        }
        gpu.window.request_redraw();
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn set_slice(&mut self, top: Option<i32>) {
        self.render_event(UserEvent::SetSlice(top));
    }

    /// Move the layer view `steps` layers up (negative: down), or show every
    /// layer with `None`. Going above the top layer also shows them all.
    #[cfg(not(target_arch = "wasm32"))]
    fn step_slice(&mut self, steps: Option<i32>) {
        let (Some([min, max]), Some(gpu)) = (self.bounds, &self.gpu) else { return };
        let (bottom, top) = (min[1].floor() as i32, max[1].ceil() as i32 - 1);
        let next = match (gpu.renderer.slice(), steps) {
            (_, None) => None,
            (None, Some(steps)) if steps < 0 => Some(top + 1 + steps),
            (None, Some(_)) => None,
            (Some(y), Some(steps)) => Some(y + steps),
        };
        let next = next.filter(|&y| y <= top).map(|y| y.max(bottom));
        if let Some(y) = next {
            gpu.window.set_title(&format!("{} - layer {}/{}", self.title, y - bottom + 1, top - bottom + 1));
        } else {
            gpu.window.set_title(&self.title);
        }
        self.set_slice(next);
    }

    fn redraw(&self) {
        if let Some(gpu) = &self.gpu {
            gpu.window.request_redraw();
        }
    }
}

impl ApplicationHandler<UserEvent> for Viewer {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        let attributes = Window::default_attributes().with_title(&self.title);
        #[cfg(not(target_arch = "wasm32"))]
        let attributes = attributes.with_inner_size(winit::dpi::LogicalSize::new(1280, 800));
        #[cfg(target_arch = "wasm32")]
        let attributes = {
            use winit::platform::web::WindowAttributesExtWebSys;
            attributes.with_canvas(Some(self.canvas.clone()))
        };
        let window = Arc::new(event_loop.create_window(attributes).expect("create window"));

        #[cfg(not(target_arch = "wasm32"))]
        {
            let gpu = pollster::block_on(Gpu::new(window, event_loop.owned_display_handle()));
            let _ = self.proxy.send_event(UserEvent::GpuReady(Box::new(gpu)));
        }
        #[cfg(target_arch = "wasm32")]
        {
            let proxy = self.proxy.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let gpu = Gpu::new(window).await;
                let _ = proxy.send_event(UserEvent::GpuReady(Box::new(gpu)));
            });
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::GpuReady(gpu) => {
                self.gpu = Some(*gpu);
                for event in std::mem::take(&mut self.pending) {
                    self.render_event(event);
                }
            }
            UserEvent::Exit => event_loop.exit(),
            event => self.render_event(event),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(gpu) = &mut self.gpu else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                gpu.resize(size.width, size.height);
                gpu.window.request_redraw();
            }
            WindowEvent::RedrawRequested => gpu.render(&self.camera),
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = state == ElementState::Pressed;
                match button {
                    MouseButton::Left if self.modifiers.shift_key() => self.drag.pan = pressed,
                    MouseButton::Left => self.drag.orbit = pressed,
                    MouseButton::Right | MouseButton::Middle => self.drag.pan = pressed,
                    _ => {}
                }
                if !pressed {
                    self.drag.orbit = false;
                    self.drag.pan = false;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(last) = self.drag.last {
                    let (dx, dy) = ((position.x - last.x) as f32, (position.y - last.y) as f32);
                    if self.drag.orbit {
                        self.camera.orbit(dx, dy);
                        gpu.window.request_redraw();
                    } else if self.drag.pan {
                        self.camera.pan(dx, dy, gpu.config.height as f32);
                        gpu.window.request_redraw();
                    }
                }
                self.drag.last = Some(position);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let steps = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    // Trackpads report pixels; ~50 px feels like one notch.
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 50.0,
                };
                self.camera.zoom(steps);
                self.redraw();
            }
            WindowEvent::Touch(touch) => {
                let before = pinch(&self.drag.touches);
                let index = self.drag.touches.iter().position(|(id, _)| *id == touch.id);
                match (touch.phase, index) {
                    (TouchPhase::Started, None) => self.drag.touches.push((touch.id, touch.location)),
                    (TouchPhase::Moved, Some(i)) => {
                        let last = std::mem::replace(&mut self.drag.touches[i].1, touch.location);
                        if self.drag.touches.len() == 1 {
                            let (dx, dy) = ((touch.location.x - last.x) as f32, (touch.location.y - last.y) as f32);
                            self.camera.orbit(dx, dy);
                            gpu.window.request_redraw();
                        } else if let (Some((m0, d0)), Some((m1, d1))) = (before, pinch(&self.drag.touches)) {
                            self.camera.pan((m1.x - m0.x) as f32, (m1.y - m0.y) as f32, gpu.config.height as f32);
                            if d0 > 1.0 && d1 > 1.0 {
                                // zoom() scales distance by 0.9^steps; match the finger spread.
                                self.camera.zoom(((d1 / d0).ln() / (1.0f64 / 0.9).ln()) as f32);
                            }
                            gpu.window.request_redraw();
                        }
                    }
                    (TouchPhase::Ended | TouchPhase::Cancelled, Some(i)) => {
                        self.drag.touches.remove(i);
                    }
                    _ => {}
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let reframe = matches!(event.logical_key.as_ref(), Key::Character("r" | "R") | Key::Named(NamedKey::Home));
                if reframe && let Some([min, max]) = self.bounds {
                    let (yaw, pitch) = (self.camera.yaw, self.camera.pitch);
                    self.camera = OrbitCamera { yaw, pitch, ..Default::default() };
                    self.camera.frame(Vec3::from(min), Vec3::from(max));
                    self.redraw();
                }
                #[cfg(not(target_arch = "wasm32"))]
                match event.logical_key.as_ref() {
                    Key::Named(NamedKey::PageDown) | Key::Character("[") => self.step_slice(Some(-1)),
                    Key::Named(NamedKey::PageUp) | Key::Character("]") => self.step_slice(Some(1)),
                    Key::Named(NamedKey::Escape) => self.step_slice(None),
                    Key::Character("t" | "T") => {
                        self.time = TimeOfDay::ALL[(self.time as usize + 1) % TimeOfDay::ALL.len()];
                        self.render_event(UserEvent::SetTime(self.time));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

impl Viewer {
    fn new(proxy: EventLoopProxy<UserEvent>, title: &str, #[cfg(target_arch = "wasm32")] canvas: web_sys::HtmlCanvasElement) -> Self {
        Viewer {
            proxy,
            title: title.to_owned(),
            #[cfg(target_arch = "wasm32")]
            canvas,
            gpu: None,
            pending: Vec::new(),
            bounds: None,
            camera: OrbitCamera::default(),
            drag: Drag::default(),
            modifiers: ModifiersState::empty(),
            #[cfg(not(target_arch = "wasm32"))]
            time: TimeOfDay::Day,
            on_exit: None,
        }
    }
}

/// Open a window showing `mesh` and block until it is closed.
#[cfg(not(target_arch = "wasm32"))]
pub fn run(mesh: Mesh, title: &str) -> Result<(), winit::error::EventLoopError> {
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    // Only redraw in response to input, so an idle viewer uses no GPU time.
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut viewer = Viewer::new(event_loop.create_proxy(), title);
    viewer.pending.push(UserEvent::SetMesh(Arc::new(mesh)));
    event_loop.run_app(&mut viewer)
}

/// Start the viewer on a canvas. The event loop runs in the background;
/// send [`UserEvent::SetMesh`] through the returned proxy to show a model.
///
/// Only one viewer can run at a time. Send [`UserEvent::Exit`] to stop it;
/// `on_exit` runs while winit tears the loop down, and once the current
/// JavaScript task has finished a new viewer can be spawned.
#[cfg(target_arch = "wasm32")]
pub fn spawn(
    canvas: web_sys::HtmlCanvasElement,
    on_exit: impl FnOnce() + 'static,
) -> Result<EventLoopProxy<UserEvent>, winit::error::EventLoopError> {
    use winit::platform::web::EventLoopExtWebSys;
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let mut viewer = Viewer::new(proxy.clone(), "schematic viewer", canvas);
    viewer.on_exit = Some(Box::new(on_exit));
    event_loop.spawn_app(viewer);
    Ok(proxy)
}
