// SPDX-License-Identifier: MPL-2.0

//! Fonts and text geometry shared by every view.
//!
//! Laying a *row* out lives in `rowlayout`, which needs the engine here but
//! nothing else — the split is what lets a calendar or kanban reuse the font
//! stack and the pill cache without inheriting the outline's geometry.
//!
//! Everything here works in *logical* pixels. DPI is applied once, as a root
//! `Affine::scale` on every vello draw call in `render`, so no metric in this
//! crate ever needs to know the scale factor.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use parley::BoundingBox;
use parley::fontique::FontInfoOverride;
use parley::layout::{Alignment, AlignmentOptions};
use parley::setting::Tag;
use parley::style::{
    FontFamily, FontVariation, FontVariations, FontWeight, LineHeight, OverflowWrap, StyleProperty,
};
use parley::{FontContext, InlineBox, InlineBoxKind, Layout, LayoutContext, RangedBuilder};
use peniko::Blob;

use crate::pill::{PillCache, PillKind, PillTextStyle, PillVisual};
use crate::theme::{self, Brush};

pub struct TextEngine {
    pub font_cx: FontContext,
    pub layout_cx: LayoutContext<Brush>,
    pub pills: PillCache,
    /// Shaped attribute chips (tag chip, color swatch) — the same
    /// cache-per-view contract as `pills`, so drawing a frame never shapes
    /// chip text.
    pub attr_chips: crate::chip::ChipCache,
}

impl TextEngine {
    /// A engine for one view, cloned from the process-wide [`prototype`].
    ///
    /// **Never `FontContext::new()` here.** That call discovers the system's
    /// fonts, and there is a `TextEngine` per *view* — so it is paid on every
    /// tab open, not once. See [`prototype`] for the measurement.
    pub fn new() -> Self {
        Self {
            font_cx: prototype().clone(),
            layout_cx: LayoutContext::new(),
            pills: PillCache::default(),
            attr_chips: crate::chip::ChipCache::default(),
        }
    }

    /// Lay out a standalone string — used for UI chrome (overlay rows, the
    /// placeholder line). Cheap enough to call per frame for short strings.
    ///
    /// Takes a [`theme::TextStyle`] (a role: `role::hint()`, `role::row()`, …)
    /// so size, leading and weight arrive together and no call site passes a
    /// bare `f32` or a `weight_bold: bool`.
    pub fn simple_layout(&mut self, text: &str, style: theme::TextStyle) -> Layout<Brush> {
        self.simple_layout_in(text, style, theme::ui_stack())
    }

    /// Like [`simple_layout`](Self::simple_layout) but in an explicit family
    /// stack rather than the chrome's — so a font picker can render each
    /// family's name set in that family's own face.
    pub fn simple_layout_in(
        &mut self,
        text: &str,
        style: theme::TextStyle,
        stack: &str,
    ) -> Layout<Brush> {
        self.simple_layout_at_weight(text, style.metrics, stack, style.weight.value())
    }

    /// Like [`simple_layout_in`](Self::simple_layout_in) but at a **raw** wght
    /// position. A document row's weight is not a chrome role's: the heading
    /// ramp's 650s are no `FontWeight` variant, and the title's placeholder has
    /// to be set exactly the way the row it stands in for is set — family,
    /// size and weight — or the hint reads as smaller than the text it
    /// advertises.
    pub fn simple_layout_at_weight(
        &mut self,
        text: &str,
        metrics: theme::TextMetrics,
        stack: &str,
        weight: f32,
    ) -> Layout<Brush> {
        self.shaped(
            text,
            stack,
            Setting {
                size: metrics.size,
                line_height: LineHeight::Absolute(metrics.line_height),
                weight,
                wrap: None,
                brush: theme::p().text,
            },
        )
    }

    /// Lay `text` out in an explicit family `stack` at `size` px and a raw
    /// `weight`, at the font's own line height, wrapped at word boundaries
    /// when `max_width` is given.
    ///
    /// The one entry point that takes every input raw — no role, no chrome
    /// stack, no theme-chosen family — for text whose geometry has to come
    /// out the same on every machine: a diagram's labels are measured with
    /// this and then drawn from the very same call, so nothing in their
    /// width, height or line breaks depends on a setting. The line height is
    /// the face's own (`LineHeight::MetricsRelative(1.0)`: ascent, descent
    /// and line gap) rather than a type-scale step, which is what a diagram
    /// engine assumes when it stacks the lines of a label.
    ///
    /// **Word boundaries first.** `OverflowWrap::Anywhere` is CSS's
    /// `overflow-wrap: anywhere`: a line breaks at a word boundary whenever
    /// one fits, and inside a word only when a single word is wider than
    /// `max_width` — where the alternative is a line that overflows the box
    /// it was measured for.
    pub fn layout_in_stack(
        &mut self,
        text: &str,
        stack: &str,
        size: f32,
        weight: f32,
        max_width: Option<f32>,
    ) -> Layout<Brush> {
        self.shaped(
            text,
            stack,
            Setting {
                size,
                line_height: LineHeight::MetricsRelative(1.0),
                weight,
                wrap: max_width.map(|w| (w, OverflowWrap::Anywhere)),
                brush: theme::p().text,
            },
        )
    }

