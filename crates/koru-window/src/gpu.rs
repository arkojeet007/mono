// SPDX-License-Identifier: MPL-2.0

//! The surface half of a runner: warm-up, device-keyed renderer reuse, window
//! creation and the frame that follows it.
//!
//! Shared by both runners for the reason `Renderer::new` exists in only one
//! place already — it compiles every pipeline vello has and there is no
//! pipeline cache on DX12, measured at 7.6 s in a debug build. Keying renderers
//! on the **device** rather than the surface is what makes a second window, or
//! an overlay reopened on the next hotkey, free. A multi-window runner that
//! kept its own map would pay that per window.
//!
//! ## vello 0.8 has no `render_to_surface`
//!
//! It draws into an intermediate texture and getting that onto the screen is
//! the caller's job; `RenderSurface` carries a blitter for exactly this.

use std::collections::HashMap;
use std::sync::Arc;

use vello::kurbo::Affine;
use vello::util::{RenderContext, RenderSurface};
use vello::{AaConfig, Renderer, Scene};
use winit::dpi::PhysicalSize;
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use koru_draw::text::TextEngine;
use koru_draw::theme;

use crate::dispatch::DispatchApp;
use crate::{frame, hairline, reveal};

/// Latency over vsync, the same choice the main app makes: a keystroke must not
/// wait for a refresh that has already been scheduled.
const PRESENT_MODE: wgpu::PresentMode = wgpu::PresentMode::AutoNoVsync;

/// One window that is on screen, and what it draws through.
pub(crate) struct LiveWindow {
    pub surface: RenderSurface<'static>,
    pub window: Arc<Window>,
    /// Transparent utility windows paint and frame only their own content.
    /// A native border, hairline, or opaque renderer clear would recreate the
    /// rectangular window around a rounded HUD.
    transparent: bool,
    /// Which edges need the hairline painting, cached — it is a fact about the
    /// window and changes only when the window moves or resizes.
    pub flush: hairline::Edges,
}

/// The two scenes a frame is built in, kept across frames and across windows.
///
/// The content is drawn in **logical** pixels into `content` and appended into
/// `scene` scaled, because a `Scene` has no transform of its own and threading a
/// root `Affine` through every draw function would put the correction at sixty
/// call sites instead of one.
#[derive(Default)]
pub(crate) struct Frame {
    scene: Scene,
    content: Scene,
}

/// Every device this process draws with, and one renderer for each.
pub(crate) struct Gpu {
    /// Made when the first window needs it, never in [`Gpu::new`]: `new` runs
    /// on the thread that answers people, and a wgpu instance is a loader scan
    /// of every Vulkan layer and driver — two seconds of it, on a loaded
    /// four-core machine, held an Agent's `Status` answer while its gateway
    /// started. The warm-up makes its own on its own thread, and it is the one
    /// a window normally gets; this is filled only when there was no warm-up.
    context: Option<RenderContext>,
    /// The device and renderer being made on another thread while the process
    /// starts, taken by the first [`Gpu::open`]. See [`crate::renderer::Warmup`].
    ///
    /// `None` once absorbed — and once a window has existed, `context` owns
    /// devices a surface may be using, so there is nothing left to swap in.
    warmup: Option<crate::renderer::Warmup>,
    renderers: HashMap<usize, Renderer>,
}

impl Gpu {
    pub fn new() -> Self {
        Self {
            // The device and the compiled renderer arrive from the warm-up
            // below, with the context they were made on.
            context: None,
            warmup: Some(crate::renderer::Warmup::start()),
            renderers: HashMap::new(),
        }
    }

    /// Take the warmed-up device and renderer, once.
    ///
    /// Only while this pool has never had a device of its own: after a window
    /// has existed, `context` owns devices that surfaces and renderers are
    /// keyed against, and replacing it wholesale would strand them.
    /// The context, made now if neither the warm-up nor an earlier window
    /// has supplied one.
    fn context(&mut self) -> &mut RenderContext {
        self.context.get_or_insert_with(crate::renderer::context)
    }

    fn absorb_warmup(&mut self) {
        let Some(warmup) = self.warmup.take() else {
            return;
        };
        if self
            .context
            .as_ref()
            .is_some_and(|context| !context.devices.is_empty())
        {
            return;
        }
        let Some(warmed) = warmup.take() else {
            return;
        };
        self.context = Some(warmed.context);
        if let Some((dev_id, renderer)) = warmed.renderer {
            self.renderers.insert(dev_id, renderer);
        }
    }

