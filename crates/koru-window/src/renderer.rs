// SPDX-License-Identifier: MPL-2.0

//! Building vello's `Renderer` without letting a shader-compiler panic take
//! the process with it.
//!
//! Two loops build renderers — `koru-workspace-host`'s multi-window one and this
//! crate's single-window one — and they carried two identical private copies of
//! the construction until the day the construction had to learn something:
//! naga 28's WGSL lowering **intermittently panics** (`no entry found for key`,
//! `front/wgsl/lower/mod.rs`) when vello compiles its shader set on parallel
//! worker threads. The panic escapes `std::thread::scope`, unwinds through
//! `Renderer::new`, and — uncaught — through the winit event loop, which is a
//! dead app three seconds after launch with nothing on screen. The WGSL is
//! fixed per build; what varies per launch is which thread lowers which module
//! in what order, which is why the same machine crashes one morning and not
//! the next.
//!
//! The containment is not clever: catch the unwind, say so loudly, and try
//! again with `num_init_threads = 1` — serial lowering, which sidesteps the
//! parallel path entirely. Only when the serial attempt also fails does the
//! caller get an `Err`, and what it does with one is its own business: the
//! shell shows its fatal dialog, the agent leaves its overlay closed and keeps
//! living in the tray.

use std::num::NonZeroUsize;

use vello::{AaSupport, Renderer, RendererOptions};

/// Which GPU backend this machine draws with. `Auto` is whatever the binary's
/// own default is — DX12 for the shell on Windows, vello's default elsewhere —
/// and the only value most machines ever hold.
///
/// Stored in `renderer.conf` (see
/// [`renderer_state_path`](crate::window_geometry::renderer_state_path)) with
/// the standard `key()`/`parse()` shape: an unknown value reads as `Auto`, so a
/// file from a newer build never breaks an older one.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum RendererBackend {
    #[default]
    Auto,
    Dx12,
    Vulkan,
    Gl,
}

impl RendererBackend {
    /// Every value, in the order a settings row presents them.
    pub const ALL: [Self; 4] = [Self::Auto, Self::Dx12, Self::Vulkan, Self::Gl];

    /// The stored token. A token, never a label — the label is the UI's.
    pub fn key(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dx12 => "dx12",
            Self::Vulkan => "vulkan",
            Self::Gl => "gl",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "dx12" => Self::Dx12,
            "vulkan" => Self::Vulkan,
            "gl" => Self::Gl,
            _ => Self::Auto,
        }
    }

    /// The wgpu backend set this choice names; `None` for `Auto`.
    pub fn backends(self) -> Option<wgpu::Backends> {
        match self {
            Self::Auto => None,
            Self::Dx12 => Some(wgpu::Backends::DX12),
            Self::Vulkan => Some(wgpu::Backends::VULKAN),
            Self::Gl => Some(wgpu::Backends::GL),
        }
    }

    /// What `renderer.conf` says. Missing file, unreadable file and unknown
    /// token all answer `Auto` — a broken conf must never brick a launch.
    pub fn load() -> Self {
        crate::window_geometry::renderer_state_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| Self::from_text(&text))
            .unwrap_or_default()
    }

    /// `key = value` lines, tolerant of blanks, comments and a byte-order mark.
    ///
    /// The rule used to be shared with `user_preferences.rs`, which SP-6b
    /// deleted along with its two parsers. `renderer.conf` keeps it because it
    /// keeps its exemption: it is machine-survival state, read before any
    /// service exists, so it cannot go through the settings authority — a
    /// machine that cannot start the renderer could never be told to use a
    /// different one.
    fn from_text(text: &str) -> Self {
        let mut out = Self::default();
        let pairs = text
            .trim_start_matches('\u{feff}')
            .lines()
            .map(|line| line.trim().trim_start_matches('\u{feff}'))
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| (key.trim(), value.trim()));
        for (key, value) in pairs {
            if key == "backend" {
                out = Self::parse(value);
            }
        }
        out
    }

    /// Write the choice. **One writer**: the shell's Settings tab; everything
    /// else only ever loads. `Auto` is written rather than deleting the file,
    /// so choosing it back leaves a record of the choice having been made.
    pub fn store(self) -> std::io::Result<()> {
        let Some(path) = crate::window_geometry::renderer_state_path() else {
            return Err(std::io::Error::other("no machine-local state directory"));
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(
            path,
            format!(
                "# Which GPU backend Koru draws with on this machine.\n\
                 # auto | dx12 | vulkan | gl — unknown values read as auto.\n\
                 backend = {}\n",
                self.key()
            ),
        )
    }
}