    /// The builder sequence every standalone layout in this file performs,
    /// written once: family stack, size, line height, the `wght` and `opsz`
    /// axes, one brush, an optional wrap, then line breaking and start
    /// alignment. The public entry points differ only in which of these
    /// they take from a role and which from the caller.
    fn shaped(&mut self, text: &str, stack: &str, setting: Setting) -> Layout<Brush> {
        let mut b = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, text, 1.0, true);
        b.push_default(StyleProperty::FontFamily(FontFamily::Source(
            Cow::Borrowed(stack),
        )));
        b.push_default(StyleProperty::FontSize(setting.size));
        b.push_default(StyleProperty::LineHeight(setting.line_height));
        if let Some((_, overflow)) = setting.wrap {
            b.push_default(StyleProperty::OverflowWrap(overflow));
        }
        push_axes(&mut b, setting.size, setting.weight);
        b.push_default(StyleProperty::Brush(Brush(setting.brush)));
        let mut layout = b.build(text);
        layout.break_all_lines(setting.wrap.map(|(max_width, _)| max_width));
        layout.align(Alignment::Start, AlignmentOptions::default());
        layout
    }

    // Used by the icon test and, next, by the document toolbar.

    /// The inline-box id of a placeholder's strut. It stands alone in its
    /// layout and never maps back to a document attribute, so any unique value
    /// does — but it must not collide with `rowlayout`'s own ids.
    const PLACEHOLDER_STRUT_BOX_ID: u64 = 0x5354_5255_5400_0001;

    /// Lay out a placeholder hint **as the row it stands in for would set the
    /// text**: that row's family, type step and weight, and the zero-width
    /// strut inline box `rowlayout` hangs on every row.
    ///
    /// The strut is the easy half to miss. parley gives a bare string the
    /// font's own ascent; a line carrying the strut gets the full line box as
    /// its ascent instead, so the row's text sits a couple of pixels lower
    /// than a strut-less hint — the hint floated above the glyphs the first
    /// keystroke produced, and the switch was visible.
    pub fn placeholder_layout(
        &mut self,
        text: &str,
        step: theme::TypeStep,
        stack: &str,
        weight: f32,
    ) -> Layout<Brush> {
        let size = step.size;
        let line_box = step.size * step.line_height;
        let mut b = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, text, 1.0, true);
        b.push_default(StyleProperty::FontFamily(FontFamily::Source(
            Cow::Borrowed(stack),
        )));
        b.push_default(StyleProperty::FontSize(size));
        b.push_default(StyleProperty::LineHeight(LineHeight::Absolute(line_box)));
        push_axes(&mut b, size, weight);
        b.push_default(StyleProperty::Brush(Brush(theme::p().text)));
        b.push_inline_box(InlineBox {
            id: Self::PLACEHOLDER_STRUT_BOX_ID,
            kind: InlineBoxKind::InFlow,
            index: 0,
            width: 0.0,
            height: line_box,
        });
        let mut layout = b.build(text);
        layout.break_all_lines(None);
        layout.align(Alignment::Start, AlignmentOptions::default());
        layout
    }
    #[allow(dead_code)]
    /// Lay out a single icon glyph.
    ///
    /// Its own family stack with no fallback: an icon codepoint sits in the
    /// Private Use Area, so falling through to a text face would produce
    /// whatever that face happens to have there rather than nothing. A missing
    /// glyph box is the honest answer when Phosphor is not installed.
    pub fn icon_layout(&mut self, icon: char, size: f32) -> Layout<Brush> {
        self.icon_layout_in(crate::icons::FAMILY, icon, size)
    }

    /// The same glyph in Phosphor's **Fill** weight: solid rather than stroked.
    ///
    /// For a state that is genuinely on, where the fill *is* the signal — the
    /// caption's pin while the window is above the others.
    ///
    /// **Falls back to the regular face when the fill file is not bundled.** A
    /// fresh clone that has not re-run `scripts/setup/fetch-fonts` has no such family,
    /// and the rule that a missing font is never an error means the honest
    /// answer there is an unfilled pin, not a missing-glyph box.
    pub fn icon_layout_filled(&mut self, icon: char, size: f32) -> Layout<Brush> {
        let family = if fill_is_bundled() {
            crate::icons::FAMILY_FILL
        } else {
            crate::icons::FAMILY
        };
        self.icon_layout_in(family, icon, size)
    }

    fn icon_layout_in(&mut self, family: &str, icon: char, size: f32) -> Layout<Brush> {
        let mut buf = [0u8; 4];
        let text: &str = icon.encode_utf8(&mut buf);
        let mut b = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, text, 1.0, true);
        b.push_default(StyleProperty::FontFamily(FontFamily::Source(Cow::Owned(
            family.to_owned(),
        ))));
        b.push_default(StyleProperty::FontSize(size));
        b.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(1.0)));
        b.push_default(StyleProperty::Brush(Brush(theme::p().text)));
        let mut layout = b.build(text);
        layout.break_all_lines(None);
        layout.align(Alignment::Start, AlignmentOptions::default());
        layout
    }

    /// Measure a short run in a document font. Used for whitespace whose
    /// advance the shaper does not provide itself, such as a four-space tab.
    pub fn measure_advance(
        &mut self,
        text: &str,
        stack: &'static str,
        size: f32,
        weight: f32,
    ) -> f32 {
        // A tab is measured from spaces, which are all trailing in this tiny
        // layout; `width()` deliberately excludes them.
        self.shaped(
            text,
            stack,
            Setting {
                size,
                line_height: LineHeight::FontSizeRelative(1.0),
                weight,
                wrap: None,
                brush: theme::p().text,
            },
        )
        .full_width()
    }

    /// Metrics for the caret at a cursor box: `(baseline, rise, descent)`.
    ///
    /// **Baseline** from the line. **Rise** prefers the active run's
    /// `cap_height`, else that run's ascent. **Descent** from the same run.
    /// Never strut line-box ascent/leading — that would tower over the letters.
    ///
    /// The active run is the one under the cursor (cluster at box mid); not the
    /// max of neighbouring glyphs on the line.
    pub fn line_ink(layout: &Layout<Brush>, bb: BoundingBox) -> (f32, f32, f32) {
        use parley::layout::Cluster;

        let mid_x = ((bb.x0 + bb.x1) * 0.5) as f32;
        let mid_y = ((bb.y0 + bb.y1) * 0.5) as f32;
        let line = layout.lines().find(|line| {
            let m = line.metrics();
            mid_y >= m.block_min_coord - 0.5 && mid_y <= m.block_max_coord + 0.5
        });
        let line = match line.or_else(|| layout.get(0)) {
            Some(l) => l,
            None => return (0.0, 0.0, 0.0),
        };
        let m = line.metrics();
        let baseline = m.baseline;

        // Prefer the run under the caret.
        if let Some((cluster, _)) = Cluster::from_point(layout, mid_x, mid_y) {
            let run = cluster.run();
            let rm = run.metrics();
            let rise = rm
                .cap_height
                .filter(|c| *c > 0.5)
                .unwrap_or(rm.ascent)
                .max(0.5);
            let descent = if rm.descent > 0.1 {
                rm.descent
            } else {
                (m.descent * 0.5).max(0.1)
            };
            return (baseline, rise, descent);
        }

        // Empty / no cluster: primary run on the line, then line fallbacks.
        if let Some(run) = line.runs().next() {
            let rm = run.metrics();
            let ascent = rm.ascent;
            let run_descent = rm.descent;
            let cap = rm.cap_height;
            let rise = cap.filter(|c| *c > 0.5).unwrap_or(if ascent > 0.5 {
                ascent
            } else {
                (m.ascent * 0.7).max(m.line_height * 0.55)
            });
            let descent = if run_descent > 0.1 {
                run_descent
            } else {
                (m.descent * 0.5).max(0.1)
            };
            return (baseline, rise, descent);
        }

        let rise = (m.ascent * 0.7).max(m.line_height * 0.55);
        let descent = (m.descent * 0.5).max(0.1);
        (baseline, rise, descent)
    }

    /// Baseline of the document font at a fixed line height. Used as a
    /// zero-width layout strut so a fallback-only run (notably colour emoji)
    /// cannot choose different vertical metrics from ordinary text.
    pub fn measure_baseline(
        &mut self,
        stack: &'static str,
        size: f32,
        weight: f32,
        line_height: f32,
    ) -> f32 {
        self.shaped(
            "M",
            stack,
            Setting {
                size,
                line_height: LineHeight::Absolute(line_height),
                weight,
                wrap: None,
                brush: theme::p().text,
            },
        )
        .get(0)
        .map(|line| line.metrics().baseline)
        .unwrap_or(line_height)
    }

    /// Measure a pill, caching the result. Returns `(width, ascent)` for the
    /// inline layout box; the cached visual retains the full painted height.
    /// Pill labels bake a brush in at measure time, so a theme change has to
    /// drop them all.
    pub fn clear_pill_cache(&mut self) {
        self.pills.clear();
        // Chip labels bake their ink at measure time exactly the way pill
        // labels do, so a theme change has to drop both together — one call,
        // because the site that remembers one and forgets the other draws
        // last theme's ink on this theme's fill.
        self.attr_chips.clear();
    }

    /// Measure an attribute chip, caching the shaped visual. Returns its
    /// width; [`crate::chip::CHIP_H`] is every chip's height.
    ///
    /// `accent` is the `AccentColor` storage token behind the chip's tint, or
    /// `None` for the quiet default — part of the key because the label's ink
    /// is baked here.
    pub fn measure_attr_chip(
        &mut self,
        kind: crate::chip::ChipKind,
        label: &str,
        accent: Option<&str>,
        lead: Option<char>,
        struck: bool,
    ) -> f32 {
        use crate::chip::{
            CHIP_ICON_GAP, CHIP_PAD_X, ChipCache, ChipKind, ChipVisual, SWATCH_D, chip_colors,
        };
        let key = ChipCache::key(kind, label, accent, lead);
        if let Some(v) = self.attr_chips.0.get(&key) {
            return v.width;
        }
        // Shaped here beside the label so a frame that has drawn this chip
        // lays out nothing, and **measured into the width**: the glyph adds a
        // lane, and `chip_boxes` hit-tests the rectangle it measured.
        let lead_layout =
            lead.map(|glyph| self.icon_layout(glyph, theme::role::meta().metrics.size));
        let lead_w = lead_layout
            .as_ref()
            .map(|l| l.width() + CHIP_ICON_GAP)
            .unwrap_or(0.0);
        let visual = match kind {
            // **The swatch takes neither.** It has no label to strike and no
            // text lane to put a glyph in; a dot with an icon beside it would
            // be two chips wearing one lozenge.
            ChipKind::Swatch => ChipVisual {
                kind,
                width: SWATCH_D,
                label: None,
                lead: None,
                struck: false,
            },
            ChipKind::Tag | ChipKind::Due | ChipKind::Priority => {
                let (_, ink) = chip_colors(accent);
                let style = theme::role::meta();
                // `simple_layout` bakes the theme's text color; a chip label
                // wants its tone's ink.
                let label_layout = self.shaped(
                    label,
                    theme::ui_stack(),
                    Setting {
                        size: style.metrics.size,
                        line_height: LineHeight::Absolute(style.metrics.line_height),
                        weight: style.weight.value(),
                        wrap: None,
                        brush: ink,
                    },
                );
                ChipVisual {
                    kind,
                    // **The strike is paint, not width.** A struck chip is
                    // exactly as wide as an unstruck one with the same label:
                    // the line is drawn across an advance that is already
                    // there.
                    width: label_layout.width() + CHIP_PAD_X * 2.0 + lead_w,
                    label: Some(label_layout),
                    lead: lead_layout,
                    struck,
                }
            }
        };
        let width = visual.width;
        self.attr_chips.0.insert(key, visual);
        width
    }

    pub fn measure_pill(&mut self, kind: PillKind, label: &str, url: Option<&str>) -> (f32, f32) {
        self.measure_pill_with_style(kind, label, url, PillTextStyle::body())
    }

    pub fn measure_pill_with_style(
        &mut self,
        kind: PillKind,
        label: &str,
        url: Option<&str>,
        style: PillTextStyle,
    ) -> (f32, f32) {
        let key = PillCache::key(kind, label, url, style);
        if let Some(v) = self.pills.0.get(&key) {
            return (v.width, v.inline_ascent);
        }

        let fg = Brush(kind.palette().fg);
        let horizontal_padding = match kind {
            // No inset for the pills that sit inside a sentence: the space
            // around them belongs to the document's own text.
            PillKind::Document
            | PillKind::Link
            | PillKind::Run
            | PillKind::Schedule
            | PillKind::SchedulePaused => 0.0,
            PillKind::File | PillKind::Contact | PillKind::Date => theme::PILL_PAD_X,
        };
        // Document titles: peel a leading emoji into the icon slot (Explore/tabs
        // rule) so spacing matches a kind glyph and the emoji is not drawn twice.
        let (page_emoji, display_label) = if kind == PillKind::Document {
            let (e, rest) = crate::emoji::split_leading_emoji(label);
            (e, if e.is_some() { rest } else { label })
        } else {
            (None, label)
        };
        let label_layout = if kind == PillKind::Link {
            // A URL is an opaque token an inline box cannot wrap, so cap it: past
            // the cap, drop the scheme and path and show the host (with an
            // ellipsis if even the host is too long). The full URL stays on
            // hover/click — this is presentation, not the stored value.
            let icon_guess = style.size;
            let max_w = theme::LINK_PILL_MAX_W
                - horizontal_padding * 2.0
                - icon_guess
                - theme::PILL_ICON_GAP;
            // A data-URI or other enormous token (a base64 image left behind as
            // a link) costs seconds to shape before the truncation below can even
            // run. Anything this long cannot possibly fit, so it is never shaped
            // in full — the full value is still what hover and click see.
            //
            // **The host is still preferred.** A very long *URL* is exactly the
            // case the truncation below was written for, so it takes the same
            // path a merely-too-wide one does; only a token with no host at all
            // (a `data:` URI has no `://`) falls back to a shaped prefix, which
            // is a string nobody can read but is honest about there being one.
            // Cutting on `chars` rather than bytes because `len` is only the
            // trigger — a byte cut can land inside a character.
            const SHAPE_CAP: usize = 512;
            if label.len() > SHAPE_CAP {
                let display = host_of(label).unwrap_or_else(|| {
                    let mut short: String = label.chars().take(SHAPE_CAP).collect();
                    short.push('…');
                    short
                });
                self.fitted_pill_label(&display, fg, max_w, style)
            } else {
                let full = self.pill_label(label, fg, style);
                if full.width() <= max_w {
                    full
                } else {
                    let display = host_of(label).unwrap_or_else(|| label.to_owned());
                    self.fitted_pill_label(&display, fg, max_w, style)
                }
            }
        } else {
            // Empty remainder (emoji-only title) still needs a layout for metrics.
            self.pill_label(
                if display_label.is_empty() {
                    "\u{200B}"
                } else {
                    display_label
                },
                fg,
                style,
            )
        };

        let (label_baseline, label_descent) = label_layout
            .get(0)
            .map(|l| {
                let m = l.metrics();
                (m.baseline, m.descent.max(0.0))
            })
            .unwrap_or_default();
        // Icon fills the capital band (baseline ←→ top of M).
        let icon_slot = cap_height_of(&label_layout);
        let icon = if let Some(emoji) = page_emoji {
            Some(self.emoji_layout(emoji, icon_slot))
        } else if kind == PillKind::Link {
            Some(self.icon_layout(crate::icons::for_scheme(url), icon_slot.max(1.0)))
        } else if kind == PillKind::File {
            Some(self.icon_layout(crate::icons::FILE, icon_slot.max(1.0)))
        } else if kind == PillKind::Run {
            // An agent is doing this somewhere. One glyph for every state —
            // the caption is what changes as the run runs.
            Some(self.icon_layout(crate::icons::ROBOT, icon_slot.max(1.0)))
        } else if kind == PillKind::Schedule {
            Some(self.icon_layout(crate::icons::CLOCK_COUNTDOWN, icon_slot.max(1.0)))
        } else if kind == PillKind::SchedulePaused {
            Some(self.icon_layout(crate::icons::PAUSE, icon_slot.max(1.0)))
        } else {
            None
        };
        // Hit box: ascent line + pad above, descent line + pad below.
        let gap = if icon_slot > 0.0 {
            theme::PILL_ICON_GAP
        } else {
            0.0
        };
        let visual = PillVisual {
            width: horizontal_padding * 2.0 + icon_slot + gap + label_layout.width(),
            height: theme::PILL_PAD_Y * 2.0 + label_baseline + label_descent,
            horizontal_padding,
            inline_ascent: theme::PILL_PAD_Y + label_baseline,
            label_baseline,
            label_descent,
            icon_slot,
            label: label_layout,
            icon,
        };
        let size = (visual.width, visual.inline_ascent);
        self.pills.0.insert(key, visual);
        size
    }

    /// A color emoji sized into a square icon `slot` — a page emoji in a
    /// pill's icon band, a callout's icon in its box.
    pub fn emoji_layout(&mut self, emoji: &str, slot: f32) -> Layout<Brush> {
        // Colour emoji fill their em box more than outline icons — a touch
        // under the slot keeps them from reading larger than a document mark.
        let size = (slot * 0.95).max(8.0);
        let style = theme::TextStyle::new(
            theme::TextMetrics::new(size, size * 1.25),
            theme::FontWeight::Medium,
        );
        self.simple_layout(emoji, style)
    }

    /// Pill label: document body size, semibold — same face as the paragraph.
    fn pill_label(&mut self, text: &str, fg: Brush, style: PillTextStyle) -> Layout<Brush> {
        self.shaped(
            text,
            theme::body_stack(),
            Setting {
                size: style.size,
                line_height: LineHeight::Absolute(style.line_height),
                weight: theme::FontWeight::SemiBold.value(),
                wrap: None,
                brush: fg.0,
            },
        )
    }

    /// A pill label truncated with `…` to fit `max_w`, keeping the full label
    /// when it already fits. Oversized labels are rare (a very long host), so a
    /// binary search over the prefix length is plenty.
    fn fitted_pill_label(
        &mut self,
        text: &str,
        fg: Brush,
        max_w: f32,
        style: PillTextStyle,
    ) -> Layout<Brush> {
        let full = self.pill_label(text, fg, style);
        if full.width() <= max_w || text.is_empty() {
            return full;
        }
        let chars: Vec<char> = text.chars().collect();
        let mut lo = 1;
        let mut hi = chars.len();
        let mut best = 1;
        while lo <= hi {
            let mid = (lo + hi) / 2;
            let mut s: String = chars[..mid].iter().collect();
            s.push('…');
            if self.pill_label(&s, fg, style).width() <= max_w {
                best = mid;
                lo = mid + 1;
            } else {
                hi = mid - 1;
            }
        }
        let mut s: String = chars[..best].iter().collect();
        s.push('…');
        self.pill_label(&s, fg, style)
    }

    /// A standalone string laid out wrapping at `max_w` — for chrome that may
    /// be long, like a link tooltip's URL or a toast path with no spaces.
    ///
    /// **`OverflowWrap::Anywhere`**, so a continuous token (path, URL) still
    /// breaks at the width rather than overflowing the box.
    pub fn simple_layout_wrapped(
        &mut self,
        text: &str,
        style: theme::TextStyle,
        max_w: f32,
    ) -> Layout<Brush> {
        self.simple_layout_wrapped_in(text, style, theme::ui_stack(), max_w)
    }

    /// Wrapped chrome text using an explicit font stack, including diagnostic numbers.
    pub fn simple_layout_wrapped_in(
        &mut self,
        text: &str,
        style: theme::TextStyle,
        stack: &str,
        max_w: f32,
    ) -> Layout<Brush> {
        self.shaped(
            text,
            stack,
            Setting {
                size: style.metrics.size,
                line_height: LineHeight::Absolute(style.metrics.line_height),
                weight: style.weight.value(),
                wrap: Some((max_w, OverflowWrap::Anywhere)),
                brush: theme::p().text,
            },
        )
    }

    /// Like [`simple_layout_wrapped`](Self::simple_layout_wrapped) with
    /// per-range ink and strikethrough on top — what a surface needs to mark
    /// words inside a wrapped paragraph (a diff's insertions and deletions, a
    /// linted phrase) without owning parley's style vocabulary itself.
    ///
    /// `base` recolours the whole string before the ranges apply; `None` keeps
    /// the theme's text colour. `weight` is a raw wght position, for the same
    /// reason [`simple_layout_at_weight`](Self::simple_layout_at_weight) takes
    /// one.
    pub fn styled_layout(
        &mut self,
        text: &str,
        metrics: theme::TextMetrics,
        weight: f32,
        max_w: f32,
        base: Option<peniko::Color>,
        ranges: &[RangeStyle],
    ) -> Layout<Brush> {
        let size = metrics.size;
        let mut b = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, text, 1.0, true);
        b.push_default(StyleProperty::FontFamily(FontFamily::Source(
            Cow::Borrowed(theme::ui_stack()),
        )));
        b.push_default(StyleProperty::FontSize(size));
        b.push_default(StyleProperty::LineHeight(LineHeight::Absolute(
            metrics.line_height,
        )));
        b.push_default(StyleProperty::OverflowWrap(OverflowWrap::Anywhere));
        push_axes(&mut b, size, weight);
        b.push_default(StyleProperty::Brush(Brush(base.unwrap_or(theme::p().text))));
        for styled in ranges {
            if styled.range.is_empty() {
                continue;
            }
            if let Some(color) = styled.color {
                b.push(StyleProperty::Brush(Brush(color)), styled.range.clone());
            }
            if styled.strikethrough {
                b.push(StyleProperty::Strikethrough(true), styled.range.clone());
            }
        }
        let mut layout = b.build(text);
        layout.break_all_lines(Some(max_w));
        layout.align(Alignment::Start, AlignmentOptions::default());
        layout
    }

    /// `text`, cut with an ellipsis if it will not fit in `max_w`.
    ///
    /// **What wrapping cannot do.** Laying a label out wrapped and drawing one
    /// line deep breaks at a *word boundary* for free, which is why chrome does
    /// that — but a name with no boundary in it (`Screenshot_20260728-2310…`,
    /// a URL, a German compound) has nowhere to break, so the line simply
    /// overflows and the label runs out of its column. Everything reads as
    /// `Screenshot_2026072` with no sign that there was more.
    ///
    /// **Measured, not counted.** Cutting at a character count is wrong at any
    /// proportional size — `WWW` and `iii` are not the same width — so this
    /// measures, estimates a cut from the overshoot, and corrects. Bounded at
    /// [`ELIDE_TRIES`] layouts, and only for a label that actually overflows: a
    /// row that fits costs the one layout it already cost.
    ///
    /// Cuts on a **character** boundary, never a byte, so a multi-byte glyph is
    /// never sliced in half.
    pub fn elided(&mut self, text: &str, style: theme::TextStyle, max_w: f32) -> String {
        self.elided_in(text, style, theme::ui_stack(), max_w)
    }

    /// Measure and elide in the same explicit stack used to paint the result.
    pub fn elided_in(
        &mut self,
        text: &str,
        style: theme::TextStyle,
        stack: &str,
        max_w: f32,
    ) -> String {
        if max_w <= 0.0 {
            return String::new();
        }
        let full = self.simple_layout_in(text, style, stack).width();
        if full <= max_w {
            return text.to_string();
        }
        let chars: Vec<char> = text.chars().collect();
        // The proportion that fits, less room for the ellipsis. A first guess
        // from the ratio lands within a character or two on almost any string,
        // and the loop below only has to walk the remainder.
        let room = (max_w / full).clamp(0.0, 1.0);
        let mut keep = ((chars.len() as f32 * room).floor() as usize).min(chars.len());
        for _ in 0..ELIDE_TRIES {
            if keep == 0 {
                return ELLIPSIS.to_string();
            }
            let candidate: String = chars[..keep].iter().collect::<String>() + ELLIPSIS;
            if self.simple_layout_in(&candidate, style, stack).width() <= max_w {
                return candidate;
            }
            // Shrink by the overshoot rather than by one: a wildly wrong guess
            // converges in the same handful of steps as a nearly-right one.
            let over = 1.max(keep / 8);
            keep = keep.saturating_sub(over);
        }
        chars[..keep].iter().collect::<String>() + ELLIPSIS
    }
}

