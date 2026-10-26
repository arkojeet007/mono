// SPDX-License-Identifier: MPL-2.0

//! Reusable first-run welcome surface.
//!
//! Geometry, drawing, and hit-testing live together here so desktop onboarding
//! and any later account entry point cannot disagree about the split panel,
//! provider actions, the no-account choice, or legal links. It owns no account or Profile
//! state; callers translate [`Action`] into their own typed effects.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{inspect, scene, theme};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Shape as _};
use vello::peniko::Fill;

use crate::button::{
    ButtonRole, draw_apple_button, draw_button, draw_button_enabled, draw_failed_outline,
    draw_google_button,
};

pub const TERMS_URL: &str = "https://www.koru.do/terms";
pub const PRIVACY_URL: &str = "https://www.koru.do/privacy";
pub const CHAT_URL: &str =
    "https://www.koru.do/?utm_source=desktop-welcome&utm_medium=app&utm_campaign=welcome&chat=true";
pub const SUPPORT_ADDRESS: &str = "support@koru.do";
pub const SUPPORT_MAILTO: &str = "mailto:support@koru.do";

const EDGE: f32 = 24.0;
const ACTION_H: f32 = 44.0;
/// A field's label sits on its own line above it; this is that line's share
/// of the column, measured rather than guessed nowhere because every label
/// here is one line of `theme::role::meta()`.
const LABEL_H: f32 = 20.0;
/// Between one field's box and the next field's label.
const FIELD_GAP: f32 = 10.0;
/// The "or" between the email form and the provider buttons: a label line,
/// and the air either side of it. The design's 36px above and below, scaled
/// to these 44px rows, is about 20px; a window too short to hold that gives
/// up divider air first, down to the floor, before anything else.
const DIVIDER_LABEL_H: f32 = 16.0;
const DIVIDER_AIR: f32 = 20.0;
const DIVIDER_AIR_MIN: f32 = 6.0;
/// Between Google and Apple, which share one row.
const PROVIDER_GAP: f32 = 12.0;
/// Between the provider row and *Continue without an account*: well clear of
/// it, so the button reads as a different way in rather than a third
/// provider. The design's 48px, scaled to these 44px rows.
const OFFLINE_GAP: f32 = 32.0;
/// The least air kept between the column and the legal line under it.
const FOOTER_GAP: f32 = 12.0;
/// Between a field and the failure written under it.
const FIELD_ERROR_GAP: f32 = 6.0;
/// The failure banner: its inner padding, the alert mark's diameter and the
/// gap after it, and the air between the banner and the row it is about.
const BANNER_PAD: f32 = 12.0;
const MARK_D: f32 = 16.0;
const MARK_GAP: f32 = 10.0;
const BANNER_GAP: f32 = 10.0;
const RETRY_GAP: f32 = 16.0;

const HEADING: &str = "Welcome to Koru";
const HEADING_GAP: f32 = 10.0;
/// Between the sentence under the heading and the first label.
const FORM_GAP: f32 = 16.0;

/// The copy the screen is measured from, kept beside the arithmetic that
/// measures it so the two cannot drift.
///
/// It says why the form is optional before the form asks for anything. It
/// wraps, and the column is measured from the wrapped height.
const LEAD: &str = "Your work stays on your devices. An account is only needed if you want cloud sync between devices — everything else works without one.";
const ADDRESS_LABEL: &str = "Email address";
const PASSWORD_LABEL: &str = "Password";
/// The email form's own submit, directly under its fields, so it does not
/// repeat "with email".
const CONTINUE_LABEL: &str = "Continue";
const DIVIDER_LABEL: &str = "or";
/// The provider buttons share a row, so each says only whose it is. The
/// accessible names keep the full "Sign in with …", which contains the label.
const GOOGLE_LABEL: &str = "Google";
const APPLE_LABEL: &str = "Apple";
/// The no-account way in: a button like the providers', set apart by
/// [`OFFLINE_GAP`]. The lead above the form already says what it means.
const OFFLINE_LABEL: &str = "Continue without an account";
pub const RETRY_LABEL: &str = "Try again";

/// The sidebar says what the website's hero says, word for word, so a person
/// who arrives from koru.do reads the same promise twice rather than two.
const HERO_COPY: &str = "Your private workspace for thinking, planning, and AI.";
const BODY_COPY: &str = "Write, organize, plan, and remember — without giving up control of your data. Offline first, synced the way you choose.";

const HERO: theme::TextStyle =
    theme::TextStyle::new(theme::text_metrics::_2XL, theme::FontWeight::Bold);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// The email address field. Hit-testable and focusable like a button,
    /// because the caller owns the editor and needs to be told which of its
    /// two fields was reached.
    Address,
    Password,
    /// The primary action: whatever this address and password turn out to
    /// be, they leave by this one button.
    ContinueWithEmail,
    Google,
    Apple,
    ContinueOffline,
    /// *Try again* in a failure banner: the caller repeats whatever the
    /// banner is about.
    Retry,
    Terms,
    Privacy,
}

/// A sign-in that did not work, and what it was about.
///
/// **Where it is said follows from `at`.** A field ([`Action::Address`],
/// [`Action::Password`]) gets its border turned red by the caller and the
/// message written directly under it. An action ([`Action::ContinueWithEmail`],
/// [`Action::Google`], [`Action::Apple`]) gets a banner under its row, the
/// provider button outlined in red, and *Try again* when `retry` is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Failure<'a> {
    pub at: Action,
    pub message: &'a str,
    pub retry: bool,
}

impl Failure<'_> {
    pub fn is_field(self) -> bool {
        matches!(self.at, Action::Address | Action::Password)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub viewport: Rect,
    pub sidebar: Rect,
    pub right: Rect,
    pub heading: Rect,
    /// The sentence under the heading, already wrapped to the column.
    /// `None` when the window is too short to hold it beside a failure: it
    /// is the one thing on the screen that only explains, and the card under
    /// the providers says the same.
    pub lead: Option<Rect>,
    /// **The two fields are geometry only.** The editor, its text and its
    /// focus ring belong to the caller — this crate holds no account state —
    /// so nothing is painted inside these rectangles here. A caller draws its
    /// own [`crate::TextInput`] into them.
    pub address: Rect,
    pub password: Rect,
    pub email_submit: Rect,
    /// The band the "or" is centred in, between the submit and the providers.
    pub divider: Rect,
    /// Google and Apple, side by side in one row.
    pub google: Rect,
    pub apple: Rect,
    /// *Continue without an account*, alone under the providers.
    pub offline: Rect,
    pub terms: Rect,
    pub privacy: Rect,
    /// The failure's own text, wrapped: under its field, or inside its
    /// banner. `None` when the caller passed no failure, so nothing is
    /// published where nothing is painted.
    pub notice: Option<Rect>,
    /// The banner around an action's failure. `None` for a field's.
    pub banner: Option<Rect>,
    /// *Try again*, at the banner's right edge.
    pub retry: Option<Rect>,
    pub legal_stacked: bool,
    /// Whether the email form and the providers are offered at all. `false`
    /// for a build with no Koru Cloud ([`Layout::without_accounts`]): their
    /// rectangles are then empty, nothing hit-tests them, and
    /// [`Self::cloud_note`] stands where they would be.
    pub accounts: bool,
    /// The note that says the build has no Koru Cloud, in place of the
    /// sign-in choices. `None` whenever they are offered.
    pub cloud_note: Option<Rect>,
}