/// The backend set a Koru process should ask wgpu for, or `None` for "the
/// binary's own default".
///
/// An explicit `WGPU_BACKEND` still wins over the conf — it is the diagnostic
/// override and predates the setting. Both are one launch-time read; a change
/// to either applies on the next launch.
/// The adapter that is actually drawing — its name and backend, recorded
/// when the device was created.
///
/// **Recorded rather than re-derived**, because asking again means asking
/// wgpu for an adapter, and creating one to answer a question about the one
/// already open would be absurd. Both halves are opaque strings here: what a
/// *vendor* is, and which vendors are worth telling apart, is knowledge
/// about a product, and this crate holds none.
static ADAPTER: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();

/// Both loops build devices — this crate's single-window one and
/// `koru-workspace-host`'s multi-window one — so both record, and whichever gets
/// there first wins. Public for the second one.
pub fn record_adapter(name: &str, backend: &str) {
    let _ = ADAPTER.set((name.to_owned(), backend.to_owned()));
}

/// `None` before any device has been created — a window that has not drawn
/// yet, or a build whose adapter never answered.
pub fn adapter_description() -> Option<(String, String)> {
    ADAPTER.get().cloned()
}

pub fn configured_backends() -> Option<wgpu::Backends> {
    wgpu::Backends::from_env().or_else(|| RendererBackend::load().backends())
}

/// What `Auto` means, and the **one** definition of it.
///
/// DX12 only on Windows, so the unused Vulkan backend is never probed — its
/// loader emits a registry-layer warning on otherwise healthy machines. Every
/// binary shares this: the whole point of a machine-local backend choice is
/// that the window, the agent's overlay and the chooser all draw the same way,
/// and `Auto` meaning "DX12" in one process and "whatever wgpu enumerates
/// first" in another would break that for the value most people are on.
pub fn default_backends() -> wgpu::Backends {
    #[cfg(target_os = "windows")]
    {
        wgpu::Backends::DX12
    }
    #[cfg(not(target_os = "windows"))]
    {
        wgpu::Backends::default()
    }
}

/// A `RenderContext` on `backends`.
pub fn context_with(backends: wgpu::Backends) -> vello::util::RenderContext {
    vello::util::RenderContext {
        instance: wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends,
            flags: wgpu::InstanceFlags::from_build_config().with_env(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::from_env_or_default(),
        }),
        devices: Vec::new(),
    }
}

/// The `RenderContext` a Koru process starts from: `WGPU_BACKEND`, then the
/// Settings tab's choice, then [`default_backends`].
pub fn context() -> vello::util::RenderContext {
    context_with(configured_backends().unwrap_or_else(default_backends))
}

/// A device and a compiled renderer, made on another thread before anything
/// asks for them.
pub struct Warmed {
    pub context: vello::util::RenderContext,
    /// `None` when no adapter answered or the build failed — both already said
    /// so at the time. The caller falls back to building one itself.
    pub renderer: Option<(usize, Renderer)>,
}

/// The GPU, got ready while the process is still starting.
///
/// **This is what makes a panel summoned by a hotkey instant.** An overlay is
/// created on demand and dropped when it closes, so without this the *first*
/// press pays for an adapter, a device and a compile of every vello pipeline
/// — there is no pipeline cache on DX12 — and what the person sees for those
/// seconds is an empty window. A process that lives in the notification area
/// waiting to be summoned has nothing better to do with its startup, and the
/// window it opens later is the only thing anybody times.
///
/// The renderer survives the window (`Shell` keeps them per device), so this
/// is paid once however many times the overlay is opened.
pub struct Warmup(Option<std::thread::JoinHandle<Warmed>>);