/// What a standalone layout is set in, beyond its text and family stack:
/// the inputs of [`TextEngine::shaped`] that the public entry points take
/// from a role, a step or the caller.
struct Setting {
    size: f32,
    line_height: LineHeight,
    weight: f32,
    /// The width to break lines at and how a too-long word breaks; `None`
    /// lays the text out as one line.
    wrap: Option<(f32, OverflowWrap)>,
    brush: peniko::Color,
}

/// One character, not three dots: it kerns, and it is what every other list in
/// the app would use.
const ELLIPSIS: &str = "…";

/// How many corrections [`TextEngine::elided`] will pay for. Each is one parley
/// layout of a short string, and only an overflowing label gets here at all —
/// but this is per row per frame in the worst case, so it is bounded rather
/// than looped to convergence.
const ELIDE_TRIES: usize = 6;

/// The recognisable part of a URL for display: the host (or mailto address),
/// dropping scheme, path and query. `None` when `label` is not a URL — a
/// hand-written caption like "Terms page" — so those render verbatim.
fn host_of(label: &str) -> Option<String> {
    let t = label.trim();
    if let Some(rest) = t.strip_prefix("mailto:") {
        let addr = rest.split('?').next().unwrap_or(rest);
        return (!addr.is_empty()).then(|| addr.to_owned());
    }
    let rest = t.split_once("://")?.1;
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    (!host.is_empty()).then(|| host.to_owned())
}

