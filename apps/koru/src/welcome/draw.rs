// SPDX-License-Identifier: MPL-2.0

//! Painting the measured page.
//!
//! Nothing here computes a position: every rectangle comes from
//! [`layout::Layout`]. Every element sits inside a named inspector scope, so
//! F12 shows *KoruWelcome → section → row → its text and controls* rather
//! than a flat list of fills and text runs.

use koru_draw::geometry::Rect;
use koru_draw::{icons, inspect, scene, theme};
use koru_ui::button::{ButtonRole, draw_button};
use koru_ui::card::{Card, CardRow};
use koru_ui::chip::Chip;
use koru_ui::swatch::Swatch;
use koru_ui::text_paragraph::{KoruTextParagraph, draw_line};
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::{Color, Fill};

use super::layout::{self, HINT_CHORD_W, Layout, RowLayout, SectionLayout, TICK, Trailing};
use super::rows::Section;
use super::{Target, WelcomeView};

/// What a draw needs to know about the pointer and the keyboard.
#[derive(Copy, Clone)]
struct Interaction {
    hot: Option<Target>,
    focus: Option<Target>,
}

impl Interaction {
    fn hot(self, target: Target) -> bool {
        self.hot == Some(target)
    }
    fn focused(self, target: Target) -> bool {
        self.focus == Some(target)
    }
}

impl WelcomeView {
    pub(super) fn draw_page(&mut self, canvas: &mut Scene, vp_root: Affine) {
        let laid = match self.laid.take() {
            Some(laid) => laid,
            None => layout::measure(self),
        };
        let (vw, vh) = self.viewport;
        let max_scroll = (laid.bottom - vh).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max_scroll);
        let ui = Interaction {
            hot: Self::target_at(&laid, self.pointer.0, self.pointer.1 + self.scroll),
            focus: self.focus,
        };
        let p = theme::p();
        let _page = inspect::scope(
            "KoruWelcome",
            "component",
            vp_root,
            scene::rect(0.0, 0.0, vw, vh),
            |r| {
                r.property(
                    "implementation",
                    "apps/koru welcome::WelcomeView",
                    "Koru's Welcome tab",
                );
                r.property("scroll", self.scroll, "view state");
                r.property("content height", laid.bottom, "layout::measure");
                r.property(
                    "docs",
                    "docs/features/design/DESIGN.md",
                    "Welcome page spec",
                );
            },
        );
        scene::fill(canvas, vp_root, p.bg, &scene::rect(0.0, 0.0, vw, vh));

        let mut drawn = Vec::new();
        {
            // The whole column scrolls under a viewport clip; the background
            // stays put.
            canvas.push_clip_layer(Fill::NonZero, vp_root, &scene::rect(0.0, 0.0, vw, vh));
            let _clip = inspect::clip(vp_root, scene::rect(0.0, 0.0, vw, vh));
            let root = vp_root * Affine::translate((0.0, -(self.scroll as f64)));
            let _column = inspect::scope(
                "Welcome column",
                "part",
                root,
                scene::rect(laid.column.x, laid.column.y, laid.column.w, laid.column.h),
                |r| r.property("max width", layout::COLUMN_W, "layout::COLUMN_W"),
            );
            self.draw_customize(canvas, root, &laid, ui, &mut drawn);
            for section in &laid.sections {
                self.draw_section(canvas, root, section, ui, &mut drawn);
            }
            canvas.pop_layer();
        }

