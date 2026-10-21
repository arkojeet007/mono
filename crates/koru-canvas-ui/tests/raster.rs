// SPDX-License-Identifier: MPL-2.0

//! **The canvas, rasterized.** Every other proof in this crate is arithmetic —
//! that a point maps back to itself, that a hit lands where the geometry says.
//! None of them says what a person sees. This one draws a frame through
//! vello's own renderer, with the anti-aliasing the product ships, under the
//! transform a host composes, and reads the pixels back: the backdrop is the
//! page backdrop, a card is a raised card where it should be at 100 % and at
//! 200 %, its text is inked, and the selection wears its ring.
//!
//! It needs a GPU. When there is none it says so and returns, like the
//! session view's raster test it is modelled on
//! (`crates/koru-session-view/tests/raster.rs`).

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};

use koru_canvas_ui::{Camera, CanvasSurface, ObjectKey, ObjectKind, Scene, SceneObject};
use koru_draw::{ImageCache, theme};
use vello::kurbo::Affine;
use vello::peniko::Color;
use vello::wgpu;
use vello::{AaConfig, AaSupport, Renderer, RendererOptions};

static THEME_LOCK: Mutex<()> = Mutex::new(());

struct Nudge(std::thread::Thread);

impl Wake for Nudge {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

fn block_on<F: Future>(mut future: F) -> F::Output {
    let waker = Waker::from(Arc::new(Nudge(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    // SAFETY: `future` is a local that is never moved after this shadowing.
    let mut future = unsafe { Pin::new_unchecked(&mut future) };
    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::park_timeout(std::time::Duration::from_millis(1)),
        }
    }
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
}

impl Gpu {
    fn open() -> Option<Self> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok()?;
        // A software rasterizer is not a GPU for this proof: vello's work
        // crashes WARP, the only adapter on a CI runner, with an access
        // violation. See `crates/koru-session-view/tests/raster.rs`.
        if adapter.get_info().device_type == wgpu::DeviceType::Cpu {
            return None;
        }
        let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("canvas-raster"),
            required_features: wgpu::Features::empty(),
            required_limits: adapter.limits(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::default(),
        }))
        .ok()?;
        let renderer = Renderer::new(
            &device,
            RendererOptions {
                use_cpu: false,
                antialiasing_support: AaSupport::area_only(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )
        .ok()?;
        Some(Self {
            device,
            queue,
            renderer,
        })
    }

    fn raster(&mut self, scene: &vello::Scene, width: u32, height: u32) -> Vec<[u8; 4]> {
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.renderer
            .render_to_texture(
                &self.device,
                &self.queue,
                scene,
                &view,
                &vello::RenderParams {
                    // Magenta: anything the canvas failed to paint shows.
                    base_color: Color::from_rgba8(0xFF, 0x00, 0xFF, 0xFF),
                    width,
                    height,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .expect("vello render");
        let stride = (width * 4).div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(stride) * u64::from(height),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: None,
                },
            },
            size,
        );
        self.queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .expect("map readback");
        let data = slice.get_mapped_range();
        let mut pixels = Vec::with_capacity((width * height) as usize);
        for y in 0..height {
            let row = &data[(y * stride) as usize..];
            for x in 0..width {
                let start = (x * 4) as usize;
                pixels.push([row[start], row[start + 1], row[start + 2], row[start + 3]]);
            }
        }
        drop(data);
        buffer.unmap();
        pixels
    }
}

const VIEW: (f32, f32) = (900.0, 500.0);

fn rgb(color: Color) -> [u8; 3] {
    let c = color.to_rgba8();
    [c.r, c.g, c.b]
}

/// Draw the surface under `Affine::scale(dpi)` and hand back the pixel at a
/// **view** point.
struct Frame {
    pixels: Vec<[u8; 4]>,
    width: u32,
    dpi: f64,
}

