// SPDX-License-Identifier: MPL-2.0

//! Drawing the panels.
//!
//! Beside the components whose state they show, so a new picker is one file
//! rather than a state machine here and a draw call somewhere else.
//!
//! **Drawing a field is not here.** It moved to `koru_ui::text_field` with the
//! field itself: the selection, the scroll window and the caret are facts about
//! that type, and prompts need to paint one without linking this crate. It is
//! re-exported below, so every panel in this file calls it by the same name.

use koru_draw::icons;
use koru_draw::text::TextEngine;
use vello::Scene;
use vello::kurbo::Affine;

pub use koru_ui::text_field::{baseline_centred_y, draw_field};

use crate::components::Overlay;
use crate::components::command_menu::{self, Command};
use crate::components::emoji_picker;
use crate::components::language_picker;
use crate::components::link_prompt::{self, Field as LinkField};
use crate::components::name_prompt::{self, NamePrompt};
use crate::trigger::Anchor;
use crate::view_state::{Hover, ViewState};
use koru_draw::scene::{draw_layout, draw_row_bg, fill, rrect, stroke};
use koru_draw::theme::{self};

pub fn draw_overlay(
    scene: &mut Scene,
    root: Affine,
    view: &mut ViewState,
    panel: (f32, f32, f32, f32),
) {
    let (px, py, pw, ph) = panel;

    // Panel chrome first, so every overlay kind gets the same frame. All seven
    // of them are popovers — the slash menu, `@`, the date grid, the rename and
    // link prompts, the language list and the emoji grid — so this one call is
    // what raises the lot.
    koru_draw::scene::floating_panel(
        scene,
        root,
        px,
        py,
        pw,
        ph,
        &koru_draw::scene::Panel::popover(8.0),
    );

    // Copy the small bits of overlay state out so `engine` is free to lay out
    // text below without holding a borrow of `overlay`.
    enum Kind {
        Commands(Vec<Command>, usize, command_menu::Allows),
        Entity,
        Date,
        Name,
        Link,
        Languages(usize),
        Emoji,
        Attrs(usize),
        Tags {
            sel: usize,
            first: usize,
            has_search: bool,
            /// `(branch, leaf, color, applied)` — the path is drawn quietly
            /// ahead of the tag's own name. See `TagChoice::label`.
            rows: Vec<(String, String, Option<String>, bool)>,
            create: Option<String>,
        },
        Colors {
            sel: usize,
            rows: Vec<Option<koru_core::attributes::AccentColor>>,
            current: Option<koru_core::attributes::AccentColor>,
        },
        Priorities {
            sel: usize,
            rows: Vec<Option<koru_core::attributes::Priority>>,
            current: Option<koru_core::attributes::Priority>,
        },
        /// The list picker's lists face: `(label, swatch, checked)` per row,
        /// the Clear and the create rows resolved into the same shape.
        Lists {
            sel: usize,
            first: usize,
            rows: Vec<ListFace>,
        },
        /// Its swatches face, for the list about to be made.
        ListColors {
            sel: usize,
            name: String,
        },
    }
    enum ListFace {
        Clear,
        List {
            name: String,
            color: Option<String>,
            checked: bool,
        },
        Create(String),
    }
    // Hovering a row pre-selects it, the way a native menu does.
    let hovered_row = match view.hover {
        Some(Hover::PanelRow(r)) => Some(r),
        _ => None,
    };
    let kind = match &view.overlay {
        Overlay::Command(m) => Kind::Commands(m.hits.clone(), m.sel, m.allows),
        Overlay::Entity(_) => Kind::Entity,
        Overlay::Date(_) => Kind::Date,
        Overlay::Name(_) => Kind::Name,
        Overlay::Link(_) => Kind::Link,
        Overlay::Language(p) => Kind::Languages(p.sel),
        Overlay::Emoji(_) => Kind::Emoji,
        Overlay::Attr(m) => Kind::Attrs(m.sel),
        Overlay::Tag(t) => Kind::Tags {
            sel: t.sel,
            first: t.first_visible(),
            has_search: t.has_search(),
            rows: t
                .hits
                .iter()
                .map(|&i| {
                    let tag = &t.table[i];
                    let (branch, leaf) = tag.label();
                    (
                        branch,
                        leaf.to_owned(),
                        tag.color.clone(),
                        t.applied.contains(&tag.id),
                    )
                })
                .collect(),
            create: t.offer_create(),
        },
        Overlay::Color(c) => Kind::Colors {
            sel: c.sel,
            rows: (0..c.row_count()).filter_map(|i| c.row(i)).collect(),
            current: c.current,
        },
        Overlay::Priority(p) => Kind::Priorities {
            sel: p.sel,
            rows: (0..p.row_count()).filter_map(|i| p.row(i)).collect(),
            current: p.current,
        },
        Overlay::List(l) => match &l.stage {
            crate::components::list_picker::Stage::Lists => Kind::Lists {
                sel: l.sel,
                first: l.first_visible(),
                rows: (0..l.row_count())
                    .filter_map(|i| l.row(i))
                    .map(|row| match row {
                        crate::components::list_picker::ListRow::Clear => ListFace::Clear,
                        crate::components::list_picker::ListRow::List(t) => ListFace::List {
                            name: l.table[t].name.clone(),
                            color: l.table[t].color.clone(),
                            checked: l.current == Some(l.table[t].id),
                        },
                        crate::components::list_picker::ListRow::Create(name) => {
                            ListFace::Create(name)
                        }
                    })
                    .collect(),
            },
            crate::components::list_picker::Stage::Color { name, sel } => Kind::ListColors {
                sel: *sel,
                name: name.clone(),
            },
        },
        Overlay::None => return,
    };

    match kind {
        Kind::Commands(hits, sel, allows) => {
            let top = draw_command_search(scene, root, view, px, py, pw);
            let first = command_menu::first_visible(&view.overlay);
            let rows = command_menu::visible_rows(hits.len());
            for (n, cmd) in hits.iter().enumerate().skip(first).take(rows) {
                let i = n - first;
                let ry = py + command_menu::accessibility::row_rect(pw, top, i).y;
                let on = n == sel || hovered_row == Some(n);
                draw_row_bg(scene, root, px, ry, pw, on);
                let why = command_menu::disabled(*cmd, &allows);
                let glyph = view.engine.icon_layout(cmd.icon(), theme::CAPTION_ICON);
                draw_layout(
                    scene,
                    root,
                    &glyph,
                    px + 12.0,
                    ry + (theme::ROW_H - glyph.height()) * 0.5,
                    Some(if why.is_some() {
                        theme::p().text_dim
                    } else {
                        theme::p().text
                    }),
                );
                let name = view.engine.simple_layout(
                    cmd.label(),
                    theme::role::ui().with_weight(if on {
                        theme::FontWeight::SemiBold
                    } else {
                        theme::FontWeight::Medium
                    }),
                );
                draw_layout(
                    scene,
                    root,
                    &name,
                    px + command_menu::LABEL_X,
                    ry + 4.0,
                    // **Dim, and the reason where the shortcut goes.** A command
                    // that is listed but cannot run has to look different from
                    // one that can, or the only way to find out is to pick it.
                    Some(if why.is_some() {
                        theme::p().text_dim
                    } else {
                        theme::p().text
                    }),
                );
                // `why` is the reason a listed row cannot run and takes the
                // chord column when there is one; otherwise the row's own
                // hint, which for `/dictate` is a chord from the registry and
                // for everything else a borrowed literal.
                let hint_text = why.map_or_else(|| cmd.hint(), std::borrow::Cow::Borrowed);
                let hint = view.engine.simple_layout(&hint_text, theme::role::meta());
                let hw = hint.width();
                draw_layout(
                    scene,
                    root,
                    &hint,
                    px + pw - 12.0 - hw,
                    ry + 5.5,
                    Some(theme::p().row_hint),
                );
            }
            command_menu::draw_scrollbar(scene, root, view, px, py + top, pw, hits.len());
            if hits.is_empty() {
                let none = view.engine.simple_layout("No matches", theme::role::row());
                draw_layout(
                    scene,
                    root,
                    &none,
                    px + 10.0,
                    py + theme::PANEL_PAD + top + 5.0,
                    Some(theme::p().placeholder),
                );
            }
        }
        Kind::Entity => draw_entity_picker(scene, root, view, px, py, pw, hovered_row),
        Kind::Date => {
            // The widget draws all three of its parts — the phrase box, the
            // month and the Clear row — because the arithmetic that places
            // them is its own (ADR-196). The grid has no rows to hover, so any
            // reported panel row is the Clear one.
            if let Overlay::Date(picker) = &mut view.overlay {
                koru_ui::date_grid::draw(
                    scene,
                    root,
                    &mut view.engine,
                    &mut picker.grid,
                    px,
                    py,
                    pw,
                    hovered_row.is_some(),
                );
            }
        }
        Kind::Name => draw_name_prompt(scene, root, view, px, py, pw),
        Kind::Link => draw_link_prompt(scene, root, view, px, py, pw),
        Kind::Languages(sel) => {
            for (i, (name, _token)) in language_picker::labels().iter().enumerate() {
                let ry = py + theme::PANEL_PAD + i as f32 * theme::ROW_H;
                let on = i == sel || hovered_row == Some(i);
                draw_row_bg(scene, root, px, ry, pw, on);
                let label = view.engine.simple_layout(
                    *name,
                    theme::role::ui().with_weight(if on {
                        theme::FontWeight::SemiBold
                    } else {
                        theme::FontWeight::Medium
                    }),
                );
                draw_layout(
                    scene,
                    root,
                    &label,
                    px + 10.0,
                    ry + 4.0,
                    Some(theme::p().text),
                );
            }
        }
        Kind::Emoji => draw_emoji_picker(scene, root, view, px, py, pw),
        Kind::Attrs(sel) => {
            for (i, (label, _, icon)) in crate::components::attr_menu::rows().iter().enumerate() {
                let ry = py + theme::PANEL_PAD + i as f32 * theme::ROW_H;
                let on = i == sel || hovered_row == Some(i);
                draw_row_bg(scene, root, px, ry, pw, on);
                let glyph = view.engine.icon_layout(*icon, 15.0);
                draw_layout(
                    scene,
                    root,
                    &glyph,
                    px + 10.0,
                    ry + 7.0,
                    Some(theme::p().row_hint),
                );
                let label = view.engine.simple_layout(
                    label,
                    theme::role::ui().with_weight(if on {
                        theme::FontWeight::SemiBold
                    } else {
                        theme::FontWeight::Medium
                    }),
                );
                draw_layout(
                    scene,
                    root,
                    &label,
                    px + 32.0,
                    ry + 4.0,
                    Some(theme::p().text),
                );
            }
        }
        Kind::Tags {
            sel,
            first,
            has_search,
            rows,
            create,
        } => {
            let top = draw_tag_search(scene, root, view, px, py, pw, has_search);
            let shown = command_menu::visible_rows(rows.len() + usize::from(create.is_some()));
            // List indices `first..`, drawn into the same boxes — the command
            // menu's shape, so hit-testing and drawing agree through
            // `TagPicker::item_at`.
            let mut slot = 0usize;
            for n in first.. {
                if slot >= shown {
                    break;
                }
                let ry = py + theme::PANEL_PAD + top + slot as f32 * theme::ROW_H;
                let on = n == sel || hovered_row == Some(n);
                if let Some((branch, leaf, color, applied)) = rows.get(n) {
                    draw_row_bg(scene, root, px, ry, pw, on);
                    // The tag's accent swatch, or the tag glyph for one with
                    // no color of its own.
                    match color.as_deref().and_then(theme::accent_tone) {
                        Some(tone) => fill(
                            scene,
                            root,
                            tone.swatch,
                            &vello::kurbo::Circle::new(
                                ((px + 16.0) as f64, (ry + theme::ROW_H * 0.5) as f64),
                                5.0,
                            ),
                        ),
                        None => {
                            let glyph = view.engine.icon_layout(icons::TAG, 15.0);
                            draw_layout(
                                scene,
                                root,
                                &glyph,
                                px + 10.0,
                                ry + 7.0,
                                Some(theme::p().row_hint),
                            );
                        }
                    }
                    // The branch first, quietly, then the tag's own name — so
                    // two `home`s under different parents read as two rows
                    // rather than one repeated word. A root tag draws no
                    // prefix and costs no extra layout.
                    let mut x = px + 32.0;
                    let room = pw - 64.0;
                    if !branch.is_empty() {
                        let fitted = view.engine.elided(branch, theme::role::ui(), room * 0.5);
                        let path = view.engine.simple_layout(
                            &fitted,
                            theme::role::ui().with_weight(theme::FontWeight::Medium),
                        );
                        draw_layout(scene, root, &path, x, ry + 4.0, Some(theme::p().row_hint));
                        x += path.width();
                    }
                    let fitted =
                        view.engine
                            .elided(leaf, theme::role::ui(), room - (x - px - 32.0));
                    let label = view.engine.simple_layout(
                        &fitted,
                        theme::role::ui().with_weight(if on {
                            theme::FontWeight::SemiBold
                        } else {
                            theme::FontWeight::Medium
                        }),
                    );
                    draw_layout(scene, root, &label, x, ry + 4.0, Some(theme::p().text));
                    if *applied {
                        let check = view.engine.icon_layout(icons::CHECK, 14.0);
                        draw_layout(
                            scene,
                            root,
                            &check,
                            px + pw - 12.0 - check.width(),
                            ry + 7.0,
                            Some(theme::p().accent),
                        );
                    }
                } else if n == rows.len()
                    && let Some(name) = &create
                {
                    draw_row_bg(scene, root, px, ry, pw, on);
                    let plus = view.engine.icon_layout(icons::PLUS, 15.0);
                    draw_layout(
                        scene,
                        root,
                        &plus,
                        px + 10.0,
                        ry + 7.0,
                        Some(theme::p().accent),
                    );
                    let fitted = view.engine.elided(
                        &format!("Create tag \u{201C}{name}\u{201D}"),
                        theme::role::ui(),
                        pw - 44.0,
                    );
                    let label = view.engine.simple_layout(&fitted, theme::role::ui());
                    draw_layout(
                        scene,
                        root,
                        &label,
                        px + 32.0,
                        ry + 4.0,
                        Some(theme::p().text),
                    );
                } else {
                    break;
                }
                slot += 1;
            }
            if rows.is_empty() && create.is_none() {
                let none = view.engine.simple_layout("No tags yet", theme::role::row());
                draw_layout(
                    scene,
                    root,
                    &none,
                    px + 10.0,
                    py + theme::PANEL_PAD + top + 5.0,
                    Some(theme::p().placeholder),
                );
            }
        }
        Kind::Colors { sel, rows, current } => {
            for (i, row) in rows.iter().enumerate() {
                let ry = py + theme::PANEL_PAD + i as f32 * theme::ROW_H;
                let on = i == sel || hovered_row == Some(i);
                draw_row_bg(scene, root, px, ry, pw, on);
                let label_text = match row {
                    None => "Clear",
                    Some(token) => {
                        crate::components::color_picker::LABELS
                            [koru_core::attributes::AccentColor::ALL
                                .iter()
                                .position(|t| t == token)
                                .unwrap_or(0)]
                    }
                };
                match row {
                    Some(token) => {
                        if let Some(tone) = theme::accent_tone(token.as_str()) {
                            fill(
                                scene,
                                root,
                                tone.swatch,
                                &vello::kurbo::Circle::new(
                                    ((px + 16.0) as f64, (ry + theme::ROW_H * 0.5) as f64),
                                    5.5,
                                ),
                            );
                        }
                    }
                    None => {
                        let glyph = view.engine.icon_layout(icons::CLEAR, 15.0);
                        draw_layout(
                            scene,
                            root,
                            &glyph,
                            px + 10.0,
                            ry + 7.0,
                            Some(theme::p().row_hint),
                        );
                    }
                }
                let label = view.engine.simple_layout(
                    label_text,
                    theme::role::ui().with_weight(if on {
                        theme::FontWeight::SemiBold
                    } else {
                        theme::FontWeight::Medium
                    }),
                );
                draw_layout(
                    scene,
                    root,
                    &label,
                    px + 32.0,
                    ry + 4.0,
                    Some(theme::p().text),
                );
                if row.is_some() && *row == current {
                    let check = view.engine.icon_layout(icons::CHECK, 14.0);
                    draw_layout(
                        scene,
                        root,
                        &check,
                        px + pw - 12.0 - check.width(),
                        ry + 7.0,
                        Some(theme::p().accent),
                    );
                }
            }
        }
        // The color picker's recipe over the four levels: a leading glyph
        // column, the label, a trailing check on the current one. The flag
        // is the same neutral hint tone on every row — no color per level,
        // because priority is importance, not ink (invariant 13).
        Kind::Priorities { sel, rows, current } => {
            for (i, row) in rows.iter().enumerate() {
                let ry = py + theme::PANEL_PAD + i as f32 * theme::ROW_H;
                let on = i == sel || hovered_row == Some(i);
                draw_row_bg(scene, root, px, ry, pw, on);
                let (label_text, glyph) = match row {
                    None => ("Clear", icons::CLEAR),
                    Some(level) => (crate::chips::priority_label(*level), icons::FLAG),
                };
                let icon = view.engine.icon_layout(glyph, 15.0);
                draw_layout(
                    scene,
                    root,
                    &icon,
                    px + 10.0,
                    ry + 7.0,
                    Some(theme::p().row_hint),
                );
                let label = view.engine.simple_layout(
                    label_text,
                    theme::role::ui().with_weight(if on {
                        theme::FontWeight::SemiBold
                    } else {
                        theme::FontWeight::Medium
                    }),
                );
                draw_layout(
                    scene,
                    root,
                    &label,
                    px + 32.0,
                    ry + 4.0,
                    Some(theme::p().text),
                );
                if row.is_some() && *row == current {
                    let check = view.engine.icon_layout(icons::CHECK, 14.0);
                    draw_layout(
                        scene,
                        root,
                        &check,
                        px + pw - 12.0 - check.width(),
                        ry + 7.0,
                        Some(theme::p().accent),
                    );
                }
            }
        }
        Kind::Lists { sel, first, rows } => {
            let top = draw_list_search(scene, root, view, px, py, pw);
            let shown = command_menu::visible_rows(rows.len());
            for (slot, n) in (first..).take(shown).enumerate() {
                let Some(row) = rows.get(n) else {
                    break;
                };
                let ry = py + theme::PANEL_PAD + top + slot as f32 * theme::ROW_H;
                let on = n == sel || hovered_row == Some(n);
                draw_row_bg(scene, root, px, ry, pw, on);
                let weight = if on {
                    theme::FontWeight::SemiBold
                } else {
                    theme::FontWeight::Medium
                };
                match row {
                    ListFace::Clear => {
                        let glyph = view.engine.icon_layout(icons::CLEAR, 15.0);
                        draw_layout(
                            scene,
                            root,
                            &glyph,
                            px + 10.0,
                            ry + 7.0,
                            Some(theme::p().row_hint),
                        );
                        let label = view
                            .engine
                            .simple_layout("Clear", theme::role::ui().with_weight(weight));
                        draw_layout(
                            scene,
                            root,
                            &label,
                            px + 32.0,
                            ry + 4.0,
                            Some(theme::p().text),
                        );
                    }
                    ListFace::List {
                        name,
                        color,
                        checked,
                    } => {
                        // The list's swatch, as the rail draws it; the list
                        // glyph for one wearing no colour.
                        match color.as_deref().and_then(theme::accent_tone) {
                            Some(tone) => fill(
                                scene,
                                root,
                                tone.swatch,
                                &vello::kurbo::Circle::new(
                                    ((px + 16.0) as f64, (ry + theme::ROW_H * 0.5) as f64),
                                    5.0,
                                ),
                            ),
                            None => {
                                let glyph = view.engine.icon_layout(icons::LIST, 15.0);
                                draw_layout(
                                    scene,
                                    root,
                                    &glyph,
                                    px + 10.0,
                                    ry + 7.0,
                                    Some(theme::p().row_hint),
                                );
                            }
                        }
                        let fitted = view.engine.elided(name, theme::role::ui(), pw - 64.0);
                        let label = view
                            .engine
                            .simple_layout(&fitted, theme::role::ui().with_weight(weight));
                        draw_layout(
                            scene,
                            root,
                            &label,
                            px + 32.0,
                            ry + 4.0,
                            Some(theme::p().text),
                        );
                        if *checked {
                            let check = view.engine.icon_layout(icons::CHECK, 14.0);
                            draw_layout(
                                scene,
                                root,
                                &check,
                                px + pw - 12.0 - check.width(),
                                ry + 7.0,
                                Some(theme::p().accent),
                            );
                        }
                    }
                    ListFace::Create(name) => {
                        let plus = view.engine.icon_layout(icons::PLUS, 15.0);
                        draw_layout(
                            scene,
                            root,
                            &plus,
                            px + 10.0,
                            ry + 7.0,
                            Some(theme::p().accent),
                        );
                        let fitted = view.engine.elided(
                            &format!("New list \u{201C}{name}\u{201D}"),
                            theme::role::ui(),
                            pw - 44.0,
                        );
                        let label = view.engine.simple_layout(&fitted, theme::role::ui());
                        draw_layout(
                            scene,
                            root,
                            &label,
                            px + 32.0,
                            ry + 4.0,
                            Some(theme::p().text),
                        );
                    }
                }
            }
            if rows.is_empty() {
                let none = view
                    .engine
                    .simple_layout("No lists yet", theme::role::row());
                draw_layout(
                    scene,
                    root,
                    &none,
                    px + 10.0,
                    py + theme::PANEL_PAD + top + 5.0,
                    Some(theme::p().placeholder),
                );
            }
        }
        Kind::ListColors { sel, name } => {
            // The swatches, as the colour picker draws them, one per accent
            // token — the list is named in the row that led here.
            let _ = name;
            for (i, token) in koru_core::attributes::AccentColor::ALL.iter().enumerate() {
                let ry = py + theme::PANEL_PAD + i as f32 * theme::ROW_H;
                let on = i == sel || hovered_row == Some(i);
                draw_row_bg(scene, root, px, ry, pw, on);
                if let Some(tone) = theme::accent_tone(token.as_str()) {
                    fill(
                        scene,
                        root,
                        tone.swatch,
                        &vello::kurbo::Circle::new(
                            ((px + 16.0) as f64, (ry + theme::ROW_H * 0.5) as f64),
                            5.5,
                        ),
                    );
                }
                let label = view.engine.simple_layout(
                    crate::components::color_picker::LABELS[i],
                    theme::role::ui().with_weight(if on {
                        theme::FontWeight::SemiBold
                    } else {
                        theme::FontWeight::Medium
                    }),
                );
                draw_layout(
                    scene,
                    root,
                    &label,
                    px + 32.0,
                    ry + 4.0,
                    Some(theme::p().text),
                );
            }
        }
    }
}

