// SPDX-License-Identifier: MPL-2.0

//! **The microphone, and the one thing this process is allowed to say about
//! it.**
//!
//! The worker opens the device itself and PCM never crosses the socket
//! ([ADR-249](../../../docs/decisions/ADR-226-250.md)), so this module is the
//! only place in the workspace that links `cpal`. Everything below the callback
//! is [`crate::ring`] and [`crate::resample`]; everything above it is
//! [`crate::worker`].
//!
//! ## A device has a category and a name, and only the category may be said
//!
//! A capture device's *name* is a person's hardware — `Steven's AirPods`,
//! `Yeti Nano`, the make and model of a laptop — and `docs/contributing/operations/LOGGING.md`
//! is explicit that a log carries bounded categorical states rather than that.
//! So [`DeviceCategory`] (declared in `koru_voice::status`) is built here from
//! `cpal`'s own `DeviceType` and `InterfaceType` enums and **never holds the
//! name at all**: it cannot leak a
//! name it does not have, which is the same reasoning as
//! `koru_voice::Transcript`'s redacted `Debug`. `a_category_never_carries_the_device_name`
//! is what says so, against a description built with a name in every field.
//!
//! ## A name may be *shown* and may not be *written down*
//!
//! [`input_devices`] answers names, and it is the one function here that does.
//! That is not a relaxation of the paragraph above: a picker cannot be drawn
//! out of categories, so somebody choosing between two microphones has to be
//! shown what they are called. What the rule was always about is the *log* —
//! a support file somebody attaches to a report — and that is unchanged. No
//! `tracing` call in this module takes a name, [`DeviceCategory`] still cannot
//! hold one, and `koru_voice::input`'s own note carries the same split.
//!
//! ## `no-capture`
//!
//! With the feature on, [`open`], [`describe_default`] and [`input_devices`]
//! all refuse or answer a fixture before touching `cpal` — no host, no
//! enumeration, no stream. **Enumerating is behind the same guard as opening**,
//! and deliberately: `cpal`'s `input_devices` initialises the host, which on
//! WASAPI means COM and on CoreAudio means the audio server, and a suite that
//! merely *listed* microphones would still be a suite that woke the audio
//! stack on somebody's machine. The feature is on in this package's
//! `[dev-dependencies]`, so `cargo test` cannot make a microphone light come
//! on. See the feature's note in `Cargo.toml` for why it is a feature and not
//! a `cfg(test)`.
//!
//! ## The one unproven mechanism in the roadmap
//!
//! On macOS the consent prompt should say *Koru* rather than naming an
//! executable nobody has heard of, because a binary inside
//! `Koru.app/Contents/MacOS/` inherits the bundle's TCC identity. That is
//! verified by hand rather than by a test, and the documented fallback — moving
//! capture into `koru.exe` and putting PCM on a separate raw channel — costs
//! the privacy story. See `docs/features/voice/archive/QUESTIONS.md`.

use std::sync::Arc;

use koru_voice::{ErrorCode, InputChoice, InputDevice, VoiceError};
// Only the opening bodies name a resolution; under `no-capture` there are
// none, and an import that is there for a `cfg`-ed body is a warning on the
// build that does not have it.
#[cfg(not(feature = "no-capture"))]
use koru_voice::InputResolution;

use koru_voice::status::{CaptureInfo, DeviceCategory, DeviceKind, InterfaceKind};

use crate::ring::Ring;

// The device categories are declared in `koru_voice::status`, beside the
// status report that carries them, so the Agent can read a report without
// linking this crate. What stays here is reading them out of `cpal`, which is
// the half that touches a device.

/// Read the two categories out of a `cpal` description and **drop
/// everything else** — the name, the manufacturer, the driver, the
/// address, the extended lines. Each of those is either a person's hardware
/// or a string a driver wrote, and neither belongs in a support log.
pub fn category_of(description: &cpal::DeviceDescription) -> DeviceCategory {
    DeviceCategory {
        device_type: kind_of(description.device_type()),
        interface_type: interface_of(description.interface_type()),
    }
}

