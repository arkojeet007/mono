// SPDX-License-Identifier: MPL-2.0

//! **Holds the provisional text and emits what the focused surface should do
//! about it. Names no widget and opens no microphone.**
//!
//! ```text
//!   press ──► Begin ──► the host routes it, then answers `began(accepted)`
//!     │
//!     ├── Event::Partial ──► Update   (coalesced: at most one per drain)
//!     │
//!   release ──► `finish` is requested; the stage becomes Finalising and the
//!     │         composition stays provisional
//!     │
//!     ├── Event::Final  + Event::Ended{Completed} ──► Commit
//!     └── anything else                            ──► Cancel
//! ```
//!
//! ## Why this is not in the shell, and not in the client
//!
//! The client ([`koru_voice_client`]) dials a worker and hands back events. A
//! view knows how to compose text. Between them sits one question neither can
//! answer alone: **which of the four things should the focused surface do
//! right now, and does this message still belong to the dictation it is
//! doing?** That is a state machine over generations, and it is the same
//! machine on a phone, where there is no worker and no socket — so it is a
//! crate rather than a module in `koru-workspace-host`.
//!
//! ## Every action carries the session's generation
//!
//! [`DictationAction`] has four variants and all four carry a `generation`.
//! That is not decoration. The hard case is not two dictations with different
//! ids — it is **a `Final` in flight when its session was cancelled, arriving
//! while the next one is open**
//! ([ADR-249](../../../docs/decisions/ADR-226-250.md#adr-249-voiceworker--a-voice-session-is-a-pure-state-machine-and-the-worker-performs-its-effects)).
//! The controller drops those on the way in; carrying the generation out means
//! a router can drop them a second time, against the composition it actually
//! opened, rather than trusting that this crate never has a bug.
//!
//! ## An interruption discards, and so does a local event
//!
//! Losing window focus, closing the window, sleeping, shutting down: nothing
//! is written. Half a sentence landing in a document you were not looking at
//! is worse than losing it, and it is also the only answer that survives the
//! case where you switch applications mid-hold and the key release never comes
//! back.
//!
//! **A click or a keystroke arriving before the matching `Final` discards
//! too.** Plan 2.3 gives a surface its own rule — an ordinary local event
//! *commits* an open composition, which is what a keyboard IME does — and this
//! plan says only a matching `Final` may commit. In the finalising window no
//! `Final` has arrived, so both rules point at once. The controller pre-empts,
//! which [ADR-248](../../../docs/decisions/ADR-226-250.md#adr-248-voicecomposition--dictation-is-an-ephemeral-composition-and-only-the-final-transcript-is-a-transaction)
//! anticipates in as many words: the composition is discarded first, so by the
//! time the surface sees the click there is nothing left there to settle. The
//! two alternatives were rejected on the record — holding the click until the
//! result arrives makes the app feel blocked at exactly the moment somebody is
//! trying to do something else, and committing the latest partial writes text
//! the engine had not finished revising and quietly repeals the
//! matching-`Final` rule. The same rule is applied while still *listening*,
//! which the plan does not specify and toggle mode makes ordinary — see Q-V60.
//!
//! ## Nothing here wakes on a timer
//!
//! Partials arrive because `koru-voice-client`'s reader thread wakes the
//! host's loop through the closure the host installed — an `EventLoopProxy`
//! in Koru. [`Dictation::deadline`] exists for the two *bounds* below, and is
//! `None` whenever nothing is running, so an idle process is never woken by
//! this crate.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// **`available` and `disabled` travel with the controller**, so a host that
/// already links this crate asks `koru_voice`'s one predicate rather than
/// reading `KORU_VOICE_DISABLED` a second time. Four surfaces have to agree
/// about the kill switch and each of them reaching for `koru_env::switch` on
/// its own is how three end up right and the fourth offers a feature nothing
/// will serve (Q-V44).
pub use koru_voice_client::{
    DISABLE_VAR, EndReason, ErrorCode, Event, InputChoice, InputDevice, InputResolution,
    LevelBucket, Live, ModelId, ModelTier, Off, Requested, SessionId, State, Transcript,
    VoiceClient, VoiceError, VoiceLanguage, available, disabled, off,
};