/// The list picker's search box — [`draw_tag_search`]'s twin.
fn draw_list_search(
    scene: &mut Scene,
    root: Affine,
    view: &mut ViewState,
    px: f32,
    py: f32,
    pw: f32,
) -> f32 {
    let ghost = view
        .engine
        .simple_layout("Filter lists, or name a new one", theme::role::ui());
    let r = command_menu::search_rect(pw);
    let shape = rrect(px + r.x, py + r.y, r.w, r.h, 6.0);
    fill(scene, root, theme::p().bg, &shape);
    stroke(scene, root, theme::p().focus_ring, theme::BORDER_W, &shape);
    let Overlay::List(picker) = &mut view.overlay else {
        return 0.0;
    };
    let empty = picker.query.shows_placeholder();
    draw_field(
        scene,
        root,
        &mut picker.query,
        &mut view.engine,
        empty.then_some(&ghost),
        (px + r.x, py + r.y, r.w, r.h),
        true,
    );
    command_menu::rows_top()
}

// **The date grid's drawing is not here.** The phrase box, the month and the
// Clear row went down to `koru_ui::date_grid` with the widget itself
// (ADR-196): where a cell is drawn and where a click lands are one piece of
// arithmetic, so they may not live in two crates. The `Kind::Date` arm above
// calls that one function.