impl Warmup {
    pub fn start() -> Self {
        // The same A/B switch koru.exe reads. With it set nothing is made until
        // the first window asks, and an Agent that never opens one never pays.
        // CI sets it: a runner has no GPU, and every Agent a test starts would
        // otherwise compile vello's pipelines on the software rasterizer — over
        // ten seconds of every core, while the test waits on a 10 s start.
        if koru_env::switch(koru_env::name::KORU_NO_GPU_WARMUP) {
            return Self(None);
        }
        Self(
            std::thread::Builder::new()
                .name("koru-gpu-warmup".into())
                .spawn(|| {
                    let began = std::time::Instant::now();
                    let mut context = context();
                    let dev_id = pollster::block_on(context.device(None));
                    let renderer = match dev_id {
                        None => {
                            tracing::error!(
                                target: "koru::gpu",
                                "gpu warmup found no usable adapter"
                            );
                            None
                        }
                        Some(dev_id) => {
                            let info = context.devices[dev_id].adapter().get_info();
                            record_adapter(&info.name, &format!("{:?}", info.backend));
                            tracing::info!(
                                target: "koru::gpu",
                                backend = ?info.backend,
                                adapter = %info.name,
                                "gpu device ready in {}ms",
                                began.elapsed().as_millis()
                            );
                            let built = build_renderer(&context.devices[dev_id].device);
                            if built.is_ok() {
                                tracing::info!(
                                    target: "koru::gpu",
                                    "gpu pipeline ready to render in {}ms",
                                    began.elapsed().as_millis()
                                );
                            }
                            built.ok().map(|renderer| (dev_id, renderer))
                        }
                    };
                    Warmed { context, renderer }
                })
                .ok(),
        )
    }

    /// Wait for it, or do without. A thread that could not start or panicked
    /// is a slower first open, never a failure to open.
    pub fn take(self) -> Option<Warmed> {
        self.0.and_then(|handle| handle.join().ok())
    }
}

/// Build vello's renderer for one device, absorbing the naga panic.
///
/// On failure of the platform-default attempt (panic **or** error), one
/// unconditional log line and one serial retry. `Err` carries the last failure
/// as text for the caller's dialog.
pub fn build_renderer(device: &wgpu::Device) -> Result<Renderer, String> {
    build_with_retry(&attempt_plan(), |threads| {
        try_build(device, threads).map_err(|e| e.to_string())
    })
}

/// The attempts, in order. macOS already compiles serially (vello's own
/// recommendation), so a "retry serially" would repeat the identical attempt;
/// everywhere else the default is parallel and serial is the genuine fallback.
fn attempt_plan() -> Vec<Option<NonZeroUsize>> {
    if cfg!(target_os = "macos") {
        vec![NonZeroUsize::new(1)]
    } else {
        vec![None, NonZeroUsize::new(1)]
    }
}

/// One construction attempt. This is the body both loops used to carry
/// privately, with the thread count parameterised.
fn try_build(
    device: &wgpu::Device,
    num_init_threads: Option<NonZeroUsize>,
) -> Result<Renderer, vello::Error> {
    Renderer::new(
        device,
        RendererOptions {
            use_cpu: false,
            antialiasing_support: AaSupport::area_only(),
            num_init_threads,
            // **Not available here.** `wgpu::Features::PIPELINE_CACHE` is
            // documented as Vulkan-only and explicitly unimplemented on DX12,
            // which is what wgpu selects on Windows — so a cache to avoid
            // recompiling these pipelines every launch cannot be had without
            // also choosing the backend. Written down because it is the
            // obvious next idea and it does not work.
            pipeline_cache: None,
        },
    )
}

/// Run `attempt` over `plan`, converting a panic into a failed attempt.
///
/// `AssertUnwindSafe` is justified rather than waved through: the panic
/// originates on vello's scoped worker threads and re-surfaces through
/// `std::thread::scope`, so the partially built engine is dropped whole before
/// the unwind reaches this frame; the `wgpu::Device` the caller retains is
/// internally refcounted with non-poisoning locks, so a failed attempt leaves
/// it exactly as usable as before the call.
fn build_with_retry<T>(
    plan: &[Option<NonZeroUsize>],
    mut attempt: impl FnMut(Option<NonZeroUsize>) -> Result<T, String>,
) -> Result<T, String> {
    let mut last = String::from("no attempt was made");
    for (index, &threads) in plan.iter().enumerate() {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| attempt(threads))) {
            Ok(Ok(value)) => return Ok(value),
            Ok(Err(message)) => last = message,
            Err(payload) => last = panic_message(payload.as_ref()),
        }
        // Rare and consequential, so unconditional — and to both sinks,
        // because a release build has no console and a dev run has no log.
        let retrying = index + 1 < plan.len();
        let outcome = if retrying {
            "retrying with serial shader compilation"
        } else {
            "giving up"
        };
        // `target: "koru::gpu"`: the binaries filter to `koru=info` with a
        // `warn` default, so a `koru::`-prefixed target is what guarantees the
        // line lands in the rotated log wherever this crate is linked.
        tracing::error!(target: "koru::gpu", "gpu renderer build failed ({last}); {outcome}");
        eprintln!("[Koru][gpu] renderer build failed ({last}); {outcome}");
    }
    Err(last)
}