/// Cap height of the first run, else ~0.72× ascent — the capital-M band icons use.
fn cap_height_of(layout: &Layout<Brush>) -> f32 {
    layout
        .lines()
        .next()
        .and_then(|line| line.runs().next())
        .map(|run| {
            let m = run.metrics();
            m.cap_height
                .filter(|c| *c > 0.5)
                .unwrap_or(m.ascent * 0.72)
                .max(1.0)
        })
        .unwrap_or_else(|| theme::role::body().metrics.size * 0.72)
}

/// The two axis settings every text layout in the app wants: how heavy, and at
/// what optical size.
///
/// **Weight**, because vello does no gamma correction or stem darkening — every
/// face renders lighter here than the same face does in a native control, and a
/// real Medium is the way back. On the bundled variable families fontique turns
/// this into a genuine `wght` axis position; on a static family it would
/// instead switch on faux-bold stroking, which is why the number comes from
/// [`FontChoice::weight`](theme::FontChoice::weight) rather than being a
/// constant.
///
/// **Optical size**, because fontique's synthesis only ever touches
/// `wght`/`wdth`/`ital` — nothing sets `opsz`, so a face that has the axis
/// would draw 30px display text with the letterfit meant for 14px. Setting it
/// to the font size is what CSS calls automatic optical sizing. A face without
/// the axis ignores it and out-of-range values are clamped by the shaper, so
/// this is safe for every family, bundled or not.
/// One styled byte range in a [`TextEngine::styled_layout`] build.
#[derive(Clone, Debug)]
pub struct RangeStyle {
    pub range: std::ops::Range<usize>,
    /// Ink for the range; `None` keeps the layout's base colour.
    pub color: Option<peniko::Color>,
    pub strikethrough: bool,
}

