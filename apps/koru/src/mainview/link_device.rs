// SPDX-License-Identifier: MPL-2.0

//! The global "Link another device" modal: *New device* (show a link code
//! and its QR) and *Enter link code*, then *Device linked*.
//!
//! This owns presentation only. Agent mints and accepts link codes and
//! persists trust; the modal receives whole pairing snapshots and reports
//! what the person did. The Send/Accept exchange is deliberately kept apart
//! from the device list so a later workspace invitation can reuse it without
//! inheriting all-data device semantics.
//!
//! **The list is not here any more.** The devices this computer is linked to
//! — renaming this one, the relay switch, unlinking and each device's history
//! — are the Manage tab's *Linked devices* page (`manage::devices`). After a
//! pairing succeeds the dialog says *Device linked*, and *Done* opens that
//! page (maintainers' decision 12).

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{scene, theme};
use koru_ui::{ButtonRole, InputStyle, TextInput};
use qrcode::{Color as QrColor, QrCode};
use std::time::{Duration, Instant};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::CursorIcon;

const WIDTH: f32 = 620.0;
const HEIGHT: f32 = 600.0;
const PAD: f32 = 24.0;
const TAB_H: f32 = 36.0;
const INPUT_H: f32 = 36.0;
const BUTTON_H: f32 = 32.0;
const QR_SIZE: f32 = 230.0;
const QR_QUIET_MODULES: usize = 4;
const COPY_FEEDBACK: Duration = Duration::from_secs(2);

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(super) enum Tab {
    New,
    Enter,
}

pub(super) enum Outcome {
    Keep,
    Close,
    Copy(String),
    Generate,
    Accept(String),
    /// *Done* after *Device linked*: close, and open Manage › Linked devices.
    Done,
    /// The person confirmed *Start over as a new device*.
    StartOver,
}

/// Where *Start over as a new device* stands in this window. Shown only while
/// the Agent reports an identity problem — a working identity never offers it.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum StartOver {
    Offered,
    Confirming,
    /// Sent; the Agent is stopping for it and the window will relaunch.
    Working,
    /// The Agent refused or failed; the button is offered again.
    Failed,
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum Hit {
    Tab(Tab),
    Input,
    Copy,
    Generate,
    Accept,
    Close,
    Done,
    StartOver,
    StartOverCancel,
    StartOverConfirm,
}

pub(super) struct LinkDevice {
    tab: Tab,
    input: TextInput,
    pairing: koru_agent_wire::PairingView,
    /// A pairing succeeded: the dialog says *Device linked* and offers
    /// *Done* until it closes.
    linked: bool,
    qr: Option<(usize, Vec<bool>)>,
    hover: Option<Hit>,
    copied_until: Option<Instant>,
    /// Why this device cannot sync, from the Agent; `Some` replaces the tabs
    /// with the one thing that helps (ADR-413's row, on the desktop).
    identity_problem: Option<String>,
    start_over: StartOver,
}

impl LinkDevice {
    pub(super) fn new(engine: &mut TextEngine) -> Self {
        Self {
            tab: Tab::New,
            input: TextInput::new(engine, theme::role::row()),
            pairing: idle_pairing(),
            linked: false,
            qr: None,
            hover: None,
            copied_until: None,
            identity_problem: None,
            start_over: StartOver::Offered,
        }
    }

    /// Adopt the Agent's word on this device's identity. `None` is the
    /// ordinary window.
    pub(super) fn set_identity_problem(&mut self, problem: Option<String>) {
        if problem.is_none() {
            self.start_over = StartOver::Offered;
        }
        self.identity_problem = problem;
    }

    /// The Agent refused the reset, or could not ask for it.
    pub(super) fn start_over_refused(&mut self) {
        self.start_over = StartOver::Failed;
    }

    fn start_over_rect(panel: Rect) -> Rect {
        let w = (panel.w - PAD * 2.0).min(340.0);
        Rect::new(
            panel.x + (panel.w - w) * 0.5,
            panel.y + panel.h - 126.0,
            w,
            BUTTON_H,
        )
    }