/// `cpal`'s answer as ours. The wildcard is load-bearing: the enum is
/// `#[non_exhaustive]`, and `unknown` is a true answer rather than a lossy
/// one.
fn kind_of(kind: cpal::DeviceType) -> DeviceKind {
    match kind {
        cpal::DeviceType::Speaker => DeviceKind::Speaker,
        cpal::DeviceType::Microphone => DeviceKind::Microphone,
        cpal::DeviceType::Headphones => DeviceKind::Headphones,
        cpal::DeviceType::Headset => DeviceKind::Headset,
        cpal::DeviceType::Earpiece => DeviceKind::Earpiece,
        cpal::DeviceType::Handset => DeviceKind::Handset,
        cpal::DeviceType::HearingAid => DeviceKind::HearingAid,
        cpal::DeviceType::Dock => DeviceKind::Dock,
        cpal::DeviceType::Tuner => DeviceKind::Tuner,
        cpal::DeviceType::Virtual => DeviceKind::Virtual,
        _ => DeviceKind::Unknown,
    }
}

fn interface_of(kind: cpal::InterfaceType) -> InterfaceKind {
    match kind {
        cpal::InterfaceType::BuiltIn => InterfaceKind::BuiltIn,
        cpal::InterfaceType::Usb => InterfaceKind::Usb,
        cpal::InterfaceType::Bluetooth => InterfaceKind::Bluetooth,
        cpal::InterfaceType::Pci => InterfaceKind::Pci,
        cpal::InterfaceType::FireWire => InterfaceKind::FireWire,
        cpal::InterfaceType::Thunderbolt => InterfaceKind::Thunderbolt,
        cpal::InterfaceType::Hdmi => InterfaceKind::Hdmi,
        cpal::InterfaceType::Line => InterfaceKind::Line,
        cpal::InterfaceType::Spdif => InterfaceKind::Spdif,
        cpal::InterfaceType::Network => InterfaceKind::Network,
        cpal::InterfaceType::Virtual => InterfaceKind::Virtual,
        cpal::InterfaceType::DisplayPort => InterfaceKind::DisplayPort,
        cpal::InterfaceType::Aggregate => InterfaceKind::Aggregate,
        _ => InterfaceKind::Unknown,
    }
}

/// A running capture. Dropping it stops the device.
///
/// `cpal::Stream` is deliberately not `Send` on every backend, so this type is
/// not either — it is built on the inference thread, held there, and dropped
/// there. That is not a limitation to work around: the thread that performs
/// `Effect::StartCapture` and `Effect::StopCapture` is the thread that should
/// own the device.
pub struct Capture {
    #[allow(dead_code)]
    stream: cpal::Stream,
    info: CaptureInfo,
}

impl Capture {
    pub fn info(&self) -> CaptureInfo {
        self.info
    }
}

impl std::fmt::Debug for Capture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `cpal::Stream`'s own `Debug` is a raw handle; the category is the part
        // a reader can do something with.
        f.debug_struct("Capture").field("info", &self.info).finish()
    }
}

/// The category of the default input device, without opening a stream.
///
/// `None` when there is no device, when the host will not answer, or under
/// `no-capture`. Called once when the worker starts so that `status` can answer
/// before anybody has dictated — enumerating is not opening, and a status that
/// said `unknown` until the first session would be least useful exactly when
/// somebody is asking why the first session failed.
#[cfg(not(feature = "no-capture"))]
pub fn describe_default() -> Option<DeviceCategory> {
    use cpal::traits::{DeviceTrait as _, HostTrait as _};
    let device = cpal::default_host().default_input_device()?;
    Some(category_of(&device.description().ok()?))
}

/// `None`, under `no-capture` — a build that opens no device enumerates none
/// either.
#[cfg(feature = "no-capture")]
pub fn describe_default() -> Option<DeviceCategory> {
    None
}

/// **Every capture device this machine has, with the host's own choice
/// flagged.**
///
/// The list a picker draws, and the list [`koru_voice::input::resolve`] reads
/// a stored name against. In the host's own order, because that order means
/// something to the operating system and nothing here is better placed to
/// improve on it — and *not* deduplicated, because two identical microphones
/// really are two devices and hiding one would make the second unselectable.
///
/// A device whose name cannot be read is skipped rather than listed as
/// *unknown*: a row nobody can identify is a row nobody can choose, and a
/// choice stored against an empty name would match the next unreadable device
/// instead.
#[cfg(not(feature = "no-capture"))]
pub fn input_devices() -> Vec<InputDevice> {
    use cpal::traits::{DeviceTrait as _, HostTrait as _};
    let host = cpal::default_host();
    // The default's *name*, taken now, so the flag is a comparison rather
    // than a second enumeration. `description` is cpal's only accessor for a
    // name: `DeviceTrait` has `id` and `description`, and nothing else.
    let default = host
        .default_input_device()
        .and_then(|device| device.description().ok())
        .map(|described| described.name().to_owned());
    let Ok(devices) = host.input_devices() else {
        // A host that will not enumerate is *no list*, which is the same
        // reading the tray already takes of a cache it cannot open: what a
        // person can act on is "nothing here", and the surface draws the
        // system-default row it always has.
        tracing::debug!("this host would not enumerate its input devices");
        return Vec::new();
    };
    devices
        .filter_map(|device| {
            let described = device.description().ok()?;
            let name = described.name().to_owned();
            let default = default.as_deref() == Some(name.as_str());
            Some(InputDevice::new(name, default))
        })
        .collect()
}