/// The tag chooser's search box, mirroring [`draw_command_search`]. Zero when
/// the picker is document-driven (`#tag`), whose query lives in the row.
fn draw_tag_search(
    scene: &mut Scene,
    root: Affine,
    view: &mut ViewState,
    px: f32,
    py: f32,
    pw: f32,
    has_search: bool,
) -> f32 {
    if !has_search {
        return 0.0;
    }
    let ghost = view.engine.simple_layout("Filter tags", theme::role::ui());
    let r = command_menu::search_rect(pw);
    let shape = rrect(px + r.x, py + r.y, r.w, r.h, 6.0);
    fill(scene, root, theme::p().bg, &shape);
    stroke(scene, root, theme::p().focus_ring, theme::BORDER_W, &shape);
    let Overlay::Tag(picker) = &mut view.overlay else {
        return 0.0;
    };
    if let Some(field) = &mut picker.query {
        // `shows_placeholder`, never `is_empty`: an authored-empty field
        // holding a dictation transcript is still empty, and the ghost would
        // be drawn underneath the words (ADR-272).
        let empty = field.shows_placeholder();
        draw_field(
            scene,
            root,
            field,
            &mut view.engine,
            empty.then_some(&ghost),
            (px + r.x, py + r.y, r.w, r.h),
            true,
        );
    }
    command_menu::rows_top()
}