/// What a build with no Koru Cloud says where the sign-in choices would be.
///
/// The caller's words, in the reader's language: this crate holds no account
/// state and does not decide whether there is a Cloud.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloudNote<'a> {
    pub title: &'a str,
    pub body: &'a str,
}

/// Inside the note: its padding, and the gap between its two lines.
const NOTE_PAD: f32 = 14.0;
const NOTE_LINE_GAP: f32 = 4.0;

/// The note's two paragraphs, measured to the width they wrap in.
fn measure_note(engine: &mut TextEngine, note: CloudNote<'_>, content_w: f32) -> (f32, f32) {
    let inner = (content_w - NOTE_PAD * 2.0).max(40.0);
    let title_h = engine
        .simple_layout_wrapped(note.title, note_title_style(), inner)
        .height()
        .max(theme::role::row().metrics.line_height);
    let body_h = engine
        .simple_layout_wrapped(note.body, theme::role::meta(), inner)
        .height()
        .max(theme::role::meta().metrics.line_height);
    (title_h, body_h)
}

fn note_title_style() -> theme::TextStyle {
    theme::TextStyle::new(theme::role::row().metrics, theme::FontWeight::Bold)
}

/// Where the legal line goes along the bottom of the right-hand pane: the
/// two links, whether it wraps onto two rows, and where its first row starts.
fn legal_geometry(engine: &mut TextEngine, right: Rect, height: f32) -> (Rect, Rect, bool, f32) {
    let prefix = legal_width(engine, LEGAL_PREFIX);
    let terms_w = engine
        .simple_layout("Terms of Service", theme::role::meta())
        .width();
    let join = legal_width(engine, LEGAL_JOIN);
    let privacy_w = engine
        .simple_layout("Privacy Policy", theme::role::meta())
        .width();
    let total = prefix + terms_w + join + privacy_w + 2.0;
    let legal_stacked = total > right.w - 16.0;
    let footer_y = (height - 38.0).max(18.0);
    let legal_top = if legal_stacked {
        footer_y - 18.0
    } else {
        footer_y
    };
    let links_w = terms_w + join + privacy_w;
    let footer_x = if legal_stacked {
        right.x + (right.w - links_w) * 0.5
    } else {
        right.x + (right.w - total) * 0.5 + prefix
    };
    let terms = Rect::new(footer_x, footer_y, terms_w, 24.0);
    let privacy = Rect::new(terms.x + terms.w + join, footer_y, privacy_w, 24.0);
    (terms, privacy, legal_stacked, legal_top)
}

/// What a failure adds to the column, measured before anything is placed so
/// the column knows its own height.
struct Measured {
    /// The height inserted under the field or row the failure is about.
    extra: f32,
    text_h: f32,
    text_w: f32,
    retry_w: f32,
}

fn measure_failure(engine: &mut TextEngine, failure: Failure<'_>, content_w: f32) -> Measured {
    let style = theme::role::meta();
    let line = style.metrics.line_height;
    if failure.is_field() {
        let text_w = content_w - MARK_D - MARK_GAP;
        let text_h = engine
            .simple_layout_wrapped(failure.message, style, text_w)
            .height()
            .max(line);
        return Measured {
            extra: FIELD_ERROR_GAP + text_h,
            text_h,
            text_w,
            retry_w: 0.0,
        };
    }
    let retry_w = if failure.retry {
        engine.simple_layout(RETRY_LABEL, style).width()
    } else {
        0.0
    };
    let text_w = content_w
        - BANNER_PAD * 2.0
        - MARK_D
        - MARK_GAP
        - if failure.retry {
            retry_w + RETRY_GAP
        } else {
            0.0
        };
    let text_h = engine
        .simple_layout_wrapped(failure.message, style, text_w.max(40.0))
        .height()
        .max(line);
    Measured {
        extra: BANNER_GAP + text_h + BANNER_PAD * 2.0,
        text_h,
        text_w,
        retry_w,
    }
}

impl Layout {
    pub fn new(engine: &mut TextEngine, width: f32, height: f32) -> Self {
        Self::new_with_top(engine, width, height, 0.0, None)
    }

    /// Build the same welcome surface below an application-drawn caption.
    /// The split backgrounds still begin at y=0; only the column is kept
    /// below the caption so a press on it can never become a window drag.
    ///
    /// `failure` is measured here rather than at the call site because it
    /// changes the column it sits in: the rows under it move down by its
    /// height, and a short window gives up the lead sentence to hold it.
    pub fn new_with_top(
        engine: &mut TextEngine,
        width: f32,
        height: f32,
        top_inset: f32,
        failure: Option<Failure<'_>>,
    ) -> Self {
        Self::new_in(engine, width, height, top_inset, failure, true)
    }