/// **Whether this build's controller is allowed to reach a worker at all.**
///
/// `false` in a shipped binary. `true` under this crate's own tests and in any
/// crate above it that turned on the `no-engine` feature in its
/// `[dev-dependencies]` — which every crate whose suite constructs a
/// [`Dictation`] must, because a dependency is never compiled with
/// `cfg(test)`. A test above asserts this is `true`, the way
/// `a_surface_built_under_test_dials_nothing` does for `koru-search-ui`.
pub const ENGINE_OFF: bool = cfg!(test) || cfg!(feature = "no-engine");

/// **How long one hold may last.**
///
/// A key that is never released is not a hypothetical: a hotkey that consumes
/// its own release, an application switch mid-hold, a wedged keyboard driver.
/// Without a bound the microphone stays open until the process ends. Two
/// minutes is far past the longest thing anybody dictates into a text field
/// and short enough that a stuck key is not a recording session.
///
/// It **finishes** rather than cancels: the words were really said, and the
/// person is more likely to want them than to want them thrown away.
pub const HOLD_CAP: Duration = Duration::from_secs(120);

/// **How long finalisation may take before the utterance is given up.**
///
/// `finish` is a request to the engine, and an engine can wedge, take an
/// unbounded time on a long utterance, or be inside a worker that has just
/// died in a way the socket has not noticed. The composition is provisional
/// the whole time, so the cost of waiting is a surface that will not settle.
/// Ending it is a discard, because a partial is text the engine had not
/// finished revising.
pub const FINALISE_TIMEOUT: Duration = Duration::from_secs(10);

/// **What the focused surface should do about the provisional text.**
///
/// Four verbs, the same four a keyboard IME uses, and the same four
/// `koru_ui::dictation::DictationTarget` answers. Every one carries the
/// generation of the session that produced it — see the module note.
///
/// The transcript is a [`Transcript`] rather than a `String` so that the
/// redaction survives all the way to the view: a `{:?}` on an action, added
/// by somebody chasing something else, prints a character count and not what
/// was said.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DictationAction {
    /// Open a composition over the current selection, or at the caret. The
    /// host answers [`Dictation::began`] with whether the surface accepted.
    Begin { generation: u64 },
    /// Replace the provisional text. No undo entry, no Loro operation.
    Update { generation: u64, text: Transcript },
    /// Make the transcript real: one logical edit, one undo step.
    Commit { generation: u64, text: Transcript },
    /// Throw the composition away: no edit, no undo entry, no trace.
    Cancel { generation: u64 },
}

impl DictationAction {
    /// The generation of the session this belongs to.
    pub fn generation(&self) -> u64 {
        match self {
            Self::Begin { generation }
            | Self::Update { generation, .. }
            | Self::Commit { generation, .. }
            | Self::Cancel { generation } => *generation,
        }
    }

    /// The transcript, for the two variants that carry one.
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Update { text, .. } | Self::Commit { text, .. } => Some(text.as_str()),
            Self::Begin { .. } | Self::Cancel { .. } => None,
        }
    }

    /// Whether this action closes the composition it names.
    pub fn closes(&self) -> bool {
        matches!(self, Self::Commit { .. } | Self::Cancel { .. })
    }
}

/// How the gesture works.
///
/// **Push-to-talk is the default.** The toggle exists for accessibility and
/// for dictation longer than a comfortable hold, and it is what a microphone
/// button offers.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    #[default]
    PushToTalk,
    Toggle,
}

/// The four roaming choices a dictation host takes from the Global section.
///
/// **This used to be the third parser of `preferences.conf`** — SP-6b deleted
/// it. It read the file itself, from a path a caller happened to pass, and it
/// disagreed with the other two about what a value meant: it accepted
/// `dictation_toggle=true` where the shell did not, and it resolved a language
/// tag against `koru-voice`'s closed set where the shell resolved it against
/// its own. One authority answers all of that now, and what is left here is
/// the *snapshot*: four values a controller is given together, so a gesture
/// can never begin against half a change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preferences {
    pub mode: Mode,
    pub language: Requested,
    pub tier: ModelTier,
    pub input: InputChoice,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            mode: Mode::PushToTalk,
            language: Requested::Auto,
            tier: ModelTier::default(),
            input: InputChoice::SystemDefault,
        }
    }
}