fn draw_entity_picker(
    scene: &mut Scene,
    root: Affine,
    view: &mut ViewState,
    px: f32,
    py: f32,
    pw: f32,
    hovered_row: Option<usize>,
) {
    use crate::components::entity_picker::{self, HEADER_H, MAX_ROWS};
    let band = match &view.overlay {
        Overlay::Entity(picker) => picker.field_band(),
        _ => return,
    };
    // The move-mode query field, above everything: the mention picker's query
    // is the `@…` text in the document, but a move opens over a selection, so
    // the typing has to land — and be visible — in the panel itself. Drawn
    // before `picker` is borrowed for the rest of the panel, because the field
    // needs the overlay mutably.
    if band > 0.0 {
        let r = entity_picker::field_rect(pw);
        let shape = rrect(px + r.x, py + r.y, r.w, r.h, 6.0);
        fill(scene, root, theme::p().bg, &shape);
        stroke(scene, root, theme::p().focus_ring, theme::BORDER_W, &shape);
        let ghost = view.engine.simple_layout("Move to…", theme::role::ui());
        if let Overlay::Entity(picker) = &mut view.overlay
            && let Some(field) = &mut picker.own_query
        {
            // `shows_placeholder`, never `is_empty`: an authored-empty field
            // holding a dictation transcript is still empty, and the ghost would
            // be drawn underneath the words (ADR-272).
            let empty = field.shows_placeholder();
            draw_field(
                scene,
                root,
                field,
                &mut view.engine,
                empty.then_some(&ghost),
                (px + r.x, py + r.y, r.w, r.h),
                true,
            );
        }
    }
    let Overlay::Entity(picker) = &view.overlay else {
        return;
    };
    let scope = picker.scope_label();
    let filter_open = picker.filter_open();
    let selected = picker.selected_workspaces.clone();
    let workspaces = picker.workspaces.clone();
    let hits = picker.hits.clone();
    let sel = picker.sel;
    let loading = picker.loading;
    let unavailable = picker.unavailable;
    let scrollbar = picker.scrolls().then(|| picker.scrollbar_thumb(pw));

    let chip_x = px + theme::PANEL_PAD;
    let chip_y = py + band + theme::PANEL_PAD + 3.0;
    let chip_w = pw - theme::PANEL_PAD * 2.0;
    fill(
        scene,
        root,
        theme::p().hover_tint,
        &rrect(chip_x, chip_y, chip_w, HEADER_H - 6.0, 7.0),
    );
    let icon = view.engine.icon_layout(icons::LIST_CHECKS, 15.0);
    draw_layout(
        scene,
        root,
        &icon,
        chip_x + 9.0,
        chip_y + (HEADER_H - 6.0 - icon.height()) * 0.5,
        Some(theme::p().accent),
    );
    let fitted = view.engine.elided(&scope, theme::role::ui(), chip_w - 42.0);
    let label = view.engine.simple_layout(&fitted, theme::role::ui());
    draw_layout(
        scene,
        root,
        &label,
        chip_x + 31.0,
        chip_y + (HEADER_H - 6.0 - label.height()) * 0.5,
        Some(theme::p().text),
    );

    let rows_y = py + band + theme::PANEL_PAD + HEADER_H;
    if let Some(headings) = picker.headings() {
        let headings = headings.clone();
        let first = match &view.overlay {
            Overlay::Entity(p) => p.first,
            _ => 0,
        };
        for (i, heading) in headings.shown.iter().skip(first).take(MAX_ROWS).enumerate() {
            let index = first + i;
            let ry = rows_y + i as f32 * theme::ROW_H;
            let on = index == sel || hovered_row == Some(index);
            draw_row_bg(scene, root, px, ry, pw, on);
            let glyph = view.engine.icon_layout(
                match heading.level {
                    1 => icons::HEADING_1,
                    2 => icons::HEADING_2,
                    3 => icons::HEADING_3,
                    _ => icons::HEADING_4,
                },
                15.0,
            );
            draw_layout(
                scene,
                root,
                &glyph,
                px + 10.0,
                ry + 7.0,
                Some(theme::p().row_hint),
            );
            // Indented by level, so the shape of the document is visible in
            // the list rather than having to be read out of six similar rows.
            let indent = 32.0 + f32::from(heading.level.saturating_sub(1)) * 9.0;
            let text = if heading.text.trim().is_empty() {
                // An untitled heading is still a place you can jump to.
                "Untitled heading".to_owned()
            } else {
                heading.text.clone()
            };
            let fitted = view
                .engine
                .elided(&text, theme::role::ui(), pw - indent - 12.0);
            let row = view.engine.simple_layout(
                &fitted,
                theme::role::ui().with_weight(if on {
                    theme::FontWeight::SemiBold
                } else {
                    theme::FontWeight::Medium
                }),
            );
            draw_layout(
                scene,
                root,
                &row,
                px + indent,
                ry + 4.0,
                Some(theme::p().text),
            );
        }
        if headings.shown.is_empty() {
            // Three different sentences, because they ask for three different
            // things from the person reading them.
            let message = if headings.loading {
                "Reading headings…"
            } else if !headings.projected {
                "Indexing…"
            } else if headings.all.is_empty() {
                "No headings in this document"
            } else {
                "No matching heading"
            };
            draw_entity_status(scene, root, view, px, rows_y, message);
        }
        if let Some(thumb) = scrollbar {
            draw_entity_scrollbar(scene, root, px, py, thumb);
        }
        return;
    }
    if filter_open {
        let first = match &view.overlay {
            Overlay::Entity(p) => p.first,
            _ => 0,
        };
        for (i, workspace) in workspaces.iter().skip(first).take(MAX_ROWS).enumerate() {
            let index = first + i;
            let ry = rows_y + i as f32 * theme::ROW_H;
            let on = index == sel || hovered_row == Some(index);
            draw_row_bg(scene, root, px, ry, pw, on);
            if selected.contains(&workspace.id) {
                let check = view.engine.icon_layout(icons::CHECK, 14.0);
                draw_layout(
                    scene,
                    root,
                    &check,
                    px + 10.0,
                    ry + 7.0,
                    Some(theme::p().text),
                );
            }
            let context = if workspace.profile_name.is_empty() {
                workspace.name.clone()
            } else {
                format!("{} — {}", workspace.name, workspace.profile_name)
            };
            let fitted = view.engine.elided(&context, theme::role::ui(), pw - 42.0);
            let row = view.engine.simple_layout(&fitted, theme::role::ui());
            draw_layout(
                scene,
                root,
                &row,
                px + 32.0,
                ry + 4.0,
                Some(theme::p().text),
            );
        }
        if workspaces.is_empty() {
            draw_entity_status(scene, root, view, px, rows_y, "Loading workspaces…");
        }
        if let Some(thumb) = scrollbar {
            draw_entity_scrollbar(scene, root, px, py, thumb);
        }
        return;
    }

    let first = match &view.overlay {
        Overlay::Entity(p) => p.first,
        _ => 0,
    };
    for (i, hit) in hits
        .iter()
        .skip(first)
        .take(MAX_ROWS.saturating_sub(1))
        .enumerate()
    {
        let index = first + i;
        let ry = rows_y + i as f32 * theme::ROW_H;
        let on = index == sel || hovered_row == Some(index);
        draw_row_bg(scene, root, px, ry, pw, on);
        let glyph = view.engine.icon_layout(icons::DOCUMENT, 15.0);
        draw_layout(
            scene,
            root,
            &glyph,
            px + 10.0,
            ry + 7.0,
            Some(theme::p().row_hint),
        );
        let fitted = view
            .engine
            .elided(&hit.title, theme::role::ui(), pw - 118.0);
        let title = view.engine.simple_layout(
            &fitted,
            theme::role::ui().with_weight(if on {
                theme::FontWeight::SemiBold
            } else {
                theme::FontWeight::Medium
            }),
        );
        draw_layout(
            scene,
            root,
            &title,
            px + 32.0,
            ry + 4.0,
            Some(theme::p().text),
        );
        let crumb = view
            .engine
            .elided(&hit.breadcrumb, theme::role::meta(), 76.0);
        let crumb = view.engine.simple_layout(&crumb, theme::role::meta());
        draw_layout(
            scene,
            root,
            &crumb,
            px + pw - 10.0 - crumb.width(),
            ry + 5.5,
            Some(theme::p().row_hint),
        );
    }

    let create_index = hits.len();
    let create_i = hits.len().saturating_sub(first).min(MAX_ROWS - 1);
    let ry = rows_y + create_i as f32 * theme::ROW_H;
    let on = create_index == sel || hovered_row == Some(create_index);
    draw_row_bg(scene, root, px, ry, pw, on);
    let plus = view.engine.icon_layout(icons::PLUS, 15.0);
    draw_layout(
        scene,
        root,
        &plus,
        px + 10.0,
        ry + 7.0,
        Some(theme::p().accent),
    );
    let add = view
        .engine
        .simple_layout("Add new document", theme::role::ui());
    draw_layout(
        scene,
        root,
        &add,
        px + 32.0,
        ry + 4.0,
        Some(theme::p().text),
    );

    if hits.is_empty() && (loading || unavailable) {
        let message = if unavailable {
            "Search unavailable"
        } else {
            "Searching…"
        };
        // Put status at the right without replacing the always-available action.
        let status = view.engine.simple_layout(message, theme::role::meta());
        draw_layout(
            scene,
            root,
            &status,
            px + pw - 10.0 - status.width(),
            ry + 5.5,
            Some(theme::p().row_hint),
        );
    }
    if let Some(thumb) = scrollbar {
        draw_entity_scrollbar(scene, root, px, py, thumb);
    }
}