/// **A fixture, under `no-capture`.**
///
/// Two devices, one of them the default. It is a fixture rather than an empty
/// list because every test above this crate needs a list to resolve a stored
/// name *against*, and a suite that could only ever see nothing would prove
/// only the fall-back path. `a_test_build_enumerates_a_fixture_and_no_device`
/// is what says this is the body a test binary gets.
#[cfg(feature = "no-capture")]
pub fn input_devices() -> Vec<InputDevice> {
    FIXTURE_DEVICES
        .iter()
        .map(|(name, default)| InputDevice::new(*name, *default))
        .collect()
}

/// The fixture [`input_devices`] answers under `no-capture`. Spelled here so a
/// test can name a device without repeating a literal.
#[cfg(feature = "no-capture")]
pub const FIXTURE_DEVICES: [(&str, bool); 2] = [
    ("Microphone Array (Realtek(R) Audio)", true),
    ("Yeti Nano", false),
];

/// **Open the microphone somebody chose, or the host's own.**
///
/// The resolution happens *here*, against a list read at this moment, because
/// that is the only moment at which "is that headset plugged in" has an
/// answer. A name that is no longer present is
/// [`InputResolution::FellBack`]: the default device is opened and the
/// fall-back is logged as a *fact that it happened*, never with the name —
/// see the module note.
///
/// A dictation is never refused for this. Somebody whose headset is asleep
/// gets the built-in microphone and a sentence in the settings section, which
/// is a worse recording and a working feature; the alternative is a feature
/// that stops for a reason nobody would connect to the headset.
#[cfg(not(feature = "no-capture"))]
pub fn open(choice: &InputChoice, ring: Arc<Ring>) -> Result<Capture, VoiceError> {
    let resolved = koru_voice::input::resolve(choice, &input_devices());
    if resolved.missing().is_some() {
        tracing::info!(
            "the chosen input device is not present; opening this machine's default instead"
        );
    }
    open_resolved(&resolved, ring)
}

/// Refuse, under `no-capture`. See the module note and the other body.
#[cfg(feature = "no-capture")]
pub fn open(_choice: &InputChoice, _ring: Arc<Ring>) -> Result<Capture, VoiceError> {
    Err(ErrorCode::CaptureFailed
        .with("this build was compiled with `no-capture`, so it opens no audio device"))
}

/// **Open the default input device and start filling `ring`.**
///
/// Everything between the device and the ring happens inside the callback:
/// interleaved frames are averaged to mono, resampled to
/// [`koru_voice::SAMPLE_RATE`], and pushed one sample at a time. No allocation,
/// no lock, and no branch that can wait — see [`crate::ring`].
///
/// The stream is returned already playing: `cpal` hands back a stopped stream
/// and a caller that forgot `play` would get a session that listens to silence
/// forever, which looks exactly like a microphone that is muted.
///
/// **Two bodies, chosen at compile time rather than one body with a runtime
/// guard**, so that under `no-capture` the `cpal` calls are not merely
/// unreached — they are not in the test binary at all. A runtime guard is a
/// line somebody can move; an absent call cannot be reached by any rearranging.
#[cfg(not(feature = "no-capture"))]
pub fn open_default(ring: Arc<Ring>) -> Result<Capture, VoiceError> {
    open_resolved(&InputResolution::SystemDefault, ring)
}