impl Preferences {
    /// Take the four from an authoritative Global section.
    ///
    /// **A language tag this build does not know is automatic detection**, not
    /// a refusal — the authority validates the *shape* of a tag and leaves the
    /// closed set to the consumer, which is here.
    pub fn from_section(section: &koru_settings::GlobalSettings) -> Self {
        Self {
            mode: match section.dictation_gesture {
                koru_settings::DictationGesture::Toggle => Mode::Toggle,
                koru_settings::DictationGesture::PushToTalk => Mode::PushToTalk,
            },
            language: match &section.dictation_language {
                koru_settings::DictationLanguage::Automatic => Requested::Auto,
                koru_settings::DictationLanguage::Tag(tag) => {
                    VoiceLanguage::matching(tag).map_or(Requested::Auto, Requested::Fixed)
                }
            },
            tier: ModelTier::parse(section.dictation_tier.token()).unwrap_or_default(),
            input: InputChoice::parse(section.dictation_input.token()),
        }
    }

    /// Apply one coherent snapshot to a controller before a gesture starts.
    pub fn apply_to(&self, dictation: &mut Dictation) {
        dictation.set_mode(self.mode);
        dictation.set_language(self.language);
        dictation.set_tier(self.tier);
        dictation.set_input(self.input.clone());
    }
}

/// Why a dictation was thrown away rather than committed. Never the words.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Interruption {
    /// A click, a keystroke, a menu — an ordinary local event. See the module
    /// note on why this discards rather than committing.
    LocalEvent,
    /// Escape.
    Escape,
    /// The window stopped being the focused one.
    FocusLost,
    /// The window is going away.
    WindowClosed,
    /// The machine is sleeping, or the process is shutting down.
    Suspended,
}

impl Interruption {
    /// A stable token for a log line. Never prose, never a transcript.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalEvent => "local_event",
            Self::Escape => "escape",
            Self::FocusLost => "focus_lost",
            Self::WindowClosed => "window_closed",
            Self::Suspended => "suspended",
        }
    }
}

/// Something to say to the person, once. A code and never a sentence: the
/// translation belongs to whoever draws the toast.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Refusal {
    pub code: ErrorCode,
    /// `true` when the *surface* declined — a masked credential field, a
    /// read-only document, a welcome screen. There is no [`ErrorCode`] for
    /// that, because it is not something the worker can know.
    pub surface: bool,
}

/// Counters, not content. **Ids, counts, stages and milliseconds** is the
/// whole vocabulary a voice diagnostic is allowed.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct Stats {
    /// Dictations begun.
    pub sessions: u64,
    /// Partials that reached a surface, after coalescing.
    pub updates: u64,
    /// Dictations that ended in a commit.
    pub commits: u64,
    /// Dictations thrown away, for any reason.
    pub discards: u64,
    /// Messages dropped because they named a session this controller is not
    /// running. The ordinary reason is a decoder still finishing after a
    /// cancel; a steady stream of them is worth knowing and is not a fault.
    pub stale: u64,
    /// `Final`s the worker volunteered while still listening. **Its own
    /// counter**, because that one means an adapter is breaking a contract
    /// rather than losing a race, and folding the two together would let a
    /// real defect hide inside an ordinary one.
    pub volunteered_finals: u64,
    /// Holds ended by [`HOLD_CAP`].
    pub capped: u64,
    /// Finalisations abandoned by [`FINALISE_TIMEOUT`].
    pub finalise_timeouts: u64,
}

/// Where the gesture is. Not the worker's [`State`], which is reported
/// separately: a meter wants to know that a model is still loading, and the
/// composition does not care.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum Stage {
    #[default]
    Idle,
    /// A session has been asked for and the composition is open. Covers the
    /// worker's `Loading` and `Listening` alike — from the gesture's point of
    /// view there is one state.
    Live,
    /// The gesture is over and `finish` has been requested. **The composition
    /// stays provisional**: a key going up is not a reason to commit the last
    /// partial.
    Finalising,
}

/// What a host should do with a routed press before it touches the shared
/// dictation state machine. The route token is only correlation; it must never
/// be able to adopt or stop an unrelated native or command-started session.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum RoutedPress {
    /// Nothing is active, so this route may begin a session.
    Begin,
    /// The same native physical press already began this session; attach the
    /// route without pressing the toggle a second time.
    Adopt,
    /// This route already owns the session. A second toggle press may finish it.
    Continue,
    /// A different route owns the session; ignore stale or hostile input.
    Ignore,
    /// A native/commanded session is active but cannot be proven to be this
    /// exact physical press.
    Refuse,
}

