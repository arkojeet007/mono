// SPDX-License-Identifier: MPL-2.0

//! **The grid, rasterized.** Every other proof in this crate is arithmetic —
//! that two edges are one number, that a number is a whole device pixel. None
//! of them can say what a person sees, and this bug was reported twice from a
//! screenshot and twice mis-diagnosed from geometry that was locally correct.
//!
//! So the frame is drawn: vello's own renderer, the anti-aliasing the product
//! ships (`AaConfig::Area`, whose docs name conflation artifacts as its known
//! failure), the transform chain the window really composes — the view's `root`
//! under `koru-window`'s DPI append — and the pixels read back and counted.
//!
//! It needs a GPU. There is one on the machine this was written on, and the
//! test says so and returns when there is not, because a suite that cannot run
//! on a headless box is worse than one that skips a check there.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};

use koru_draw::{text::TextEngine, theme};
use koru_session_protocol::{
    AttentionState, CellStyle, CellWidth, ColumnIndex, CursorShape, CursorState, GridSize,
    RenderSnapshot, RenderUpdate, RowIndex, RowRange, SessionId, StreamSequence, TerminalCell,
    TerminalColor, TerminalGeneration, TerminalModes, TerminalRow, ViewportGeneration,
};
use koru_session_view::TerminalView;
use vello::wgpu;
use vello::{
    AaConfig, AaSupport, Renderer, RendererOptions, Scene,
    kurbo::{Affine, Rect},
    peniko::Color,
};

static THEME_LOCK: Mutex<()> = Mutex::new(());

const COLUMNS: u16 = 40;
const ROWS: u16 = 12;
/// A full-screen TUI's own canvas — near-black, and deliberately *not* the
/// terminal's own ground in either palette, so any pixel of the ground that
/// leaks between two cells is a number this test can count.
const CANVAS: (u8, u8, u8) = (0x0d, 0x0d, 0x0d);

// ------------------------------------------------------------------- the GPU

struct Nudge(std::thread::Thread);