/// The body both entry points share: a resolved choice, opened.
#[cfg(not(feature = "no-capture"))]
fn open_resolved(resolved: &InputResolution, ring: Arc<Ring>) -> Result<Capture, VoiceError> {
    use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};

    let host = cpal::default_host();
    let device = match resolved.device() {
        // Re-found by name at the moment of opening rather than carried as a
        // `cpal::Device` from the enumeration: the list was built to be shown
        // to somebody, and a device handle held across that is a handle to
        // hardware that may have gone in between.
        Some(wanted) => host
            .input_devices()
            .ok()
            .and_then(|mut devices| {
                devices.find(|device| {
                    device
                        .description()
                        .is_ok_and(|described| described.name() == wanted)
                })
            })
            .ok_or_else(|| {
                ErrorCode::CaptureFailed
                    .with("the chosen input device went away while it was being opened")
            })?,
        None => host.default_input_device().ok_or_else(|| {
            ErrorCode::CaptureFailed.with("this machine has no default input device")
        })?,
    };
    let category = device
        .description()
        .map(|d| category_of(&d))
        .unwrap_or(DeviceCategory::UNKNOWN);
    let supported = device.default_input_config().map_err(|error| {
        refusal(&error).with(format!("the default input config was refused: {error}"))
    })?;
    let format = supported.sample_format();
    let config = supported.config();
    let info = CaptureInfo {
        category,
        device_sample_rate: config.sample_rate,
        device_channels: config.channels,
        engine_sample_rate: koru_voice::SAMPLE_RATE,
    };

    let stream = match format {
        cpal::SampleFormat::F32 => build::<f32>(&device, &config, &ring),
        cpal::SampleFormat::I16 => build::<i16>(&device, &config, &ring),
        cpal::SampleFormat::U16 => build::<u16>(&device, &config, &ring),
        cpal::SampleFormat::I32 => build::<i32>(&device, &config, &ring),
        cpal::SampleFormat::I8 => build::<i8>(&device, &config, &ring),
        cpal::SampleFormat::U8 => build::<u8>(&device, &config, &ring),
        cpal::SampleFormat::F64 => build::<f64>(&device, &config, &ring),
        // Named rather than swallowed: a device offering only a format we do
        // not convert is a real answer somebody can act on, and the
        // alternative — a silent stream — looks like a broken microphone.
        other => Err(ErrorCode::CaptureFailed.with(format!(
            "the device offers only the {other:?} sample format"
        ))),
    }?;
    stream.play().map_err(|error| {
        refusal(&error).with(format!("the capture stream would not start: {error}"))
    })?;
    tracing::debug!(
        category = %info.category,
        device_sample_rate = info.device_sample_rate,
        device_channels = info.device_channels,
        "capture opened"
    );
    Ok(Capture { stream, info })
}

/// Refuse, under `no-capture`. See the module note and the other body.
#[cfg(feature = "no-capture")]
pub fn open_default(_ring: Arc<Ring>) -> Result<Capture, VoiceError> {
    Err(ErrorCode::CaptureFailed
        .with("this build was compiled with `no-capture`, so it opens no audio device"))
}

/// Build the stream for one sample format.
///
/// Generic over the format because the conversion is `dasp`'s and the rest is
/// identical — seven near-copies of this function is how a fix reaches six of
/// them.
#[cfg(not(feature = "no-capture"))]
fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    ring: &Arc<Ring>,
) -> Result<cpal::Stream, VoiceError>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    use cpal::traits::DeviceTrait as _;

    let ring = ring.clone();
    let mix = crate::resample::Downmix::new(config.channels);
    let mut resampler =
        crate::resample::Resampler::new(config.sample_rate, koru_voice::SAMPLE_RATE);
    let channels = mix.channels();

    device
        .build_input_stream(
            config.clone(),
            move |data: &[T], _info: &cpal::InputCallbackInfo| {
                // **The real-time path.** Nothing here allocates, locks or can
                // wait. The `chunks_exact` is over borrowed data, the mix is an
                // average, the resampler holds three numbers, and the ring's
                // push is two atomic stores.
                for frame in data.chunks_exact(channels) {
                    let mut sum = 0.0f32;
                    for sample in frame {
                        sum += <f32 as cpal::FromSample<T>>::from_sample_(*sample);
                    }
                    resampler.push(sum / channels as f32, |sample| ring.push(sample));
                }
            },
            |error| {
                // A device error is not a session failure by itself — the
                // session fails when the audio stops arriving — so this is a
                // `warn` naming the kind and never the device.
                tracing::warn!(kind = ?error.kind(), "the capture stream reported an error");
            },
            None,
        )
        .map_err(|error| {
            refusal(&error).with(format!("the capture stream would not open: {error}"))
        })
}