    /// Adopt Agent's pairing state. Success is a completed navigation event:
    /// the one-use secret leaves the field and the dialog says *Device
    /// linked* rather than leaving the person on a submitted form.
    pub(super) fn set_pairing(
        &mut self,
        engine: &mut TextEngine,
        pairing: koru_agent_wire::PairingView,
    ) -> bool {
        let succeeded = pairing.state == koru_agent_wire::PairingState::Succeeded;
        self.qr = pairing.qr_payload.as_deref().and_then(qr_matrix);
        self.pairing = pairing;
        if succeeded {
            self.input.set_text(engine, "");
            self.linked = true;
            self.hover = None;
        }
        succeeded
    }

    pub(super) fn mark_copied(&mut self, now: Instant) {
        self.copied_until = Some(now + COPY_FEEDBACK);
    }

    /// The dialog's one deadline: copy feedback.
    pub(super) fn maintenance_deadline(&self) -> Option<Instant> {
        self.copied_until
    }

    pub(super) fn maintain(&mut self, now: Instant) -> bool {
        if self.copied_until.is_some_and(|deadline| now >= deadline) {
            self.copied_until = None;
            return true;
        }
        false
    }

    fn copy_label(&self, now: Instant) -> &'static str {
        if self.copied_until.is_some_and(|deadline| now < deadline) {
            "Code copied"
        } else {
            "Copy link code"
        }
    }

    fn panel(viewport: (f32, f32)) -> Rect {
        let w = WIDTH.min((viewport.0 - 32.0).max(280.0));
        let h = HEIGHT.min((viewport.1 - 32.0).max(320.0));
        Rect::new(
            ((viewport.0 - w) * 0.5).max(8.0),
            ((viewport.1 - h) * 0.5).max(8.0),
            w,
            h,
        )
    }

    fn tabs(panel: Rect) -> [(Tab, Rect); 2] {
        let y = panel.y + 70.0;
        let w = (panel.w - PAD * 2.0) / 2.0;
        [
            (Tab::New, Rect::new(panel.x + PAD, y, w, TAB_H)),
            (Tab::Enter, Rect::new(panel.x + PAD + w, y, w, TAB_H)),
        ]
    }

    fn input_rect(panel: Rect) -> Rect {
        Rect::new(panel.x + PAD, panel.y + 184.0, panel.w - PAD * 2.0, INPUT_H)
    }

    fn action_rect(panel: Rect, left: bool) -> Rect {
        let w = 150.0;
        let gap = 12.0;
        let total = w * 2.0 + gap;
        Rect::new(
            panel.x + (panel.w - total) * 0.5 + if left { 0.0 } else { w + gap },
            panel.y + panel.h - 126.0,
            w,
            BUTTON_H,
        )
    }

    /// *Close*, or *Done* once a device is linked.
    fn close_rect(panel: Rect) -> Rect {
        Rect::new(
            panel.x + panel.w - PAD - 88.0,
            panel.y + panel.h - PAD - BUTTON_H,
            88.0,
            BUTTON_H,
        )
    }

    fn hit_at(&self, viewport: (f32, f32), x: f32, y: f32) -> Option<Hit> {
        let panel = Self::panel(viewport);
        if self.identity_problem.is_some() {
            if Self::close_rect(panel).contains(x, y) {
                return Some(Hit::Close);
            }
            return match self.start_over {
                StartOver::Offered | StartOver::Failed
                    if Self::start_over_rect(panel).contains(x, y) =>
                {
                    Some(Hit::StartOver)
                }
                StartOver::Confirming if Self::action_rect(panel, true).contains(x, y) => {
                    Some(Hit::StartOverCancel)
                }
                StartOver::Confirming if Self::action_rect(panel, false).contains(x, y) => {
                    Some(Hit::StartOverConfirm)
                }
                _ => None,
            };
        }
        if Self::close_rect(panel).contains(x, y) {
            return Some(if self.linked { Hit::Done } else { Hit::Close });
        }
        if self.linked {
            return None;
        }
        for (tab, rect) in Self::tabs(panel) {
            if rect.contains(x, y) {
                return Some(Hit::Tab(tab));
            }
        }
        match self.tab {
            Tab::New => {
                if Self::action_rect(panel, true).contains(x, y) {
                    Some(Hit::Copy)
                } else if Self::action_rect(panel, false).contains(x, y) {
                    Some(Hit::Generate)
                } else {
                    None
                }
            }
            Tab::Enter => {
                if Self::input_rect(panel).contains(x, y) {
                    Some(Hit::Input)
                } else if Self::action_rect(panel, false).contains(x, y) {
                    Some(Hit::Accept)
                } else {
                    None
                }
            }
        }
    }

    pub(super) fn key(
        &mut self,
        engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Outcome {
        if matches!(key, Key::Named(NamedKey::Escape)) {
            if self.start_over == StartOver::Confirming {
                self.start_over = StartOver::Offered;
                return Outcome::Keep;
            }
            return Outcome::Close;
        }
        if self.identity_problem.is_some() {
            return Outcome::Keep;
        }
        if self.linked {
            // *Done* is the one answer left, and the default.
            return if matches!(key, Key::Named(NamedKey::Enter)) {
                Outcome::Done
            } else {
                Outcome::Keep
            };
        }
        if self.tab == Tab::Enter {
            if matches!(key, Key::Named(NamedKey::Enter)) {
                let code = self.input.value();
                return if code.is_empty() {
                    Outcome::Keep
                } else {
                    Outcome::Accept(code)
                };
            }
            self.input.key(engine, key, text, mods);
        }
        Outcome::Keep
    }

    pub(super) fn mouse_down(
        &mut self,
        engine: &mut TextEngine,
        viewport: (f32, f32),
        x: f32,
        y: f32,
    ) -> Outcome {
        let panel = Self::panel(viewport);
        match self.hit_at(viewport, x, y) {
            Some(Hit::Tab(tab)) => {
                self.tab = tab;
                Outcome::Keep
            }
            Some(Hit::Input) => {
                self.input.click(engine, Self::input_rect(panel), x, y);
                Outcome::Keep
            }
            Some(Hit::Copy) => self
                .pairing
                .qr_payload
                .as_deref()
                .map_or(Outcome::Keep, |payload| {
                    Outcome::Copy(link_code(payload).to_owned())
                }),
            Some(Hit::Generate) => Outcome::Generate,
            Some(Hit::Accept) => {
                let code = self.input.value();
                if code.is_empty() {
                    Outcome::Keep
                } else {
                    Outcome::Accept(code)
                }
            }
            Some(Hit::StartOver) => {
                self.start_over = StartOver::Confirming;
                Outcome::Keep
            }
            Some(Hit::StartOverCancel) => {
                self.start_over = StartOver::Offered;
                Outcome::Keep
            }
            Some(Hit::StartOverConfirm) => {
                self.start_over = StartOver::Working;
                Outcome::StartOver
            }
            Some(Hit::Close) => Outcome::Close,
            Some(Hit::Done) => Outcome::Done,
            None if !panel.contains(x, y) => Outcome::Close,
            None => Outcome::Keep,
        }
    }

    pub(super) fn mouse_move(
        &mut self,
        engine: &mut TextEngine,
        viewport: (f32, f32),
        x: f32,
        y: f32,
    ) -> bool {
        let panel = Self::panel(viewport);
        if !self.linked
            && self.identity_problem.is_none()
            && self.tab == Tab::Enter
            && self.input.drag(engine, Self::input_rect(panel), x)
        {
            return true;
        }
        let was = self.hover.clone();
        self.hover = self.hit_at(viewport, x, y);
        was != self.hover
    }

    pub(super) fn mouse_up(&mut self) {
        self.input.release();
    }

    pub(super) fn cursor(&self) -> CursorIcon {
        match self.hover {
            Some(Hit::Input) => CursorIcon::Text,
            Some(_) => koru_ui::cursor::PRESS,
            None => CursorIcon::Default,
        }
    }

    pub(super) fn draw(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        viewport: (f32, f32),
    ) {
        let p = theme::p();
        scene::fill(
            canvas,
            root,
            p.scrim,
            &scene::rect(0.0, 0.0, viewport.0, viewport.1),
        );
        let panel = Self::panel(viewport);
        scene::floating_panel(
            canvas,
            root,
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            &scene::Panel::popover(10.0),
        );
        draw_text(
            canvas,
            root,
            engine,
            "Link another device",
            panel.x + PAD,
            panel.y + 22.0,
            theme::role::row().with_weight(theme::FontWeight::SemiBold),
            p.text,
        );
        draw_text(
            canvas,
            root,
            engine,
            "Link another computer or phone to keep a complete, synchronized copy of your Koru data.",
            panel.x + PAD,
            panel.y + 48.0,
            theme::role::meta(),
            p.text_dim,
        );

        if self.identity_problem.is_some() {
            self.draw_identity_problem(canvas, root, engine, panel);
        } else if self.linked {
            self.draw_linked(canvas, root, engine, panel);
        } else {
            for (tab, rect) in Self::tabs(panel) {
                if self.tab == tab {
                    scene::fill(
                        canvas,
                        root,
                        p.row_selected,
                        &scene::rrect(rect.x + 2.0, rect.y + 2.0, rect.w - 4.0, rect.h - 4.0, 6.0),
                    );
                }
                let label = match tab {
                    Tab::New => "New device",
                    Tab::Enter => "Enter link code",
                };
                draw_centered(canvas, root, engine, label, rect, p.text);
            }
            match self.tab {
                Tab::New => self.draw_new(canvas, root, engine, panel),
                Tab::Enter => self.draw_enter(canvas, root, engine, panel),
            }
        }

        draw_text(
            canvas,
            root,
            engine,
            "Linked devices have full access to all current and future Koru profiles, workspaces, and documents.",
            panel.x + PAD,
            panel.y + panel.h - 76.0,
            theme::role::hint(),
            p.text_dim,
        );
        let close = Self::close_rect(panel);
        let (label, role, hit) = if self.linked {
            ("Done", ButtonRole::Primary, Hit::Done)
        } else {
            ("Close", ButtonRole::Secondary, Hit::Close)
        };
        koru_ui::draw_button(
            canvas,
            root,
            engine,
            close,
            label,
            role,
            self.hover == Some(hit),
            false,
        );
    }

    /// The identity problem, in the phones' words (ADR-413), and the one way
    /// out. The confirmation says what starting over costs before it happens.
    fn draw_identity_problem(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        panel: Rect,
    ) {
        use koru_i18n::Key;
        let p = theme::p();
        let lang = crate::format::lang();
        let width = panel.w - PAD * 2.0;
        let (title, body) = if self.start_over == StartOver::Confirming {
            (
                koru_i18n::t(lang, Key::LinkStartOverAsNewDevice),
                koru_i18n::t(lang, Key::LinkStartOverExplainer),
            )
        } else {
            (
                koru_i18n::t(lang, Key::LinkIdentityMismatch),
                koru_i18n::t(lang, Key::LinkIdentityMismatchExplainer),
            )
        };
        let title = engine.simple_layout_wrapped(
            title,
            theme::role::row().with_weight(theme::FontWeight::SemiBold),
            width,
        );
        scene::draw_layout(
            canvas,
            root,
            &title,
            panel.x + PAD,
            panel.y + 96.0,
            Some(p.text),
        );
        let body_y = panel.y + 104.0 + title.height();
        let body = engine.simple_layout_wrapped(body, theme::role::meta(), width);
        scene::draw_layout(canvas, root, &body, panel.x + PAD, body_y, Some(p.text_dim));
        if self.start_over == StartOver::Failed {
            let failed = engine.simple_layout_wrapped(
                koru_i18n::t(lang, Key::LinkStartOverFailed),
                theme::role::meta(),
                width,
            );
            scene::draw_layout(
                canvas,
                root,
                &failed,
                panel.x + PAD,
                body_y + body.height() + 16.0,
                Some(p.close_hover),
            );
        }
        match self.start_over {
            StartOver::Confirming => {
                koru_ui::draw_button(
                    canvas,
                    root,
                    engine,
                    Self::action_rect(panel, true),
                    koru_i18n::t(lang, Key::LinkStartOverCancel),
                    ButtonRole::Secondary,
                    self.hover == Some(Hit::StartOverCancel),
                    false,
                );
                koru_ui::draw_button(
                    canvas,
                    root,
                    engine,
                    Self::action_rect(panel, false),
                    koru_i18n::t(lang, Key::LinkStartOverConfirm),
                    ButtonRole::Destructive,
                    self.hover == Some(Hit::StartOverConfirm),
                    false,
                );
            }
            StartOver::Offered | StartOver::Failed | StartOver::Working => {
                koru_ui::button::draw_button_enabled(
                    canvas,
                    root,
                    engine,
                    Self::start_over_rect(panel),
                    koru_i18n::t(lang, Key::LinkStartOverAsNewDevice),
                    ButtonRole::Destructive,
                    self.hover == Some(Hit::StartOver),
                    false,
                    self.start_over != StartOver::Working,
                );
            }
        }
    }

    /// *Device linked*, and which device when Agent named it. *Done* takes
    /// the person to the page that lists it.
    fn draw_linked(&self, canvas: &mut Scene, root: Affine, engine: &mut TextEngine, panel: Rect) {
        let p = theme::p();
        draw_text(
            canvas,
            root,
            engine,
            pairing_label(koru_agent_wire::PairingState::Succeeded),
            panel.x + PAD,
            panel.y + 112.0,
            theme::role::section_heading(),
            p.text,
        );
        let body = match self.pairing.peer_name.as_deref() {
            Some(name) if !name.trim().is_empty() => format!(
                "\u{201c}{name}\u{201d} is linked to this computer. Choose Done to see it with your other linked devices."
            ),
            _ => "Choose Done to see it with your other linked devices.".to_owned(),
        };
        let layout = engine.simple_layout_wrapped(&body, theme::role::meta(), panel.w - PAD * 2.0);
        scene::draw_layout(
            canvas,
            root,
            &layout,
            panel.x + PAD,
            panel.y + 144.0,
            Some(p.text_dim),
        );
    }

    fn draw_new(&self, canvas: &mut Scene, root: Affine, engine: &mut TextEngine, panel: Rect) {
        let p = theme::p();
        draw_text(
            canvas,
            root,
            engine,
            "On your other device, open Koru and scan this QR code or enter the link code.",
            panel.x + PAD,
            panel.y + 128.0,
            theme::role::meta(),
            p.text_dim,
        );
        if let Some((width, bits)) = &self.qr {
            let area = Rect::new(
                panel.x + (panel.w - QR_SIZE) * 0.5,
                panel.y + 156.0,
                QR_SIZE,
                QR_SIZE,
            );
            scene::fill(
                canvas,
                root,
                vello::peniko::Color::from_rgb8(255, 255, 255),
                &scene::rect(area.x, area.y, area.w, area.h),
            );
            // A QR code needs four clear modules around its data. Drawing the
            // matrix flush to the white square made camera acquisition brittle.
            let full_width = *width + QR_QUIET_MODULES * 2;
            let cell = area.w / full_width as f32;
            let origin_x = area.x + QR_QUIET_MODULES as f32 * cell;
            let origin_y = area.y + QR_QUIET_MODULES as f32 * cell;
            for (index, dark) in bits.iter().copied().enumerate() {
                if dark {
                    let x = origin_x + (index % *width) as f32 * cell;
                    let y = origin_y + (index / *width) as f32 * cell;
                    scene::fill(
                        canvas,
                        root,
                        vello::peniko::Color::from_rgb8(0, 0, 0),
                        &scene::rect(x, y, cell.ceil(), cell.ceil()),
                    );
                }
            }
        } else {
            draw_text(
                canvas,
                root,
                engine,
                pairing_label(self.pairing.state),
                panel.x + PAD,
                panel.y + 260.0,
                theme::role::row(),
                p.text,
            );
        }
        draw_text(
            canvas,
            root,
            engine,
            &format!(
                "Code expires in {} seconds",
                self.pairing.expires_in_seconds
            ),
            panel.x + PAD,
            panel.y + 402.0,
            theme::role::hint(),
            p.text_dim,
        );
        let copy = Self::action_rect(panel, true);
        let generate = Self::action_rect(panel, false);
        koru_ui::draw_button(
            canvas,
            root,
            engine,
            copy,
            self.copy_label(Instant::now()),
            ButtonRole::Primary,
            self.hover == Some(Hit::Copy),
            false,
        );
        koru_ui::draw_button(
            canvas,
            root,
            engine,
            generate,
            "Generate new code",
            ButtonRole::Secondary,
            self.hover == Some(Hit::Generate),
            false,
        );
    }

    fn draw_enter(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        panel: Rect,
    ) {
        let p = theme::p();
        draw_text(
            canvas,
            root,
            engine,
            "Enter the code shown by Koru on your other computer or phone.",
            panel.x + PAD,
            panel.y + 140.0,
            theme::role::meta(),
            p.text_dim,
        );
        let input = Self::input_rect(panel);
        self.input.draw(
            canvas,
            root,
            engine,
            input,
            InputStyle::search(),
            Some("Paste or enter a link code"),
            true,
        );
        if self.pairing.state != koru_agent_wire::PairingState::Idle {
            draw_text(
                canvas,
                root,
                engine,
                pairing_label(self.pairing.state),
                panel.x + PAD,
                panel.y + 238.0,
                theme::role::row(),
                if self.pairing.state == koru_agent_wire::PairingState::Failed {
                    p.close_hover
                } else {
                    p.text_dim
                },
            );
            if let Some(details) = pairing_error_label(self.pairing.error_code.as_deref()) {
                draw_text(
                    canvas,
                    root,
                    engine,
                    details,
                    panel.x + PAD,
                    panel.y + 266.0,
                    theme::role::meta(),
                    p.close_hover,
                );
            }
        }
        let accept = Self::action_rect(panel, false);
        koru_ui::draw_button(
            canvas,
            root,
            engine,
            accept,
            "Link device",
            ButtonRole::Primary,
            self.hover == Some(Hit::Accept),
            false,
        );
    }
}