impl Frame {
    fn at(&self, x: f64, y: f64) -> [u8; 3] {
        let (px, py) = ((x * self.dpi) as u32, (y * self.dpi) as u32);
        let p = self.pixels[(py * self.width + px) as usize];
        [p[0], p[1], p[2]]
    }
}

fn frame(gpu: &mut Gpu, surface: &mut CanvasSurface, dpi: f64) -> Frame {
    let mut scene = vello::Scene::new();
    surface.draw(&mut scene, Affine::scale(dpi), &ImageCache::new());
    let width = (f64::from(VIEW.0) * dpi).ceil() as u32;
    let height = (f64::from(VIEW.1) * dpi).ceil() as u32;
    Frame {
        pixels: gpu.raster(&scene, width, height),
        width,
        dpi,
    }
}

fn near(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 3)
}

#[test]
fn a_card_is_drawn_where_it_is_at_every_zoom_in_both_palettes() {
    let Some(mut gpu) = Gpu::open() else {
        eprintln!("no hardware wgpu adapter here: the rasterized canvas proof did not run");
        return;
    };
    let _guard = THEME_LOCK.lock().unwrap_or_else(|held| held.into_inner());
    let was_dark = theme::is_dark();
    let key = ObjectKey(uuid::Uuid::from_u128(1));
    let text = "Hello, canvas — a card with enough words in it to wrap onto a second line";
    let mut failures = Vec::new();
    for dark in [false, true] {
        theme::set_dark(dark);
        let p = theme::p();
        for dpi in [1.0, 1.5] {
            let mut surface = CanvasSurface::new(koru_i18n::Lang::En);
            surface.set_viewport(VIEW.0, VIEW.1);
            surface.set_scene(Scene {
                objects: vec![
                    SceneObject::new(key, ObjectKind::Card, 0, 0, 240, text),
                    // Far off screen: must leave no mark.
                    SceneObject::new(
                        ObjectKey(uuid::Uuid::from_u128(2)),
                        ObjectKind::Card,
                        50_000,
                        50_000,
                        240,
                        "Elsewhere",
                    ),
                ],
                ..Scene::default()
            });
            // The camera's centre is canvas (0, 0): the card's top-left
            // corner is at the middle of the view, (450, 250).
            let one = frame(&mut gpu, &mut surface, dpi);
            let label = format!("dark={dark} dpi={dpi}");
            if !near(one.at(20.0, 20.0), rgb(p.canvas)) {
                failures.push(format!("{label}: the backdrop is not the page backdrop"));
            }
            if !near(one.at(454.0, 270.0), rgb(p.panel_bg)) {
                failures.push(format!(
                    "{label}: the card's padding is not the sheet colour"
                ));
            }
            if !near(one.at(800.0, 256.0), rgb(p.canvas)) {
                failures.push(format!("{label}: at 100 % the card ends at 690"));
            }
            // Ink: some of the text area differs from the sheet.
            let inked = (0..200)
                .flat_map(|x| (0..40).map(move |y| (x, y)))
                .filter(|&(x, y)| {
                    !near(
                        one.at(462.0 + f64::from(x), 262.0 + f64::from(y)),
                        rgb(p.panel_bg),
                    )
                })
                .count();
            if inked < 40 {
                failures.push(format!(
                    "{label}: the card's text left {inked} inked pixels"
                ));
            }

            surface.set_camera(Camera::new(0.0, 0.0, 2.0));
            let two = frame(&mut gpu, &mut surface, dpi);
            if !near(two.at(800.0, 256.0), rgb(p.panel_bg)) {
                failures.push(format!("{label}: at 200 % the card reaches past 800"));
            }

            surface.set_camera(Camera::default());
            surface.select(Some(key));
            let selected = frame(&mut gpu, &mut surface, dpi);
            if !near(selected.at(447.0, 270.0), rgb(p.accent)) {
                failures.push(format!(
                    "{label}: no selection ring left of the card ({:?})",
                    selected.at(447.0, 270.0)
                ));
            }
        }
    }
    theme::set_dark(was_dark);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