fn draw_entity_scrollbar(
    scene: &mut Scene,
    root: Affine,
    px: f32,
    py: f32,
    thumb: koru_draw::geometry::Rect,
) {
    fill(
        scene,
        root,
        theme::p().scrollbar_thumb,
        &rrect(px + thumb.x, py + thumb.y, thumb.w, thumb.h, thumb.w * 0.5),
    );
}

fn draw_entity_status(
    scene: &mut Scene,
    root: Affine,
    view: &mut ViewState,
    px: f32,
    y: f32,
    text: &str,
) {
    let row = view.engine.simple_layout(text, theme::role::meta());
    draw_layout(
        scene,
        root,
        &row,
        px + 10.0,
        y + 5.5,
        Some(theme::p().row_hint),
    );
}

/// The command menu's search box, and how far down the rows begin.
///
/// Zero for a menu with no field — the suite builds those by hand, and a panel
/// that reserved room for a box it never drew would leave a gap nothing
/// explains.
fn draw_command_search(
    scene: &mut Scene,
    root: Affine,
    view: &mut ViewState,
    px: f32,
    py: f32,
    pw: f32,
) -> f32 {
    let Overlay::Command(menu) = &view.overlay else {
        return 0.0;
    };
    if !menu.has_search() {
        return 0.0;
    }
    // Laid out before the field is borrowed mutably below.
    let ghost = view
        .engine
        .simple_layout(menu.placeholder(), theme::role::ui());
    let r = command_menu::search_rect(pw);
    let shape = rrect(px + r.x, py + r.y, r.w, r.h, 6.0);
    fill(scene, root, theme::p().bg, &shape);
    // The focus ring, not the panel border: the caret is in here the moment the
    // menu opens, so the box has to look like the thing being typed into.
    stroke(scene, root, theme::p().focus_ring, theme::BORDER_W, &shape);
    let Overlay::Command(menu) = &mut view.overlay else {
        return 0.0;
    };
    if let Some(field) = &mut menu.query {
        // `shows_placeholder`, never `is_empty`: an authored-empty field
        // holding a dictation transcript is still empty, and the ghost would
        // be drawn underneath the words (ADR-272).
        let empty = field.shows_placeholder();
        draw_field(
            scene,
            root,
            field,
            &mut view.engine,
            empty.then_some(&ghost),
            (px + r.x, py + r.y, r.w, r.h),
            true,
        );
    }
    command_menu::rows_top()
}