impl Wake for Nudge {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/// Drive one future to completion on this thread. wgpu's adapter and device
/// requests are async and the crate has no runtime; ten lines of `std::task`
/// are cheaper than a dependency for two awaits in a test.
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
        // A software rasterizer is not a GPU for this proof. WARP is the only
        // adapter a CI runner or a VM without one has, and vello's work crashes
        // it with an access violation — the whole process, on an empty scene —
        // so there is nothing to skip to after the fact.
        if adapter.get_info().device_type == wgpu::DeviceType::Cpu {
            return None;
        }
        let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("terminal-grid-raster"),
            required_features: wgpu::Features::empty(),
            required_limits: adapter.limits(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::default(),
        }))
        .ok()?;
        // The product's own renderer settings: area anti-aliasing, GPU stages.
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

    /// Render `scene` at `width` × `height` device pixels and read it back.
    fn raster(&mut self, scene: &Scene, width: u32, height: u32, base: Color) -> Vec<[u8; 4]> {
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
                    base_color: base,
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

// ------------------------------------------------------------- the TUI screen

/// A screen every cell of which carries the TUI's own background, split into as
/// many style runs as a row can have: the foreground alternates per cell, and
/// `group_runs` merges only cells whose whole resolved style matches — so this
/// is one background rect per cell, forty abutting edges per row, which is the
/// worst case the grouping can produce and the one the light theme reported as
/// "a box around every run of text".
fn tui_snapshot() -> RenderSnapshot {
    let rows = (0..ROWS)
        .map(|row| TerminalRow {
            index: RowIndex(u64::from(row)),
            cells: (0..COLUMNS)
                .map(|column| TerminalCell {
                    column: ColumnIndex(column),
                    // A blank: the background is then the only thing painted,
                    // so every pixel that is not the canvas is a seam and not a
                    // glyph's anti-aliased edge.
                    text: " ".into(),
                    width: CellWidth::One,
                    style: CellStyle {
                        background: TerminalColor::Rgb {
                            red: CANVAS.0,
                            green: CANVAS.1,
                            blue: CANVAS.2,
                        },
                        foreground: TerminalColor::Indexed {
                            index: (column % 7) as u8 + 1,
                        },
                        ..CellStyle::default()
                    },
                })
                .collect(),
            wrapped_to_next: false,
        })
        .collect();
    RenderSnapshot {
        session_id: SessionId::mint(),
        terminal_generation: TerminalGeneration::new(1).unwrap(),
        viewport_generation: ViewportGeneration::new(1).unwrap(),
        sequence: StreamSequence::new(1).unwrap(),
        size: GridSize {
            rows: ROWS,
            columns: COLUMNS,
        },
        viewport: RowRange {
            first: RowIndex(0),
            count: ROWS,
        },
        rows,
        cursor: CursorState {
            row: RowIndex(0),
            column: ColumnIndex(0),
            shape: CursorShape::Hidden,
            blinking: false,
        },
        modes: TerminalModes {
            alternate_screen: true,
            ..TerminalModes::default()
        },
        attention: AttentionState::default(),
    }
}

/// Draw the screen the way the window does and count the pixels inside the grid
/// that are not the TUI's canvas colour.
/// **Where a session surface actually sits.** The shell hands each surface
/// `root * translate(region.x, region.y)` and a *local* rect that starts at the
/// origin, so the grid's real position lives in the transform rather than in
/// the bounds. The terminal then insets its content by `CONTENT_INSET_X` inside
/// that.
///
/// It has to be in the harness because a translation the snapping does not know
/// about moves every snapped edge by the *same* sub-pixel amount — a uniform
/// hairline rather than a scattering. The caption is 36 logical pixels, a whole
/// number of device pixels at every DPI Windows offers and a fractional one the
/// moment a display zoom multiplies it: 36 x 1.25 = 45, but 36 x 1.25 x 1.25 =
/// 56.25.
#[derive(Clone, Copy, Debug)]
struct Origin {
    /// The left pane's width. It is draggable, so it is any float at all.
    pane: f64,
    /// The caption band above the surface.
    caption: f64,
}

const ORIGINS: [Origin; 4] = [
    // The window's own zero — what a harness that forgets the translation
    // tests, and the reason an earlier attempt at this bug looked proven.
    Origin {
        pane: 0.0,
        caption: 0.0,
    },
    // The shipped chrome: `theme::CAPTION_H` over a default pane.
    Origin {
        pane: 232.0,
        caption: 36.0,
    },
    // Panes the owner has dragged, landing between logical pixels.
    Origin {
        pane: 231.3,
        caption: 36.0,
    },
    Origin {
        pane: 187.5,
        caption: 36.0,
    },
];

/// Count the pixels strictly inside the grid that are not the TUI's canvas
/// colour. Shared by every chain this file rasterizes: the geometry of "what is
/// a seam" is one rule, whatever transform produced the frame.
fn count_seams(pixels: &[[u8; 4]], width: u32, x0: f64, y0: f64, x1: f64, y1: f64) -> usize {
    // The grid's own outer edge against the pane is allowed to be anti-aliased,
    // so the scan starts a whole device pixel inside it. Everything in there is
    // the TUI's canvas and nothing else.
    let mut seams = 0;
    for y in (y0.ceil() as u32 + 1)..(y1.floor() as u32 - 1) {
        for x in (x0.ceil() as u32 + 1)..(x1.floor() as u32 - 1) {
            let pixel = pixels[(y * width + x) as usize];
            let off = pixel[0].abs_diff(CANVAS.0)
                + pixel[1].abs_diff(CANVAS.1)
                + pixel[2].abs_diff(CANVAS.2);
            // A tolerance of one 8-bit step per channel, because the readback
            // is a round trip through a normalized texture and a driver is
            // allowed to land a whole-coverage pixel one unit either way.
            if off > 3 {
                seams += 1;
            }
        }
    }
    seams
}

/// Draw the screen the way the window does and count the pixels inside the grid
/// that are not the TUI's canvas colour.
fn seam_pixels(gpu: &mut Gpu, scale: f32, zoom: f64, origin: Origin) -> usize {
    let mut view = TerminalView::new(());
    view.set_visible_rows(RowRange {
        first: RowIndex(0),
        count: ROWS,
    });
    view.apply(RenderUpdate::Snapshot {
        snapshot: tui_snapshot(),
    })
    .expect("snapshot");
    let mut engine = TextEngine::new();
    view.prepare_style(&mut engine, scale);
    let (cell_width, cell_height) = view.cell_size().expect("style prepared");
    // `SessionSurface::content_rect` insets the pane it was handed; the pane
    // itself starts at the surface's local origin, because the shell put the
    // surface's position in the transform instead.
    let inset = f64::from(koru_session_view::CONTENT_INSET_X);
    let bounds = Rect::new(
        inset,
        0.0,
        inset + f64::from(cell_width) * f64::from(COLUMNS),
        f64::from(cell_height) * f64::from(ROWS),
    );
    let mut content = Scene::new();
    // The chain in the order the window composes it: the shell's display zoom,
    // then the surface's own position, then `koru-window`'s DPI append.
    let root = Affine::scale(zoom) * Affine::translate((origin.pane, origin.caption));
    view.draw(&mut engine, &mut content, root, bounds, scale);
    let mut frame = Scene::new();
    frame.append(&content, Some(Affine::scale(f64::from(scale))));

    let device = f64::from(scale) * zoom;
    let x0 = (bounds.x0 + origin.pane) * device;
    let y0 = (bounds.y0 + origin.caption) * device;
    let x1 = (bounds.x1 + origin.pane) * device;
    let y1 = (bounds.y1 + origin.caption) * device;
    let width = x1.ceil() as u32 + 2;
    let height = y1.ceil() as u32 + 2;
    let pixels = gpu.raster(
        &frame,
        width,
        height,
        koru_session_view::terminal_background(),
    );
    count_seams(&pixels, width, x0, y0, x1, y1)
}

/// **The other product's chain: koru.exe (Koru Workspace).**
///
/// `koru-workspace-host` composes the *entire* glass transform into the root it
/// hands a `View` — `Affine::scale(dpi × zoom)` times the tab content's origin
/// — and renders the scene 1:1, with no appended transform at all. So the
/// `scale` a session tab passes the surface is exactly `1.0`: it is the factor
/// the window applies *after* the scene, and this window applies none. It also
/// keeps `theme::zoom()` at the user's display zoom (its zoom indicator reads
/// the global), so this harness sets it the same way — the metrics the grid is
/// laid out from are whatever that chain really produces.
///
/// The bug this pins: the session tab used to pass `root`'s own scale
/// coefficient (`dpi·zoom`) as that append factor, which snapped every cell
/// edge to a `(dpi·zoom)²` grid under a `dpi·zoom` frame — off every device
/// pixel the moment the zoom left 100%, and the ground antialiased through
/// between cells (Q-E8 P10, the OpenCode block-art screenshot).
fn workspace_host_seam_pixels(gpu: &mut Gpu, dpi: f64, zoom: f64, origin: Origin) -> usize {
    let mut view = TerminalView::new(());
    view.set_visible_rows(RowRange {
        first: RowIndex(0),
        count: ROWS,
    });
    view.apply(RenderUpdate::Snapshot {
        snapshot: tui_snapshot(),
    })
    .expect("snapshot");
    let mut engine = TextEngine::new();
    // The fixed adapter's contract: no post-scene append, so the scale is 1.
    view.prepare_style(&mut engine, 1.0);
    let (cell_width, cell_height) = view.cell_size().expect("style prepared");
    let inset = f64::from(koru_session_view::CONTENT_INSET_X);
    let bounds = Rect::new(
        inset,
        0.0,
        inset + f64::from(cell_width) * f64::from(COLUMNS),
        f64::from(cell_height) * f64::from(ROWS),
    );
    let mut scene = Scene::new();
    // The chain as `koru-workspace-host` composes it: one whole-window
    // `Affine::scale(dpi·zoom)` (`redraw`), then the tab content's origin
    // (`MainView::draw`'s `leaf_root`) — and nothing after the scene.
    let root = Affine::scale(dpi * zoom) * Affine::translate((origin.pane, origin.caption));
    view.draw(&mut engine, &mut scene, root, bounds, 1.0);

    let device = dpi * zoom;
    let x0 = (bounds.x0 + origin.pane) * device;
    let y0 = (bounds.y0 + origin.caption) * device;
    let x1 = (bounds.x1 + origin.pane) * device;
    let y1 = (bounds.y1 + origin.caption) * device;
    let width = x1.ceil() as u32 + 2;
    let height = y1.ceil() as u32 + 2;
    let pixels = gpu.raster(
        &scene,
        width,
        height,
        koru_session_view::terminal_background(),
    );
    count_seams(&pixels, width, x0, y0, x1, y1)
}

/// **Both symptoms, on pixels, at the scales the product renders at.**
///
/// Light was the reported one: a white hairline around every run of text, which
/// is the ground leaking between two cell backgrounds that meet mid-pixel. Dark
/// showed the same tear in a colour nobody could see until display zoom made
/// the row pitch fractional too, and then it was a lighter line at every row
/// boundary across the whole grid.
///
/// One rule covers both: the ground is painted once and ordinary cells add no
/// edge to it, and every rect that *is* painted is snapped to `scale × root` —
/// the transform the frame is really rasterized under, not the DPI alone.
#[test]
fn a_full_screen_tui_has_no_seams_in_either_palette() {
    let Some(mut gpu) = Gpu::open() else {
        eprintln!("no hardware wgpu adapter here: the rasterized grid proof did not run");
        return;
    };
    let _guard = THEME_LOCK.lock().unwrap_or_else(|held| held.into_inner());
    let was_dark = theme::is_dark();
    let was_zoom = theme::zoom();
    // Koru Terminal kept this global at 1.0 and took its zoom from the
    // shell's transform. The cell no longer reads the global at all; this is
    // the fixture stated explicitly rather than inherited from the last test.
    theme::set_zoom(1.0);
    let mut failures = Vec::new();
    for dark in [true, false] {
        theme::set_dark(dark);
        for scale in [1.0_f32, 1.25, 1.5, 1.75, 2.0] {
            for zoom in [1.0_f64, 1.1, 1.25] {
                for origin in ORIGINS {
                    let seams = seam_pixels(&mut gpu, scale, zoom, origin);
                    if seams > 0 {
                        failures.push(format!(
                            "dark={dark} scale={scale} zoom={zoom} pane={} caption={}: {seams} seam pixels",
                            origin.pane, origin.caption
                        ));
                    }
                }
            }
        }
    }
    theme::set_dark(was_dark);
    theme::set_zoom(was_zoom);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// **Q-E8 P10 on pixels: koru.exe's session grid at fractional zoom.**
///
/// The reported case is 90% and 110% display zoom at ordinary DPI — cells
/// seamed there and were clean at 100%. The sweep also crosses the zoom with
/// every DPI Windows ships, because the broken snapping was wrong by the
/// *square* of the whole factor: a 125% monitor at 100% zoom seamed for exactly
/// the same reason, whether or not anyone had photographed it yet. Both
/// palettes, and the same dragged-to-a-fraction pane origins as the
/// Koru Terminal sweep — a translation the snap must know about, not a fresh
/// guess.
#[test]
fn the_workspace_host_chain_has_no_seams_at_fractional_zoom() {
    let Some(mut gpu) = Gpu::open() else {
        eprintln!("no hardware wgpu adapter here: the rasterized grid proof did not run");
        return;
    };
    let _guard = THEME_LOCK.lock().unwrap_or_else(|held| held.into_inner());
    let was_dark = theme::is_dark();
    let was_zoom = theme::zoom();
    let mut failures = Vec::new();
    for dark in [true, false] {
        theme::set_dark(dark);
        for dpi in [1.0_f64, 1.25, 1.5, 2.0] {
            // The maintainers' two reported zooms around the clean 100%, and the
            // clamp's far end, where any residual rounding error is largest.
            for zoom in [0.9_f64, 1.0, 1.1, 2.0] {
                // koru.exe mirrors the user zoom into the process global (its
                // zoom indicator reads it), so the real chain runs with it set.
                theme::set_zoom(zoom as f32);
                for origin in ORIGINS {
                    let seams = workspace_host_seam_pixels(&mut gpu, dpi, zoom, origin);
                    if seams > 0 {
                        failures.push(format!(
                            "dark={dark} dpi={dpi} zoom={zoom} pane={} caption={}: {seams} seam pixels",
                            origin.pane, origin.caption
                        ));
                    }
                }
            }
        }
    }
    theme::set_dark(was_dark);
    theme::set_zoom(was_zoom);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