pub fn push_axes(b: &mut RangedBuilder<'_, Brush>, size: f32, weight: f32) {
    b.push_default(StyleProperty::FontWeight(FontWeight::new(weight)));
    b.push_default(StyleProperty::FontVariations(FontVariations::List(
        Cow::Owned(vec![FontVariation::new(Tag::new(b"opsz"), size)]),
    )));
}

/// Every bundled face, read from disk **once per process**.
///
/// This is a cache because [`TextEngine::new`] is not called once — there is an
/// engine per *view*: one per open tab, one for the container's chrome, one per
/// welcome tab. Reading the files per engine meant opening a tab cost a fresh
/// ~15 MB of `std::fs::read` and its own copy of every byte, which is the sort
/// of thing the performance rules in `AGENTS.md` exist to catch.
///
/// `Blob` is `Arc`-backed, so handing each `FontContext` a clone shares the
/// bytes instead of duplicating them, and every engine after the first touches
/// no disk at all.
static BUNDLED: OnceLock<Vec<(Blob<u8>, &'static str)>> = OnceLock::new();

fn bundled() -> &'static [(Blob<u8>, &'static str)] {
    BUNDLED.get_or_init(|| {
        let mut out = Vec::new();
        let mut load = |file: &'static str, family: &'static str| {
            let Some(bytes) = read_asset(file) else {
                // Not fatal by design: the font stacks fall through to the OS,
                // so text still renders, just not identically across platforms.
                eprintln!(
                    "note: {file} not found; run scripts/setup/fetch-fonts.sh (or .ps1) \
                     to bundle it. Falling back to system fonts for {family}"
                );
                return;
            };
            let blob = Blob::new(Arc::new(bytes) as Arc<dyn AsRef<[u8]> + Send + Sync>);
            out.push((blob, family));
        };

        // Each family registers all of its files (a variable pair, a static
        // instance set, or a lone variable roman) under one family name with
        // `style: None`. That is what lets the italic member join its family
        // for Ctrl+I — and for a `RomanOnly` family (Manrope), it is what
        // leaves fontique no italic to find, so Ctrl+I faux-skews by design.
        for family in theme::BUNDLED_FAMILIES {
            match *family {
                theme::BundledFamily::Variable {
                    roman,
                    italic,
                    family,
                } => {
                    load(roman, family);
                    load(italic, family);
                }
                theme::BundledFamily::Static { files, family } => {
                    for file in files {
                        load(file, family);
                    }
                }
                theme::BundledFamily::RomanOnly { roman, family } => {
                    load(roman, family);
                }
            }
        }
        load(theme::NOTO_KR_FILE, theme::NOTO_KR_FAMILY);
        // Icons are glyphs, so they register exactly like a text face and
        // parley shapes them with everything else.
        load(theme::PHOSPHOR_FILE, crate::icons::FAMILY);
        // Its Fill weight, under its own family — same codepoints, solid
        // shapes. Optional in practice: a tree that has not re-run
        // `fetch-fonts` simply has no such family, and `icon_layout_filled`
        // notices and draws the regular face.
        load(theme::PHOSPHOR_FILL_FILE, crate::icons::FAMILY_FILL);
        out
    })
}

/// Whether Phosphor's Fill face was found beside the exe.
///
/// Read off the same one-shot cache the registration uses, so it costs a slice
/// scan and never a second file read.
fn fill_is_bundled() -> bool {
    bundled()
        .iter()
        .any(|(_, family)| *family == crate::icons::FAMILY_FILL)
}