pub fn routed_press(
    current_route: Option<u64>,
    proposed_route: u64,
    stage: Stage,
    same_physical_key: bool,
) -> RoutedPress {
    match current_route {
        Some(current) if current == proposed_route => RoutedPress::Continue,
        Some(_) => RoutedPress::Ignore,
        None if stage == Stage::Idle => RoutedPress::Begin,
        None if stage == Stage::Live && same_physical_key => RoutedPress::Adopt,
        None => RoutedPress::Refuse,
    }
}

/// **Everything a listening surface draws, and nothing else.**
///
/// Four `Copy` fields, none of them a transcript. A surface that could reach
/// the words would eventually print one, and a meter has no reason to know
/// them: what it shows is *that* somebody is speaking, how loudly, and which
/// of the three stages the gesture is in.
///
/// The generation travels with it for the same reason it travels on a
/// [`DictationAction`] — a host holding one from a session that has ended can
/// tell, rather than drawing a meter for a dictation nobody is doing.
///
/// [`Dictation::status`] answers `None` while nothing is running, and that
/// `None` is what makes `View::animating` honest: the surface is alive exactly
/// as long as the session is.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Status {
    pub generation: u64,
    /// Where the gesture is: [`Stage::Live`] or [`Stage::Finalising`]. Never
    /// [`Stage::Idle`] — that case is the `None`.
    pub stage: Stage,
    /// What the worker last said it was doing, so a surface can say *Starting*
    /// while a model loads rather than *Listening* at a microphone that is not
    /// open yet.
    pub worker: State,
    /// The last input level. Nine steps and never a float — see
    /// [`LevelBucket`].
    pub level: LevelBucket,
}

/// **The controller.** One per host process.
pub struct Dictation {
    client: VoiceClient,
    mode: Mode,
    language: Requested,
    tier: ModelTier,
    model: Option<ModelId>,
    /// **Which microphone**, by name. Follows the person, in
    /// `preferences.conf`, and is read on the press rather than held twice --
    /// the same arrangement `language` has, and for the same reason: a change
    /// made in the Settings section takes effect on the next dictation rather
    /// than on the next launch.
    input: InputChoice,
    stage: Stage,
    live: Option<Live>,
    /// Whether a surface answered `true` to [`DictationAction::Begin`]. Until
    /// it has, nothing is emitted — there is no composition to update.
    open: bool,
    started: Option<Instant>,
    finalising_since: Option<Instant>,
    /// The final transcript, held until the `Ended` that authorises it.
    /// **Only [`EndReason::Completed`] does**, which is the one rule that
    /// makes a commit mean the engine finished rather than the socket
    /// happening to deliver.
    finished: Option<Transcript>,
    queue: VecDeque<DictationAction>,
    refusal: Option<Refusal>,
    heard: State,
    level: LevelBucket,
    detected: Option<VoiceLanguage>,
    stats: Stats,
    /// Only used with the engine off: the client is what mints these in a
    /// shipped build, and under test nothing dials it.
    test_generation: u64,
    /// **What somebody agreed to**, or `None` — which is the default, and the
    /// reason a controller nobody has told refuses rather than dials.
    ///
    /// Latched rather than read per press: the record is a file, `press` is on
    /// the keystroke path, and a `Granted` cannot change under a running
    /// process without the host putting it there. The host sets it at startup
    /// and again whenever the consent step answers.
    consent: Option<koru_voice_consent::Granted>,
}

impl Dictation {
    /// A controller for the worker on this channel's own root.
    pub fn new() -> Dictation {
        Dictation::at(None)
    }

    /// A controller for the worker on an explicit root — `--root`, or a test's
    /// temporary directory.
    pub fn at(root: Option<PathBuf>) -> Dictation {
        Dictation {
            client: VoiceClient::at(root),
            mode: Mode::default(),
            language: Requested::Auto,
            tier: ModelTier::default(),
            model: None,
            input: InputChoice::SystemDefault,
            stage: Stage::Idle,
            live: None,
            open: false,
            started: None,
            finalising_since: None,
            finished: None,
            queue: VecDeque::new(),
            refusal: None,
            heard: State::Idle,
            level: LevelBucket::SILENT,
            detected: None,
            stats: Stats::default(),
            test_generation: 0,
            // **`None` is the safe default and it is deliberate.** A
            // controller that has not been told anything opens no microphone,
            // so forgetting to wire the record up is a feature that does not
            // work rather than a gate that is not there.
            consent: None,
        }
    }

