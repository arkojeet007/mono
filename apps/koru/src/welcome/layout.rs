// SPDX-License-Identifier: MPL-2.0

//! Every rectangle on the page, measured before anything is drawn.
//!
//! **One geometry.** [`measure`] produces a [`Layout`]; the draw pass paints
//! from it and `WelcomeView::target_at` hit-tests against its `hits`. Neither
//! computes a position of its own, so a control cannot be pressable somewhere
//! other than where it is painted. Coordinates are content space: the scroll
//! offset is applied by the caller.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme;
use koru_ui::card::CardRow;
use koru_ui::swatch::Swatch;
use koru_ui::text_paragraph::KoruTextParagraph;

use super::rows::{RowId, Section};
use super::{Target, WelcomeView};

/// Widest the column ever draws.
pub(super) const COLUMN_W: f32 = 640.0;
pub(super) const TOP: f32 = 64.0;
/// Between the Customize card and each section.
pub(super) const SECTION_GAP: f32 = 32.0;
/// A section's label line and the air under it.
pub(super) const LABEL_H: f32 = 16.0;
const LABEL_GAP: f32 = 10.0;
const LINK_GAP: f32 = 10.0;
/// The tick circle.
pub(super) const TICK: f32 = 19.0;
const TICK_GAP: f32 = 12.0;
const SENTENCE_GAP: f32 = 2.0;
const CHIPS_GAP: f32 = 10.0;
const HINTS_GAP: f32 = 8.0;
const CODE_GAP: f32 = 8.0;
pub(super) const HINT_LINE_H: f32 = 24.0;
pub(super) const HINT_CHORD_W: f32 = 112.0;
/// The key / command chip at a row's right end.
pub(super) const KEY_H: f32 = koru_ui::KeyChip::H;
const CARET_W: f32 = 16.0;
/// The pairing square and its caption.
pub(super) const QR: f32 = 92.0;
const QR_CAPTION_GAP: f32 = 8.0;
const BOTTOM_PAD: f32 = 64.0;

pub(super) fn title_style() -> theme::TextStyle {
    theme::role::ui()
}

pub(super) fn sentence_style() -> theme::TextStyle {
    theme::role::meta()
}

pub(super) struct Layout {
    pub column: Rect,
    pub customize: CustomizeLayout,
    pub sections: Vec<SectionLayout>,
    /// Every pressable rectangle in visual order — which is also the Tab
    /// order. A target may appear more than once (a link that wrapped).
    pub hits: Vec<(Target, Rect)>,
    /// The bottom of the page, padding included: the scroll range.
    pub bottom: f32,
}

pub(super) struct CustomizeLayout {
    pub card: Rect,
    pub swatches: Vec<Rect>,
    /// The text half of the row, swatches excluded.
    pub row: Rect,
    pub text_w: f32,
    pub button: Rect,
}

pub(super) struct SectionLayout {
    pub section: Section,
    pub label_y: f32,
    pub progress: Option<(String, Rect)>,
    pub toggle: Option<(&'static str, Rect)>,
    pub card: Rect,
    pub dividers: Vec<f32>,
    pub rows: Vec<RowLayout>,
    /// The one line a completed section folds to.
    pub folded: Option<Rect>,
    pub qr: Option<QrLayout>,
}

pub(super) struct QrLayout {
    /// The square and its caption together: what is pressed.
    pub hit: Rect,
    pub square: Rect,
    pub caption: (f32, f32),
}

pub(super) struct RowLayout {
    pub id: RowId,
    pub rect: Rect,
    pub tick: Option<Rect>,
    pub title: &'static str,
    pub title_rect: Rect,
    pub sentence: Vec<Run>,
    pub trailing: Trailing,
    pub chips: Vec<(&'static str, String, Rect)>,
    pub hints: Vec<HintLine>,
    /// A command to copy, in a code chip under the sentence: what is drawn,
    /// shortened in the middle when it does not fit.
    pub code: Option<(String, Rect)>,
}

/// A stretch of one sentence line in one style.
pub(super) struct Run {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    /// `Some` for a link.
    pub link: Option<Target>,
}

impl Run {
    pub fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, self.w, sentence_style().metrics.line_height)
    }
}