    /// Create one window, its surface and its renderer.
    ///
    /// `None` on any GPU failure: a window that cannot draw stays closed rather
    /// than killing the process it lives in (the agent, in its tray). The
    /// runner's next settle tries again.
    pub fn open<T: DispatchApp>(
        &mut self,
        events: &ActiveEventLoop,
        app: &mut T,
        frame_scenes: &mut Frame,
        text: &mut TextEngine,
    ) -> Option<LiveWindow> {
        self.absorb_warmup();
        let paint_before_showing = app.paint_before_showing();
        let passive = app.window_activation() == crate::WindowActivation::Passive;
        let attributes = if paint_before_showing || passive {
            app.window_attributes()
                .with_visible(false)
                .with_active(!passive)
        } else {
            app.window_attributes()
        };
        let transparent = attributes.transparent;
        // An HWND-backed DX12 surface only offers opaque composition in wgpu
        // 28. Keep the semantic request, but make the surface itself opaque on
        // Windows and clip the HWND to the rounded content below.
        #[cfg(windows)]
        let attributes = attributes.with_transparent(false);
        let window = Arc::new(events.create_window(attributes).expect("create window"));
        if transparent && !crate::rounded_region::apply(&window) {
            window.set_visible(false);
            tracing::error!(
                target: "koru::window",
                "rounded utility window refused: native clipping could not be applied"
            );
            return None;
        }
        if transparent {
            frame::apply_utility_shadow(&window);
        }
        if passive && !retain_passive_style(&window, "creation") {
            return None;
        }
        window.set_ime_allowed(app.wants_ime());

        let flush = hairline::edges(&window);
        if !paint_before_showing && !transparent {
            frame::apply(&window, theme::p().panel_border, window.is_maximized());
            if passive && !retain_passive_style(&window, "initial frame") {
                return None;
            }
        }
        app.set_maximised(window.is_maximized());

        let surface = self.surface_for(&window)?;
        let live = LiveWindow {
            surface,
            window: window.clone(),
            transparent,
            flush,
        };
        if paint_before_showing {
            // Real pixels before it is on screen, rather than a blank freshly
            // created surface. The window is still hidden here.
            self.redraw(&live, app, frame_scenes, text);
            if passive && !retain_passive_style(&window, "before reveal") {
                return None;
            }
            reveal::when_painted(&window);
            // The compositor shadow goes on after the window is up: it was
            // ordered this way because animating a shadowed HWND had a visibly
            // jerky edge, and there is no reason to move it back.
            if !transparent {
                frame::apply(&window, theme::p().panel_border, window.is_maximized());
            }
            if passive && !retain_passive_style(&window, "reveal and final frame") {
                return None;
            }
        }
        if !paint_before_showing && passive {
            if !retain_passive_style(&window, "before reveal") {
                return None;
            }
            window.set_visible(true);
            if !retain_passive_style(&window, "reveal") {
                return None;
            }
        }
        if passive {
            let position = window.outer_position().ok();
            let size = window.inner_size();
            tracing::info!(
                target: "koru::window",
                visible = window.is_visible().unwrap_or(false),
                x = position.map(|point| point.x),
                y = position.map(|point| point.y),
                width = size.width,
                height = size.height,
                "passive utility window revealed"
            );
        }
        // A panel summoned by a global shortcut is the keyboard destination,
        // not merely a visible always-on-top surface. Window creation normally
        // activates, but skip-taskbar undecorated windows are not guaranteed to
        // inherit that behavior on Windows, so make the request explicit.
        if !passive {
            window.focus_window();
        }
        Some(live)
    }

    /// A configured surface and a renderer for its device.
    fn surface_for(&mut self, window: &Arc<Window>) -> Option<RenderSurface<'static>> {
        let size = window.inner_size();
        let mut surface = match pollster::block_on(self.context().create_surface(
            window.clone(),
            size.width.max(1),
            size.height.max(1),
            PRESENT_MODE,
        )) {
            Ok(surface) => surface,
            Err(error) => {
                // A configured backend degrades to the default; a default that
                // also fails leaves the window closed rather than killing the
                // process it lives in.
                let retried = crate::renderer::configured_backends().is_some().then(|| {
                    tracing::error!(
                        target: "koru::gpu",
                        "surface creation failed on the configured backend ({error}); \
                         retrying with the default"
                    );
                    self.context = Some(RenderContext::new());
                    // The renderers belong to the abandoned backend's devices.
                    self.renderers.clear();
                    pollster::block_on(self.context().create_surface(
                        window.clone(),
                        size.width.max(1),
                        size.height.max(1),
                        PRESENT_MODE,
                    ))
                });
                match retried {
                    Some(Ok(surface)) => surface,
                    Some(Err(retry_error)) => {
                        tracing::error!(
                            target: "koru::gpu",
                            "window has no surface: {retry_error}"
                        );
                        return None;
                    }
                    None => {
                        tracing::error!(target: "koru::gpu", "window has no surface: {error}");
                        return None;
                    }
                }
            }
        };
        // vello's helper hard-codes two, which lets the driver hold a finished
        // frame back while the next click is already waiting behind it.
        surface.config.desired_maximum_frame_latency = 1;
        self.context().set_present_mode(&mut surface, PRESENT_MODE);

