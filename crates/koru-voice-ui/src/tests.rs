// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use vello::wgpu;
use vello::{AaConfig, AaSupport, Renderer, RendererOptions};

struct Nudge(std::thread::Thread);

impl Wake for Nudge {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

fn block_on<F: Future>(mut future: F) -> F::Output {
    let waker = Waker::from(Arc::new(Nudge(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    // SAFETY: the local future is pinned here and never moved.
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
        // A software rasterizer is not a GPU for these proofs: vello's work
        // crashes WARP, the only adapter on a CI runner, with an access
        // violation. See `crates/koru-session-view/tests/raster.rs`.
        if adapter.get_info().device_type == wgpu::DeviceType::Cpu {
            return None;
        }
        let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("koru-voice-ui-raster"),
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

    fn raster_signature(&mut self, scene: &Scene, width: u32, height: u32) -> u64 {
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
                    base_color: theme::p().bg,
                    width,
                    height,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .expect("Voice raster");
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
            .expect("Voice raster readback");
        let data = slice.get_mapped_range();
        let base = theme::p().bg.to_rgba8();
        let base_rgb = [base.r, base.g, base.b];
        let mut changed = 0usize;
        let mut signature = 0xcbf29ce484222325_u64;
        for y in 0..height {
            let row = &data[(y * stride) as usize..];
            for x in 0..width {
                let start = (x * 4) as usize;
                if row[start..start + 3] != base_rgb {
                    changed += 1;
                }
                for byte in &row[start..start + 4] {
                    signature ^= u64::from(*byte);
                    signature = signature.wrapping_mul(0x100000001b3);
                }
            }
        }
        assert!(changed > 500, "Voice raster contained no visible controls");
        signature
    }
}

fn enabled() -> ViewModel {
    ViewModel {
        saved: Saved::Enabled,
        effective_enabled: true,
        start_on_login: true,
        shortcut: Some("Ctrl+Shift+D".into()),
        ..ViewModel::default()
    }
}

#[test]
fn every_language_has_complete_canonical_voice_copy() {
    for lang in Lang::ALL {
        for key in [
            I18nKey::VoiceGlobalTitle,
            I18nKey::VoiceGlobalNotice,
            I18nKey::VoiceGlobalNoticeNoStartup,
            I18nKey::VoiceGlobalStartup,
            I18nKey::VoiceGlobalEnable,
            I18nKey::VoiceGlobalDisable,
            I18nKey::VoiceGlobalStart,
            I18nKey::VoiceGlobalCancel,
            I18nKey::VoiceGlobalShortcut,
            I18nKey::VoiceGlobalRetry,
            I18nKey::VoiceGlobalOpen,
            I18nKey::VoiceGlobalSettingRefused,
        ] {
            assert!(!t(lang, key).trim().is_empty(), "{lang:?} {key:?}");
        }
    }
}

#[test]
fn notice_is_only_in_the_never_enabled_layout() {
    let never = layout(&ViewModel::default(), WIDTH, false);
    let mut disabled = ViewModel::default();
    disabled.saved = Saved::Disabled;
    let disabled = layout(&disabled, WIDTH, false);
    assert!(never.controls[0].rect.y > disabled.controls[0].rect.y);
}

#[test]
fn a_host_without_startup_ownership_omits_that_control() {
    let mut model = enabled();
    model.start_on_login_available = false;
    let geometry = layout(&model, WIDTH, false);
    assert!(
        geometry
            .controls
            .iter()
            .all(|item| item.control != Control::Startup)
    );

    let mut surface = VoiceSurface::new(Lang::En, model);
    assert!(
        surface
            .key(&Key::Named(NamedKey::Tab), ModifiersState::empty())
            .changed
    );
    assert_eq!(
        surface
            .key(&Key::Named(NamedKey::Enter), ModifiersState::empty())
            .action,
        Some(Action::PickShortcut)
    );
}

#[test]
fn picker_reserves_a_full_text_row_before_controls() {
    let model = enabled();
    {
        let ordinary = layout(&model, WIDTH, false);
        let picker = layout(&model, WIDTH, true);
        assert_eq!(
            picker.controls[0].rect.y,
            ordinary.controls[0].rect.y + 48.0
        );
        let last = picker.controls.last().unwrap().rect;
        assert!(last.y + last.h <= HEIGHT - 18.0);
    }

    for mut model in [ViewModel::default(), enabled()] {
        model.problem = Some(Problem::Disconnected);
        let problem = layout(&model, WIDTH, false);
        let last = problem.controls.last().unwrap().rect;
        assert!(last.y + last.h <= HEIGHT - 18.0);
    }
}

#[test]
fn shared_control_geometry_never_overlaps_and_hits_its_centres() {
    let mut model = enabled();
    model.problem = Some(Problem::Disconnected);
    let geometry = layout(&model, WIDTH, false);
    for (index, left) in geometry.controls.iter().enumerate() {
        for right in geometry.controls.iter().skip(index + 1) {
            assert!(!left.rect.intersects(right.rect));
        }
    }
    let mut surface = VoiceSurface::new(Lang::En, model);
    for item in geometry.controls {
        surface.picking = false;
        let (x, y) = item.rect.centre();
        assert!(surface.pointer_moved(x, y));
        assert!(surface.mouse_down(x, y));
        assert!(surface.mouse_up().is_some());
    }
}

#[test]
fn keyboard_reaches_startup_shortcut_start_and_retry_in_both_directions() {
    let mut model = enabled();
    model.problem = Some(Problem::Refused);
    let mut surface = VoiceSurface::new(Lang::En, model);
    let tab = Key::Named(NamedKey::Tab);
    assert!(surface.key(&tab, ModifiersState::empty()).changed);
    assert_eq!(
        surface
            .key(&Key::Named(NamedKey::Space), ModifiersState::empty())
            .action,
        Some(Action::SetStartOnLogin(false))
    );
    assert!(surface.key(&tab, ModifiersState::empty()).changed);
    assert_eq!(
        surface
            .key(&Key::Named(NamedKey::Enter), ModifiersState::empty())
            .action,
        Some(Action::PickShortcut)
    );
    surface.picking = false;
    assert!(
        surface
            .key(&Key::Named(NamedKey::ArrowDown), ModifiersState::empty())
            .changed
    );
    assert_eq!(
        surface
            .key(&Key::Named(NamedKey::Space), ModifiersState::empty())
            .action,
        Some(Action::Start)
    );
    assert!(surface.key(&tab, ModifiersState::empty()).changed);
    assert_eq!(
        surface
            .key(&Key::Named(NamedKey::Enter), ModifiersState::empty())
            .action,
        Some(Action::Retry)
    );
    assert!(surface.key(&tab, ModifiersState::SHIFT).changed);
    assert_eq!(
        surface
            .key(&Key::Named(NamedKey::Space), ModifiersState::empty())
            .action,
        Some(Action::Start)
    );
}

#[test]
fn picker_accepts_every_settings_shape_and_escape_closes_only_picker() {
    use winit::keyboard::{KeyCode, PhysicalKey};

    let keys = [
        (KeyCode::KeyA, "A"),
        (KeyCode::KeyB, "B"),
        (KeyCode::KeyC, "C"),
        (KeyCode::KeyD, "D"),
        (KeyCode::KeyE, "E"),
        (KeyCode::KeyF, "F"),
        (KeyCode::KeyG, "G"),
        (KeyCode::KeyH, "H"),
        (KeyCode::KeyI, "I"),
        (KeyCode::KeyJ, "J"),
        (KeyCode::KeyK, "K"),
        (KeyCode::KeyL, "L"),
        (KeyCode::KeyM, "M"),
        (KeyCode::KeyN, "N"),
        (KeyCode::KeyO, "O"),
        (KeyCode::KeyP, "P"),
        (KeyCode::KeyQ, "Q"),
        (KeyCode::KeyR, "R"),
        (KeyCode::KeyS, "S"),
        (KeyCode::KeyT, "T"),
        (KeyCode::KeyU, "U"),
        (KeyCode::KeyV, "V"),
        (KeyCode::KeyW, "W"),
        (KeyCode::KeyX, "X"),
        (KeyCode::KeyY, "Y"),
        (KeyCode::KeyZ, "Z"),
        (KeyCode::Digit0, "0"),
        (KeyCode::Digit1, "1"),
        (KeyCode::Digit2, "2"),
        (KeyCode::Digit3, "3"),
        (KeyCode::Digit4, "4"),
        (KeyCode::Digit5, "5"),
        (KeyCode::Digit6, "6"),
        (KeyCode::Digit7, "7"),
        (KeyCode::Digit8, "8"),
        (KeyCode::Digit9, "9"),
        (KeyCode::Space, "Space"),
        (KeyCode::BracketLeft, "["),
        (KeyCode::BracketRight, "]"),
        (KeyCode::Backslash, "\\"),
        (KeyCode::Slash, "/"),
    ];
    assert_eq!(
        keys.iter().map(|(_, name)| *name).collect::<Vec<_>>(),
        koru_settings::DictationShortcut::KEYS
    );
    #[cfg(windows)]
    let modifiers = [
        (
            ModifiersState::CONTROL | ModifiersState::SHIFT,
            "Ctrl+Shift",
        ),
        (ModifiersState::CONTROL | ModifiersState::ALT, "Ctrl+Alt"),
        (ModifiersState::CONTROL | ModifiersState::SUPER, "Ctrl+Win"),
        (ModifiersState::SHIFT | ModifiersState::SUPER, "Shift+Win"),
    ];
    #[cfg(target_os = "macos")]
    let modifiers = [
        (
            ModifiersState::CONTROL | ModifiersState::SHIFT,
            "Ctrl+Shift",
        ),
        (ModifiersState::CONTROL | ModifiersState::ALT, "Ctrl+Option"),
        (ModifiersState::SHIFT | ModifiersState::ALT, "Shift+Option"),
    ];
    #[cfg(not(any(windows, target_os = "macos")))]
    let modifiers = [
        (
            ModifiersState::CONTROL | ModifiersState::SHIFT,
            "Ctrl+Shift",
        ),
        (ModifiersState::CONTROL | ModifiersState::ALT, "Ctrl+Alt"),
    ];
    for (mods, modifier) in modifiers {
        for (physical, name) in keys {
            let mut surface = VoiceSurface::new(Lang::En, enabled());
            surface.picking = true;
            let expected = format!("{modifier}+{name}");
            assert_eq!(
                surface
                    .shortcut_key(PhysicalKey::Code(physical), mods)
                    .action,
                Some(Action::SetShortcut(expected.clone()))
            );
            assert!(koru_settings::DictationShortcut::try_new(expected).is_ok());
        }
    }
    let mut surface = VoiceSurface::new(Lang::En, enabled());
    surface.picking = true;
    assert_eq!(
        surface
            .key(&Key::Named(NamedKey::Escape), ModifiersState::empty())
            .action,
        None
    );
    assert!(!surface.picking());
}

#[test]
fn shortcut_conflict_uses_the_picker_instead_of_a_misleading_retry() {
    let mut model = enabled();
    model.problem = Some(Problem::ShortcutTaken);
    let geometry = layout(&model, WIDTH, false);
    assert!(
        geometry
            .controls
            .iter()
            .any(|item| item.control == Control::Shortcut)
    );
    assert!(
        geometry
            .controls
            .iter()
            .all(|item| item.control != Control::Retry)
    );
}

#[test]
fn pending_truth_disables_every_mutating_control() {
    for pending in [
        Pending::Enable,
        Pending::Disable,
        Pending::Startup,
        Pending::Shortcut,
    ] {
        let mut model = enabled();
        model.pending = Some(pending);
        model.problem = Some(Problem::Refused);
        let surface = VoiceSurface::new(Lang::En, model);
        for control in surface.controls() {
            assert_eq!(surface.action_for(control), None);
        }
    }
}

#[test]
fn pending_settings_never_disable_active_dictation_cancel() {
    for pending in [
        Pending::Enable,
        Pending::Disable,
        Pending::Startup,
        Pending::Shortcut,
    ] {
        let mut model = enabled();
        model.pending = Some(pending);
        model.phase = Phase::Listening;
        let surface = VoiceSurface::new(Lang::En, model);
        assert_eq!(surface.action_for(Control::Start), Some(Action::Cancel));
        assert_eq!(surface.action_for(Control::Startup), None);
    }
}

#[test]
fn close_never_changes_the_enable_model() {
    let model = enabled();
    let mut surface = VoiceSurface::new(Lang::En, model.clone());
    assert_eq!(
        surface
            .key(&Key::Named(NamedKey::Escape), ModifiersState::empty())
            .action,
        Some(Action::Close)
    );
    assert_eq!(surface.model(), &model);
}

#[test]
fn every_required_state_language_and_scale_builds_a_nonempty_scene() {
    let mut states = vec![ViewModel::default()];
    let mut disabled = ViewModel::default();
    disabled.saved = Saved::Disabled;
    states.push(disabled);
    states.push(enabled());
    for phase in [Phase::Preparing, Phase::Listening, Phase::Finalising] {
        let mut model = enabled();
        model.phase = phase;
        states.push(model);
    }
    for problem in [
        Problem::ShortcutTaken,
        Problem::ProtectedTarget,
        Problem::TargetChanged,
        Problem::TargetUnavailable,
        Problem::ConsentRequired,
        Problem::WorkerUnavailable,
        Problem::ModelUnavailable,
        Problem::Disconnected,
        Problem::TimedOut,
        Problem::Refused,
        Problem::SettingRefused,
        Problem::UnsavedRuntime,
    ] {
        let mut model = enabled();
        model.problem = Some(problem);
        states.push(model);
    }
    let mut requested = enabled();
    requested.requested_shortcut = Some("Ctrl+Alt+Space".into());
    requested.pending = Some(Pending::Shortcut);
    states.push(requested);

    for lang in Lang::ALL {
        for scale in [1.0_f64, 1.25, 1.5, 2.0] {
            for model in &states {
                let surface = VoiceSurface::new(lang, model.clone());
                let mut logical = Scene::new();
                let mut engine = TextEngine::new();
                surface.draw(&mut logical, &mut engine, WIDTH, HEIGHT);
                let mut device = Scene::new();
                device.append(&logical, Some(Affine::scale(scale)));
                assert!(
                    !device.encoding().is_empty(),
                    "{lang:?} scale={scale} model={model:?}"
                );
            }
        }
    }
}

#[test]
fn representative_languages_states_and_fractional_scales_reach_real_pixels() {
    let Some(mut gpu) = Gpu::open() else {
        eprintln!("no hardware wgpu adapter here: Voice raster proof did not run");
        return;
    };
    let mut cases = Vec::new();
    for lang in Lang::ALL {
        cases.push((lang, enabled(), 1.25_f64, false));
    }
    for phase in [Phase::Preparing, Phase::Listening, Phase::Finalising] {
        let mut model = enabled();
        model.phase = phase;
        cases.push((Lang::En, model, 1.5, false));
    }
    for problem in [
        Problem::ShortcutTaken,
        Problem::ProtectedTarget,
        Problem::TargetChanged,
        Problem::TargetUnavailable,
        Problem::ConsentRequired,
        Problem::WorkerUnavailable,
        Problem::ModelUnavailable,
        Problem::Disconnected,
        Problem::TimedOut,
        Problem::Refused,
        Problem::SettingRefused,
        Problem::UnsavedRuntime,
    ] {
        let mut model = enabled();
        model.problem = Some(problem);
        cases.push((Lang::En, model, 2.0, false));
    }
    cases.push((Lang::En, ViewModel::default(), 1.0, false));
    let mut no_shortcut = enabled();
    no_shortcut.shortcut = None;
    cases.push((Lang::En, no_shortcut.clone(), 1.25, false));
    cases.push((Lang::En, no_shortcut, 1.25, true));
    let mut signatures = std::collections::HashSet::new();
    let expected = cases.len();
    for (lang, model, scale, picking) in cases {
        let mut surface = VoiceSurface::new(lang, model.clone());
        surface.picking = picking;
        let mut logical = Scene::new();
        let mut engine = TextEngine::new();
        surface.draw(&mut logical, &mut engine, WIDTH, HEIGHT);
        let mut device = Scene::new();
        device.append(&logical, Some(Affine::scale(scale)));
        let signature = gpu.raster_signature(
            &device,
            (f64::from(WIDTH) * scale).round() as u32,
            (f64::from(HEIGHT) * scale).round() as u32,
        );
        assert!(
            signatures.insert(signature),
            "state raster collided: {lang:?} scale={scale} picking={picking} model={model:?}"
        );
    }
    assert_eq!(signatures.len(), expected);
}

#[test]
fn picker_and_no_shortcut_are_explicit_render_states() {
    let mut no_shortcut = enabled();
    no_shortcut.shortcut = None;
    no_shortcut.problem = Some(Problem::ShortcutTaken);
    let mut picker = VoiceSurface::new(Lang::En, no_shortcut.clone());
    picker.picking = true;
    let mut picker_scene = Scene::new();
    picker.draw(&mut picker_scene, &mut TextEngine::new(), WIDTH, HEIGHT);
    let mut fallback_scene = Scene::new();
    VoiceSurface::new(Lang::En, no_shortcut).draw(
        &mut fallback_scene,
        &mut TextEngine::new(),
        WIDTH,
        HEIGHT,
    );
    assert!(!picker_scene.encoding().is_empty());
    assert!(!fallback_scene.encoding().is_empty());
    assert!(picker.picking());
}

#[test]
fn hud_states_reach_distinct_real_pixels() {
    let Some(mut gpu) = Gpu::open() else {
        eprintln!("no hardware wgpu adapter here: HUD raster proof did not run");
        return;
    };
    let mut signatures = std::collections::HashSet::new();
    for state in [
        HudState::recording(0),
        HudState::recording(8),
        HudState::Finalising,
        HudState::Refused,
        HudState::Failed,
    ] {
        let hud = VoiceHud::new(Lang::En, state);
        let mut scene = Scene::new();
        hud.draw(&mut scene, &mut TextEngine::new(), HUD_WIDTH, HUD_HEIGHT);
        let signature = gpu.raster_signature(&scene, HUD_WIDTH as u32, HUD_HEIGHT as u32);
        assert!(
            signatures.insert(signature),
            "HUD raster collided: {state:?}"
        );
    }
}