pub(super) enum Trailing {
    None,
    /// A shortcut, from the registry.
    Key(String, Rect),
    Button(&'static str, Target, Rect),
    /// `›` closed, `⌄` open.
    Caret(bool, Rect),
    /// A command to type, drawn as a code chip.
    Code(&'static str, Rect),
}

pub(super) struct HintLine {
    pub chord: String,
    pub what: &'static str,
    pub y: f32,
}

/// What one row says, before it is measured.
struct RowCopy {
    title: &'static str,
    sentence: Vec<(String, Option<Target>)>,
}

fn copy(row: RowId, workspace: &str) -> RowCopy {
    use super::*;
    let plain = |s: &str| vec![(s.to_owned(), None)];
    let (title, sentence) = match row {
        RowId::FirstDocument => (
            FIRST_DOCUMENT_TITLE,
            vec![
                (FIRST_DOCUMENT_BEFORE.to_owned(), None),
                (FIRST_DOCUMENT_MIC.to_owned(), Some(Target::MicLink)),
                (FIRST_DOCUMENT_AFTER.to_owned(), None),
            ],
        ),
        RowId::Documents => (DOCUMENTS_TITLE, plain(DOCUMENTS_HINT)),
        RowId::Files => (FILES_TITLE, plain(&files_hint(workspace))),
        RowId::Learn => (LEARN_TITLE, plain(LEARN_HINT)),
        RowId::TaskList => (TASK_LIST_TITLE, Vec::new()),
        RowId::Reminder => (REMINDER_TITLE, plain(REMINDER_HINT)),
        RowId::Feedback => (
            FEEDBACK_TITLE,
            vec![
                (FEEDBACK_BEFORE.to_owned(), None),
                (FEEDBACK_FORM.to_owned(), Some(Target::FeedbackForm)),
                (FEEDBACK_OR.to_owned(), None),
                (FEEDBACK_EMAIL.to_owned(), Some(Target::Email)),
                (FEEDBACK_AFTER.to_owned(), None),
            ],
        ),
        RowId::Phone => (PHONE_TITLE, plain(PHONE_HINT)),
        RowId::Computer => (COMPUTER_TITLE, plain(COMPUTER_HINT)),
        RowId::Watch => (WATCH_TITLE, plain(WATCH_HINT)),
        RowId::Ai => (AI_TITLE, plain(AI_HINT)),
        RowId::Mcp => (MCP_TITLE, plain(MCP_HINT)),
        RowId::TerminalAgent => (AGENT_TITLE, plain(AGENT_HINT)),
        RowId::OwnApps => (APPS_TITLE, plain(APPS_HINT)),
    };
    RowCopy { title, sentence }
}

/// Whether the whole row is the row's action.
pub(super) fn row_is_target(row: RowId) -> bool {
    matches!(
        row,
        RowId::FirstDocument
            | RowId::Learn
            | RowId::Reminder
            | RowId::Phone
            | RowId::Computer
            | RowId::Ai
            | RowId::TerminalAgent
    )
}

/// The width of a key or code chip for `text` — the shared [`koru_ui::KeyChip`].
pub(super) fn key_w(engine: &mut TextEngine, text: &str) -> f32 {
    koru_ui::KeyChip::width(engine, text)
}

/// `text` cut in the middle, with an ellipsis, to fit a key chip no wider than
/// `max_w`. A path's end says the most — the bridge's name and its flags — so
/// the end keeps two thirds of what is kept.
pub(super) fn elide_middle(engine: &mut TextEngine, text: &str, max_w: f32) -> String {
    if key_w(engine, text) <= max_w {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let cut = |keep: usize| {
        let head = keep / 3;
        let tail = keep - head;
        let mut out: String = chars[..head].iter().collect();
        out.push('\u{2026}');
        out.extend(&chars[chars.len() - tail..]);
        out
    };
    // The most characters that still fit: `lo` always fits, `hi` never does.
    let (mut lo, mut hi) = (0, chars.len());
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if key_w(engine, &cut(mid)) <= max_w {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    cut(lo)
}

/// Lay a sentence of styled segments into lines no wider than `width`,
/// wrapping between words. A piece that follows another with no space
/// between (a comma after a link) never starts a line.
fn flow(
    engine: &mut TextEngine,
    segments: &[(String, Option<Target>)],
    x: f32,
    y: f32,
    width: f32,
) -> (Vec<Run>, f32) {
    let style = sentence_style();
    let line_h = style.metrics.line_height;
    let space =
        engine.simple_layout("a b", style).width() - engine.simple_layout("ab", style).width();
    let right = x + width;
    let mut runs: Vec<Run> = Vec::new();
    let mut cx = x;
    let mut line = 0usize;
    let mut glued = false;
    for (text, link) in segments {
        for piece in text.split_inclusive(' ') {
            let word = piece.trim_end_matches(' ');
            let trailing = piece.len() - word.len();
            let w = if word.is_empty() {
                0.0
            } else {
                engine.simple_layout(word, style).width()
            };
            if !glued && cx > x && cx + w > right {
                line += 1;
                cx = x;
            }
            let ry = y + line as f32 * line_h;
            let extends = runs
                .last()
                .is_some_and(|run| run.link == *link && (run.y - ry).abs() < 0.5);
            if extends {
                let run = runs.last_mut().expect("checked above");
                run.text.push_str(piece);
                run.w = cx + w - run.x;
            } else if !word.is_empty() || trailing > 0 {
                runs.push(Run {
                    text: piece.to_owned(),
                    x: cx,
                    y: ry,
                    w,
                    link: *link,
                });
            }
            cx += w + space * trailing as f32;
            glued = trailing == 0;
        }
    }
    // A run's text may end in spaces; its width never includes them.
    for run in &mut runs {
        let trimmed = run.text.trim_end().len();
        run.text.truncate(trimmed);
    }
    runs.retain(|run| !run.text.is_empty());
    let height = if runs.is_empty() {
        0.0
    } else {
        (line + 1) as f32 * line_h
    };
    (runs, height)
}

/// Measure the page as `view` stands now.
pub(super) fn measure(view: &mut WelcomeView) -> Layout {
    // Everything the measure reads from the view, taken before the engine is
    // borrowed for the rest of the function.
    let (vw, _) = view.viewport;
    let identity = view.identity.clone();
    let learn_open = view.learn_open;
    let mcp_command = view.mcp_command.clone();
    let progress: Vec<(Section, (usize, usize), bool, bool)> = Section::ALL
        .into_iter()
        .map(|s| {
            (
                s,
                view.progress_of(s),
                view.collapsed(s),
                view.shown.contains(&s),
            )
        })
        .collect();
    let engine = &mut view.engine;

    let w = COLUMN_W.min((vw - theme::MARGIN_X * 2.0).max(0.0));
    let x = ((vw - w) * 0.5).max(0.0);
    let column = Rect::new(x, TOP, w, 0.0);
    let mut hits: Vec<(Target, Rect)> = Vec::new();

    // No header for now: the page opens on the Customize card.
    let mut y = TOP;

    // ---------------------------------------------------------- customize
    let swatch_w = Swatch::row_width(3);
    let row_x = x + CardRow::PAD_X + swatch_w;
    let row_w = (w - CardRow::PAD_X - swatch_w).max(0.0);
    let button_w = koru_ui::button::button_width(engine, super::CUSTOMIZE_BUTTON);
    let customize_text_w = CardRow::text_w(row_w, button_w);
    let customize_h = CardRow::new(super::CUSTOMIZE_TITLE)
        .hint(super::CUSTOMIZE_HINT)
        .height(engine, customize_text_w, koru_ui::button::H);
    let card = Rect::new(x, y, w, customize_h);
    let swatches = Swatch::row(
        x + CardRow::PAD_X,
        y + (customize_h - Swatch::SIZE) * 0.5,
        3,
    );
    let row = Rect::new(row_x, y, row_w, customize_h);
    let button = CardRow::control_slot(row, button_w, koru_ui::button::H);
    hits.push((Target::Customize, button));
    let customize = CustomizeLayout {
        card,
        swatches,
        row,
        text_w: customize_text_w,
        button,
    };
    y += customize_h + SECTION_GAP;

    // ----------------------------------------------------------- sections
    let mut sections = Vec::new();
    for (section, (n, m), collapsed, shown) in progress {
        let label_y = y;
        let mut right = x + w;
        let toggle = (section.counts() && m > 0 && n == m).then(|| {
            let label = if shown { super::HIDE } else { super::SHOW };
            let lw = engine.simple_layout(label, sentence_style()).width();
            let rect = Rect::new(right - lw, label_y, lw, LABEL_H);
            right -= lw + LINK_GAP;
            (label, rect)
        });
        let progress = section.counts().then(|| {
            let text = super::progress(n, m);
            let pw = engine.simple_layout(&text, sentence_style()).width();
            (text, Rect::new(right - pw, label_y, pw, LABEL_H))
        });
        if let Some((_, rect)) = toggle {
            hits.push((Target::Toggle(section), rect));
        }
        y += LABEL_H + LABEL_GAP;

        let card_top = y;
        let mut rows = Vec::new();
        let mut dividers = Vec::new();
        let mut folded = None;
        let mut qr = None;
        if collapsed {
            let rect = Rect::new(x, y, w, CardRow::MIN_H);
            folded = Some(rect);
            y += rect.h;
        } else {
            let rows_w = if section == Section::Devices {
                (w - QR - CardRow::PAD_X * 2.0).max(120.0)
            } else {
                w
            };
            for (i, &id) in section.rows().iter().enumerate() {
                if i > 0 {
                    dividers.push(y);
                }
                let laid = measure_row(
                    engine,
                    id,
                    &identity.workspace,
                    Rect::new(x, y, rows_w, 0.0),
                    learn_open,
                    &mcp_command,
                );
                if row_is_target(id) {
                    hits.push((Target::Row(id), laid.rect));
                }
                for run in &laid.sentence {
                    if let Some(link) = run.link {
                        hits.push((link, run.rect()));
                    }
                }
                for (format, _, rect) in &laid.chips {
                    hits.push((Target::Import(format), *rect));
                }
                if let Some((_, rect)) = &laid.code {
                    hits.push((Target::CopyMcp, *rect));
                }
                if let Trailing::Button(_, target, rect) = &laid.trailing {
                    hits.push((*target, *rect));
                }
                y += laid.rect.h;
                rows.push(laid);
            }
            if section == Section::Devices {
                let rows_h = y - card_top;
                let caption_h = sentence_style().metrics.line_height;
                let block = QR + QR_CAPTION_GAP + caption_h;
                let qx = x + w - CardRow::PAD_X - QR;
                let qy = card_top + ((rows_h - block) * 0.5).max(CardRow::PAD_Y);
                let caption_w = engine
                    .simple_layout(super::SCAN_TO_PAIR, sentence_style())
                    .width();
                let square = Rect::new(qx, qy, QR, QR);
                let hit = Rect::new(
                    qx.min(qx + (QR - caption_w) * 0.5),
                    qy,
                    QR.max(caption_w),
                    block,
                );
                hits.push((Target::PairQr, hit));
                qr = Some(QrLayout {
                    hit,
                    square,
                    caption: (qx + (QR - caption_w) * 0.5, qy + QR + QR_CAPTION_GAP),
                });
                y = y.max(qy + block + CardRow::PAD_Y);
            }
        }
        let card = Rect::new(x, card_top, w, y - card_top);
        sections.push(SectionLayout {
            section,
            label_y,
            progress,
            toggle,
            card,
            dividers,
            rows,
            folded,
            qr,
        });
        y += SECTION_GAP;
    }

    // The last section ends the page: there is no footer under it.
    let bottom = y - SECTION_GAP + BOTTOM_PAD;

    Layout {
        column: Rect::new(column.x, column.y, column.w, bottom - TOP),
        customize,
        sections,
        hits,
        bottom,
    }
}

/// One row: its rect grows to whatever its text, chips and hints need.
fn measure_row(
    engine: &mut TextEngine,
    id: RowId,
    workspace: &str,
    at: Rect,
    learn_open: bool,
    mcp_command: &str,
) -> RowLayout {
    let RowCopy { title, sentence } = copy(id, workspace);
    let trailing_w = match id {
        RowId::FirstDocument => {
            super::hints::new_document_chord().map(|chord| key_w(engine, &chord))
        }
        RowId::Files => Some(koru_ui::button::button_width(engine, super::FILES_BUTTON)),
        RowId::Learn => Some(CARET_W),
        RowId::Mcp => Some(koru_ui::button::button_width(engine, super::MCP_COPY)),
        RowId::TerminalAgent => Some(key_w(engine, super::AGENT_COMMAND)),
        _ => None,
    };
    let text_x = at.x + CardRow::PAD_X + TICK + TICK_GAP;
    let text_w = (at.w
        - CardRow::PAD_X * 2.0
        - TICK
        - TICK_GAP
        - trailing_w.map_or(0.0, |tw| tw + CardRow::GAP))
    .max(40.0);
    let top = at.y + CardRow::PAD_Y;
    let title_h =
        KoruTextParagraph::new(title, title_style(), "", theme::p().text).measure(engine, text_w);
    let title_rect = Rect::new(text_x, top, text_w, title_h);
    let mut bottom = top + title_h;
    let (runs, sentence_h) = flow(engine, &sentence, text_x, bottom + SENTENCE_GAP, text_w);
    if sentence_h > 0.0 {
        bottom += SENTENCE_GAP + sentence_h;
    }
    let mut chips = Vec::new();
    if id == RowId::Documents {
        let formats: Vec<(&'static str, String)> =
            koru_workspace_host::formats::import_menu_formats()
                .into_iter()
                .map(|descriptor| {
                    let label = koru_workspace_host::formats::import_menu_name(
                        descriptor.id,
                        crate::format::lang(),
                    )
                    .unwrap_or(descriptor.name);
                    (descriptor.id, label.to_owned())
                })
                .collect();
        let rects = koru_ui::chip::wrap(
            engine,
            text_x,
            bottom + CHIPS_GAP,
            text_x + text_w,
            formats.iter().map(|(_, label)| label.as_str()),
        );
        bottom = koru_ui::chip::bottom(&rects, bottom);
        chips = formats
            .into_iter()
            .zip(rects)
            .map(|((format, label), rect)| (format, label, rect))
            .collect();
    }
    let mut code = None;
    if id == RowId::Mcp {
        let shown = elide_middle(engine, mcp_command, text_w);
        let rect = Rect::new(
            text_x,
            bottom + CODE_GAP,
            key_w(engine, &shown).min(text_w),
            KEY_H,
        );
        bottom = rect.y + rect.h;
        code = Some((shown, rect));
    }
    let mut hints = Vec::new();
    if id == RowId::Learn && learn_open {
        let lines = super::hints::hint_lines();
        if !lines.is_empty() {
            bottom += HINTS_GAP;
            for (chord, what) in lines {
                hints.push(HintLine {
                    chord,
                    what,
                    y: bottom,
                });
                bottom += HINT_LINE_H;
            }
        }
    }
    let content_h = (bottom - top).max(TICK);
    let trailing_h = match id {
        RowId::Files | RowId::Mcp => koru_ui::button::H,
        _ => KEY_H,
    };
    let h = (content_h.max(if trailing_w.is_some() {
        trailing_h
    } else {
        0.0
    }) + CardRow::PAD_Y * 2.0)
        .max(CardRow::MIN_H);
    let rect = Rect::new(at.x, at.y, at.w, h);
    let slot = |tw: f32, th: f32| CardRow::control_slot(rect, tw, th);
    let trailing = match (id, trailing_w) {
        (RowId::FirstDocument, Some(tw)) => super::hints::new_document_chord()
            .map_or(Trailing::None, |chord| {
                Trailing::Key(chord, slot(tw, KEY_H))
            }),
        (RowId::Files, Some(tw)) => Trailing::Button(
            super::FILES_BUTTON,
            Target::ChooseFiles,
            slot(tw, koru_ui::button::H),
        ),
        (RowId::Learn, Some(tw)) => Trailing::Caret(learn_open, slot(tw, KEY_H)),
        (RowId::Mcp, Some(tw)) => Trailing::Button(
            super::MCP_COPY,
            Target::CopyMcp,
            slot(tw, koru_ui::button::H),
        ),
        (RowId::TerminalAgent, Some(tw)) => Trailing::Code(super::AGENT_COMMAND, slot(tw, KEY_H)),
        _ => Trailing::None,
    };
    // The circle sits on the title's first line.
    let first_line = title_style().metrics.line_height;
    let tick = id.ticks().then(|| {
        Rect::new(
            at.x + CardRow::PAD_X,
            top + (first_line - TICK) * 0.5,
            TICK,
            TICK,
        )
    });
    RowLayout {
        id,
        rect,
        tick,
        title,
        title_rect,
        sentence: runs,
        trailing,
        chips,
        hints,
        code,
    }
}