    /// The client, for a host that has to install its wake and its ensure —
    /// and for nothing else. Requests leave through this controller so that
    /// the stage and the worker cannot disagree about what is running.
    pub fn client(&self) -> &VoiceClient {
        &self.client
    }

    /// Push-to-talk or toggle. Follows the person, in `preferences.conf`.
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Which language to ask for. `Auto` unless the person pinned one.
    pub fn set_language(&mut self, language: Requested) {
        self.language = language;
    }

    /// Which quality tier the worker should resolve for the next dictation.
    pub fn set_tier(&mut self, tier: ModelTier) {
        self.tier = tier;
    }

    pub fn tier(&self) -> ModelTier {
        self.tier
    }

    /// Pin a model. `None` — the ordinary case — leaves the choice to the
    /// worker, which knows the catalog; a window has no business knowing model
    /// ids.
    pub fn set_model(&mut self, model: Option<ModelId>) {
        self.model = model;
    }

    /// **Which microphone to listen with.**
    ///
    /// [`InputChoice::SystemDefault`] -- the default -- follows the operating
    /// system. A name is resolved by the *worker*, against what is actually
    /// plugged in, and a name that is not there falls back to the default
    /// rather than refusing: see `koru_voice::input`.
    pub fn set_input(&mut self, input: InputChoice) {
        self.input = input;
    }

    /// What this controller will ask to listen with.
    pub fn input(&self) -> &InputChoice {
        &self.input
    }

    /// **Tell the controller what somebody agreed to**, or take it back.
    ///
    /// The host reads `koru-voice-consent`'s record — it owns the directory,
    /// per ADR-111 — and hands the token here. Without one, [`press`] refuses
    /// with [`ErrorCode::ConsentRequired`] and nothing is dialled.
    ///
    /// [`press`]: Dictation::press
    pub fn set_consent(&mut self, consent: Option<koru_voice_consent::Granted>) {
        self.consent = consent;
    }

    /// Whether a dictation could be started at all. A host asks this to decide
    /// between *begin* and *open the consent step*; the refusal below is what
    /// makes the answer binding rather than advisory.
    pub fn consented(&self) -> bool {
        self.consent.is_some()
    }

    // ----------------------------------------------------------- the gesture

    /// **The gesture began.**
    ///
    /// Answers the [`DictationAction::Begin`] the host must route, or `None`
    /// when nothing started — a key repeat, a toggle press that ended the
    /// dictation instead of starting one, or a worker that could not be
    /// reached (which leaves a [`Refusal`] for [`Dictation::take_refusal`]).
    ///
    /// A host that gets `Some` **must** answer [`Dictation::began`], or the
    /// controller will hold a session no surface is composing for.
    pub fn press(&mut self, now: Instant) -> Option<DictationAction> {
        match self.stage {
            // A toggle press while a dictation is running ends it, the same
            // way releasing the key would in the other mode.
            Stage::Live if self.mode == Mode::Toggle => {
                self.finish(now);
                return None;
            }
            // A key repeat, or a press arriving while the last utterance is
            // still being finalised. Neither starts anything.
            Stage::Live | Stage::Finalising => return None,
            Stage::Idle => {}
        }
        // **The microphone half of the consent gate**, and the enforcement
        // rather than the offer: a host that meets this refusal opens the
        // consent step, and a host that has none — a bench, a phone, a future
        // surface — still cannot dial past it. The download half is
        // `ModelStore::install`'s signature.
        //
        // **After the stage, not before it.** A press that would *end* a
        // toggled dictation is not a press that opens a microphone, and
        // refusing it would leave somebody mid-sentence with no way to stop —
        // consent taken back while a session is live has to end the session,
        // not freeze it.
        if self.consent.is_none() {
            self.refusal = Some(Refusal {
                code: ErrorCode::ConsentRequired,
                surface: false,
            });
            return None;
        }
        let live = match self.open_session() {
            Ok(live) => live,
            Err(error) => {
                self.refusal = Some(Refusal {
                    code: error.code,
                    surface: false,
                });
                return None;
            }
        };
        self.live = Some(live);
        self.stage = Stage::Live;
        self.open = false;
        self.started = Some(now);
        self.finished = None;
        self.heard = State::Loading;
        self.level = LevelBucket::SILENT;
        self.detected = None;
        self.stats.sessions += 1;
        Some(DictationAction::Begin {
            generation: live.generation,
        })
    }