/// The text of a panic payload, which is a `&str` or a `String` in every case
/// naga produces and opaque otherwise.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        String::from("panic with a non-text payload")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Windows plan is parallel-then-serial; a panic on the first attempt
    /// must reach the second and succeed there.
    #[test]
    fn a_panic_on_the_parallel_attempt_is_survived_by_the_serial_one() {
        let plan = [None, NonZeroUsize::new(1)];
        let mut calls = Vec::new();
        let result = build_with_retry(&plan, |threads| {
            calls.push(threads);
            if threads.is_none() {
                panic!("no entry found for key");
            }
            Ok(42)
        });
        assert_eq!(result, Ok(42));
        assert_eq!(calls, vec![None, NonZeroUsize::new(1)]);
    }

    /// Both attempts failing hands the caller the last failure's text — the
    /// serial one, which is the one worth showing in a dialog.
    #[test]
    fn two_panics_surface_the_last_message() {
        let plan = [None, NonZeroUsize::new(1)];
        let result: Result<(), String> = build_with_retry(&plan, |threads| {
            if threads.is_none() {
                panic!("parallel failure");
            }
            panic!("serial failure");
        });
        assert_eq!(result, Err(String::from("serial failure")));
    }

    /// A first-attempt success makes exactly one attempt.
    #[test]
    fn a_success_makes_no_second_attempt() {
        let plan = [None, NonZeroUsize::new(1)];
        let mut calls = 0;
        let result = build_with_retry(&plan, |_| {
            calls += 1;
            Ok("fine")
        });
        assert_eq!(result, Ok("fine"));
        assert_eq!(calls, 1);
    }

    /// An `Err` (not a panic) also moves to the next attempt — a vello error
    /// and a naga panic deserve the same second chance.
    #[test]
    fn an_error_result_also_retries() {
        let plan = [None, NonZeroUsize::new(1)];
        let result = build_with_retry(&plan, |threads| {
            if threads.is_none() {
                Err(String::from("vello said no"))
            } else {
                Ok(7)
            }
        });
        assert_eq!(result, Ok(7));
    }

    /// `String` and `&str` payloads both come through as their text.
    #[test]
    fn panic_payload_text_is_recovered() {
        let of_str = std::panic::catch_unwind(|| panic!("as &str")).unwrap_err();
        assert_eq!(panic_message(of_str.as_ref()), "as &str");
        let text = String::from("as String");
        let of_string = std::panic::catch_unwind(move || std::panic::panic_any(text)).unwrap_err();
        assert_eq!(panic_message(of_string.as_ref()), "as String");
    }

    /// The stored token round-trips, and anything unknown — a newer build's
    /// value, a typo — reads as `Auto` rather than failing the launch.
    #[test]
    fn a_backend_token_round_trips_and_unknowns_read_as_auto() {
        for backend in RendererBackend::ALL {
            assert_eq!(RendererBackend::parse(backend.key()), backend);
        }
        assert_eq!(RendererBackend::parse("metal-next"), RendererBackend::Auto);
        assert_eq!(RendererBackend::parse(""), RendererBackend::Auto);
        assert_eq!(RendererBackend::parse(" Vulkan "), RendererBackend::Vulkan);
    }

    /// The conf is the tolerant `key = value` shape every other conf uses; a
    /// missing key or an unrelated file is `Auto`.
    #[test]
    fn the_conf_reads_like_every_other_conf() {
        assert_eq!(
            RendererBackend::from_text("\u{feff}# machine renderer\nbackend = vulkan\n"),
            RendererBackend::Vulkan
        );
        assert_eq!(
            RendererBackend::from_text("theme = dark\n"),
            RendererBackend::Auto
        );
        assert_eq!(RendererBackend::from_text(""), RendererBackend::Auto);
    }

    /// `Auto` names no backend set — it is the caller's default — and the
    /// named ones name exactly theirs.
    #[test]
    fn auto_defers_to_the_binary_default() {
        assert_eq!(RendererBackend::Auto.backends(), None);
        assert_eq!(
            RendererBackend::Vulkan.backends(),
            Some(wgpu::Backends::VULKAN)
        );
    }

    /// macOS is already serial, so its plan has no pointless second identical
    /// attempt; everywhere else parallel is tried first.
    #[test]
    fn the_plan_matches_the_platform() {
        let plan = attempt_plan();
        if cfg!(target_os = "macos") {
            assert_eq!(plan, vec![NonZeroUsize::new(1)]);
        } else {
            assert_eq!(plan, vec![None, NonZeroUsize::new(1)]);
        }
    }
}