        if !self.renderers.contains_key(&surface.dev_id) {
            match crate::renderer::build_renderer(&self.context().devices[surface.dev_id].device) {
                Ok(renderer) => {
                    self.renderers.insert(surface.dev_id, renderer);
                }
                Err(message) => {
                    tracing::error!(target: "koru::gpu", "window has no renderer: {message}");
                    return None;
                }
            }
        }
        Some(surface)
    }

    pub fn resize(&mut self, live: &mut LiveWindow, size: PhysicalSize<u32>) {
        self.context()
            .resize_surface(&mut live.surface, size.width.max(1), size.height.max(1));
        if live.transparent && !crate::rounded_region::apply(&live.window) {
            live.window.set_visible(false);
            tracing::error!(
                target: "koru::window",
                "rounded utility window hidden: native clipping could not be resized"
            );
            return;
        }
        live.flush = hairline::edges(&live.window);
        live.window.request_redraw();
    }

    /// Build one frame, put it on the screen, and tell the app it landed.
    pub fn redraw<T: DispatchApp>(
        &mut self,
        live: &LiveWindow,
        app: &mut T,
        frame_scenes: &mut Frame,
        text: &mut TextEngine,
    ) {
        let size = live.window.inner_size();
        let (width, height) = (size.width.max(1), size.height.max(1));
        let scale = live.window.scale_factor() as f32;

        // **Everything above draws in logical pixels.** `inner_size` is
        // physical, so on a 150% display a 30px heading would reach the screen
        // as 30 device pixels — a fifth smaller than it is anywhere else in the
        // product.
        frame_scenes.content.reset();
        app.draw(
            &mut frame_scenes.content,
            text,
            width as f32 / scale,
            height as f32 / scale,
        );
        frame_scenes.scene.reset();
        frame_scenes
            .scene
            .append(&frame_scenes.content, Some(Affine::scale(scale as f64)));
        // The hairline is a **device**-pixel line along the window's own edges,
        // so it is drawn after the append and in physical coordinates: scaling
        // it would make it 1.5px wide and blurry.
        if !live.transparent {
            hairline::draw(
                &mut frame_scenes.scene,
                width,
                height,
                live.flush,
                theme::p().panel_border,
            );
        }

        // A live window was made on this context, so it is there.
        let Some(context) = &self.context else {
            return;
        };
        let device = &context.devices[live.surface.dev_id];
        let Some(renderer) = self.renderers.get_mut(&live.surface.dev_id) else {
            return;
        };
        if let Err(e) = renderer.render_to_texture(
            &device.device,
            &device.queue,
            &frame_scenes.scene,
            &live.surface.target_view,
            &vello::RenderParams {
                base_color: window_base_color(live.transparent),
                width,
                height,
                antialiasing_method: AaConfig::Area,
            },
        ) {
            eprintln!("vello render failed: {e:?}");
            return;
        }

        let frame = match live.surface.surface.get_current_texture() {
            Ok(f) => f,
            Err(e) => {
                eprintln!("surface texture unavailable: {e:?}");
                return;
            }
        };
        let mut encoder = device
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("blit"),
            });
        live.surface.blitter.copy(
            &device.device,
            &mut encoder,
            &live.surface.target_view,
            &frame
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default()),
        );
        device.queue.submit([encoder.finish()]);
        frame.present();
        app.presented();
    }
}

fn window_base_color(transparent: bool) -> vello::peniko::Color {
    if transparent {
        #[cfg(windows)]
        return theme::p().panel_bg;
        #[cfg(not(windows))]
        vello::peniko::Color::from_rgba8(0, 0, 0, 0)
    } else {
        theme::p().bg
    }
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::theme;
    use super::window_base_color;

    #[test]
    fn utility_windows_use_the_platform_composition_base() {
        #[cfg(windows)]
        assert_eq!(
            window_base_color(true).to_rgba8(),
            theme::p().panel_bg.to_rgba8()
        );
        #[cfg(not(windows))]
        assert_eq!(window_base_color(true).to_rgba8().a, 0);
        assert_eq!(window_base_color(false).to_rgba8().a, 255);
    }
}

fn retain_passive_style(window: &Window, stage: &'static str) -> bool {
    if crate::non_activating::apply(window) {
        return true;
    }
    // Fail closed even if verification fails after ShowWindow: a passive
    // surface whose activation contract is unknown must not remain visible.
    window.set_visible(false);
    tracing::error!(
        target: "koru::window",
        stage,
        "passive window refused: WS_EX_NOACTIVATE was not retained"
    );
    false
}