/// Register the bundled fonts under stable family names, so the stacks in
/// [`FontChoice::stack`](theme::FontChoice::stack) resolve the same way on
/// Windows, macOS, and Linux.
///
/// Most text faces are variable: parley drives their `wght` axis through
/// `Run::normalized_coords`, so one file covers every weight. Poppins is the
/// exception — it ships a static instance per weight, so fontique matches the
/// nearest weight from the registered set. See `theme::BundledFamily` for the
/// three delivery shapes (Variable / Static / RomanOnly).
/// A `FontContext` with every bundled face already registered, built **once per
/// process** and cloned into each [`TextEngine`].
///
/// ## Why this exists — measured, because the numbers are not guessable
///
/// `FontContext::new()` "discovers system fonts", which on macOS means
/// enumerating the Core Text collection. On this machine, debug build:
///
/// | | |
/// | --- | --- |
/// | `FontContext::new()` | **30–258 ms** |
/// | `register_bundled_fonts` | 44 µs |
/// | `FontContext::clone()` | **375 ns** (23 µs for the first) |
///
/// There is a `TextEngine` per view — one per open tab, one for the container's
/// chrome, one per welcome tab — so before this, **opening a tab paid a fresh
/// system-font enumeration**. `apps/koru/src/perf.rs` measured 48 ms per tab on
/// macOS against 384 µs on Windows, and the same test's own comment had already
/// recorded the Windows figure without anyone being able to see the gap. That is
/// squarely what the performance rules in `AGENTS.md` call a bug rather than a
/// trade: switching tabs has to feel instant.
///
/// The blob cache below ([`bundled`]) was doing its job perfectly — 44 µs — and
/// was not the problem. The cost sat one level up, in the call the cache could
/// not see.
///
/// **Cloning shares the discovery.** parley's own documentation calls
/// `FontContext` "a global resource with only one per-application", which is the
/// shape this restores without giving up the per-view engine that
/// `AGENTS.md` describes: each view still owns its `LayoutContext` and its pill
/// cache, and only the font database is shared.
static PROTOTYPE: OnceLock<FontContext> = OnceLock::new();

fn prototype() -> &'static FontContext {
    PROTOTYPE.get_or_init(|| {
        let mut font_cx = FontContext::new();
        register_bundled_fonts(&mut font_cx);
        font_cx
    })
}