    /// The host routed [`DictationAction::Begin`] and the surface answered.
    ///
    /// `false` is a real answer and a common one: a masked credential field, a
    /// read-only document, a welcome screen, a tab with no caret. The session
    /// is cancelled and a [`Refusal`] is left for the toast — **the refusal
    /// comes from the host**, because which box is a secret is not something a
    /// widget can know (Q-V25).
    pub fn began(&mut self, accepted: bool) {
        if self.stage != Stage::Live || self.open {
            return;
        }
        if accepted {
            self.open = true;
            return;
        }
        self.dial_cancel();
        self.refusal = Some(Refusal {
            code: ErrorCode::WrongState,
            surface: true,
        });
        self.reset();
    }

    /// **The gesture ended.** Requests the final result; it does not commit
    /// the latest partial because a key went up.
    ///
    /// Answers whether anything was running. A release with no matching press
    /// is inert — which is the ordinary case, since every other key in the app
    /// also comes back up.
    pub fn release(&mut self, now: Instant) -> bool {
        if self.mode == Mode::Toggle {
            // The key coming up is not the end of a toggled dictation. Only
            // the next press is.
            return false;
        }
        if self.stage != Stage::Live {
            return false;
        }
        self.finish(now);
        true
    }

    /// **Stop, and keep the words** — what the listening surface's microphone
    /// button does.
    ///
    /// The same request [`release`](Self::release) makes, without the mode
    /// test: a pointer has no key to let go of, so a press on the surface ends
    /// a push-to-talk hold and a toggled dictation alike. It is a *finish*, not
    /// an [`interrupt`](Self::interrupt) — the composition stays provisional
    /// until the matching `Final` arrives, and the sentence somebody has just
    /// finished saying is not thrown away by the gesture that says they have
    /// finished saying it.
    ///
    /// Answers whether anything was running. Inert in [`Stage::Finalising`],
    /// because the finish it would ask for has already been asked for.
    pub fn stop(&mut self, now: Instant) -> bool {
        if self.stage != Stage::Live {
            return false;
        }
        self.finish(now);
        true
    }

    /// Throw the dictation away. Every interruption ends here.
    pub fn interrupt(&mut self, why: Interruption) {
        if self.stage == Stage::Idle {
            return;
        }
        tracing::debug!(
            target: "koru::voice",
            reason = why.as_str(),
            stage = ?self.stage,
            "dictation discarded"
        );
        self.dial_cancel();
        self.discard();
    }

    // ------------------------------------------------------------- the queue

    /// **Everything the surface should do now**, in order, with at most one
    /// [`DictationAction::Update`] however many partials arrived.
    ///
    /// Called once per pass of the host's loop. Drains the client first, so a
    /// wake and a drain are one step rather than two that can be ordered
    /// wrongly.
    pub fn pump(&mut self, now: Instant) -> Vec<DictationAction> {
        for event in self.drain_client() {
            self.apply(event);
        }
        self.check_deadlines(now);
        self.queue.drain(..).collect()
    }

    /// Feed one event in directly, for a host that has its own transport — and
    /// for the suite, which has no worker at all.
    pub fn observe(&mut self, event: Event) {
        self.apply(event);
    }

    /// The next moment this controller has something to do, so a loop can
    /// sleep until then. `None` whenever nothing is running: **an idle process
    /// is never woken by this crate.**
    pub fn deadline(&self) -> Option<Instant> {
        match self.stage {
            Stage::Idle => None,
            Stage::Live => self.started.map(|at| at + HOLD_CAP),
            Stage::Finalising => self.finalising_since.map(|at| at + FINALISE_TIMEOUT),
        }
    }

    /// Something to tell the person, taken by the asking so it is said once.
    pub fn take_refusal(&mut self) -> Option<Refusal> {
        self.refusal.take()
    }