fn idle_pairing() -> koru_agent_wire::PairingView {
    koru_agent_wire::PairingView {
        state: koru_agent_wire::PairingState::Idle,
        qr_payload: None,
        expires_in_seconds: 0,
        peer_name: None,
        error_code: None,
    }
}

/// The code alone, without the `https://koru.do/link-device#` it travels in.
/// The QR keeps the link so a phone's camera opens Koru; a pasted code is
/// accepted either way.
fn link_code(payload: &str) -> &str {
    payload.split_once('#').map_or(payload, |(_, code)| code)
}

fn qr_matrix(value: &str) -> Option<(usize, Vec<bool>)> {
    let code = QrCode::new(value.as_bytes()).ok()?;
    Some((
        code.width(),
        code.to_colors()
            .into_iter()
            .map(|colour| colour == QrColor::Dark)
            .collect(),
    ))
}

fn pairing_label(state: koru_agent_wire::PairingState) -> &'static str {
    use koru_agent_wire::PairingState::*;
    match state {
        Idle => "Preparing link code…",
        Displaying => "Link code ready",
        Connecting => "Connecting devices…",
        Confirming | Preparing | Finishing => "Securing your devices…",
        Succeeded => "Device linked",
        Expired => "This link code expired",
        Cancelled => "Linking cancelled",
        Failed => "Could not link this device",
    }
}