fn register_bundled_fonts(font_cx: &mut FontContext) {
    for (blob, family) in bundled() {
        let registered = font_cx.collection.register_fonts(
            blob.clone(),
            Some(FontInfoOverride {
                family_name: Some(family),
                width: None,
                // Left to the face. This is what lets the italic file join its
                // family as the italic member rather than overwriting the roman.
                style: None,
                weight: None,
                axes: None,
            }),
        );
        if registered.is_empty() {
            eprintln!("warning: a bundled face for {family} could not be parsed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **What wrapping could not do.** A screenshot's name has no word boundary
    /// in it, so laying it out wrapped and drawing one line deep left it
    /// overflowing its column with nothing to say it had been cut.
    #[test]
    fn a_name_with_nowhere_to_break_is_cut_with_an_ellipsis() {
        let mut e = TextEngine::new();
        let long = "Screenshot_20260728-231045_Chrome_and_then_some_more";
        let full = e.simple_layout(long, theme::role::row()).width();
        let room = full * 0.4;

        let cut = e.elided(long, theme::role::row(), room);
        assert!(cut.ends_with('…'), "{cut:?}");
        assert!(cut.len() < long.len());
        assert!(
            e.simple_layout(&cut, theme::role::row()).width() <= room,
            "and it actually fits: {cut:?}"
        );
    }

    /// A label that fits is returned untouched — no ellipsis, and no second
    /// layout paid for it.
    #[test]
    fn a_name_that_fits_is_left_alone() {
        let mut e = TextEngine::new();
        let name = "Brief";
        let room = e.simple_layout(name, theme::role::row()).width() + 10.0;
        assert_eq!(e.elided(name, theme::role::row(), room), name);
    }

    /// Measured rather than counted: `WWW` and `iii` are not the same width, so
    /// a character count would cut one of them in the wrong place.
    #[test]
    fn the_cut_is_by_width_and_not_by_character_count() {
        let mut e = TextEngine::new();
        let room = 60.0;
        let wide = e.elided(&"W".repeat(40), theme::role::row(), room);
        let narrow = e.elided(&"i".repeat(40), theme::role::row(), room);
        assert!(
            narrow.chars().count() > wide.chars().count(),
            "more narrow glyphs fit in the same room: {narrow:?} vs {wide:?}"
        );
    }

    /// Cuts on a character boundary, never a byte — slicing a multi-byte glyph
    /// in half would panic.
    #[test]
    fn a_multi_byte_name_is_never_sliced_in_half() {
        let mut e = TextEngine::new();
        for room in [1.0, 5.0, 20.0, 60.0] {
            let cut = e.elided("한국어문서제목입니다아주깁니다", theme::role::row(), room);
            assert!(cut.is_char_boundary(cut.len()));
        }
    }

    /// No room at all is no text, rather than an ellipsis floating in a column
    /// that has been squeezed shut.
    #[test]
    fn no_room_is_no_label() {
        let mut e = TextEngine::new();
        assert_eq!(e.elided("Brief", theme::role::row(), 0.0), "");
    }

    /// The icon font has to actually register and actually resolve, or every
    /// toolbar button silently becomes a missing-glyph box.
    ///
    /// Skipped when `assets/fonts/Phosphor.ttf` is absent — a missing font is
    /// never a build error, and a fresh clone that has not run `fonts` yet
    /// should not fail its test suite over it.
    #[test]
    fn an_icon_glyph_resolves_to_the_phosphor_family() {
        if read_asset(theme::PHOSPHOR_FILE).is_none() {
            eprintln!("skipping: run ./koru.sh fonts to exercise this");
            return;
        }
        let mut engine = TextEngine::new();
        let layout = engine.icon_layout(crate::icons::BOLD, theme::ICON_SIZE);

        assert_eq!(layout.len(), 1, "one line");
        assert!(layout.width() > 0.0, "the glyph has advance");

        // A codepoint in the Private Use Area resolving to a *text* face would
        // mean Phosphor never registered and something else claimed it.
        let family = layout
            .lines()
            .flat_map(|l| l.items())
            .find_map(|item| match item {
                parley::layout::PositionedLayoutItem::GlyphRun(gr) => Some(gr.run().font().clone()),
                _ => None,
            });
        assert!(family.is_some(), "the icon produced a glyph run");
    }

    /// **An icon codepoint asked of a text role is not the same glyph**, and
    /// the difference is a missing-glyph box on screen.
    ///
    /// The run views' Good / Not good chips shipped as tofu because their
    /// Phosphor codepoint went through [`TextEngine::simple_layout`] in the
    /// row role: the Private Use Area is empty in every text face, so parley
    /// shapes `.notdef`. The two calls therefore have to resolve to two
    /// different fonts, and this says so once for every surface that draws a
    /// mark beside a word.
    ///
    /// Skipped when the font is absent, like the two tests around it.
    #[test]
    fn a_text_role_does_not_reach_the_icon_family() {
        if read_asset(theme::PHOSPHOR_FILE).is_none() {
            eprintln!("skipping: run ./koru.sh fonts to exercise this");
            return;
        }
        let font_of = |layout: &Layout<Brush>| {
            layout
                .lines()
                .flat_map(|line| line.items())
                .find_map(|item| match item {
                    parley::layout::PositionedLayoutItem::GlyphRun(run) => {
                        Some(run.run().font().clone())
                    }
                    _ => None,
                })
        };
        let mut engine = TextEngine::new();
        let icon = engine.icon_layout(crate::icons::THUMBS_UP, 12.0);
        let icon_font = font_of(&icon).expect("the icon produced a glyph run");
        let mut buf = [0u8; 4];
        let as_text = engine.simple_layout(
            crate::icons::THUMBS_UP.encode_utf8(&mut buf),
            theme::role::row(),
        );
        let text_font = font_of(&as_text).expect("the text role produced a glyph run too");
        assert_ne!(
            icon_font, text_font,
            "the icon family is the only place a Phosphor codepoint has a glyph"
        );
    }

    /// A link pill carries a scheme icon — a Phosphor glyph chosen by the URL —
    /// while other pill kinds have none (they still draw vectors). Two links
    /// with the same caption but different schemes are distinct cache entries,
    /// so neither steals the other's icon.
    ///
    /// Skipped when the font is absent, like the test above.
    #[test]
    fn a_link_pill_carries_the_scheme_icon() {
        if read_asset(theme::PHOSPHOR_FILE).is_none() {
            eprintln!("skipping: run ./koru.sh fonts to exercise this");
            return;
        }
        let mut engine = TextEngine::new();
        // Measure first (mutable), then read back (immutable) — the two cannot
        // overlap, since each borrow of the cache lives until the assertions.
        let _ = engine.measure_pill(PillKind::Link, "example.com", Some("https://example.com"));
        let _ = engine.measure_pill(PillKind::Link, "example.com", Some("mailto:a@b.com"));
        let _ = engine.measure_pill(PillKind::Date, "Aug 2", None);

        let https = engine
            .pills
            .get(PillKind::Link, "example.com", Some("https://example.com"))
            .expect("cached");
        let https_icon = https.icon.as_ref().expect("a link has a scheme glyph");
        assert!(https_icon.width() > 0.0, "the glyph has advance");

        // Same caption, different scheme -> a distinct entry with its own icon.
        let mailto = engine
            .pills
            .get(PillKind::Link, "example.com", Some("mailto:a@b.com"))
            .expect("cached");
        let mailto_icon = mailto.icon.as_ref().expect("mailto link has a glyph");
        assert!(
            !std::ptr::eq(https_icon, mailto_icon),
            "different schemes are different visuals"
        );

        // A non-link pill has no glyph icon; it keeps its vector icon.
        let date = engine
            .pills
            .get(PillKind::Date, "Aug 2", None)
            .expect("cached");
        assert!(date.icon.is_none(), "non-link pills keep their vector icon");
    }

    /// Leading emoji is peeled into the icon slot and off the label — same
    /// spacing as a document mark (Explore/tabs rule).
    #[test]
    fn a_document_pill_uses_leading_emoji_as_its_icon() {
        let mut engine = TextEngine::new();
        let _ = engine.measure_pill(PillKind::Document, "🚀 Ship it", None);
        let (emoji_slot, emoji_label_w, has_icon) = {
            let with = engine
                .pills
                .get(PillKind::Document, "🚀 Ship it", None)
                .expect("cached");
            (with.icon_slot, with.label.width(), with.icon.is_some())
        };
        assert!(emoji_slot > 1.0, "emoji occupies the icon slot");
        assert!(has_icon, "emoji is laid out as the icon");
        let _ = engine.measure_pill(PillKind::Document, "Ship it", None);
        let plain = engine
            .pills
            .get(PillKind::Document, "Ship it", None)
            .expect("cached");
        assert!(
            (emoji_label_w - plain.label.width()).abs() < 2.0,
            "peeled label matches a plain title of the remainder"
        );
        assert!(
            (emoji_slot - plain.icon_slot).abs() < 0.5,
            "emoji slot matches document-mark slot"
        );
    }

    /// Pad is 2px above the ascent line and 2px below the descent line.
    #[test]
    fn a_pill_pads_ascent_and_descent_by_pill_pad_y() {
        let mut engine = TextEngine::new();
        let _ = engine.measure_pill(PillKind::Document, "Catalog", None);
        let v = engine
            .pills
            .get(PillKind::Document, "Catalog", None)
            .expect("cached");
        assert!((v.inline_ascent - (theme::PILL_PAD_Y + v.label_baseline)).abs() < 0.01);
        assert!(
            (v.height - (theme::PILL_PAD_Y * 2.0 + v.label_baseline + v.label_descent)).abs()
                < 0.01,
            "h={} baseline={} descent={}",
            v.height,
            v.label_baseline,
            v.label_descent
        );
        assert!(v.label_descent > 0.5, "Catalog has a descender");
        // Below baseline: descent + pad, not pad alone.
        let below = v.height - v.inline_ascent;
        assert!(
            (below - (v.label_descent + theme::PILL_PAD_Y)).abs() < 0.01,
            "below baseline should be descent+pad, got {below}"
        );
    }

    #[test]
    fn document_and_link_pills_have_no_horizontal_padding() {
        let mut engine = TextEngine::new();
        for (kind, url) in [
            (PillKind::Document, None),
            (PillKind::Link, Some("https://example.com")),
        ] {
            let _ = engine.measure_pill(kind, "Example", url);
            let visual = engine.pills.get(kind, "Example", url).expect("cached");
            assert_eq!(visual.horizontal_padding, 0.0);
        }

        let _ = engine.measure_pill(PillKind::Date, "Aug 2", None);
        let date = engine
            .pills
            .get(PillKind::Date, "Aug 2", None)
            .expect("cached");
        assert_eq!(date.horizontal_padding, theme::PILL_PAD_X);
    }

    /// A line of Latin and Hangul must resolve **more than one face**: the Latin
    /// stack has no Hangul coverage, so parley falls through the family list per
    /// character. The old demo seed carried Hangul and Cyrillic lines to make
    /// this visible by eye; nothing asserted it. This does.
    ///
    /// It deliberately does not name the face. Hangul resolves to the bundled
    /// Noto Sans KR when `assets/` is present and to an OS font otherwise, and
    /// the property worth pinning — that fallback happens at all — holds either
    /// way. Faces are compared by the blob they came from, which is the only
    /// identity `FontData` exposes.
    #[test]
    fn a_mixed_script_line_resolves_more_than_one_face() {
        let mut engine = TextEngine::new();
        let layout = engine.simple_layout("reward 보상 terms", theme::role::body());

        let faces: std::collections::HashSet<u64> = layout
            .lines()
            .flat_map(|l| l.items())
            .filter_map(|item| match item {
                parley::layout::PositionedLayoutItem::GlyphRun(gr) => {
                    Some(gr.run().font().data.id())
                }
                _ => None,
            })
            .collect();

        assert!(
            faces.len() >= 2,
            "Latin and Hangul came from one face ({} resolved) — either fallback \
             broke or this machine has no Hangul coverage at all",
            faces.len()
        );
    }

    /// `host_of` keeps the recognisable part of a URL — the host, or the mailto
    /// address — and returns `None` for anything that is not a URL, so a caption
    /// renders verbatim.
    #[test]
    fn host_of_keeps_only_the_host() {
        assert_eq!(
            host_of("https://edgetrade-my.sharepoint.com/:x:/g/personal/p").as_deref(),
            Some("edgetrade-my.sharepoint.com")
        );
        assert_eq!(
            host_of("http://example.com/path?q=1").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            host_of("mailto:a@b.com?subject=x").as_deref(),
            Some("a@b.com")
        );
        assert_eq!(host_of("Terms page"), None, "a caption is not a URL");
        assert_eq!(host_of("example.com"), None, "no scheme");
    }

    /// A link pill wider than the cap drops to the host (so it never overflows
    /// the line); a short URL is shown in full.
    #[test]
    fn a_long_url_pill_caps_to_the_host() {
        let mut engine = TextEngine::new();
        let url =
            "https://edgetrade-my.sharepoint.com/:x:/g/personal/someone_example_com/long/path?q=1";
        let (w, _) = engine.measure_pill(PillKind::Link, url, Some(url));
        assert!(
            w <= theme::LINK_PILL_MAX_W,
            "long pill is capped: {w} > {}",
            theme::LINK_PILL_MAX_W
        );
        // The host measured on its own produces the same label width — the cap
        // showed the host rather than truncating the full URL mid-string.
        let host = "edgetrade-my.sharepoint.com";
        let (hw, _) = engine.measure_pill(PillKind::Link, host, Some(url));
        assert!(
            (w - hw).abs() < 0.5,
            "capped pill shows the host: {w} vs {hw}"
        );
    }

    #[test]
    fn a_short_url_pill_keeps_the_full_label() {
        let mut engine = TextEngine::new();
        let url = "https://x.com";
        let (w, _) = engine.measure_pill(PillKind::Link, url, Some(url));
        // Not capped. A non-link pill with the same label is wider only by its
        // two retained horizontal insets, so nothing was truncated.
        let (plain, _) = engine.measure_pill(PillKind::Date, url, None);
        assert!(w < theme::LINK_PILL_MAX_W);
        assert!(
            ((plain - w) - theme::PILL_PAD_X * 2.0).abs() < 0.5,
            "short URL shown in full: {w} vs {plain}"
        );
    }
}

/// Where the `assets/` directory is looked for, in order — covering `cargo
/// run`, a packaged bundle, and `cargo test`. Fonts are read from its `fonts/`
/// subdirectory (see [`read_asset`]), as reminder sounds are from `reminders/`.
///
/// **The exe-relative root comes before the source-tree one**, and the order
/// matters more than it looks. `CARGO_MANIFEST_DIR` is an absolute path baked
/// in at compile time, so on the machine that built it a packaged copy would
/// happily read the source tree's fonts — and a bundle missing a face would
/// look perfect right up until somebody else ran it. Shipping layout first;
/// the build tree is the fallback, which is what `cargo run` needs.
fn asset_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("KORU_ASSETS") {
        roots.push(PathBuf::from(dir));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            roots.push(dir.join("assets"));
            // A mac release is a bundle, and codesign's seal refuses data
            // inside Contents/MacOS — so the fonts live in the sibling
            // Resources/. After the exe-relative root, so a flat layout
            // still wins where both exist.
            if let Some(contents) = dir.parent() {
                roots.push(contents.join("Resources/assets"));
            }
        }
    }
    // This crate lives at crates/koru-draw; assets/ is at the workspace root,
    // where the fetch scripts write the fonts.
    if let Some(workspace_root) = workspace_root() {
        roots.push(workspace_root.join("assets"));
    }
    roots.push(PathBuf::from("assets"));
    roots
}

/// The source tree this crate was built from, two levels above its manifest.
fn workspace_root() -> Option<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
}