/// **Was the microphone refused, or was it merely unavailable?**
///
/// The split matters because only one of the two answers has anything a person
/// can do about it, and it is not the one they would guess: a refused
/// microphone is a switch in the operating system's privacy settings, and a
/// build that could only say *the microphone would not open* sends somebody to
/// look at their hardware.
///
/// Two sources, in order, because neither is enough on its own.
///
/// **`cpal::ErrorKind::PermissionDenied` first.** It is a real variant in
/// cpal 0.18 and the CoreAudio, ALSA and PulseAudio backends all produce it —
/// so on macOS, where this matters most, a TCC denial arrives already
/// classified and nothing here has to guess.
///
/// **Then the message, for WASAPI only, which does not.** cpal's
/// `From<windows::core::Error>` maps a dozen `AUDCLNT_*` codes and lets
/// everything else fall to `BackendError`, and a per-app privacy denial on
/// Windows is `E_ACCESSDENIED` — not an `AUDCLNT_*` code at all. What reaches
/// us is `std::io::Error::from_raw_os_error(hresult)` rendered by `Display`,
/// which is `"<localized text> (os error <hresult>)"`.
///
/// **The needle is the number, not the sentence.** The human half is in the
/// machine's own language, so matching on *Access is denied* would work on an
/// English installation and silently stop working on a German one. The number
/// is `windows_result`'s own conversion — `from_raw_os_error(code().0)`, the
/// whole HRESULT rather than its Win32 half — so `E_ACCESSDENIED` arrives as
/// **-2147024891**, and that is what is matched.
/// [`ACCESS_DENIED_RENDERINGS`] is the list, and
/// `the_denial_needle_is_what_windows_actually_renders` builds one of them
/// from the HRESULT rather than trusting this paragraph.
///
/// Written as a free function over the message so it is testable without an
/// audio device.
pub fn refusal(error: &cpal::Error) -> ErrorCode {
    if error.kind() == cpal::ErrorKind::PermissionDenied {
        return ErrorCode::PermissionDenied;
    }
    match error.message() {
        Some(message) if reads_as_denied(message) => ErrorCode::PermissionDenied,
        _ => ErrorCode::CaptureFailed,
    }
}

/// Every way `E_ACCESSDENIED` is spelled where it can reach us.
///
/// The first is what Windows renders today (see [`refusal`]); the second and
/// third are the HRESULT written the two other ways a backend might print it.
/// The fourth is the Win32 half, parenthesised so that `os error 55` — a
/// device already in use — cannot match a prefix of it, which is a mistake this
/// list has already made once.
pub const ACCESS_DENIED_RENDERINGS: [&str; 4] = [
    "os error -2147024891",
    "0x80070005",
    "e_accessdenied",
    "(os error 5)",
];

/// Whether a backend's message names an access denial. See [`refusal`] for why
/// this matches a number rather than a sentence.
pub fn reads_as_denied(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    ACCESS_DENIED_RENDERINGS
        .iter()
        .any(|needle| lowered.contains(needle))
}

/// The loudest sample in a buffer, as one of nine steps.
///
/// **A bucket, never a float**, and that is `koru_voice::wire::LevelBucket`'s
/// privacy decision rather than a compression one: a per-frame amplitude is a
/// signal, and a signal is something an attacker with the transcript stream can
/// do more with than a meter needs.
///
/// The scale is logarithmic because loudness is: a linear meter spends eight of
/// its nine steps on the top 10 dB and reads as dead for ordinary speech. Below
/// -60 dBFS is [`koru_voice::LevelBucket::SILENT`].
pub fn level_of(samples: &[f32]) -> koru_voice::LevelBucket {
    let peak = samples.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
    if peak <= 0.0 {
        return koru_voice::LevelBucket::SILENT;
    }
    let db = 20.0 * peak.log10();
    if db <= -60.0 {
        return koru_voice::LevelBucket::SILENT;
    }
    // -60 dB..0 dB across the eight steps above silence.
    let step = ((db + 60.0) / 60.0 * koru_voice::LevelBucket::MAX as f32).ceil();
    koru_voice::LevelBucket::new(step.clamp(0.0, koru_voice::LevelBucket::MAX as f32) as u8)
}

#[cfg(test)]
mod tests;
