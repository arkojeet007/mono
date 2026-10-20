// SPDX-License-Identifier: MPL-2.0

//! The measurer, and the one shaping call it shares with the painter.

use std::cell::RefCell;

use koru_diagram::{MeasuredLine, TEXT_PX, TextMeasure, TextMetrics};
use parley::Layout;

use crate::text::TextEngine;
use crate::theme::Brush;

/// The family stack every diagram label is measured and drawn in: the
/// bundled Inter, then the symbol faces the mono stacks already fall back
/// to, so a `☕` in a label draws with whatever symbol face the OS has
/// rather than as a missing-glyph box. **Never the person's body font**:
/// the point of a fixed stack is that a label breaks on the same word on
/// every machine, and Inter's bytes are pinned by hash in
/// `scripts/setup/fonts.manifest` for exactly that reason. A symbol glyph
/// may differ by OS; the text Inter covers does not.
pub const STACK: &str = "Inter, Segoe UI Symbol, Apple Symbols, Noto Sans Symbols 2";

/// The weight every label is drawn at, whatever the diagram's text asked
/// for: the regular, because labels in a drawing read smaller than body text
/// and vello's lack of stem darkening matters less at a fixed 14 px than it
/// does across the type scale.
pub const WEIGHT: f32 = 400.0;

thread_local! {
    // One engine per thread, not per measurer: `TextEngine` holds a
    // `LayoutContext`, which is not `Sync`, while the measurer is shared as
    // an `Arc` across the host's worker threads. Cloning the process-wide
    // `FontContext` prototype is the cheap part (hundreds of nanoseconds);
    // the first engine on a process pays the font discovery once.
    static ENGINE: RefCell<TextEngine> = RefCell::new(TextEngine::new());
}

/// Runs `f` with this thread's engine.
pub(super) fn with_engine<R>(f: impl FnOnce(&mut TextEngine) -> R) -> R {
    ENGINE.with_borrow_mut(f)
}

/// The size the painter shapes at. The measurer is asked with it on every
/// call; the painter has no caller to be asked by, so it reads the constant.
pub(super) fn px() -> f32 {
    TEXT_PX
}

/// One line of `text` shaped exactly as the measurer measured it: same
/// stack, size and weight, no wrapping. The painter draws these; the
/// measurer's numbers come from the same call, which is what keeps a label
/// inside the box that was sized for it.
pub(super) fn shape(engine: &mut TextEngine, text: &str, px: f32) -> Layout<Brush> {
    engine.layout_in_stack(text, STACK, px, WEIGHT, None)
}

/// The host's measurer: parley on Inter at the size it is asked for, weight
/// [`WEIGHT`], in the stack [`STACK`].
///
/// Deterministic — the same text gives the same numbers on every call and
/// every thread — because nothing in it reads a setting, and because the
/// face is one pinned file. A unit struct: it owns nothing, and the engine
/// behind it is per thread.
#[derive(Clone, Copy, Debug, Default)]
pub struct InterMeasure;

impl TextMeasure for InterMeasure {
    fn measure(&self, text: &str, px: f32) -> TextMetrics {
        with_engine(|engine| {
            // An empty line still has a height — a label with no text is
            // laid out as one empty line by the engine — and parley gives a
            // layout of nothing no line at all, so the metrics are read from
            // a space and the width is reported as none.
            let layout = shape(engine, if text.is_empty() { " " } else { text }, px);
            let line = layout.get(0);
            let metrics = line.as_ref().map(|line| *line.metrics());
            TextMetrics {
                width: if text.is_empty() {
                    0.0
                } else {
                    layout.full_width()
                },
                height: metrics.map_or(px, |m| m.line_height),
                ascent: metrics.map_or(px, |m| m.baseline),
            }
        })
    }

    fn wrap(&self, text: &str, px: f32, max_width: f32) -> Vec<MeasuredLine> {
        with_engine(|engine| {
            let layout = engine.layout_in_stack(text, STACK, px, WEIGHT, Some(max_width.max(0.0)));
            layout
                .lines()
                .map(|line| {
                    let metrics = line.metrics();
                    MeasuredLine {
                        text: text[line.text_range()].trim_end().to_owned(),
                        width: metrics.advance - metrics.trailing_whitespace,
                    }
                })
                .collect()
        })
    }
}