    /// [`Self::new_with_top`], with or without the presentation pane
    /// ([`split_with`]). Without it the form is embedded in a host page, and
    /// its column starts at the left edge, under the page's own head.
    pub fn new_in(
        engine: &mut TextEngine,
        width: f32,
        height: f32,
        top_inset: f32,
        failure: Option<Failure<'_>>,
        pane: bool,
    ) -> Self {
        let width = width.max(1.0);
        let height = height.max(1.0);
        let (sidebar, right) = split_with(width, height, pane);
        let viewport = Rect::new(0.0, 0.0, width, height);
        let content_w = (right.w - EDGE * 2.0).min(392.0).max(240.0);
        let content_x = column_x(right, content_w, pane);
        let (terms, privacy, legal_stacked, legal_top) = legal_geometry(engine, right, height);

        // The column is measured before it is placed, so it knows where its
        // own bottom is and therefore whether it reaches the legal line.
        let lead_h = engine
            .simple_layout_wrapped(LEAD, theme::role::row(), content_w)
            .height()
            .max(theme::role::row().metrics.line_height);
        let measured = failure.map(|f| measure_failure(engine, f, content_w));
        let extra = measured.as_ref().map_or(0.0, |m| m.extra);
        // A title, so measured like one: it wraps rather than running off a
        // narrow column.
        let heading_h = crate::KoruTextTitle::new(HEADING).measure(engine, content_w);
        // Everything but the lead and the divider's air.
        let fixed = heading_h
            + FORM_GAP
            + (LABEL_H + ACTION_H) * 2.0
            + FIELD_GAP * 2.0
            + ACTION_H
            + DIVIDER_LABEL_H
            + ACTION_H
            + OFFLINE_GAP
            + ACTION_H;
        let lead_block = HEADING_GAP + lead_h;
        let top_limit = top_inset + 12.0;
        let bottom_limit = legal_top - FOOTER_GAP;
        let room = bottom_limit - top_limit;
        // The divider's air either side, for a column of `content` without it.
        let air_for = |content: f32| ((room - content) * 0.5).clamp(DIVIDER_AIR_MIN, DIVIDER_AIR);
        // Placed as if there were no failure, so a failure moves only what is
        // under it; the heading stays where the person was already reading.
        let resting = fixed + lead_block;
        let resting_h = resting + air_for(resting) * 2.0;
        let resting_y = ((height - resting_h) * 0.40)
            .min(bottom_limit - resting_h)
            .max(top_limit);
        // The lead is the last thing given up: only once the divider is down
        // to its floor and the column still does not fit beside a failure.
        let show_lead = failure.is_none() || resting + extra + DIVIDER_AIR_MIN * 2.0 <= room;
        let content = fixed + extra + if show_lead { lead_block } else { 0.0 };
        let air = air_for(content);
        let column_h = content + air * 2.0;
        let content_y = resting_y.min(bottom_limit - column_h).max(top_limit);

        let row = |y: f32| Rect::new(content_x, y, content_w, ACTION_H);
        let heading = Rect::new(content_x, content_y, content_w, heading_h);
        let mut y = heading.y + heading.h;
        let lead = show_lead.then(|| {
            let lead = Rect::new(content_x, y + HEADING_GAP, content_w, lead_h);
            y = lead.y + lead.h;
            lead
        });
        y += FORM_GAP;

        let mut notice = None;
        let mut banner = None;
        let mut retry = None;
        // Inserts the failure under `bottom` when it is about `at`, and
        // returns where the next row starts.
        let mut after = |at: Action, bottom: f32| -> f32 {
            let (Some(failure), Some(m)) = (failure, measured.as_ref()) else {
                return bottom;
            };
            if failure.at != at {
                return bottom;
            }
            let style = theme::role::meta();
            if failure.is_field() {
                notice = Some(Rect::new(
                    content_x + MARK_D + MARK_GAP,
                    bottom + FIELD_ERROR_GAP,
                    m.text_w,
                    m.text_h,
                ));
            } else {
                let b = Rect::new(
                    content_x,
                    bottom + BANNER_GAP,
                    content_w,
                    m.text_h + BANNER_PAD * 2.0,
                );
                notice = Some(Rect::new(
                    b.x + BANNER_PAD + MARK_D + MARK_GAP,
                    b.y + BANNER_PAD,
                    m.text_w,
                    m.text_h,
                ));
                if failure.retry {
                    retry = Some(Rect::new(
                        b.x + b.w - BANNER_PAD - m.retry_w,
                        b.y + BANNER_PAD,
                        m.retry_w,
                        style.metrics.line_height,
                    ));
                }
                banner = Some(b);
            }
            bottom + m.extra
        };

        let address = row(y + LABEL_H);
        y = after(Action::Address, address.y + address.h) + FIELD_GAP;
        let password = row(y + LABEL_H);
        y = after(Action::Password, password.y + password.h) + FIELD_GAP;
        let email_submit = row(y);
        y = after(Action::ContinueWithEmail, email_submit.y + email_submit.h);
        let divider = Rect::new(content_x, y, content_w, DIVIDER_LABEL_H + air * 2.0);
        y = divider.y + divider.h;
        let provider_w = (content_w - PROVIDER_GAP) * 0.5;
        let google = Rect::new(content_x, y, provider_w, ACTION_H);
        let apple = Rect::new(
            content_x + provider_w + PROVIDER_GAP,
            y,
            provider_w,
            ACTION_H,
        );
        let provider_bottom = google.y + google.h;
        y = after(Action::Google, provider_bottom);
        y = after(Action::Apple, y) + OFFLINE_GAP;
        let offline = row(y);

        Self {
            viewport,
            sidebar,
            right,
            heading,
            lead,
            address,
            password,
            email_submit,
            divider,
            google,
            apple,
            offline,
            terms,
            privacy,
            notice,
            banner,
            retry,
            legal_stacked,
            accounts: true,
            cloud_note: None,
        }
    }

    /// The same surface for a build with no Koru Cloud: the heading, the
    /// lead, a quiet note saying there is nothing to sign in to, and
    /// *Continue without an account* as the one way in.
    ///
    /// The email form and the providers are not drawn disabled — there is no
    /// state of this build in which they would work — so their rectangles
    /// are empty and [`Self::hit`] never answers them. Nothing here can fail,
    /// so there is no failure to measure.
    pub fn without_accounts(
        engine: &mut TextEngine,
        width: f32,
        height: f32,
        top_inset: f32,
        note: CloudNote<'_>,
    ) -> Self {
        Self::without_accounts_in(engine, width, height, top_inset, note, true)
    }

    /// [`Self::without_accounts`], with or without the presentation pane
    /// ([`split_with`]).
    pub fn without_accounts_in(
        engine: &mut TextEngine,
        width: f32,
        height: f32,
        top_inset: f32,
        note: CloudNote<'_>,
        pane: bool,
    ) -> Self {
        let width = width.max(1.0);
        let height = height.max(1.0);
        let (sidebar, right) = split_with(width, height, pane);
        let viewport = Rect::new(0.0, 0.0, width, height);
        let content_w = (right.w - EDGE * 2.0).min(392.0).max(240.0);
        let content_x = column_x(right, content_w, pane);
        let (terms, privacy, legal_stacked, legal_top) = legal_geometry(engine, right, height);

        let lead_h = engine
            .simple_layout_wrapped(LEAD, theme::role::row(), content_w)
            .height()
            .max(theme::role::row().metrics.line_height);
        let heading_h = crate::KoruTextTitle::new(HEADING).measure(engine, content_w);
        let (title_h, body_h) = measure_note(engine, note, content_w);
        let note_h = NOTE_PAD * 2.0 + title_h + NOTE_LINE_GAP + body_h;
        let fixed = heading_h + FORM_GAP + note_h + OFFLINE_GAP + ACTION_H;
        let lead_block = HEADING_GAP + lead_h;
        let top_limit = top_inset + 12.0;
        let bottom_limit = legal_top - FOOTER_GAP;
        // The lead only explains, and the note says the same thing more
        // exactly, so a window too short for both gives up the lead.
        let show_lead = fixed + lead_block <= bottom_limit - top_limit;
        let content = fixed + if show_lead { lead_block } else { 0.0 };
        let content_y = ((height - content) * 0.40)
            .min(bottom_limit - content)
            .max(top_limit);

        let heading = Rect::new(content_x, content_y, content_w, heading_h);
        let mut y = heading.y + heading.h;
        let lead = show_lead.then(|| {
            let lead = Rect::new(content_x, y + HEADING_GAP, content_w, lead_h);
            y = lead.y + lead.h;
            lead
        });
        y += FORM_GAP;
        let cloud_note = Rect::new(content_x, y, content_w, note_h);
        y = cloud_note.y + cloud_note.h + OFFLINE_GAP;
        let offline = Rect::new(content_x, y, content_w, ACTION_H);
        // Where the sign-in choices would start, and empty.
        let none = Rect::new(content_x, cloud_note.y, 0.0, 0.0);

        Self {
            viewport,
            sidebar,
            right,
            heading,
            lead,
            address: none,
            password: none,
            email_submit: none,
            divider: none,
            google: none,
            apple: none,
            offline,
            terms,
            privacy,
            notice: None,
            banner: None,
            retry: None,
            legal_stacked,
            accounts: false,
            cloud_note: Some(cloud_note),
        }
    }

    /// Where the legal line's first row starts.
    pub fn legal_top(self) -> f32 {
        if self.legal_stacked {
            self.terms.y - 18.0
        } else {
            self.terms.y
        }
    }

    pub fn hit(self, x: f32, y: f32) -> Option<Action> {
        // A build with no Koru Cloud offers none of these; their rectangles
        // are empty, and an empty rectangle still contains its own corner.
        let accounts = self.accounts;
        if accounts && self.address.contains(x, y) {
            Some(Action::Address)
        } else if accounts && self.password.contains(x, y) {
            Some(Action::Password)
        } else if accounts && self.email_submit.contains(x, y) {
            Some(Action::ContinueWithEmail)
        } else if accounts && self.google.contains(x, y) {
            Some(Action::Google)
        } else if accounts && self.apple.contains(x, y) {
            Some(Action::Apple)
        } else if self.retry.is_some_and(|r| r.contains(x, y)) {
            Some(Action::Retry)
        } else if self.offline.contains(x, y) {
            Some(Action::ContinueOffline)
        } else if self.terms.contains(x, y) {
            Some(Action::Terms)
        } else if self.privacy.contains(x, y) {
            Some(Action::Privacy)
        } else {
            None
        }
    }
}