        if let Some(geom) = koru_ui::scrollbar::ScrollbarGeom::for_content(
            koru_draw::geometry::Rect::new(0.0, 0.0, vw, vh),
            laid.bottom,
            self.scroll,
        ) {
            let _bar = inspect::scope(
                "Welcome scrollbar",
                "part",
                vp_root,
                scene::rect(geom.gutter_x, 0.0, geom.gutter_w, vh),
                |r| r.property("max scroll", geom.max_scroll, "layout::measure"),
            );
            let over = geom.in_gutter(self.pointer.0, self.pointer.1);
            koru_ui::scrollbar::draw(canvas, vp_root, geom, over);
        }
        self.drawn = drawn;
        self.laid = Some(laid);
    }

    fn draw_customize(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        laid: &Layout,
        ui: Interaction,
        drawn: &mut Vec<(Target, Rect)>,
    ) {
        let p = theme::p();
        let c = &laid.customize;
        let _card = Card::new(None).draw(canvas, root, &mut self.engine, c.card, &[], 1.0);
        // Light, dark, and the caret: what Customize changes, at a glance.
        for (rect, color) in c
            .swatches
            .iter()
            .zip([theme::LIGHT.bg, theme::DARK.bg, p.caret])
        {
            Swatch::draw(canvas, root, *rect, Some(color), false, false, false, 1.0);
        }
        let _row = CardRow::new(super::CUSTOMIZE_TITLE)
            .hint(super::CUSTOMIZE_HINT)
            .draw(canvas, root, &mut self.engine, c.row, c.text_w, 1.0);
        draw_button(
            canvas,
            root,
            &mut self.engine,
            c.button,
            super::CUSTOMIZE_BUTTON,
            ButtonRole::Secondary,
            ui.hot(Target::Customize),
            ui.focused(Target::Customize),
        );
        drawn.push((Target::Customize, c.button));
    }

    fn draw_section(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        laid: &SectionLayout,
        ui: Interaction,
        drawn: &mut Vec<(Target, Rect)>,
    ) {
        let p = theme::p();
        let label_style = layout::sentence_style();
        let top = laid.label_y;
        let _section = inspect::scope(
            "Welcome section",
            "part",
            root,
            scene::rect(
                laid.card.x,
                top,
                laid.card.w,
                laid.card.y + laid.card.h - top,
            ),
            |r| {
                r.property("label", laid.section.label(), "welcome copy");
                r.property(
                    "folded",
                    laid.folded.is_some(),
                    "all counted rows done and not shown",
                );
            },
        );
        {
            let _header = inspect::scope(
                "Welcome section header",
                "part",
                root,
                scene::rect(laid.card.x, top, laid.card.w, layout::LABEL_H),
                |_| {},
            );
            draw_line(
                canvas,
                root,
                &mut self.engine,
                laid.section.label(),
                laid.card.x,
                top,
                label_style,
                "theme::role::meta()",
                p.text_dim,
            );
            if let Some((text, rect)) = &laid.progress {
                draw_line(
                    canvas,
                    root,
                    &mut self.engine,
                    text,
                    rect.x,
                    rect.y,
                    label_style,
                    "theme::role::meta()",
                    p.placeholder,
                );
            }
            if let Some((text, rect)) = laid.toggle {
                let target = Target::Toggle(laid.section);
                self.draw_link(canvas, root, text, rect, ui, target);
                drawn.push((target, rect));
            }
        }

        let _card = Card::new(None).draw(
            canvas,
            root,
            &mut self.engine,
            laid.card,
            if laid.section == Section::Devices {
                &[]
            } else {
                &laid.dividers
            },
            1.0,
        );
        if let Some(folded) = laid.folded {
            let _row = inspect::scope(
                "Welcome row",
                "part",
                root,
                scene::rect(folded.x, folded.y, folded.w, folded.h),
                |r| r.property("row", "all done", "folded section"),
            );
            let tick = Rect::new(
                folded.x + CardRow::PAD_X,
                folded.y + (folded.h - TICK) * 0.5,
                TICK,
                TICK,
            );
            self.draw_tick(canvas, root, tick, true);
            let style = layout::title_style();
            draw_line(
                canvas,
                root,
                &mut self.engine,
                laid.section.done_line(),
                tick.x + TICK + 12.0,
                folded.y + (folded.h - style.metrics.line_height) * 0.5,
                style,
                "theme::role::ui()",
                p.text_dim,
            );
            return;
        }
        // The device card's rows share it with the pairing square, so their
        // hairlines stop at the rows' own width.
        if laid.section == Section::Devices {
            for row in laid.rows.iter().skip(1) {
                scene::fill(
                    canvas,
                    root,
                    p.panel_border.multiply_alpha(0.6),
                    &scene::rect(
                        row.rect.x + CardRow::PAD_X,
                        row.rect.y,
                        row.rect.w - CardRow::PAD_X * 2.0,
                        1.0,
                    ),
                );
            }
        }
        let card_shape = scene::rrect(
            laid.card.x,
            laid.card.y,
            laid.card.w,
            laid.card.h,
            Card::RADIUS,
        );
        for row in &laid.rows {
            self.draw_row(canvas, root, row, &card_shape, ui, drawn);
        }
        if let Some(qr) = &laid.qr {
            let _qr = inspect::scope(
                "Welcome pair QR",
                "part",
                root,
                scene::rect(qr.hit.x, qr.hit.y, qr.hit.w, qr.hit.h),
                |r| {
                    r.property(
                        "behavior",
                        "decorative; opens Link another device",
                        "the real code lives in that window",
                    )
                },
            );
            let hot = ui.hot(Target::PairQr);
            let square = qr.square;
            let shape = scene::rrect(square.x, square.y, square.w, square.h, 8.0);
            scene::fill(canvas, root, p.bg, &shape);
            if hot {
                scene::fill(canvas, root, p.hover_tint, &shape);
            }
            scene::stroke(
                canvas,
                root,
                if ui.focused(Target::PairQr) {
                    p.focus_ring
                } else {
                    p.control_border
                },
                if ui.focused(Target::PairQr) {
                    theme::BORDER_W * 2.0
                } else {
                    theme::CONTROL_BORDER_W
                },
                &shape,
            );
            draw_qr_motif(canvas, root, square, p.text);
            draw_line(
                canvas,
                root,
                &mut self.engine,
                super::SCAN_TO_PAIR,
                qr.caption.0,
                qr.caption.1,
                layout::sentence_style(),
                "theme::role::meta()",
                p.text_dim,
            );
            drawn.push((Target::PairQr, qr.hit));
        }
    }

    fn draw_row(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        row: &RowLayout,
        card_shape: &vello::kurbo::RoundedRect,
        ui: Interaction,
        drawn: &mut Vec<(Target, Rect)>,
    ) {
        let p = theme::p();
        let r = row.rect;
        let _row = inspect::scope(
            "Welcome row",
            "part",
            root,
            scene::rect(r.x, r.y, r.w, r.h),
            |rec| {
                rec.property("row", row.id.key(), "stable row id");
                rec.property("done", self.done.contains(&row.id), "machine-wide tick");
                rec.property(
                    "action",
                    if row.id.actionable() {
                        "yes"
                    } else {
                        "none yet"
                    },
                    "rows::RowId::actionable",
                );
            },
        );
        let whole = Target::Row(row.id);
        if layout::row_is_target(row.id) {
            if ui.hot(whole) {
                canvas.push_clip_layer(Fill::NonZero, root, card_shape);
                scene::fill(canvas, root, p.hover_tint, &scene::rect(r.x, r.y, r.w, r.h));
                canvas.pop_layer();
            }
            if ui.focused(whole) {
                scene::stroke(
                    canvas,
                    root,
                    p.focus_ring,
                    theme::BORDER_W * 2.0,
                    &scene::rrect(r.x + 2.0, r.y + 2.0, r.w - 4.0, r.h - 4.0, 6.0),
                );
            }
            drawn.push((whole, r));
        }
        if let Some(tick) = row.tick {
            self.draw_tick(canvas, root, tick, self.done.contains(&row.id));
        }
        KoruTextParagraph::new(
            row.title,
            layout::title_style(),
            "theme::role::ui()",
            p.text,
        )
        .draw(canvas, root, &mut self.engine, row.title_rect);
        if !row.sentence.is_empty() {
            let x0 = row
                .sentence
                .iter()
                .map(|run| run.x)
                .fold(f32::MAX, f32::min);
            let y0 = row.sentence[0].y;
            let x1 = row
                .sentence
                .iter()
                .map(|run| run.x + run.w)
                .fold(f32::MIN, f32::max);
            let y1 = row
                .sentence
                .iter()
                .map(|run| run.rect().y + run.rect().h)
                .fold(f32::MIN, f32::max);
            let _sentence = inspect::scope(
                "Welcome sentence",
                "part",
                root,
                scene::rect(x0, y0, x1 - x0, y1 - y0),
                |_| {},
            );
            for run in &row.sentence {
                match run.link {
                    Some(link) => {
                        self.draw_link(canvas, root, &run.text, run.rect(), ui, link);
                        drawn.push((link, run.rect()));
                    }
                    None => draw_line(
                        canvas,
                        root,
                        &mut self.engine,
                        &run.text,
                        run.x,
                        run.y,
                        layout::sentence_style(),
                        "theme::role::meta()",
                        p.text_dim,
                    ),
                }
            }
        }
        for (format, label, rect) in &row.chips {
            let target = Target::Import(format);
            Chip::new(label, false).draw(
                canvas,
                root,
                &mut self.engine,
                *rect,
                ui.hot(target),
                ui.focused(target),
            );
            drawn.push((target, *rect));
        }
        if let Some((text, rect)) = &row.code {
            self.draw_code(canvas, root, text, *rect, ui, Target::CopyMcp);
            drawn.push((Target::CopyMcp, *rect));
        }
        if !row.hints.is_empty() {
            let first = row.hints[0].y;
            let last = row.hints[row.hints.len() - 1].y + layout::HINT_LINE_H;
            let _hints = inspect::scope(
                "Welcome shortcuts",
                "part",
                root,
                scene::rect(row.title_rect.x, first, row.title_rect.w, last - first),
                |r| r.property("source", "koru_keys::show_hint", "shortcut registry"),
            );
            for hint in &row.hints {
                draw_line(
                    canvas,
                    root,
                    &mut self.engine,
                    &hint.chord,
                    row.title_rect.x,
                    hint.y,
                    theme::role::row(),
                    "theme::role::row()",
                    p.text,
                );
                draw_line(
                    canvas,
                    root,
                    &mut self.engine,
                    hint.what,
                    row.title_rect.x + HINT_CHORD_W,
                    hint.y,
                    theme::role::row(),
                    "theme::role::row()",
                    p.text_dim,
                );
            }
        }
        match &row.trailing {
            Trailing::None => {}
            Trailing::Key(text, rect) => self.draw_key_chip(canvas, root, text, *rect),
            Trailing::Code(text, rect) => self.draw_key_chip(canvas, root, text, *rect),
            Trailing::Button(label, target, rect) => {
                draw_button(
                    canvas,
                    root,
                    &mut self.engine,
                    *rect,
                    label,
                    ButtonRole::Secondary,
                    ui.hot(*target),
                    ui.focused(*target),
                );
                drawn.push((*target, *rect));
            }
            Trailing::Caret(open, rect) => {
                let _caret = inspect::scope(
                    "Welcome disclosure",
                    "part",
                    root,
                    scene::rect(rect.x, rect.y, rect.w, rect.h),
                    |r| r.property("open", *open, "view state"),
                );
                let icon = if *open {
                    icons::CARET_DOWN
                } else {
                    icons::CARET_RIGHT
                };
                let glyph = self.engine.icon_layout(icon, theme::CAPTION_ICON);
                scene::draw_layout(
                    canvas,
                    root,
                    &glyph,
                    rect.x + (rect.w - glyph.width()) * 0.5,
                    rect.y + (rect.h - glyph.height()) * 0.5,
                    Some(p.text_dim),
                );
            }
        }
    }

    /// The 19 px circle: an outline while open, filled with a check when done.
    fn draw_tick(&mut self, canvas: &mut Scene, root: Affine, tick: Rect, done: bool) {
        let p = theme::p();
        let _tick = inspect::scope(
            "Welcome tick",
            "part",
            root,
            scene::rect(tick.x, tick.y, tick.w, tick.h),
            |r| r.property("done", done, "machine-wide tick"),
        );
        let circle = scene::rrect(tick.x, tick.y, tick.w, tick.h, tick.w * 0.5);
        if done {
            scene::fill(canvas, root, p.accent, &circle);
            let check = self.engine.icon_layout(icons::CHECK, 13.0);
            scene::draw_layout(
                canvas,
                root,
                &check,
                tick.x + (tick.w - check.width()) * 0.5,
                tick.y + (tick.h - check.height()) * 0.5,
                Some(p.on_accent),
            );
        } else {
            scene::stroke(
                canvas,
                root,
                p.control_border,
                theme::CONTROL_BORDER_W,
                &circle,
            );
        }
    }

    /// A shortcut or a command, in the shared [`koru_ui::KeyChip`] at a
    /// row's right end.
    fn draw_key_chip(&mut self, canvas: &mut Scene, root: Affine, text: &str, rect: Rect) {
        koru_ui::KeyChip::draw(canvas, root, &mut self.engine, rect, text);
    }

    /// A command to copy: a key chip that is pressed like a link, outlined
    /// while hovered and ringed while focused.
    fn draw_code(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        text: &str,
        rect: Rect,
        ui: Interaction,
        target: Target,
    ) {
        let p = theme::p();
        let _code = inspect::scope(
            "Welcome command",
            "part",
            root,
            scene::rect(rect.x, rect.y, rect.w, rect.h),
            |r| r.property("copies", "welcome::mcp_command", "the whole command"),
        );
        self.draw_key_chip(canvas, root, text, rect);
        if ui.hot(target) {
            scene::stroke(
                canvas,
                root,
                p.link.fg,
                theme::BORDER_W,
                &scene::rrect(rect.x, rect.y, rect.w, rect.h, 5.0),
            );
        }
        if ui.focused(target) {
            scene::stroke(
                canvas,
                root,
                p.focus_ring,
                theme::BORDER_W * 2.0,
                &scene::rrect(rect.x - 2.0, rect.y - 2.0, rect.w + 4.0, rect.h + 4.0, 6.0),
            );
        }
    }

    /// Link text, underlined while hovered and ringed while focused.
    fn draw_link(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        text: &str,
        rect: Rect,
        ui: Interaction,
        target: Target,
    ) {
        let p = theme::p();
        let _link = inspect::scope(
            "Welcome link",
            "part",
            root,
            scene::rect(rect.x, rect.y, rect.w, rect.h),
            |r| r.property("text", text, "welcome copy"),
        );
        draw_line(
            canvas,
            root,
            &mut self.engine,
            text,
            rect.x,
            rect.y,
            layout::sentence_style(),
            "theme::role::meta()",
            p.link.fg,
        );
        if ui.hot(target) {
            scene::fill(
                canvas,
                root,
                p.link.fg,
                &scene::rect(rect.x, rect.y + rect.h - 2.0, rect.w, 1.0),
            );
        }
        if ui.focused(target) {
            scene::stroke(
                canvas,
                root,
                p.focus_ring,
                theme::BORDER_W * 2.0,
                &scene::rrect(rect.x - 3.0, rect.y - 1.0, rect.w + 6.0, rect.h + 2.0, 4.0),
            );
        }
    }
}

/// A placeholder QR: three finder squares and a fixed scatter of modules.
/// Decorative — the real code is in the Link another device window.
fn draw_qr_motif(canvas: &mut Scene, root: Affine, square: Rect, color: Color) {
    const N: usize = 9;
    const MODULES: [&str; N] = [
        "xxx.x.xxx",
        "x.x..xx.x",
        "xxx.x.xxx",
        "...x.x...",
        "x.xx.x.xx",
        ".x..xx.x.",
        "xxx.x.x.x",
        "x.x.xx..x",
        "xxx.x.xx.",
    ];
    let pad = 12.0;
    let cell = (square.w - pad * 2.0) / N as f32;
    for (r, line) in MODULES.iter().enumerate() {
        for (c, bit) in line.bytes().enumerate() {
            if bit == b'x' {
                scene::fill(
                    canvas,
                    root,
                    color,
                    &scene::rect(
                        square.x + pad + c as f32 * cell,
                        square.y + pad + r as f32 * cell,
                        cell,
                        cell,
                    ),
                );
            }
        }
    }
}