fn draw_emoji_picker(
    scene: &mut Scene,
    root: Affine,
    view: &mut ViewState,
    px: f32,
    py: f32,
    pw: f32,
) {
    let (engine, overlay) = (&mut view.engine, &mut view.overlay);
    let Overlay::Emoji(picker) = overlay else {
        return;
    };
    draw_emoji_picker_surface(scene, root, engine, picker, px, py, pw);
}

/// Draw the complete `/emoji` surface for callers outside a document overlay.
///
/// Search, scrolling, categories, recents and skin-tone strips all remain the
/// picker widget's implementation; appearance dialogs embed this instead of
/// maintaining a second emoji grid.
pub fn draw_emoji_picker_surface(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    picker: &mut emoji_picker::EmojiPicker,
    px: f32,
    py: f32,
    pw: f32,
) {
    // Promote an armed hold into the tone strip on the frame the deadline hits.
    picker.tick_long_press();
    let (hits, sel, first, has_search, category, thumb, selected_record, tone_strip, tone_hover) = (
        picker.visible_hits().to_vec(),
        picker.sel,
        picker.first,
        picker.query.is_some(),
        picker.category,
        emoji_picker::scrollbar_thumb(picker),
        picker.hits.get(picker.sel).copied(),
        picker.tone_strip.clone(),
        picker.tone_hover,
    );

    let geometry = picker.geometry();
    let _ = pw;
    // The look picker's tint strip: the shared swatch row, so the chosen
    // tint is ringed the way every other color choice in the app is.
    if let Some(strip) = picker.tints
        && let Some(rect) = geometry.tint_strip_rect()
    {
        let colors = emoji_picker::tint_colors();
        koru_ui::ColorPickerRow::new(&colors)
            .selected(Some(emoji_picker::tint_index(strip.current)))
            .hovered(strip.hover)
            .draw(
                scene,
                root,
                engine,
                koru_draw::geometry::Rect::new(
                    px + rect.x + koru_ui::Swatch::RING_REACH,
                    py + rect.y + koru_ui::Swatch::RING_REACH,
                    rect.w - koru_ui::Swatch::RING_REACH * 2.0,
                    koru_ui::Swatch::SIZE,
                ),
            );
    }
    if has_search {
        let field_rect = geometry.search_rect();
        if let Some(field) = &mut picker.query {
            // The shared text input, so the search looks like every other
            // field: its box, its focus border, its placeholder and caret.
            koru_ui::text_input::draw_text_input(
                scene,
                root,
                field,
                engine,
                koru_draw::geometry::Rect::new(
                    px + field_rect.x,
                    py + field_rect.y,
                    field_rect.w,
                    field_rect.h,
                ),
                koru_ui::InputStyle {
                    style: theme::role::ui(),
                    ..koru_ui::InputStyle::form(true)
                },
                Some("Search emoji"),
                true,
                0.0,
            );
        }
    }

    let grid = geometry.grid_rect();
    let track = geometry.scrollbar_rect();
    let list = koru_draw::geometry::Rect::new(
        px + grid.x,
        py + grid.y,
        track.x + track.w - grid.x,
        geometry.category_top() - grid.y,
    );
    {
        let _list = koru_draw::inspect::scope(
            "EmojiList",
            "component",
            root,
            koru_draw::scene::rect(list.x, list.y, list.w, list.h),
            |record| {
                record.property(
                    "implementation",
                    "koru_editor::components::emoji_picker",
                    "shared implementation",
                );
                record.property("cell", geometry.cell, "Geometry::cell");
                record.property(
                    "columns / rows",
                    format!("{} / {}", emoji_picker::COLS, emoji_picker::ROWS),
                    "emoji_picker",
                );
            },
        );
        let cell_w = geometry.cell;
        for (cell, &record_index) in hits.iter().enumerate() {
            let row = cell / emoji_picker::COLS;
            let col = cell % emoji_picker::COLS;
            let x = px + grid.x + col as f32 * cell_w;
            let y = py + grid.y + row as f32 * cell_w;
            let record = &crate::emoji::catalog()[record_index];
            let shown = emoji_picker::EmojiPicker::display_value(record.value);
            let selected = first + cell == sel;
            let _emoji = koru_draw::inspect::scope(
                "Emoji",
                "component",
                root,
                koru_draw::scene::rect(x, y, cell_w, cell_w),
                |r| {
                    r.property("emoji", shown.as_str(), "catalog");
                    r.property("name", record.name, "catalog");
                    r.property("selected", selected, "picker state");
                },
            );
            if selected {
                fill(
                    scene,
                    root,
                    theme::selection_color(),
                    &rrect(x + 2.0, y + 2.0, cell_w - 4.0, cell_w - 4.0, 6.0),
                );
            }
            let glyph = engine.simple_layout(&shown, theme::role::body());
            draw_layout(
                scene,
                root,
                &glyph,
                x + (cell_w - glyph.width()) * 0.5,
                y + (cell_w - glyph.height()) * 0.5,
                None,
            );
        }

        // A persistent position indicator makes mouse-wheel and keyboard
        // movement legible. The track is intentionally quiet; the thumb
        // carries the state.
        fill(
            scene,
            root,
            theme::p().panel_border,
            &rrect(px + track.x + 1.5, py + track.y, 2.0, track.h, 1.0),
        );
        fill(
            scene,
            root,
            theme::p().scrollbar_thumb,
            &rrect(px + thumb.x, py + thumb.y, thumb.w, thumb.h, thumb.w * 0.5),
        );

        if let Some(record_index) = selected_record {
            let record = &crate::emoji::catalog()[record_index];
            let label = record
                .shortcode
                .map(|shortcode| format!(":{shortcode}: {}", record.name))
                .unwrap_or_else(|| record.name.to_owned());
            let fitted = engine.elided(&label, theme::role::hint(), list.w);
            let label = engine.simple_layout(&fitted, theme::role::hint());
            draw_layout(
                scene,
                root,
                &label,
                list.x,
                py + geometry.footer_top() + 4.0,
                Some(theme::p().row_hint),
            );
        }
    }

    {
        let bar = geometry.category_rect(0).map(|first| {
            koru_draw::scene::rect(
                px + first.x,
                py + first.y,
                first.w * emoji_picker::GROUPS.len() as f32,
                first.h,
            )
        });
        let _bar = bar.map(|bar| {
            koru_draw::inspect::scope("EmojiCategoryBar", "component", root, bar, |_| {})
        });
        for (index, group) in emoji_picker::GROUPS.into_iter().enumerate() {
            let Some(rect) = geometry.category_rect(index) else {
                continue;
            };
            let selected = category == Some(group);
            if selected {
                fill(
                    scene,
                    root,
                    theme::selection_color(),
                    &rrect(
                        px + rect.x + 3.0,
                        py + rect.y + 2.0,
                        rect.w - 6.0,
                        rect.h - 4.0,
                        6.0,
                    ),
                );
            }
            let icon = engine.simple_layout(
                emoji_picker::category_icon(group),
                theme::role::body().with_weight(if selected {
                    theme::FontWeight::SemiBold
                } else {
                    theme::FontWeight::Medium
                }),
            );
            draw_layout(
                scene,
                root,
                &icon,
                px + rect.x + (rect.w - icon.width()) * 0.5,
                py + rect.y + (rect.h - icon.height()) * 0.5,
                None,
            );
        }
    }

    if let Some(strip) = tone_strip {
        let panel = emoji_picker::tone_strip_rect(&strip);
        koru_draw::scene::floating_panel(
            scene,
            root,
            px + panel.x,
            py + panel.y,
            panel.w,
            panel.h,
            &koru_draw::scene::Panel::popover(8.0),
        );
        for (i, tone) in strip.tones.iter().enumerate() {
            let Some(cell) = emoji_picker::tone_cell_rect(&strip, i) else {
                continue;
            };
            if tone_hover == Some(i) {
                fill(
                    scene,
                    root,
                    theme::selection_color(),
                    &rrect(
                        px + cell.x + 2.0,
                        py + cell.y + 2.0,
                        cell.w - 4.0,
                        cell.h - 4.0,
                        6.0,
                    ),
                );
            }
            let glyph = engine.simple_layout(tone, theme::role::body());
            draw_layout(
                scene,
                root,
                &glyph,
                px + cell.x + (cell.w - glyph.width()) * 0.5,
                py + cell.y + (cell.h - glyph.height()) * 0.5,
                None,
            );
        }
    }
}