/// The width from which the presentation pane is shown. Below it the pane
/// would leave the column too narrow for the legal line on one row and the
/// lead in three, and the welcome column would no longer fit a 640px window
/// beside a failure.
pub const SIDEBAR_FROM: f32 = 760.0;

/// The persistent first-run frame. Form pages use the same split as Welcome
/// without constructing Welcome's actions or legal footer.
pub fn split(width: f32, height: f32) -> (Rect, Rect) {
    split_with(width, height, true)
}

/// Where the column starts in `right`: centred beside the pane, or at the
/// left edge when the form is embedded in a host page with its own head.
fn column_x(right: Rect, content_w: f32, pane: bool) -> f32 {
    if pane {
        right.x + (right.w - content_w) * 0.5
    } else {
        right.x
    }
}

/// [`split`], or no presentation pane at all when `pane` is false: the form
/// embedded in a page that already has its own frame (the Profile manager's
/// *New profile*) takes the whole width.
pub fn split_with(width: f32, height: f32, pane: bool) -> (Rect, Rect) {
    let width = width.max(1.0);
    let height = height.max(1.0);
    let sidebar_w = if pane && width >= SIDEBAR_FROM {
        (width * 0.42).clamp(272.0, 520.0)
    } else {
        0.0
    };
    (
        Rect::new(0.0, 0.0, sidebar_w, height),
        Rect::new(sidebar_w, 0.0, width - sidebar_w, height),
    )
}

/// The background, and the presentation pane unless `pane` is false
/// ([`split_with`]).
pub fn draw_backdrop(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    width: f32,
    height: f32,
    pane: bool,
) {
    let (sidebar, _) = split_with(width, height, pane);
    paint_backdrop(
        canvas,
        root,
        engine,
        Rect::new(0.0, 0.0, width, height),
        sidebar,
    );
}

fn paint_backdrop(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    viewport: Rect,
    sidebar: Rect,
) {
    let p = theme::p();
    scene::fill(
        canvas,
        root,
        p.bg,
        &scene::rect(viewport.x, viewport.y, viewport.w, viewport.h),
    );
    if sidebar.w > 0.0 {
        draw_sidebar(canvas, root, engine, sidebar);
    }
}

/// `failure` is the one [`Layout::new_with_top`] was given: the rectangles
/// were measured from it, so the two have to be the same.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    layout: Layout,
    failure: Option<Failure<'_>>,
    hovered: Option<Action>,
    focused: Option<Action>,
    // Whether both fields have been filled in. The caller owns the editors,
    // so only the caller can say; the button is drawn disabled until it does.
    submit_enabled: bool,
    // The caller's two editors, drawn into `layout.address` and
    // `layout.password`. Called **inside** the content scope, after the
    // labels and before the submit, so the fields sit in `WelcomeContent`
    // in the inspector and in reading order — not after it as strangers.
    fields: impl FnOnce(&mut Scene, &mut TextEngine),
) {
    paint_backdrop(canvas, root, engine, layout.viewport, layout.sidebar);
    draw_right(
        canvas,
        root,
        engine,
        layout,
        failure,
        hovered,
        focused,
        submit_enabled,
        fields,
    );
}

/// The welcome surface of a build with no Koru Cloud, laid out by
/// [`Layout::without_accounts`] from the same `note`.
///
/// The note is information, not a failure: the panel's own colors, no alert
/// mark and no *Try again*. *Continue without an account* is the primary
/// button, because it is the only way in.
pub fn draw_without_accounts(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    layout: Layout,
    note: CloudNote<'_>,
    hovered: Option<Action>,
    focused: Option<Action>,
) {
    paint_backdrop(canvas, root, engine, layout.viewport, layout.sidebar);
    let _content = inspect::scope(
        "WelcomeContent",
        "container",
        root,
        scene::rect(
            layout.right.x,
            layout.right.y,
            layout.right.w,
            layout.right.h,
        ),
        |_| {},
    );
    let p = theme::p();
    crate::KoruTextTitle::new(HEADING).draw(canvas, root, engine, layout.heading);
    if let Some(lead) = layout.lead {
        wrapped(
            canvas,
            root,
            engine,
            LEAD,
            lead.x,
            lead.y,
            lead.w,
            theme::role::row(),
            "theme::role::row()",
            p.text_dim,
        );
    }
    if let Some(rect) = layout.cloud_note {
        draw_cloud_note(canvas, root, engine, rect, note);
    }
    draw_button(
        canvas,
        root,
        engine,
        layout.offline,
        OFFLINE_LABEL,
        ButtonRole::Primary,
        hovered == Some(Action::ContinueOffline),
        focused == Some(Action::ContinueOffline),
    );
    draw_legal(canvas, root, engine, layout, hovered);
}

/// The quiet panel that says the build has no Koru Cloud: its title, and the
/// sentence under it.
fn draw_cloud_note(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    rect: Rect,
    note: CloudNote<'_>,
) {
    let p = theme::p();
    let _scope = inspect::scope(
        "WelcomeCloudNote",
        "component",
        root,
        scene::rect(rect.x, rect.y, rect.w, rect.h),
        |r| {
            r.property("title", note.title, "caller words");
            r.property("tone", "information, not a failure", "welcome");
        },
    );
    let shape = scene::rrect(rect.x, rect.y, rect.w, rect.h, 8.0);
    scene::fill(canvas, root, p.panel_bg, &shape);
    scene::stroke(canvas, root, p.panel_border, theme::BORDER_W, &shape);
    let inner = (rect.w - NOTE_PAD * 2.0).max(40.0);
    let (title_h, _) = measure_note(engine, note, rect.w);
    wrapped(
        canvas,
        root,
        engine,
        note.title,
        rect.x + NOTE_PAD,
        rect.y + NOTE_PAD,
        inner,
        note_title_style(),
        "theme::role::row() Bold",
        p.text,
    );
    wrapped(
        canvas,
        root,
        engine,
        note.body,
        rect.x + NOTE_PAD,
        rect.y + NOTE_PAD + title_h + NOTE_LINE_GAP,
        inner,
        theme::role::meta(),
        "theme::role::meta()",
        p.text_dim,
    );
}