fn pairing_error_label(code: Option<&str>) -> Option<&'static str> {
    match code? {
        "same_device" => {
            Some("This code belongs to this device. Use the code from the other device.")
        }
        "invalid_link_code" => Some("Paste the complete link code or Koru link URL."),
        "identity_unavailable" => {
            Some("This device cannot sync until it starts over as a new device.")
        }
        "connect_failed" => {
            Some("The other device could not be reached. Keep it online and try again.")
        }
        _ => Some("Keep the other device online, generate a new code, and try again."),
    }
}

fn draw_text(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    value: &str,
    x: f32,
    y: f32,
    style: theme::TextStyle,
    colour: vello::peniko::Color,
) {
    let layout = engine.simple_layout(value, style);
    scene::draw_layout_first_line(canvas, root, &layout, x, y, Some(colour));
}

fn draw_centered(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    value: &str,
    rect: Rect,
    colour: vello::peniko::Color,
) {
    let layout = engine.simple_layout(value, theme::role::row());
    scene::draw_layout_first_line(
        canvas,
        root,
        &layout,
        rect.x + (rect.w - layout.width()) * 0.5,
        rect.y + (rect.h - scene::first_line_height(&layout)) * 0.5,
        Some(colour),
    );
}

#[cfg(test)]
mod tests;