    // -------------------------------------------------------------- questions

    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// Whether a dictation is running at all — listening or finalising.
    pub fn is_live(&self) -> bool {
        self.stage != Stage::Idle
    }

    /// The generation of the running dictation.
    pub fn generation(&self) -> Option<u64> {
        self.live.map(|live| live.generation)
    }

    /// The session id of the running dictation.
    pub fn session(&self) -> Option<SessionId> {
        self.live.map(|live| live.session)
    }

    /// What the worker last said it was doing. For a meter, not for the
    /// composition.
    pub fn worker_state(&self) -> State {
        self.heard
    }

    /// The last input level the worker reported. Nine steps and never a float
    /// — see `koru_voice::LevelBucket`.
    pub fn level(&self) -> LevelBucket {
        self.level
    }

    /// **What a listening surface should draw**, or `None` when there is
    /// nothing to draw.
    ///
    /// One value rather than four getters, because a surface that read the
    /// stage on one pass and the level on the next would eventually draw a
    /// meter for a session that had ended between them. `None` is the whole
    /// lifetime rule: the indicator exists exactly while this answers `Some`,
    /// and both bounds — [`HOLD_CAP`] and [`FINALISE_TIMEOUT`] — are what stop
    /// it being `Some` forever.
    pub fn status(&self) -> Option<Status> {
        let live = self.live?;
        if self.stage == Stage::Idle {
            return None;
        }
        Some(Status {
            generation: live.generation,
            stage: self.stage,
            worker: self.heard,
            level: self.level,
        })
    }