fn draw_sidebar(canvas: &mut Scene, root: Affine, engine: &mut TextEngine, area: Rect) {
    let _sidebar = inspect::scope(
        "WelcomeSidebar",
        "container",
        root,
        scene::rect(area.x, area.y, area.w, area.h),
        |_| {},
    );
    let (background, foreground, dim, accent) = theme::welcome_sidebar_colors();
    scene::fill(
        canvas,
        root,
        background,
        &scene::rect(area.x, area.y, area.w, area.h),
    );
    const LOGO: f32 = 34.0;
    const LOGO_Y: f32 = 40.0;
    crate::logo::KoruLogo::new(LOGO).draw(canvas, root, area.x + 44.0, LOGO_Y);
    // The product's name is a title, the same component as the content's
    // *Welcome to Koru*, centred on the mark beside it.
    let brand = crate::KoruTextTitle::new("Koru").with_color(foreground);
    let brand_w = area.w - 90.0 - 44.0;
    let brand_h = brand.measure(engine, brand_w);
    brand.draw(
        canvas,
        root,
        engine,
        Rect::new(area.x + 90.0, LOGO_Y + (LOGO - brand_h) * 0.5, brand_w, 0.0),
    );
    let hero_y = (area.h * 0.39).clamp(190.0, 300.0);
    let copy_w = area.w - 88.0;
    let hero_style = if area.w < 380.0 {
        theme::TextStyle::new(theme::text_metrics::XL, theme::FontWeight::Bold)
    } else {
        HERO
    };
    // The paragraph's returned height is the wrapped layout's — one measure,
    // one draw, no second layout for the stacking arithmetic.
    let hero_h = crate::text_paragraph::KoruTextParagraph::new(
        HERO_COPY,
        hero_style,
        "welcome::HERO (theme::text_metrics::XL Bold under 380 px)",
        foreground,
    )
    .draw(
        canvas,
        root,
        engine,
        Rect::new(area.x + 44.0, hero_y, copy_w, 0.0),
    );
    let body_y = hero_y + hero_h + 16.0;
    let body_h = crate::text_paragraph::KoruTextParagraph::new(
        BODY_COPY,
        theme::role::row(),
        "theme::role::row()",
        dim,
    )
    .draw(
        canvas,
        root,
        engine,
        Rect::new(area.x + 44.0, body_y, copy_w, 0.0),
    );
    let mut bullet_y = body_y + body_h + 20.0;
    for (index, label) in [
        "Works fully offline",
        "Private — your data stays yours",
        "Syncs peer-to-peer across your devices",
    ]
    .into_iter()
    .enumerate()
    {
        let dot = Circle::new(((area.x + 48.0) as f64, (bullet_y + 7.0) as f64), 3.0);
        inspect::leaf("Bullet", root, dot.bounding_box(), |record| {
            record.property("shape", "circle, radius 3", "welcome sidebar");
            record.property(
                "colour",
                inspect::color(accent),
                "theme::welcome_sidebar_colors",
            );
        });
        // The named Bullet above is the record; the paint below is vello's, so
        // the dot does not also appear as an anonymous fill.
        canvas.fill(Fill::NonZero, root, accent, None, &dot);
        let label_h = crate::text_paragraph::KoruTextParagraph::new(
            label,
            theme::role::meta(),
            "theme::role::meta()",
            foreground,
        )
        .draw(
            canvas,
            root,
            engine,
            Rect::new(area.x + 61.0, bullet_y, area.w - 105.0, 0.0),
        );
        bullet_y += label_h + if index == 2 { 0.0 } else { 10.0 };
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_right(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    layout: Layout,
    failure: Option<Failure<'_>>,
    hovered: Option<Action>,
    focused: Option<Action>,
    submit_enabled: bool,
    fields: impl FnOnce(&mut Scene, &mut TextEngine),
) {
    let _content = inspect::scope(
        "WelcomeContent",
        "container",
        root,
        scene::rect(
            layout.right.x,
            layout.right.y,
            layout.right.w,
            layout.right.h,
        ),
        |_| {},
    );
    let p = theme::p();
    crate::KoruTextTitle::new(HEADING).draw(canvas, root, engine, layout.heading);
    if let Some(lead) = layout.lead {
        wrapped(
            canvas,
            root,
            engine,
            LEAD,
            lead.x,
            lead.y,
            lead.w,
            theme::role::row(),
            "theme::role::row()",
            p.text_dim,
        );
    }
    // The labels, not the fields: the editor is the caller's, and so is
    // everything inside these two rectangles.
    for (rect, label) in [
        (layout.address, ADDRESS_LABEL),
        (layout.password, PASSWORD_LABEL),
    ] {
        text(
            canvas,
            root,
            engine,
            label,
            rect.x,
            rect.y - LABEL_H,
            theme::role::meta(),
            "theme::role::meta()",
            p.text_dim,
        );
    }
    fields(canvas, engine);
    draw_button_enabled(
        canvas,
        root,
        engine,
        layout.email_submit,
        CONTINUE_LABEL,
        ButtonRole::Primary,
        hovered == Some(Action::ContinueWithEmail),
        focused == Some(Action::ContinueWithEmail),
        submit_enabled,
    );
    draw_divider(canvas, root, engine, layout);
    draw_google_button(
        canvas,
        root,
        engine,
        layout.google,
        GOOGLE_LABEL,
        hovered == Some(Action::Google),
        focused == Some(Action::Google),
    );
    draw_apple_button(
        canvas,
        root,
        engine,
        layout.apple,
        APPLE_LABEL,
        hovered == Some(Action::Apple),
        focused == Some(Action::Apple),
        true,
    );
    let failed = failure.map(|f| f.at);
    if failed == Some(Action::Google) {
        draw_failed_outline(canvas, root, layout.google);
    } else if failed == Some(Action::Apple) {
        draw_failed_outline(canvas, root, layout.apple);
    }
    draw_button(
        canvas,
        root,
        engine,
        layout.offline,
        OFFLINE_LABEL,
        ButtonRole::Secondary,
        hovered == Some(Action::ContinueOffline),
        focused == Some(Action::ContinueOffline),
    );
    if let Some(failure) = failure {
        draw_failure(canvas, root, engine, layout, failure, hovered);
    }
    draw_legal(canvas, root, engine, layout, hovered);
}

/// The failure, where the layout put it: a red mark and the message under a
/// field, or the same inside a tinted banner under an action's row, with
/// *Try again* at its right edge.
fn draw_failure(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    layout: Layout,
    failure: Failure<'_>,
    hovered: Option<Action>,
) {
    let Some(rect) = layout.notice else {
        return;
    };
    let p = theme::p();
    let red = p.close_hover;
    let _scope = inspect::scope(
        "WelcomeFailure",
        "component",
        root,
        scene::rect(
            layout.banner.map_or(rect.x - MARK_D - MARK_GAP, |b| b.x),
            layout.banner.map_or(rect.y, |b| b.y),
            layout.banner.map_or(rect.w + MARK_D + MARK_GAP, |b| b.w),
            layout.banner.map_or(rect.h, |b| b.h),
        ),
        |r| {
            r.property("about", format!("{:?}", failure.at), "caller failure");
            r.property("retry", failure.retry, "caller failure");
        },
    );
    if let Some(banner) = layout.banner {
        let shape = scene::rrect(banner.x, banner.y, banner.w, banner.h, 8.0);
        scene::fill(canvas, root, red.multiply_alpha(0.08), &shape);
        scene::stroke(
            canvas,
            root,
            red.multiply_alpha(0.35),
            theme::BORDER_W,
            &shape,
        );
    }
    draw_alert_mark(
        canvas,
        root,
        engine,
        rect.x - MARK_GAP - MARK_D,
        rect.y + (theme::role::meta().metrics.line_height - MARK_D) * 0.5,
    );
    wrapped(
        canvas,
        root,
        engine,
        failure.message,
        rect.x,
        rect.y,
        rect.w,
        theme::role::meta(),
        "theme::role::meta()",
        red,
    );
    if let Some(retry) = layout.retry {
        let style = theme::TextStyle::new(theme::role::meta().metrics, theme::FontWeight::Bold);
        let label = engine.simple_layout(RETRY_LABEL, style);
        crate::typography::line(
            "Retry link",
            canvas,
            root,
            &label,
            retry.x,
            retry.y,
            style,
            "theme::role::meta() Bold",
            if hovered == Some(Action::Retry) {
                p.text
            } else {
                red
            },
        );
        let underline = retry.y + scene::first_line_height(&label) - 2.0;
        scene::stroke(
            canvas,
            root,
            red,
            1.0,
            &Line::new(
                (retry.x as f64, underline as f64),
                ((retry.x + label.width()) as f64, underline as f64),
            ),
        );
    }
}

/// A filled red disc with an exclamation mark: the alert the failure starts
/// with. Drawn rather than taken from the icon font, which has no circled
/// alert glyph.
fn draw_alert_mark(canvas: &mut Scene, root: Affine, engine: &mut TextEngine, x: f32, y: f32) {
    let p = theme::p();
    let r = MARK_D * 0.5;
    let disc = Circle::new(((x + r) as f64, (y + r) as f64), r as f64);
    inspect::leaf("Alert mark", root, disc.bounding_box(), |record| {
        record.property("shape", "circle with !", "welcome failure");
    });
    canvas.fill(Fill::NonZero, root, p.close_hover, None, &disc);
    let style = theme::TextStyle::new(theme::role::meta().metrics, theme::FontWeight::Bold);
    let bang = engine.simple_layout("!", style);
    crate::typography::line(
        "Alert mark glyph",
        canvas,
        root,
        &bang,
        x + r - bang.width() * 0.5,
        y + r - scene::first_line_height(&bang) * 0.5,
        style,
        "theme::role::meta() Bold",
        p.bg,
    );
}

/// A hairline either side of a quiet "or", across the whole column between
/// the email form and the provider row. It is a separator, not a control:
/// nothing hit-tests against it.
fn draw_divider(canvas: &mut Scene, root: Affine, engine: &mut TextEngine, layout: Layout) {
    let p = theme::p();
    let label = engine.simple_layout(DIVIDER_LABEL, theme::role::meta());
    let column = layout.email_submit;
    let y = layout.divider.y + layout.divider.h * 0.5;
    let gap = label.width() + 20.0;
    let side = (column.w - gap) * 0.5;
    for x in [column.x, column.x + column.w - side] {
        scene::stroke(
            canvas,
            root,
            p.panel_border,
            theme::BORDER_W,
            &Line::new((x as f64, y as f64), ((x + side) as f64, y as f64)),
        );
    }
    crate::typography::line(
        "Divider label",
        canvas,
        root,
        &label,
        column.x + side + 10.0,
        y - scene::first_line_height(&label) * 0.5,
        theme::role::meta(),
        "theme::role::meta()",
        p.text_dim,
    );
}

fn draw_legal(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    layout: Layout,
    hovered: Option<Action>,
) {
    let p = theme::p();
    let prefix = LEGAL_PREFIX;
    let join = LEGAL_JOIN;
    let prefix_w = legal_width(engine, prefix);
    let x = if layout.legal_stacked {
        layout.right.x + (layout.right.w - prefix_w) * 0.5
    } else {
        layout.terms.x - prefix_w
    };
    let y = if layout.legal_stacked {
        layout.terms.y - 18.0
    } else {
        layout.terms.y
    };
    text(
        canvas,
        root,
        engine,
        prefix,
        x,
        y,
        theme::role::meta(),
        "theme::role::meta()",
        p.text_dim,
    );
    text(
        canvas,
        root,
        engine,
        "Terms of Service",
        layout.terms.x,
        layout.terms.y,
        theme::role::meta(),
        "theme::role::meta()",
        if hovered == Some(Action::Terms) {
            p.text
        } else {
            p.accent
        },
    );
    text(
        canvas,
        root,
        engine,
        join,
        layout.terms.x + layout.terms.w,
        layout.terms.y,
        theme::role::meta(),
        "theme::role::meta()",
        p.text_dim,
    );
    text(
        canvas,
        root,
        engine,
        "Privacy Policy",
        layout.privacy.x,
        layout.privacy.y,
        theme::role::meta(),
        "theme::role::meta()",
        if hovered == Some(Action::Privacy) {
            p.text
        } else {
            p.accent
        },
    );
}

const LEGAL_PREFIX: &str = "By continuing, you agree to the ";
const LEGAL_JOIN: &str = " and the ";

/// The advance of a legal-line fragment, **trailing space included**.
///
/// A layout's width ends at its last glyph's ink, so `"… the "` measures the
/// same as `"… the"` and the link placed after it touched the word: "theTerms
/// of Service". The space is measured between two letters, where it does
/// count, and added back.
fn legal_width(engine: &mut TextEngine, fragment: &str) -> f32 {
    let style = theme::role::meta();
    let trimmed = fragment.trim_end();
    let spaces = (fragment.len() - trimmed.len()) as f32;
    let space =
        engine.simple_layout("a a", style).width() - engine.simple_layout("aa", style).width();
    engine.simple_layout(trimmed, style).width() + spaces * space
}

#[allow(clippy::too_many_arguments)]
fn text(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    value: &str,
    x: f32,
    y: f32,
    style: theme::TextStyle,
    role: &'static str,
    color: vello::peniko::Color,
) {
    crate::text_paragraph::draw_line(canvas, root, engine, value, x, y, style, role, color);
}

#[allow(clippy::too_many_arguments)]
fn wrapped(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    value: &str,
    x: f32,
    y: f32,
    width: f32,
    style: theme::TextStyle,
    role: &'static str,
    color: vello::peniko::Color,
) {
    crate::text_paragraph::KoruTextParagraph::new(value, style, role, color).draw(
        canvas,
        root,
        engine,
        Rect::new(x, y, width, 0.0),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: CloudNote<'static> = CloudNote {
        title: "Koru Cloud isn’t set up in this build",
        body: "This build of Koru has no Koru Cloud server, so sign-in and Cloud sync are off. Everything on this device works without them.",
    };

    #[test]
    fn a_build_without_koru_cloud_offers_no_sign_in_anywhere_on_the_welcome() {
        let mut engine = TextEngine::new();
        for (w, h) in [(1234.0, 702.0), (452.0, 640.0), (800.0, 420.0)] {
            let layout = Layout::without_accounts(&mut engine, w, h, 0.0, NOTE);
            assert!(!layout.accounts);
            let note = layout.cloud_note.expect("the note stands in their place");
            assert!(note.h > 0.0);
            assert!(
                layout.offline.y >= note.y + note.h + OFFLINE_GAP - 0.01,
                "{w}x{h}"
            );
            assert!(
                layout.offline.y + layout.offline.h <= layout.legal_top(),
                "{w}x{h}"
            );
            assert_eq!(layout.notice, None);
            assert_eq!(layout.retry, None, "nothing to try again");
            // No point on the screen is a way to sign in.
            let mut x = 0.0;
            while x <= w {
                let mut y = 0.0;
                while y <= h {
                    assert!(
                        !matches!(
                            layout.hit(x, y),
                            Some(
                                Action::Address
                                    | Action::Password
                                    | Action::ContinueWithEmail
                                    | Action::Google
                                    | Action::Apple
                                    | Action::Retry
                            )
                        ),
                        "{x},{y} at {w}x{h}"
                    );
                    y += 2.0;
                }
                x += 2.0;
            }
            let centre = |r: Rect| (r.x + r.w * 0.5, r.y + r.h * 0.5);
            let (x, y) = centre(layout.offline);
            assert_eq!(layout.hit(x, y), Some(Action::ContinueOffline));
            let (x, y) = centre(layout.terms);
            assert_eq!(layout.hit(x, y), Some(Action::Terms));
        }
        // The ordinary welcome is unchanged: it offers accounts and no note.
        let ordinary = Layout::new(&mut engine, 1234.0, 702.0);
        assert!(ordinary.accounts);
        assert_eq!(ordinary.cloud_note, None);
    }

    #[test]
    fn wide_layout_has_a_dark_pane_and_bounded_actions() {
        let mut engine = TextEngine::new();
        let layout = Layout::new(&mut engine, 1234.0, 702.0);
        assert!(layout.sidebar.w >= 272.0);
        assert_eq!(layout.right.x, layout.sidebar.w);
        for rect in [
            layout.address,
            layout.password,
            layout.email_submit,
            layout.google,
            layout.apple,
            layout.offline,
            layout.offline,
            layout.terms,
            layout.privacy,
        ] {
            assert!(layout.viewport.contains(rect.x, rect.y));
            assert!(layout.viewport.contains(rect.x + rect.w, rect.y + rect.h));
        }
    }

    #[test]
    fn the_providers_share_one_row_and_the_offline_button_stands_apart_under_them() {
        let mut engine = TextEngine::new();
        for (w, h) in [(1234.0, 702.0), (452.0, 640.0)] {
            let layout = Layout::new(&mut engine, w, h);
            assert_eq!(layout.google.y, layout.apple.y, "{w}x{h}");
            assert_eq!(layout.google.w, layout.apple.w, "{w}x{h}");
            assert!(
                layout.google.x + layout.google.w < layout.apple.x,
                "{w}x{h}"
            );
            assert_eq!(
                layout.apple.x + layout.apple.w,
                layout.email_submit.x + layout.email_submit.w,
                "the row spans the column at {w}x{h}"
            );
            // Set apart from the providers, and as wide as the column.
            assert!(
                layout.offline.y >= layout.google.y + layout.google.h + OFFLINE_GAP - 0.01,
                "{w}x{h}"
            );
            assert_eq!(layout.offline.x, layout.email_submit.x, "{w}x{h}");
            assert_eq!(layout.offline.w, layout.email_submit.w, "{w}x{h}");
            assert_eq!(
                layout.hit(
                    layout.offline.x + layout.offline.w * 0.5,
                    layout.offline.y + layout.offline.h * 0.5
                ),
                Some(Action::ContinueOffline)
            );
            // The gap above it is not part of it.
            assert_eq!(
                layout.hit(
                    layout.offline.x + 20.0,
                    layout.offline.y - OFFLINE_GAP * 0.5
                ),
                None
            );
        }
    }

    #[test]
    fn the_drawn_surface_registers_logo_bullets_and_paragraphs_once() {
        let mut engine = TextEngine::new();
        let layout = Layout::new(&mut engine, 1234.0, 702.0);
        let mut canvas = Scene::new();
        let root = Affine::IDENTITY;
        let capture =
            koru_draw::inspect::Frame::begin(1, root, scene::rect(0.0, 0.0, 1234.0, 702.0));
        draw(
            &mut canvas,
            root,
            &mut engine,
            layout,
            None,
            None,
            None,
            true,
            |_, _| {},
        );
        let snapshot = capture.finish();
        assert_eq!(
            snapshot
                .records
                .iter()
                .filter(|r| r.name == "WelcomeSidebar")
                .count(),
            1,
            "one surface, one sidebar — a second paint is the old double-draw"
        );
        assert_eq!(
            snapshot
                .records
                .iter()
                .filter(|r| r.name == "KoruLogo")
                .count(),
            1
        );
        assert_eq!(
            snapshot
                .records
                .iter()
                .filter(|r| r.name == "Bullet")
                .count(),
            3
        );
        assert!(
            snapshot
                .records
                .iter()
                .any(|r| r.name == "KoruTextParagraph")
        );
        // Every wrapped visual line hangs inside a paragraph — never loose
        // beside it, which was the sibling complaint the component answers.
        let paragraphs: Vec<usize> = snapshot
            .records
            .iter()
            .enumerate()
            .filter_map(|(i, r)| (r.name == "KoruTextParagraph").then_some(i))
            .collect();
        assert!(!paragraphs.is_empty());
        for record in &snapshot.records {
            if record.name != "text line" {
                continue;
            }
            let mut ancestor = record.parent;
            let mut inside = false;
            while let Some(p) = ancestor {
                if paragraphs.contains(&p) {
                    inside = true;
                    break;
                }
                ancestor = snapshot.records[p].parent;
            }
            assert!(inside, "a text line escaped its paragraph");
        }
    }

    #[test]
    fn narrow_layout_drops_the_presentation_pane_without_overflow() {
        let mut engine = TextEngine::new();
        let layout = Layout::new(&mut engine, 360.0, 520.0);
        assert_eq!(layout.sidebar.w, 0.0);
        assert!(layout.google.x >= 0.0);
        assert!(layout.google.x + layout.google.w <= 360.0);
        assert!(layout.privacy.x + layout.privacy.w <= 360.0);
    }

    /// The message the welcome screen actually shows when a sign-in fails.
    const NOTICE: &str = "Your local Profiles and content are still available. You can try again without losing your place.";

    fn at(action: Action) -> Failure<'static> {
        Failure {
            at: action,
            message: NOTICE,
            retry: !matches!(action, Action::Address | Action::Password),
        }
    }

    /// The onboarding window's 452x640 minimum with and without the caption,
    /// the widths either side of the point where the presentation pane
    /// appears, the default window, and a tall one.
    const SIZES: [(f32, f32, f32); 6] = [
        (452.0, 640.0, 0.0),
        (452.0, 640.0, theme::CAPTION_H),
        (SIDEBAR_FROM - 1.0, 640.0, theme::CAPTION_H),
        (SIDEBAR_FROM, 640.0, theme::CAPTION_H),
        (1000.0, 680.0, theme::CAPTION_H),
        (2560.0, 1440.0, theme::CAPTION_H),
    ];

    const PLACES: [Action; 5] = [
        Action::Address,
        Action::Password,
        Action::ContinueWithEmail,
        Action::Google,
        Action::Apple,
    ];

    #[test]
    fn there_is_no_notice_rectangle_when_there_is_no_message() {
        let mut engine = TextEngine::new();
        let layout = Layout::new(&mut engine, 1000.0, 680.0);
        assert!(layout.notice.is_none());
        assert!(layout.banner.is_none());
        assert!(layout.retry.is_none());
        assert!(layout.lead.is_some());
    }

    #[test]
    fn the_resting_column_clears_the_legal_line_at_every_supported_size() {
        let mut engine = TextEngine::new();
        for (w, h, top) in SIZES {
            let layout = Layout::new_with_top(&mut engine, w, h, top, None);
            assert!(layout.heading.y >= top, "{w}x{h}+{top}");
            assert!(
                layout.offline.y + layout.offline.h <= layout.legal_top(),
                "the card runs into the legal line at {w}x{h}+{top}"
            );
        }
    }

    #[test]
    fn a_field_failure_is_written_directly_under_its_field() {
        let mut engine = TextEngine::new();
        for field in [Action::Address, Action::Password] {
            let layout = Layout::new_with_top(
                &mut engine,
                1000.0,
                680.0,
                theme::CAPTION_H,
                Some(at(field)),
            );
            let rect = if field == Action::Address {
                layout.address
            } else {
                layout.password
            };
            let notice = layout.notice.expect("a rectangle to write the failure in");
            assert!(layout.banner.is_none() && layout.retry.is_none());
            assert!(notice.y >= rect.y + rect.h, "{field:?}");
            assert!(notice.x > rect.x, "the mark sits before the text");
            assert!(notice.x + notice.w <= rect.x + rect.w, "{field:?}");
        }
        // And the field under it moved down to make room.
        let plain = Layout::new_with_top(&mut engine, 1000.0, 680.0, theme::CAPTION_H, None);
        let failed = Layout::new_with_top(
            &mut engine,
            1000.0,
            680.0,
            theme::CAPTION_H,
            Some(at(Action::Address)),
        );
        assert!(failed.password.y > plain.password.y);
        assert!(failed.notice.unwrap().y + failed.notice.unwrap().h <= failed.password.y - LABEL_H);
    }

    #[test]
    fn an_action_failure_is_a_banner_under_its_row_with_try_again() {
        let mut engine = TextEngine::new();
        for (action, row) in [
            (
                Action::ContinueWithEmail,
                (|l: Layout| l.email_submit) as fn(Layout) -> Rect,
            ),
            (Action::Google, |l: Layout| l.google),
            (Action::Apple, |l: Layout| l.apple),
        ] {
            let layout = Layout::new_with_top(
                &mut engine,
                1000.0,
                680.0,
                theme::CAPTION_H,
                Some(at(action)),
            );
            let banner = layout.banner.expect("a banner");
            let notice = layout.notice.expect("its text");
            let retry = layout.retry.expect("try again");
            assert!(banner.y >= row(layout).y + row(layout).h, "{action:?}");
            assert_eq!(banner.x, layout.email_submit.x, "{action:?}");
            assert_eq!(banner.w, layout.email_submit.w, "{action:?}");
            for inner in [notice, retry] {
                assert!(banner.contains(inner.x, inner.y), "{action:?}");
                assert!(
                    banner.contains(inner.x + inner.w, inner.y + inner.h),
                    "{action:?}"
                );
            }
            assert!(notice.x + notice.w <= retry.x, "{action:?}");
            assert_eq!(
                layout.hit(retry.x + 1.0, retry.y + retry.h * 0.5),
                Some(Action::Retry)
            );
        }
        // A provider failure sits under the whole row, not one half of it.
        let layout = Layout::new_with_top(
            &mut engine,
            1000.0,
            680.0,
            theme::CAPTION_H,
            Some(at(Action::Apple)),
        );
        assert!(layout.banner.unwrap().y >= layout.google.y + layout.google.h);
        assert!(layout.offline.y >= layout.banner.unwrap().y + layout.banner.unwrap().h);
    }

    #[test]
    fn without_try_again_the_banner_has_no_link() {
        let mut engine = TextEngine::new();
        let failure = Failure {
            retry: false,
            ..at(Action::Google)
        };
        let layout = Layout::new_with_top(&mut engine, 1000.0, 680.0, 0.0, Some(failure));
        assert!(layout.banner.is_some());
        assert!(layout.retry.is_none());
    }

    #[test]
    fn a_failure_never_runs_into_the_legal_line_at_any_supported_size() {
        let mut engine = TextEngine::new();
        for (w, h, top) in SIZES {
            for place in PLACES {
                let layout = Layout::new_with_top(&mut engine, w, h, top, Some(at(place)));
                let what = format!("{place:?} at {w}x{h}+{top}");
                assert!(layout.heading.y >= top, "{what}");
                assert!(
                    layout.offline.y + layout.offline.h <= layout.legal_top(),
                    "the card runs into the legal line: {what}"
                );
                let notice = layout.notice.expect("a rectangle");
                assert!(notice.x + notice.w <= w, "{what}");
            }
        }
    }

    #[test]
    fn a_failure_moves_only_what_is_under_it_when_there_is_room() {
        let mut engine = TextEngine::new();
        let plain = Layout::new_with_top(&mut engine, 2560.0, 1440.0, theme::CAPTION_H, None);
        let failed = Layout::new_with_top(
            &mut engine,
            2560.0,
            1440.0,
            theme::CAPTION_H,
            Some(at(Action::Google)),
        );
        assert_eq!(failed.heading, plain.heading);
        assert_eq!(failed.lead, plain.lead);
        assert_eq!(failed.google, plain.google);
        assert!(failed.offline.y > plain.offline.y);
        assert_eq!(failed.terms, plain.terms);
    }

    #[test]
    fn the_divider_gets_the_designs_air_where_there_is_room_and_gives_it_up_first() {
        let mut engine = TextEngine::new();
        let full = DIVIDER_LABEL_H + DIVIDER_AIR * 2.0;
        // The default window has the full air; a taller one keeps the lead
        // beside a three-line banner too.
        let resting = Layout::new_with_top(&mut engine, 1000.0, 680.0, theme::CAPTION_H, None);
        assert_eq!(resting.divider.h, full);
        let failed = Layout::new_with_top(
            &mut engine,
            1000.0,
            760.0,
            theme::CAPTION_H,
            Some(at(Action::Google)),
        );
        assert!(failed.lead.is_some());
        // The "or" sits between the submit and the providers.
        assert_eq!(
            resting.divider.y,
            resting.email_submit.y + resting.email_submit.h
        );
        assert_eq!(resting.google.y, resting.divider.y + resting.divider.h);
        // Every size: never under the floor.
        for (w, h, top) in SIZES {
            for failure in [None, Some(at(Action::ContinueWithEmail))] {
                let layout = Layout::new_with_top(&mut engine, w, h, top, failure);
                assert!(layout.divider.h >= DIVIDER_LABEL_H + DIVIDER_AIR_MIN * 2.0 - 0.01);
                assert!(layout.divider.h <= full + 0.01);
            }
        }
    }

    #[test]
    fn a_short_window_gives_up_the_lead_before_the_failure() {
        let mut engine = TextEngine::new();
        // Every supported size holds the lead beside a banner…
        for (w, h, top) in SIZES {
            let layout = Layout::new_with_top(&mut engine, w, h, top, Some(at(Action::Google)));
            assert!(layout.lead.is_some(), "{w}x{h}+{top}");
        }
        // …and a window shorter than any of them gives it up first.
        let layout = Layout::new_with_top(
            &mut engine,
            452.0,
            560.0,
            theme::CAPTION_H,
            Some(at(Action::Google)),
        );
        assert!(layout.lead.is_none());
        assert!(layout.notice.is_some());
    }

    #[test]
    fn the_legal_links_do_not_touch_the_words_before_them() {
        let mut engine = TextEngine::new();
        let space = legal_width(&mut engine, "a ") - legal_width(&mut engine, "a");
        assert!(space > 1.0, "a trailing space has width");
        let layout = Layout::new(&mut engine, 1234.0, 702.0);
        let and_the = engine
            .simple_layout(LEGAL_JOIN.trim_end(), theme::role::meta())
            .width();
        assert!(layout.privacy.x - (layout.terms.x + layout.terms.w) > and_the + 1.0);
    }

    #[test]
    fn fixed_destinations_are_https_and_support_is_a_mail_action() {
        assert_eq!(TERMS_URL, "https://www.koru.do/terms");
        assert_eq!(PRIVACY_URL, "https://www.koru.do/privacy");
        assert_eq!(
            CHAT_URL,
            "https://www.koru.do/?utm_source=desktop-welcome&utm_medium=app&utm_campaign=welcome&chat=true"
        );
        assert_eq!(SUPPORT_MAILTO, format!("mailto:{SUPPORT_ADDRESS}"));
    }
}