/// The Ctrl+K prompt: a name field and a URL field, with the active one ringed.
pub fn draw_link_prompt(
    scene: &mut Scene,
    root: Affine,
    view: &mut ViewState,
    px: f32,
    py: f32,
    pw: f32,
) {
    let active = match &view.overlay {
        Overlay::Link(p) => p.field,
        _ => return,
    };

    // Lay out the chrome first, so `engine` is free before the overlay is
    // borrowed for the two fields below.
    let labels = [
        view.engine.simple_layout("Name", theme::role::hint()),
        view.engine.simple_layout("Link", theme::role::hint()),
    ];
    let hint = view
        .engine
        .simple_layout("Tab switches field · Enter adds", theme::role::hint());
    let ghosts = [
        view.engine.simple_layout("Reward terms", theme::role::ui()),
        view.engine
            .simple_layout("example.com/terms", theme::role::ui()),
    ];

    let fx = px + theme::PANEL_PAD + 2.0;

    for (i, which) in LinkField::BOTH.into_iter().enumerate() {
        // The same rects the click handler tests against, so a field is
        // clickable exactly where it is drawn.
        let box_ = link_prompt::field_rect(which, pw);
        let (field_y, fw) = (py + box_.y, box_.w);
        draw_layout(
            scene,
            root,
            &labels[i],
            fx + 2.0,
            field_y - 16.0,
            Some(theme::p().row_hint),
        );

        let shape = rrect(fx, field_y, fw, link_prompt::FIELD_H, 5.0);
        fill(scene, root, theme::p().bg, &shape);
        stroke(
            scene,
            root,
            if which == active {
                theme::p().focus_ring
            } else {
                theme::p().panel_border
            },
            if which == active { 1.6 } else { 1.0 },
            &shape,
        );

        // Disjoint field borrows: the prompt lives in `overlay`, the shaping
        // contexts in `engine`.
        let Overlay::Link(link_prompt::LinkPrompt { name, url, .. }) = &mut view.overlay else {
            return;
        };
        let field = match which {
            LinkField::Name => name,
            LinkField::Url => url,
        };
        // `shows_placeholder`, never `is_empty` -- see ADR-272.
        let ghost = field.shows_placeholder().then_some(&ghosts[i]);
        draw_field(
            scene,
            root,
            field,
            &mut view.engine,
            ghost,
            (fx, field_y, fw, link_prompt::FIELD_H),
            which == active,
        );
    }

    draw_layout(
        scene,
        root,
        &hint,
        fx + 2.0,
        py + theme::PANEL_PAD + link_prompt::PANEL_H - 14.0,
        Some(theme::p().row_hint),
    );
}