/// The subdirectory of each asset root that holds the fonts:
/// `<root>/fonts/<file>`, where `fetch-fonts` writes them and the package
/// carries them.
const FONTS_DIR: &str = "fonts";

/// A bundled font's bytes, from the first root that has it.
fn read_asset(file: &str) -> Option<Vec<u8>> {
    asset_roots()
        .iter()
        .find_map(|root| std::fs::read(root.join(FONTS_DIR).join(file)).ok())
}

#[cfg(test)]
mod asset_tests {
    use super::*;

    /// What makes a packaged bundle self-contained. `assets/` beside the exe
    /// has to win over the build tree, or a bundle short of a font renders
    /// perfectly on the machine that built it and wrongly everywhere else —
    /// which is the kind of bug that ships.
    #[test]
    fn the_directory_beside_the_binary_beats_the_build_tree() {
        let roots = asset_roots();
        let source_tree = workspace_root().expect("workspace root").join("assets");
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.join("assets")))
            .expect("a test binary has a path");

        let at = |p: &PathBuf| roots.iter().position(|r| r == p);
        let (Some(exe_at), Some(source_at)) = (at(&exe_dir), at(&source_tree)) else {
            panic!("both roots are searched: {roots:#?}");
        };
        assert!(
            exe_at < source_at,
            "exe-relative must come first: {roots:#?}"
        );
    }

    /// The override is the override. Anything that displaces it makes
    /// `KORU_ASSETS` advisory, which is not what it is for.
    #[test]
    fn the_env_override_outranks_everything() {
        // Read rather than set: the variable is process-global and this suite
        // runs in parallel, so setting it here would race the other tests.
        if let Some(dir) = std::env::var_os("KORU_ASSETS") {
            assert_eq!(asset_roots().first(), Some(&PathBuf::from(dir)));
        }
        // The relative fallback stays last, so it can never shadow a real one.
        assert_eq!(asset_roots().last(), Some(&PathBuf::from("assets")));
    }

    /// Every face the registry names is a file that exists.
    ///
    /// **Half of a drift guard**, and it is worth saying which half. The list of
    /// fonts to ship is written down three times — here in `BUNDLED_FAMILIES`,
    /// in `scripts/setup/fetch-fonts.{ps1,sh}`, and in `koru-release`'s `FONTS` — and
    /// nothing used to compare them. It could not be one test, because
    /// `koru-release` must not link this crate: a program that writes a ZIP has
    /// no business pulling vello and parley into its build graph. So this end
    /// asserts the registry against `assets/fonts/`, `koru-release`'s
    /// `fonts_are_the_whole_set` asserts its own list against `assets/fonts/`
    /// and against a count, and the two meet at the directory.
    ///
    /// This is the end that catches the shipping bug, which is not a missing
    /// file: `read_asset` treats a miss as non-fatal *by design*, so a face that
    /// was never fetched falls silently through to a system font and the picker
    /// goes on offering it. Nothing fails; the letters are simply not the ones
    /// that were chosen.
    ///
    /// Skipped on a tree that has not run `fonts`, like the icon tests above.
    #[test]
    fn every_registered_face_is_a_file_that_exists() {
        let fonts = workspace_root()
            .expect("workspace root")
            .join("assets")
            .join(FONTS_DIR);
        if !fonts.join(theme::PHOSPHOR_FILE).is_file() {
            eprintln!("skipping: run ./koru.sh fonts to exercise this");
            return;
        }

        let mut files: Vec<&'static str> = Vec::new();
        for family in theme::BUNDLED_FAMILIES {
            match *family {
                theme::BundledFamily::Variable { roman, italic, .. } => {
                    files.push(roman);
                    files.push(italic);
                }
                // Bound under another name: `files` is the accumulator here.
                theme::BundledFamily::Static { files: set, .. } => files.extend_from_slice(set),
                theme::BundledFamily::RomanOnly { roman, .. } => files.push(roman),
            }
        }
        files.push(theme::NOTO_KR_FILE);
        files.push(theme::PHOSPHOR_FILE);
        files.push(theme::PHOSPHOR_FILL_FILE);

        for file in &files {
            assert!(
                fonts.join(file).is_file(),
                "{file} is registered but missing from {}",
                fonts.display()
            );
        }
        // The count `koru-release`'s half is pinned against. Changing it here
        // without changing it there is what this pair exists to make loud.
        assert_eq!(files.len(), 30, "the packaged set is thirty faces");
    }
}