    /// The language the worker detected, when the request said `Auto`.
    pub fn detected_language(&self) -> Option<VoiceLanguage> {
        self.detected
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    // -------------------------------------------------------------- internals

    /// Stop capturing and wait for the matching `Final`.
    fn finish(&mut self, now: Instant) {
        self.stage = Stage::Finalising;
        self.finalising_since = Some(now);
        self.heard = State::Finalising;
        self.dial_finish();
    }

    fn apply(&mut self, event: Event) {
        let Some(live) = self.live else {
            self.stats.stale += 1;
            return;
        };
        // **Dropped on its generation**, which is what makes a late `Final`
        // from a cancelled session commit nothing. The session id is compared
        // too: two clients of one worker are distinguished by generation
        // alone, and belt-and-braces costs one comparison.
        if event.generation() != live.generation || event.session() != live.session {
            self.stats.stale += 1;
            return;
        }
        match event {
            Event::State { state, .. } => self.heard = state,
            Event::Level { bucket, .. } => self.level = bucket,
            Event::Language { language, .. } => self.detected = Some(language),
            Event::Partial { text, .. } => self.push_update(text),
            Event::Final { text, .. } => match self.stage {
                Stage::Finalising => self.finished = Some(text),
                // Volunteered while still listening: an adapter breaking the
                // contract that `finish` is the only thing that may produce a
                // `Final`. Counted under its own name so a real defect cannot
                // hide inside an ordinary late message.
                Stage::Live => self.stats.volunteered_finals += 1,
                Stage::Idle => self.stats.stale += 1,
            },
            Event::Ended { reason, .. } => self.ended(reason),
            Event::Refused { code, .. } => {
                self.refusal = Some(Refusal {
                    code,
                    surface: false,
                });
                self.discard();
            }
            // **A device list is not a dictation event and never reaches
            // here.** `koru-voice-client` routes it to whoever asked, before
            // the generation filter this method repeats -- so the arm is a
            // statement about that routing rather than a case to handle. It is
            // named rather than swallowed by a wildcard, so a new *session*
            // event cannot be added and quietly ignored.
            Event::Devices { .. } => self.stats.stale += 1,
        }
    }

    /// The session is over, and why.
    ///
    /// **Only [`EndReason::Completed`] commits**, and only when a matching
    /// `Final` is in hand. A completed session that produced nothing cancels:
    /// a dictation that said nothing records no undo step, which is the rule a
    /// backspace at offset zero already follows.
    fn ended(&mut self, reason: EndReason) {
        match reason {
            EndReason::Completed => match self.finished.take() {
                Some(text) if !text.is_empty() => {
                    if self.open
                        && let Some(live) = self.live
                    {
                        self.queue.push_back(DictationAction::Commit {
                            generation: live.generation,
                            text,
                        });
                        self.stats.commits += 1;
                    }
                    self.reset();
                }
                _ => self.discard(),
            },
            EndReason::Cancelled => self.discard(),
            EndReason::Failed { code, .. } => {
                self.refusal = Some(Refusal {
                    code,
                    surface: false,
                });
                self.discard();
            }
        }
    }

    /// **Coalesced to at most one `Update` per drain.**
    ///
    /// A recogniser revises itself several times a second and a host draws at
    /// most once a frame, so every revision but the last is a layout nobody
    /// ever sees. Replacing the queued one rather than appending keeps the
    /// order — `Begin` still comes first — without a second pass to squash.
    fn push_update(&mut self, text: Transcript) {
        if !self.open {
            return;
        }
        let Some(live) = self.live else { return };
        if let Some(DictationAction::Update { text: last, .. }) = self.queue.back_mut() {
            *last = text;
            return;
        }
        self.stats.updates += 1;
        self.queue.push_back(DictationAction::Update {
            generation: live.generation,
            text,
        });
    }

    /// End the dictation with nothing written.
    fn discard(&mut self) {
        if self.open
            && let Some(live) = self.live
        {
            // Whatever was queued for this composition is about to be
            // irrelevant: the surface is being told to throw the whole thing
            // away, and an `Update` in front of that is a layout for text that
            // is going to vanish in the same pass.
            self.queue.clear();
            self.queue.push_back(DictationAction::Cancel {
                generation: live.generation,
            });
        }
        self.stats.discards += 1;
        self.reset();
    }

    fn reset(&mut self) {
        self.stage = Stage::Idle;
        self.live = None;
        self.open = false;
        self.started = None;
        self.finalising_since = None;
        self.finished = None;
        self.heard = State::Idle;
        self.level = LevelBucket::SILENT;
    }

    /// The two bounds. Both are ends, not warnings: nothing here retries.
    fn check_deadlines(&mut self, now: Instant) {
        match self.stage {
            Stage::Live => {
                if let Some(at) = self.started
                    && now.saturating_duration_since(at) >= HOLD_CAP
                {
                    self.stats.capped += 1;
                    tracing::info!(
                        target: "koru::voice",
                        seconds = HOLD_CAP.as_secs(),
                        "dictation hold cap reached; finishing"
                    );
                    self.finish(now);
                }
            }
            Stage::Finalising => {
                if let Some(at) = self.finalising_since
                    && now.saturating_duration_since(at) >= FINALISE_TIMEOUT
                {
                    self.stats.finalise_timeouts += 1;
                    tracing::warn!(
                        target: "koru::voice",
                        seconds = FINALISE_TIMEOUT.as_secs(),
                        "dictation finalisation timed out; discarding"
                    );
                    self.refusal = Some(Refusal {
                        code: ErrorCode::FinishFailed,
                        surface: false,
                    });
                    self.dial_cancel();
                    self.discard();
                }
            }
            Stage::Idle => {}
        }
    }

    // ---------------------------------------------------- the one transport

    /// **The three places this crate touches a worker, and the only three.**
    ///
    /// Each begins with the same guard, so a reviewer can see at a glance that
    /// a test cannot dial anything. Under [`ENGINE_OFF`] `open_session` still
    /// mints an id and a generation, because the whole state machine — every
    /// failure path and both late-message races — has to be drivable without a
    /// microphone anywhere near it.
    fn open_session(&mut self) -> Result<Live, VoiceError> {
        if ENGINE_OFF {
            self.test_generation += 1;
            return Ok(Live {
                session: SessionId::new(),
                generation: self.test_generation,
            });
        }
        self.client.begin_with_tier(
            self.language,
            self.tier,
            self.model.clone(),
            self.input.clone(),
        )
    }

    fn dial_finish(&self) {
        if ENGINE_OFF {
            return;
        }
        let _ = self.client.finish();
    }

    fn dial_cancel(&self) {
        if ENGINE_OFF {
            return;
        }
        let _ = self.client.cancel();
    }

    fn drain_client(&self) -> Vec<Event> {
        if ENGINE_OFF {
            return Vec::new();
        }
        self.client.drain()
    }
}

impl Default for Dictation {
    fn default() -> Dictation {
        Dictation::new()
    }
}

#[cfg(test)]
mod tests;