pub fn draw_name_prompt(
    scene: &mut Scene,
    root: Affine,
    view: &mut ViewState,
    px: f32,
    py: f32,
    pw: f32,
) {
    // The hover the editor already resolves for this button — see
    // `Hover::PanelControl`, which exists because the page icon is the one thing
    // in a prompt you press rather than type into.
    let icon_hot = view.hover == Some(Hover::PanelControl);
    let (engine, overlay) = (&mut view.engine, &mut view.overlay);
    let Overlay::Name(prompt) = overlay else {
        return;
    };
    let (label, placeholder) = if matches!(prompt.trigger.anchor, Anchor::RenameImage(_)) {
        ("Image name", "Architecture diagram")
    } else {
        ("Document name", "spec.md")
    };
    draw_name_prompt_contents(
        scene,
        root,
        prompt,
        engine,
        px,
        py,
        pw,
        label,
        placeholder,
        icon_hot,
    );
}

/// Draw the label and editable field shared by document-name prompts.
///
/// The editor normally anchors this content to a row overlay. The manager's
/// taskbar action uses the same component in a centred modal, so the content is
/// exposed without the editor-specific [`ViewState`] wrapper. The caller owns
/// the surrounding panel chrome.
#[allow(clippy::too_many_arguments)]
pub fn draw_name_prompt_contents(
    scene: &mut Scene,
    root: Affine,
    prompt: &mut NamePrompt,
    engine: &mut TextEngine,
    px: f32,
    py: f32,
    pw: f32,
    label: &str,
    placeholder: &str,
    icon_hot: bool,
) {
    let label = engine.simple_layout(label, theme::role::hint());
    draw_layout(
        scene,
        root,
        &label,
        px + theme::PANEL_PAD + 4.0,
        py + theme::PANEL_PAD + 2.0,
        Some(theme::p().row_hint),
    );

    if prompt.can_set_page_icon() {
        // **No box around it.** The frame said "this is a field", which it is
        // not — it is one glyph you click to change. Beside a field that *is*
        // boxed, two outlines read as two inputs; the emoji is its own
        // affordance, and the `+` says the rest when there is none yet.
        let icon_box = name_prompt::icon_rect();
        // The same chip the caption's buttons wear: `hover_tint`, radius 6,
        // square and centred in the slot. With the frame gone this is the only
        // thing saying the glyph is a control, so it is the thing that has to
        // say it.
        if icon_hot {
            let side = icon_box.w.min(icon_box.h);
            fill(
                scene,
                root,
                theme::p().hover_tint,
                &rrect(
                    px + icon_box.x + (icon_box.w - side) * 0.5,
                    py + icon_box.y + (icon_box.h - side) * 0.5,
                    side,
                    side,
                    6.0,
                ),
            );
        }
        let glyph = if let Some(icon) = prompt.page_icon.as_deref() {
            engine.simple_layout(icon, theme::role::body())
        } else {
            engine.icon_layout(icons::PLUS, theme::FONT_SIZE)
        };
        draw_layout(
            scene,
            root,
            &glyph,
            px + icon_box.x + (icon_box.w - glyph.width()) * 0.5,
            baseline_centred_y(&glyph, py + icon_box.y, icon_box.h),
            None,
        );
    }

    // The same rect the click handler tests against.
    let box_ = prompt.field_rect(pw);
    let (field_x, field_y, field_w) = (px + box_.x, py + box_.y, box_.w);
    fill(
        scene,
        root,
        theme::p().bg,
        &rrect(field_x, field_y, field_w, 26.0, 5.0),
    );
    stroke(
        scene,
        root,
        theme::p().focus_ring,
        theme::BORDER_W,
        &rrect(field_x, field_y, field_w, 26.0, 5.0),
    );

    // Lay the ghost text out before splitting the borrows below, so the engine
    // is not needed again while the overlay is borrowed.
    // `shows_placeholder`, never `raw_text().is_empty()`: a `TextInput`
    // holding a dictation transcript reads as authored-empty (ADR-248).
    let is_empty = prompt.input.shows_placeholder();
    let ghost = is_empty.then(|| engine.simple_layout(placeholder, theme::role::ui()));
    let NamePrompt { input, .. } = prompt;
    draw_field(
        scene,
        root,
        input,
        engine,
        ghost.as_ref(),
        (field_x, field_y, field_w, 26.0),
        true,
    );
}
