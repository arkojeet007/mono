// SPDX-License-Identifier: MPL-2.0

//! Palettes, metrics, and the static data the pickers offer.
//!
//! Colours live behind [`p()`] rather than in consts, so light and dark are one
//! code path. The active mode is a process-wide atomic: a single-window
//! prototype gains nothing from threading a `&Palette` through every draw call,
//! and this keeps call sites reading as `p().text`.
//!
//! Switching mode has to invalidate every task layout, because
//! `layout::build_task` bakes brushes into the parley layout.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use parley::BoundingBox;
use peniko::Color;
use vello::kurbo::Rect;

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgb8(r, g, b)
}

/// Fixed colors for the acquisition-style welcome split and its external
/// Google mark. These are chrome, never document data, and deliberately do not
/// become palette fields: the dark introduction panel and provider trademark
/// keep their identity in either application theme.
pub fn welcome_sidebar_colors() -> (Color, Color, Color, Color) {
    (
        rgb(0x29, 0x28, 0x25),
        rgb(0xFA, 0xFA, 0xF9),
        rgb(0xBC, 0xBB, 0xB7),
        rgb(0x20, 0x92, 0x9E),
    )
}

/// Google's four fixed brand colors, in blue/red/yellow/green order.
pub fn google_brand_colors() -> [Color; 4] {
    [
        rgb(0x42, 0x85, 0xF4),
        rgb(0xEA, 0x43, 0x35),
        rgb(0xFB, 0xBC, 0x05),
        rgb(0x34, 0xA8, 0x53),
    ]
}

/// parley's `Brush` is a blanket marker trait over `Clone + PartialEq + Default
/// + Debug`, and `peniko::Color` is not `Default` — so the brush parley carries
/// through layouts is this newtype, unwrapped again at draw time.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Brush(pub Color);

impl Default for Brush {
    fn default() -> Self {
        Self(p().text)
    }
}

impl From<Color> for Brush {
    fn from(c: Color) -> Self {
        Self(c)
    }
}

// ---------------------------------------------------------------- palettes

/// Fill, border, and label colour for one pill kind.
pub struct PillPalette {
    pub bg: Color,
    pub border: Color,
    pub fg: Color,
}

/// One accent tone: what a `koru.color` token looks like **in this theme**.
///
/// `fill` tints a chip's background, `ink` is legible text on that fill, and
/// `swatch` is the saturated dot the color picker and the color chip draw.
/// Nine per theme, tuned by hand — never a mechanical inversion of the other
/// theme's set, which is how a legible light accent becomes an unreadable
/// dark one.
pub struct AccentTone {
    pub fill: Color,
    pub ink: Color,
    pub swatch: Color,
}

/// The stable `AccentColor` storage tokens, in the order
/// [`Palette::accents`] holds their tones. `koru-draw` names no node model,
/// so the token strings are repeated here; `accent_tone` is the one lookup.
pub const ACCENT_TOKENS: [&str; 9] = [
    "red", "orange", "yellow", "green", "teal", "blue", "purple", "pink", "gray",
];

/// The tone behind an `AccentColor` storage token, in the current theme.
/// `None` for a token from a newer build's palette — the caller draws the
/// untinted default rather than guessing.
pub fn accent_tone(token: &str) -> Option<&'static AccentTone> {
    let i = ACCENT_TOKENS.iter().position(|t| *t == token)?;
    Some(&p().accents[i])
}

pub struct Palette {
    /// The reading surface: the page in Page mode, and the whole viewport in
    /// the pageless modes. User-settable per document — see `ViewState::canvas_bg`.
    pub bg: Color,
    /// The viewport behind the page sheet in Page mode. Unused by the pageless
    /// modes, which fill with [`bg`](Self::bg). A canvas colour rather than a
    /// second use of `status_bg` because the page needs to read as sitting *on*
    /// it, not as a continuation of the status bar.
    pub canvas: Color,
    pub text: Color,
    pub text_dim: Color,
    pub placeholder: Color,
    /// Fallback selection wash when a call site has no caret preference context.
    /// Live text selection uses [`selection_color`], which tracks the caret hue.
    pub selection: Color,
    pub caret: Color,
    pub checkbox: Color,
    pub checkbox_done: Color,
    /// The notch inside a ticked checkbox — must read against `checkbox_done`.
    pub checkbox_mark: Color,

    pub panel_bg: Color,
    pub panel_border: Color,
    /// The outline of a control you press or type into — a secondary
    /// button, a form field. A step stronger than [`panel_border`](Self::panel_border):
    /// a control has to read as a target, where a panel only has to end.
    ///
    /// Darker than the design's `#D9D9D9` on purpose: it is stroked at
    /// [`CONTROL_BORDER_W`] (0.5), which covers only part of a device pixel,
    /// and antialiasing lightens it toward the background. `#BFBFBF` at half
    /// width lands near the design's 1px `#D9D9D9`.
    pub control_border: Color,
    pub panel_shadow: Color,
    /// Whole-block selection, tuned to about 80% of the old contrast against `bg`.
    pub row_selected: Color,
    pub row_hint: Color,
    pub focus_ring: Color,
    /// A chart's categorical fills, in order: a pie's slices wear them round
    /// the circle and in its legend. Eight, cycled past that. Light enough
    /// under `text` in the light theme and deep enough under it in the dark,
    /// so a label placed on a slice reads in either.
    pub chart: [Color; 8],

    /// Status bar band along the bottom edge.
    pub status_bg: Color,
    /// Backing for the level-name chip on a group row.
    pub chip_bg: Color,
    /// The tint behind a code block and an inline-code span — one colour so the
    /// two read as the same feature. Chrome only; not a name a document stores.
    pub code_bg: Color,
    pub hover_tint: Color,
    /// The selection bubble's surface, and the two colours that go on it.
    ///
    /// **The one inverted surface in the app**, and the reason it needed
    /// `Palette` fields of its own when the container and the explore pane each
    /// added none. Every other floating thing is a `panel_bg` sheet the same
    /// shade as the page; the bubble sits *over* the words it is about, a
    /// couple of millimetres from the caret, and a white card there reads as
    /// part of the document. Dark, it reads as an instrument.
    ///
    /// It is dark in **both** themes rather than flipping with them — a light
    /// bubble in dark mode would be the brightest thing on the screen. In dark
    /// mode it is therefore *lighter* than `bg`, which is what separates it
    /// from the page; in light mode it is near-black. `bubble_hover` is the
    /// bubble's own hover tint, because `hover_tint` is the text colour at low
    /// alpha and would vanish against this.
    pub bubble_bg: Color,
    pub bubble_fg: Color,
    /// A second line on a bubble-coloured surface — the link tooltip's "Ctrl+
    /// click to open" under the URL it explains.
    ///
    /// `row_hint` is tuned against `panel_bg` and disappears on this chip, and
    /// `bubble_fg` at low alpha is not the same colour: alpha composites toward
    /// the *surface*, which differs between the two themes, so the two would
    /// drift apart. This is a colour, so it is written down here like the rest.
    pub bubble_dim: Color,
    pub bubble_hover: Color,
    pub grip: Color,
    pub grip_active: Color,
    pub drop_indicator: Color,
    /// The overlay scrollbar thumb. A track is deliberately not drawn; the thumb
    /// alone is the affordance. Not a name a document stores — chrome only.
    pub scrollbar_thumb: Color,
    /// Highlight swatches, in the order a picker offers them. Looked up by
    /// *name* — a document stores "yellow", not an RGB value, so the same
    /// highlight stays legible when the theme flips.
    pub highlights: [(&'static str, Color); 4],

    /// Workspace swatches, in the order the picker offers them.
    ///
    /// Named for the same reason [`Palette::highlights`] is, and the reason
    /// matters more here: a workspace's colour is stored in the *profile
    /// document*, which syncs between machines that may not be on the same
    /// theme. A stored `"teal"` is legible on both; a stored `#2F7D75` is a
    /// colour that was chosen against one background and will be seen against
    /// the other.
    ///
    /// Ten rather than four because this is an identity, not an emphasis — the
    /// point is telling one workspace from another at a glance, so the set has
    /// to be large enough that two workspaces rarely collide.
    pub workspace_colors: [(&'static str, Color); 10],

    /// The close button's hover fill and glyph.
    ///
    /// **The same in both palettes, deliberately.** The convention that closing
    /// a window goes red is stronger than any theme, and a tastefully
    /// desaturated dark-mode version of it would only be less recognisable. It
    /// is a field rather than a pair of loose constants so that every colour in
    /// the app has one home and one shape.
    pub close_hover: Color,
    pub close_hover_fg: Color,

    /// The band behind a **notice**: something happened to your work that you
    /// need to know about and can still act on.
    ///
    /// A third register, and it earns a field rather than borrowing one. Red is
    /// *this failed* — the wrong thing to say about a document somebody else
    /// moved while you were typing in it, which is not a failure and not your
    /// mistake. The neutral panel the update banner uses is the other extreme:
    /// it says *when you get a minute*, which is also wrong when the sentence
    /// underneath is "your unsynced changes are still here".
    ///
    /// Warm rather than red, and pale enough that the text on it is ordinary
    /// `text` — a notice you have to squint at is a notice you will dismiss
    /// without reading.
    pub notice_bg: Color,
    /// The hairline under a notice band, and its icon.
    pub notice_fg: Color,

    /// The spelling squiggle — the wavy underline beneath a misspelled word.
    ///
    /// A muted red (spell-check plan D10): red because every editor has taught
    /// readers that shape-plus-colour, muted because a page of prose with three
    /// typos must still read as a page of prose and not as an error report.
    /// Chrome only; nothing a document stores — the underline is derived UI
    /// state that never enters Loro.
    pub spell: Color,

    /// The colour a primary action wears — the one button in a dialog that
    /// means "go ahead". Chrome only; nothing a document stores.
    pub accent: Color,
    /// Text on top of [`accent`](Self::accent).
    pub on_accent: Color,
    /// A translucent overlay that dims whatever sits behind a modal dialog.
    pub scrim: Color,

    pub document: PillPalette,
    pub contact: PillPalette,
    pub date: PillPalette,
    pub link: PillPalette,
    /// A delegated run. Violet: the fifth hue, chosen because none of the
    /// four already means "work happening somewhere else".
    pub run: PillPalette,

    /// The nine accent tones, in [`ACCENT_TOKENS`] order. Reached through
    /// [`accent_tone`], which is what keeps the index private.
    pub accents: [AccentTone; 9],
}

/// Theme-owned terminal colours. The terminal view resolves semantic and ANSI
/// colours through this type so it never constructs a product colour itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalPalette {
    pub background: Color,
    pub foreground: Color,
    pub cursor: Color,
    pub selection: Color,
    pub ansi: [Color; 16],
}

const LIGHT_TERMINAL: TerminalPalette = TerminalPalette {
    background: rgb(0xFF, 0xFF, 0xFF),
    foreground: rgb(0x19, 0x19, 0x19),
    cursor: rgb(0x19, 0x19, 0x19),
    selection: rgb(0xCF, 0xDD, 0xEE),
    ansi: [
        rgb(0x19, 0x19, 0x19),
        rgb(0x9F, 0x31, 0x2B),
        rgb(0x2F, 0x6B, 0x38),
        rgb(0x6B, 0x60, 0x1F),
        rgb(0x24, 0x5F, 0x9E),
        rgb(0x87, 0x30, 0x5F),
        rgb(0x17, 0x6A, 0x64),
        rgb(0x73, 0x73, 0x73),
        rgb(0x56, 0x56, 0x56),
        rgb(0xB3, 0x3D, 0x35),
        rgb(0x3B, 0x7D, 0x44),
        rgb(0x74, 0x6A, 0x2C),
        rgb(0x2F, 0x6F, 0xB5),
        rgb(0xA8, 0x3C, 0x77),
        rgb(0x1F, 0x7D, 0x75),
        rgb(0x19, 0x19, 0x19),
    ],
};

const DARK_TERMINAL: TerminalPalette = TerminalPalette {
    background: rgb(0x19, 0x19, 0x19),
    foreground: rgb(0xF5, 0xF5, 0xF5),
    cursor: rgb(0xF5, 0xF5, 0xF5),
    selection: rgb(0x2E, 0x44, 0x5C),
    ansi: [
        rgb(0x85, 0x85, 0x85),
        rgb(0xD4, 0x5C, 0x54),
        rgb(0x66, 0xAE, 0x70),
        rgb(0xA6, 0x9B, 0x4F),
        rgb(0x5E, 0x9C, 0xE0),
        rgb(0xD4, 0x6E, 0xA6),
        rgb(0x45, 0xAD, 0xA2),
        rgb(0xC9, 0xC9, 0xC9),
        rgb(0x9E, 0x9E, 0x9E),
        rgb(0xE0, 0x6C, 0x63),
        rgb(0x7F, 0xC4, 0x88),
        rgb(0xC0, 0xB4, 0x5D),
        rgb(0x75, 0xB1, 0xEF),
        rgb(0xE4, 0x85, 0xBA),
        rgb(0x63, 0xC3, 0xB9),
        rgb(0xF5, 0xF5, 0xF5),
    ],
};

/// Neutral greys on a white page.
///
/// They were warm once, tuned against a cream `bg`. With the page white and the
/// text a neutral near-black, a warm grey beside it reads as a yellow cast
/// rather than as warmth, so the whole set moved together.
pub static LIGHT: Palette = Palette {
    bg: rgb(0xFF, 0xFF, 0xFF),
    canvas: rgb(0xED, 0xEE, 0xF0),
    text: rgb(0x19, 0x19, 0x19),
    text_dim: rgb(0x8A, 0x8A, 0x8A),
    placeholder: rgb(0xAB, 0xAB, 0xAB),
    selection: rgb(0xCF, 0xDD, 0xEE),
    // A separate field that happens to match `text`. It will not follow it —
    // moving one without the other leaves the caret a different colour from the
    // line it sits in.
    caret: rgb(0x19, 0x19, 0x19),
    checkbox: rgb(0x8A, 0x8A, 0x8A),
    checkbox_done: rgb(0x45, 0x45, 0x45),
    // The notch punched out of a ticked box. It exists to match `bg`, so it
    // follows it rather than being a colour of its own.
    checkbox_mark: rgb(0xFF, 0xFF, 0xFF),

    panel_bg: rgb(0xFF, 0xFF, 0xFF),
    panel_border: rgb(0xE0, 0xE0, 0xE0),
    control_border: rgb(0xBF, 0xBF, 0xBF),
    panel_shadow: Color::from_rgba8(0x19, 0x19, 0x19, 0x38),
    row_selected: rgb(0xF2, 0xF2, 0xF2),
    row_hint: rgb(0x9E, 0x9E, 0x9E),
    focus_ring: rgb(0xC7, 0xC7, 0xC7),

    status_bg: rgb(0xF6, 0xF6, 0xF6),
    chip_bg: rgb(0xEA, 0xEA, 0xEA),
    code_bg: rgb(0xF1, 0xF1, 0xF3),
    highlights: [
        ("yellow", Color::from_rgba8(0xFF, 0xE0, 0x66, 0x8C)),
        ("green", Color::from_rgba8(0x9B, 0xE0, 0x9B, 0x8C)),
        ("blue", Color::from_rgba8(0x9C, 0xC9, 0xF5, 0x8C)),
        ("pink", Color::from_rgba8(0xF7, 0xAF, 0xC8, 0x8C)),
    ],
    // Fully opaque, unlike the highlights: these sit on the panel as a chip of
    // their own rather than behind text, so there is nothing to see through.
    workspace_colors: [
        ("blue", rgb(0x2F, 0x6F, 0xB5)),
        ("purple", rgb(0x6B, 0x4F, 0xC4)),
        ("teal", rgb(0x1F, 0x7D, 0x75)),
        ("green", rgb(0x3B, 0x7D, 0x44)),
        ("olive", rgb(0x74, 0x6A, 0x2C)),
        ("orange", rgb(0xC0, 0x6A, 0x1E)),
        ("red", rgb(0xB3, 0x3D, 0x35)),
        ("magenta", rgb(0xA8, 0x3C, 0x77)),
        ("slate", rgb(0x55, 0x5F, 0x6B)),
        ("stone", rgb(0x6E, 0x66, 0x60)),
    ],
    hover_tint: Color::from_rgba8(0x19, 0x19, 0x19, 0x14),
    bubble_bg: rgb(0x23, 0x23, 0x23),
    bubble_fg: rgb(0xF5, 0xF5, 0xF5),
    bubble_dim: rgb(0xA3, 0xA3, 0xA3),
    bubble_hover: Color::from_rgba8(0xFF, 0xFF, 0xFF, 0x1F),
    grip: rgb(0xC0, 0xC0, 0xC0),
    grip_active: rgb(0x8A, 0x8A, 0x8A),
    drop_indicator: rgb(0x5B, 0x8F, 0xC7),
    // The text colour at low alpha: a soft grey thumb over the page.
    scrollbar_thumb: Color::from_rgba8(0x19, 0x19, 0x19, 0x55),

    close_hover: rgb(0xC4, 0x2B, 0x1C),
    close_hover_fg: rgb(0xFF, 0xFF, 0xFF),

    notice_bg: rgb(0xFD, 0xF3, 0xE3),
    notice_fg: rgb(0x8A, 0x5A, 0x1E),

    spell: rgb(0xC7, 0x4E, 0x47),

    accent: rgb(0x3B, 0x6B, 0xB5),
    on_accent: rgb(0xFF, 0xFF, 0xFF),
    chart: [
        rgb(0x9E, 0xC1, 0xF2),
        rgb(0xF7, 0xC5, 0x9F),
        rgb(0xA8, 0xDB, 0xA8),
        rgb(0xF4, 0xA3, 0xA3),
        rgb(0xC9, 0xB6, 0xE4),
        rgb(0xF7, 0xE2, 0x9A),
        rgb(0x9E, 0xDA, 0xD8),
        rgb(0xD9, 0xC2, 0xA7),
    ],
    scrim: Color::from_rgba8(0x12, 0x12, 0x12, 0x66),

    document: PillPalette {
        bg: rgb(0xF2, 0xF2, 0xF2),
        border: rgb(0xD8, 0xD8, 0xD8),
        fg: rgb(0x56, 0x56, 0x56),
    },
    contact: PillPalette {
        bg: rgb(0xE7, 0xEF, 0xF8),
        border: rgb(0xB6, 0xCF, 0xE7),
        fg: rgb(0x2C, 0x59, 0x83),
    },
    date: PillPalette {
        bg: rgb(0xFB, 0xF0, 0xDB),
        border: rgb(0xE6, 0xD1, 0xA3),
        fg: rgb(0x86, 0x60, 0x1D),
    },
    run: PillPalette {
        bg: rgb(0xEF, 0xEB, 0xF9),
        border: rgb(0xCE, 0xC3, 0xEB),
        fg: rgb(0x5B, 0x46, 0xA0),
    },
    link: PillPalette {
        bg: rgb(0xE8, 0xF2, 0xEC),
        border: rgb(0xB5, 0xD8, 0xC4),
        fg: rgb(0x2F, 0x6B, 0x4A),
    },
    // Pastel fills with dark ink, sitting on a white page — same register as
    // the pill palettes above. Order is `ACCENT_TOKENS`.
    accents: [
        AccentTone {
            fill: rgb(0xF9, 0xE4, 0xE4),
            ink: rgb(0x8C, 0x30, 0x30),
            swatch: rgb(0xC3, 0x4F, 0x4F),
        },
        AccentTone {
            fill: rgb(0xFB, 0xED, 0xDC),
            ink: rgb(0x8A, 0x52, 0x17),
            swatch: rgb(0xD0, 0x7A, 0x2A),
        },
        AccentTone {
            fill: rgb(0xFA, 0xF3, 0xD4),
            ink: rgb(0x7A, 0x64, 0x14),
            swatch: rgb(0xC9, 0xA2, 0x27),
        },
        AccentTone {
            fill: rgb(0xE4, 0xF2, 0xE4),
            ink: rgb(0x2F, 0x6B, 0x39),
            swatch: rgb(0x4C, 0x9B, 0x5C),
        },
        AccentTone {
            fill: rgb(0xDE, 0xF1, 0xF0),
            ink: rgb(0x1D, 0x66, 0x63),
            swatch: rgb(0x3D, 0x9E, 0x9A),
        },
        AccentTone {
            fill: rgb(0xE7, 0xEF, 0xF8),
            ink: rgb(0x2C, 0x59, 0x83),
            swatch: rgb(0x4A, 0x82, 0xB8),
        },
        AccentTone {
            fill: rgb(0xEF, 0xE7, 0xF8),
            ink: rgb(0x5C, 0x3A, 0x8C),
            swatch: rgb(0x83, 0x60, 0xBD),
        },
        AccentTone {
            fill: rgb(0xF9, 0xE4, 0xF0),
            ink: rgb(0x8C, 0x2F, 0x66),
            swatch: rgb(0xC0, 0x54, 0x93),
        },
        AccentTone {
            fill: rgb(0xF2, 0xF2, 0xF2),
            ink: rgb(0x56, 0x56, 0x56),
            swatch: rgb(0x8A, 0x8A, 0x8A),
        },
    ],
};

/// The same design at a lower luminance — neutral greys, like `LIGHT`.
///
/// **The greys mirror; the hues do not.** Each pill's fill is darkened and its
/// text lightened independently, so the four kinds stay distinguishable instead
/// of collapsing into greys, and the highlight swatches are tuned separately
/// because a highlight has to sit behind pale text without drowning it. A
/// mechanical inversion would lose both.
pub static DARK: Palette = Palette {
    // The light theme's text, inverted into a page.
    bg: rgb(0x19, 0x19, 0x19),
    canvas: rgb(0x10, 0x10, 0x12),
    text: rgb(0xF5, 0xF5, 0xF5),
    text_dim: rgb(0x8A, 0x8A, 0x8A),
    placeholder: rgb(0x6B, 0x6B, 0x6B),
    selection: rgb(0x2E, 0x44, 0x5C),
    caret: rgb(0xF5, 0xF5, 0xF5),
    checkbox: rgb(0x6E, 0x6E, 0x6E),
    checkbox_done: rgb(0xA8, 0xA8, 0xA8),
    checkbox_mark: rgb(0x19, 0x19, 0x19),

    panel_bg: rgb(0x1E, 0x1E, 0x1E),
    panel_border: rgb(0x33, 0x33, 0x33),
    control_border: rgb(0x50, 0x50, 0x50),
    panel_shadow: Color::from_rgba8(0x00, 0x00, 0x00, 0x70),
    row_selected: rgb(0x27, 0x27, 0x27),
    row_hint: rgb(0x7E, 0x7E, 0x7E),
    focus_ring: rgb(0x55, 0x55, 0x55),

    status_bg: rgb(0x1A, 0x1A, 0x1A),
    chip_bg: rgb(0x33, 0x33, 0x33),
    code_bg: rgb(0x23, 0x23, 0x23),
    // Dimmer and more transparent than the light set: a highlight has to sit
    // behind pale text without drowning it.
    highlights: [
        ("yellow", Color::from_rgba8(0x8A, 0x77, 0x1E, 0x9E)),
        ("green", Color::from_rgba8(0x2F, 0x6B, 0x38, 0x9E)),
        ("blue", Color::from_rgba8(0x2C, 0x53, 0x7A, 0x9E)),
        ("pink", Color::from_rgba8(0x7C, 0x3C, 0x55, 0x9E)),
    ],
    // Lifted, not dimmed, against #191919 — the light set at these values would
    // read as dirt. Same ten names, so a workspace keeps its colour across a
    // theme flip and across two machines that disagree about the theme.
    workspace_colors: [
        ("blue", rgb(0x5E, 0x9C, 0xE0)),
        ("purple", rgb(0x9B, 0x86, 0xEA)),
        ("teal", rgb(0x45, 0xAD, 0xA2)),
        ("green", rgb(0x66, 0xAE, 0x70)),
        ("olive", rgb(0xA6, 0x9B, 0x4F)),
        ("orange", rgb(0xE0, 0x96, 0x4A)),
        ("red", rgb(0xE0, 0x6C, 0x63)),
        ("magenta", rgb(0xD4, 0x6E, 0xA6)),
        ("slate", rgb(0x8B, 0x96, 0xA4)),
        ("stone", rgb(0x9E, 0x94, 0x8C)),
    ],
    hover_tint: Color::from_rgba8(0xF5, 0xF5, 0xF5, 0x16),
    // Lighter than the dark `bg` (#191919), not darker: the bubble has to lift
    // off the page in this theme, and there is nothing below #191919 to use.
    bubble_bg: rgb(0x33, 0x33, 0x33),
    bubble_fg: rgb(0xF5, 0xF5, 0xF5),
    // A shade lighter than the light theme's, because the chip under it is
    // lighter too — the contrast is what has to match, not the value.
    bubble_dim: rgb(0xAD, 0xAD, 0xAD),
    bubble_hover: Color::from_rgba8(0xFF, 0xFF, 0xFF, 0x1F),
    grip: rgb(0x4E, 0x4E, 0x4E),
    grip_active: rgb(0x8E, 0x8E, 0x8E),
    drop_indicator: rgb(0x6F, 0xA3, 0xD8),
    // The text colour at low alpha again, mirroring the light thumb.
    scrollbar_thumb: Color::from_rgba8(0xF5, 0xF5, 0xF5, 0x55),

    // Identical to the light set — see the field's doc.
    close_hover: rgb(0xC4, 0x2B, 0x1C),
    close_hover_fg: rgb(0xFF, 0xFF, 0xFF),

    // Dark enough to sit under body text, warm enough to still read as a
    // notice rather than as another panel.
    notice_bg: rgb(0x3A, 0x30, 0x21),
    notice_fg: rgb(0xE0, 0xB6, 0x71),

    spell: rgb(0xD8, 0x73, 0x6B),

    accent: rgb(0x6F, 0xA3, 0xD8),
    on_accent: rgb(0x12, 0x12, 0x12),
    chart: [
        rgb(0x2F, 0x5D, 0x9E),
        rgb(0x9A, 0x5A, 0x1E),
        rgb(0x2E, 0x6B, 0x3A),
        rgb(0x8C, 0x2F, 0x2F),
        rgb(0x5B, 0x3F, 0x8C),
        rgb(0x8A, 0x7A, 0x1F),
        rgb(0x1F, 0x6B, 0x69),
        rgb(0x6B, 0x4E, 0x2E),
    ],
    scrim: Color::from_rgba8(0x00, 0x00, 0x00, 0x99),

    document: PillPalette {
        bg: rgb(0x2C, 0x2C, 0x2C),
        border: rgb(0x45, 0x45, 0x45),
        fg: rgb(0xC9, 0xC9, 0xC9),
    },
    contact: PillPalette {
        bg: rgb(0x1E, 0x2E, 0x3E),
        border: rgb(0x36, 0x51, 0x6D),
        fg: rgb(0x9F, 0xC6, 0xE8),
    },
    date: PillPalette {
        bg: rgb(0x38, 0x2C, 0x18),
        border: rgb(0x5E, 0x49, 0x25),
        fg: rgb(0xE3, 0xBE, 0x77),
    },
    link: PillPalette {
        bg: rgb(0x1D, 0x30, 0x26),
        border: rgb(0x33, 0x55, 0x42),
        fg: rgb(0x8F, 0xCB, 0xA8),
    },
    // Lightened for the dark page on its own, not inverted from the light
    // violet, which reads as near-black ink here.
    run: PillPalette {
        bg: rgb(0x27, 0x24, 0x36),
        border: rgb(0x43, 0x3D, 0x60),
        fg: rgb(0xB6, 0xAB, 0xE6),
    },
    // Deep fills with light ink — tuned for the dark page, not inverted from
    // the light set. Order is `ACCENT_TOKENS`.
    accents: [
        AccentTone {
            fill: rgb(0x3A, 0x22, 0x22),
            ink: rgb(0xE0, 0xA0, 0xA0),
            swatch: rgb(0xD9, 0x7C, 0x7C),
        },
        AccentTone {
            fill: rgb(0x3A, 0x2C, 0x1A),
            ink: rgb(0xE3, 0xB8, 0x77),
            swatch: rgb(0xDB, 0xA0, 0x5B),
        },
        AccentTone {
            fill: rgb(0x38, 0x33, 0x18),
            ink: rgb(0xDF, 0xCE, 0x7E),
            swatch: rgb(0xD3, 0xB8, 0x55),
        },
        AccentTone {
            fill: rgb(0x1D, 0x30, 0x22),
            ink: rgb(0x8F, 0xCB, 0x9C),
            swatch: rgb(0x74, 0xB9, 0x83),
        },
        AccentTone {
            fill: rgb(0x1A, 0x30, 0x2F),
            ink: rgb(0x7E, 0xC8, 0xC4),
            swatch: rgb(0x66, 0xB7, 0xB2),
        },
        AccentTone {
            fill: rgb(0x1E, 0x2E, 0x3E),
            ink: rgb(0x9F, 0xC6, 0xE8),
            swatch: rgb(0x7A, 0xA8, 0xD6),
        },
        AccentTone {
            fill: rgb(0x2A, 0x22, 0x38),
            ink: rgb(0xC2, 0xA7, 0xE8),
            swatch: rgb(0xA8, 0x8B, 0xD6),
        },
        AccentTone {
            fill: rgb(0x38, 0x1E, 0x2D),
            ink: rgb(0xE3, 0x9F, 0xC6),
            swatch: rgb(0xD6, 0x83, 0xB2),
        },
        AccentTone {
            fill: rgb(0x2C, 0x2C, 0x2C),
            ink: rgb(0xC9, 0xC9, 0xC9),
            swatch: rgb(0x9A, 0x9A, 0x9A),
        },
    ],
};

static DARK_MODE: AtomicBool = AtomicBool::new(false);

/// The active palette.
pub fn p() -> &'static Palette {
    if DARK_MODE.load(Ordering::Relaxed) {
        &DARK
    } else {
        &LIGHT
    }
}

pub fn terminal_palette() -> &'static TerminalPalette {
    terminal_palette_for(is_dark())
}

/// Resolve a terminal's own theme without mutating the application's palette.
pub fn terminal_palette_for(dark: bool) -> &'static TerminalPalette {
    if dark {
        &DARK_TERMINAL
    } else {
        &LIGHT_TERMINAL
    }
}

/// Resolve an xterm indexed colour through the active semantic palette.
pub fn terminal_indexed(index: u8) -> Color {
    terminal_indexed_for(index, is_dark())
}

pub fn terminal_indexed_for(index: u8, dark: bool) -> Color {
    if index < 16 {
        return terminal_palette_for(dark).ansi[usize::from(index)];
    }
    if index < 232 {
        const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let value = index - 16;
        let red = LEVELS[usize::from(value / 36)];
        let green = LEVELS[usize::from((value / 6) % 6)];
        let blue = LEVELS[usize::from(value % 6)];
        return rgb(red, green, blue);
    }
    let level = 8 + 10 * (index - 232);
    rgb(level, level, level)
}

/// Exact RGB terminal data still enters the scene through the theme boundary.
pub fn terminal_rgb(red: u8, green: u8, blue: u8) -> Color {
    rgb(red, green, blue)
}

/// SGR dim blends the effective foreground toward its resolved background.
pub fn terminal_dim(foreground: Color, background: Color) -> Color {
    let [fr, fg, fb, fa] = foreground.components;
    let [br, bg, bb, _] = background.components;
    const KEEP: f32 = 0.62;
    Color::new([
        br + (fr - br) * KEEP,
        bg + (fg - bg) * KEEP,
        bb + (fb - bb) * KEEP,
        fa,
    ])
}

pub fn is_dark() -> bool {
    DARK_MODE.load(Ordering::Relaxed)
}

// The host updates this from its existing OS appearance observation. Terminal
// System is independent of the application's explicitly selected theme; draw
// paths must never probe native settings or temporarily change global paint.
static SYSTEM_DARK: AtomicBool = AtomicBool::new(false);

pub fn system_is_dark() -> bool {
    SYSTEM_DARK.load(Ordering::Relaxed)
}

pub fn set_system_dark(dark: bool) -> bool {
    SYSTEM_DARK.swap(dark, Ordering::Relaxed) != dark
}

pub fn set_dark(on: bool) {
    DARK_MODE.store(on, Ordering::Relaxed);
}

// --------------------------------------------------------------- theme preference
//
// Intent (System / Light / Dark), separate from the resolved paint flag above.
// System follows the OS; Light/Dark are sticky. Chosen on the welcome page and
// persisted in preferences.conf. Process-global so welcome and shell share it
// the same way density does.

/// How light/dark is chosen. Default is follow the OS.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemePreference {
    /// Written to `preferences.conf`. Stable.
    pub fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// Unknown names become System rather than an error.
    pub fn parse(s: &str) -> Self {
        match s {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }
}

/// Discriminants: System=0 (static default), Light=1, Dark=2.
static THEME_PREF: AtomicU8 = AtomicU8::new(0);

// --------------------------------------------------------------- caret colour
//
// Named swatches on the welcome page. Each token resolves to a different RGB
// in light and dark so the caret stays legible; documents never store this.

/// User-chosen caret colour. Default follows the theme's high-contrast neutral.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum CaretColor {
    #[default]
    Default,
    Blue,
    Amber,
    Violet,
}

impl CaretColor {
    pub const ALL: [Self; 4] = [Self::Default, Self::Blue, Self::Amber, Self::Violet];

    pub fn key(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Blue => "blue",
            Self::Amber => "amber",
            Self::Violet => "violet",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "blue" => Self::Blue,
            "amber" => Self::Amber,
            "violet" => Self::Violet,
            _ => Self::Default,
        }
    }

    /// Concrete paint for the current theme.
    pub fn color(self) -> Color {
        self.color_for(is_dark())
    }

    /// A surface with its own palette must not borrow the app's dark/light
    /// choice when resolving the same semantic caret color.
    pub fn color_for(self, dark: bool) -> Color {
        match (self, dark) {
            (Self::Default, false) => rgb(0x19, 0x19, 0x19),
            (Self::Default, true) => rgb(0xF4, 0xF4, 0xF5),
            (Self::Blue, false) => rgb(0x25, 0x63, 0xEB),
            (Self::Blue, true) => rgb(0x60, 0xA5, 0xFA),
            (Self::Amber, false) => rgb(0xD9, 0x77, 0x06),
            (Self::Amber, true) => rgb(0xFB, 0xBF, 0x24),
            // Light matches the product violet; dark is a mid violet that still
            // reads on #18181B without blowing out selection contrast.
            (Self::Violet, false) => rgb(0x7C, 0x3A, 0xED),
            (Self::Violet, true) => rgb(0x8B, 0x5C, 0xF6),
        }
    }

    /// Text selection fill for the current theme — same hue as [`Self::color`],
    /// not a fixed blue wash.
    ///
    /// **Light:** caret RGB at ~25% opacity over the page.  
    /// **Dark:** solid tint at ~30–40% accent strength (stronger than light so
    /// the highlight still reads on a dark ground while text stays #F4F4F5).
    pub fn selection(self) -> Color {
        match (self, is_dark()) {
            // Neutral caret → neutral wash (not a leftover blue).
            (Self::Default, false) => Color::from_rgba8(0x19, 0x19, 0x19, 0x40),
            (Self::Default, true) => rgb(0x3F, 0x3F, 0x46),
            (Self::Blue, false) => Color::from_rgba8(0x25, 0x63, 0xEB, 0x40),
            (Self::Blue, true) => rgb(0x1E, 0x3A, 0x5F),
            (Self::Amber, false) => Color::from_rgba8(0xD9, 0x77, 0x06, 0x40),
            (Self::Amber, true) => rgb(0x5C, 0x3D, 0x12),
            (Self::Violet, false) => Color::from_rgba8(0x7C, 0x3A, 0xED, 0x40),
            // Hand-tuned ~35% accent of #8B5CF6 on dark chrome.
            (Self::Violet, true) => rgb(0x4C, 0x32, 0x78),
        }
    }
}

static CARET_COLOR: AtomicU8 = AtomicU8::new(0);

pub fn caret_color() -> CaretColor {
    match CARET_COLOR.load(Ordering::Relaxed) {
        1 => CaretColor::Blue,
        2 => CaretColor::Amber,
        3 => CaretColor::Violet,
        _ => CaretColor::Default,
    }
}

pub fn set_caret_color(c: CaretColor) {
    let v = match c {
        CaretColor::Default => 0u8,
        CaretColor::Blue => 1,
        CaretColor::Amber => 2,
        CaretColor::Violet => 3,
    };
    CARET_COLOR.store(v, Ordering::Relaxed);
}

/// Live text-selection fill — tracks [`caret_color`], not a fixed palette blue.
pub fn selection_color() -> Color {
    caret_color().selection()
}

// ---------------------------------------------------------------- caret width
//
// Multiplier of [`CARET_W`]. Medium is today's bar; Thin is half; Thick / 4× /
// 8× are progressively heavier. Preference only — never stored on a document.

/// User-chosen caret width relative to [`CARET_W`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum CaretWidth {
    Thin,
    #[default]
    Medium,
    Thick,
    X4,
    X8,
}

impl CaretWidth {
    pub const ALL: [Self; 5] = [Self::Thin, Self::Medium, Self::Thick, Self::X4, Self::X8];

    pub fn key(self) -> &'static str {
        match self {
            Self::Thin => "thin",
            Self::Medium => "medium",
            Self::Thick => "thick",
            Self::X4 => "4x",
            Self::X8 => "8x",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "thin" => Self::Thin,
            "thick" => Self::Thick,
            "4x" | "x4" => Self::X4,
            "8x" | "x8" => Self::X8,
            _ => Self::Medium,
        }
    }

    pub fn scale(self) -> f32 {
        match self {
            Self::Thin => 0.5,
            Self::Medium => 1.0,
            Self::Thick => 2.0,
            Self::X4 => 4.0,
            Self::X8 => 8.0,
        }
    }

    pub fn px(self) -> f32 {
        CARET_W * self.scale()
    }

    /// Short label for the Welcome segment control.
    pub fn label(self) -> &'static str {
        match self {
            Self::Thin => "Thin",
            Self::Medium => "Med",
            Self::Thick => "Thick",
            Self::X4 => "4×",
            Self::X8 => "8×",
        }
    }
}

static CARET_WIDTH: AtomicU8 = AtomicU8::new(1); // Medium

pub fn caret_width() -> CaretWidth {
    match CARET_WIDTH.load(Ordering::Relaxed) {
        0 => CaretWidth::Thin,
        2 => CaretWidth::Thick,
        3 => CaretWidth::X4,
        4 => CaretWidth::X8,
        _ => CaretWidth::Medium,
    }
}

pub fn set_caret_width(w: CaretWidth) {
    let v = match w {
        CaretWidth::Thin => 0u8,
        CaretWidth::Medium => 1,
        CaretWidth::Thick => 2,
        CaretWidth::X4 => 3,
        CaretWidth::X8 => 4,
    };
    CARET_WIDTH.store(v, Ordering::Relaxed);
}

/// Live caret stroke width in CSS pixels (preference × [`CARET_W`]).
pub fn caret_w() -> f32 {
    caret_width().px()
}

// --------------------------------------------------------------- caret height
//
// Scales the baseline-anchored rise/foot. Medium is today's bar; Short is
// tighter; Tall is a fuller letter height without climbing the strut line box.
// Preference only — never stored on a document.

/// User-chosen caret height relative to the typographic ink.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum CaretHeight {
    Short,
    #[default]
    Medium,
    Tall,
}

impl CaretHeight {
    pub const ALL: [Self; 3] = [Self::Short, Self::Medium, Self::Tall];

    pub fn key(self) -> &'static str {
        match self {
            Self::Short => "short",
            Self::Medium => "medium",
            Self::Tall => "tall",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "short" => Self::Short,
            "tall" => Self::Tall,
            _ => Self::Medium,
        }
    }

    /// Fraction of **Tall** caret height (`capHeight + 2×descent`).
    ///
    /// Short 0.70 · Medium 0.85 (default) · Tall 1.00 — Medium/Short shrink
    /// about Tall’s vertical center, not about the baseline.
    pub fn scale(self) -> f64 {
        match self {
            Self::Short => 0.70,
            Self::Medium => 0.85,
            Self::Tall => 1.00,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Short => "Short",
            Self::Medium => "Med",
            Self::Tall => "Tall",
        }
    }
}

/// Default italic caret slant in degrees (CSS / faux-italic family).
pub const CARET_ITALIC_SLANT_DEG: f64 = 12.0;

static CARET_HEIGHT: AtomicU8 = AtomicU8::new(1); // Medium

pub fn caret_height() -> CaretHeight {
    match CARET_HEIGHT.load(Ordering::Relaxed) {
        0 => CaretHeight::Short,
        2 => CaretHeight::Tall,
        _ => CaretHeight::Medium,
    }
}

pub fn set_caret_height(h: CaretHeight) {
    let v = match h {
        CaretHeight::Short => 0u8,
        CaretHeight::Medium => 1,
        CaretHeight::Tall => 2,
    };
    CARET_HEIGHT.store(v, Ordering::Relaxed);
}

pub fn theme_preference() -> ThemePreference {
    match THEME_PREF.load(Ordering::Relaxed) {
        1 => ThemePreference::Light,
        2 => ThemePreference::Dark,
        _ => ThemePreference::System,
    }
}

/// Does not resolve or paint — the shell applies [`set_dark`] from the OS or
/// the fixed choice after this moves.
pub fn set_theme_preference(pref: ThemePreference) {
    let v = match pref {
        ThemePreference::System => 0u8,
        ThemePreference::Light => 1,
        ThemePreference::Dark => 2,
    };
    THEME_PREF.store(v, Ordering::Relaxed);
}

/// Whether the caption offers **one ⋮ button** instead of the four window
/// buttons — the experimental menu, on by default and turned off from the
/// welcome page.
///
/// This is a process global rather than a parameter, which is a knowing
/// exception to the doctrine in `koru_shell_chrome::caption`: which furniture a
/// window offers is normally the container's decision, passed in. Threading a
/// bool would have reached `window_buttons`, `buttons_w`, `context_toggle`,
/// `tab_strip`, `regions::hit` and every test that drives them, to carry a value
/// no caller ever varies — while every *other* user preference that deep drawing
/// code reads (the theme, the fonts, the density) is already an atomic here. It
/// is the same kind of setting, so it is held the same way.
/// The app font, held exactly the way the theme is and for the same reason: it
/// is read by a dozen leaf drawing calls that have no setting to be handed.
///
/// Stored as the variant's index into [`FontChoice::ALL`]. The initial value
/// is [`default_ui_font`] spelled as an index — `System` (SF Pro) on macOS,
/// `Inter` elsewhere — because a static cannot call `position()`.
static UI_FONT: AtomicU8 = AtomicU8::new(if cfg!(target_os = "macos") {
    SYSTEM
} else {
    INTER
});

/// The typeface the **chrome** is set in. Not what a document row is set in —
/// that is `Paragraph::font`.
pub fn ui_font() -> FontChoice {
    let i = UI_FONT.load(Ordering::Relaxed) as usize;
    FontChoice::ALL.get(i).copied().unwrap_or_default()
}

/// Callers must invalidate laid-out text afterwards — the font, like the
/// brushes, is baked into every parley layout.
pub fn set_ui_font(font: FontChoice) {
    let i = FontChoice::ALL.iter().position(|c| *c == font).unwrap_or(0);
    UI_FONT.store(i as u8, Ordering::Relaxed);
}

// ----------------------------------------------- the document font roles
//
// Three independent globals beside the app font, each a `FontChoice`, held as
// process globals for the same reason the app font is: `rowlayout` reads them
// from leaf draw calls that have no setting to be handed, and a change (from
// the welcome tab or the page menu) must reflow every window. Persisted by the
// shell from `Settings` on save; loaded here at startup.

/// Index of `Inter` in `FontChoice::ALL`.
const INTER: u8 = 0;
/// Index of `Manrope` in `FontChoice::ALL` — the headings role's default.
const MANROPE: u8 = 5;
/// Index of `JetBrainsMono` in `FontChoice::ALL` — the code role's default.
const JETBRAINS_MONO: u8 = 9;
/// Index of `System` in `FontChoice::ALL` — the app role's default on macOS.
const SYSTEM: u8 = 12;

static BODY_FONT: AtomicU8 = AtomicU8::new(INTER);
static HEADINGS_FONT: AtomicU8 = AtomicU8::new(MANROPE);
static CODE_FONT: AtomicU8 = AtomicU8::new(JETBRAINS_MONO);

/// The shipped default per role, as a value — what the index consts above
/// spell for the atomics. `Settings::default` reads these so the two homes of
/// the defaults cannot drift; `defaults_and_indices_agree` holds all four
/// pairs together.
pub fn default_ui_font() -> FontChoice {
    // The platform's own face for the platform's own chrome; elsewhere Inter,
    // which macOS keeps as a choice rather than a default.
    if cfg!(target_os = "macos") {
        FontChoice::System
    } else {
        FontChoice::Inter
    }
}
pub fn default_body_font() -> FontChoice {
    FontChoice::Inter
}
pub fn default_headings_font() -> FontChoice {
    FontChoice::Manrope
}
pub fn default_code_font() -> FontChoice {
    FontChoice::JetBrainsMono
}

pub fn body_font() -> FontChoice {
    let i = BODY_FONT.load(Ordering::Relaxed) as usize;
    FontChoice::ALL.get(i).copied().unwrap_or_default()
}
pub fn headings_font() -> FontChoice {
    let i = HEADINGS_FONT.load(Ordering::Relaxed) as usize;
    FontChoice::ALL.get(i).copied().unwrap_or_default()
}
pub fn code_font() -> FontChoice {
    let i = CODE_FONT.load(Ordering::Relaxed) as usize;
    FontChoice::ALL
        .get(i)
        .copied()
        .unwrap_or(FontChoice::JetBrainsMono)
}

/// Callers must invalidate laid-out text afterwards.
pub fn set_body_font(font: FontChoice) {
    let i = FontChoice::ALL.iter().position(|c| *c == font).unwrap_or(0);
    BODY_FONT.store(i as u8, Ordering::Relaxed);
}
pub fn set_headings_font(font: FontChoice) {
    let i = FontChoice::ALL.iter().position(|c| *c == font).unwrap_or(0);
    HEADINGS_FONT.store(i as u8, Ordering::Relaxed);
}
pub fn set_code_font(font: FontChoice) {
    let i = FontChoice::ALL
        .iter()
        .position(|c| *c == font)
        .unwrap_or(JETBRAINS_MONO as usize);
    CODE_FONT.store(i as u8, Ordering::Relaxed);
}

pub fn body_stack() -> &'static str {
    body_font().stack()
}
pub fn headings_stack() -> &'static str {
    headings_font().stack()
}
pub fn code_stack() -> &'static str {
    code_font().stack()
}

/// The terminal requires fixed-cell geometry. A proportional code preference
/// is valid for document blocks but cannot become the terminal's primary face.
pub fn terminal_code_font() -> FontChoice {
    let selected = code_font();
    if selected.class() == FontClass::Mono {
        selected
    } else {
        FontChoice::JetBrainsMono
    }
}

pub fn terminal_code_stack() -> &'static str {
    terminal_code_font().stack()
}

/// Display zoom (1.0 = 100%), held as a process global for the same reason the
/// theme and UI font are: the shell applies it as a multiplier on the root
/// transform, and the one reader outside the shell — the transient zoom
/// indicator — has no setting to be handed. The shell's `App::zoom` is the
/// source of truth; this mirrors it for the draw side. Stored as `f32::to_bits`
/// in a `u32` because there is no atomic `f32`.
static UI_ZOOM: AtomicU32 = AtomicU32::new(1.0f32.to_bits());

/// The current display zoom. `App::zoom` in the shell is the source of truth;
/// this is the read side for drawing code (the zoom indicator).
pub fn zoom() -> f32 {
    f32::from_bits(UI_ZOOM.load(Ordering::Relaxed))
}

pub fn set_zoom(zoom: f32) {
    UI_ZOOM.store(zoom.to_bits(), Ordering::Relaxed);
}

/// The bounds and step every display zoom obeys, **beside the global they are
/// about**.
///
/// They were written out three times before this — `koru_app_shell::state`,
/// `koru_workspace_host::app`, and a fourth reader was about to be the window
/// menu's zoom row, which has to know when `−` has nothing left to take away.
/// Three copies of a number that must agree is three chances for them not to;
/// the shell's own note already said "`koru_workspace_host`'s hold exactly
/// these", which is a comment doing a constant's job. Both crates now name
/// these, and the row asks the same question the chords do.
///
/// The whole window scales, so 0.5–2.0 keeps chrome and content both usable,
/// and a step of 0.1 reads as 10% on the indicator.
pub const ZOOM_MIN: f32 = 0.5;
pub const ZOOM_MAX: f32 = 2.0;
pub const ZOOM_STEP: f32 = 0.1;

/// Snap a zoom factor to the [`ZOOM_STEP`] grid and clamp it into
/// [`ZOOM_MIN`]..=[`ZOOM_MAX`].
///
/// Snapping stops a wheel gesture — or repeated `1.0 + 0.1` arithmetic — from
/// landing on `1.2000001`, which the indicator would round anyway and which no
/// two paths would ever agree on.
pub fn clamp_zoom(zoom: f32) -> f32 {
    if !zoom.is_finite() {
        return 1.0;
    }
    ((zoom / ZOOM_STEP).round() * ZOOM_STEP).clamp(ZOOM_MIN, ZOOM_MAX)
}

/// Whether zoom can still step **out**: `−` at 50% has nothing to take away,
/// and a control that would do nothing is dimmed rather than hidden.
///
/// Asked against the snapped grid rather than with a bare `>`, because the
/// value is a repeated float sum and `0.5000001 > 0.5` is not the question.
pub fn can_zoom_out(zoom: f32) -> bool {
    clamp_zoom(zoom) > ZOOM_MIN
}

/// Whether zoom can still step **in**. [`can_zoom_out`]'s mirror.
pub fn can_zoom_in(zoom: f32) -> bool {
    clamp_zoom(zoom) < ZOOM_MAX
}

/// The percentage a zoom factor reads as, everywhere it is shown to a person.
///
/// Integer, no decimals: the step grid is 10%, and a `112.00000001%` would be a
/// rounding artefact on screen. Written out in three places before this — the
/// shell's flash, Koru's own, and the window menu's readout — which is three
/// chances for one of them to grow a decimal point the others do not have.
pub fn zoom_percent(zoom: f32) -> String {
    format!("{}%", (zoom * 100.0).round() as u32)
}

// --------------------------------------------------------------- density
//
// A user-facing display density, held as a process global for the same reason
// the theme, UI font and zoom are: `role::ui()` and `role::body()` read it at
// layout time from a dozen leaf calls that have no setting to be handed, and a
// toggle must reflow like a font change. Only the UI and document-body roles
// respond; hint/meta/row/subtitle/section_heading are fixed across densities.

/// The three display densities a user can pick. `Normal` is the default.
///
/// Variant order fixes the discriminants: `Normal` is 0 (the static's initial
/// value), `Compact` 1, `Comfortable` 2 — `density()`'s match relies on this.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Density {
    #[default]
    Normal,
    Compact,
    Comfortable,
}

impl Density {
    /// The name written to `preferences.conf`. Stable.
    pub fn key(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Normal => "normal",
            Self::Comfortable => "comfortable",
        }
    }

    /// An unknown name is the default rather than an error.
    pub fn parse(s: &str) -> Self {
        match s {
            "compact" => Self::Compact,
            "comfortable" => Self::Comfortable,
            _ => Self::Normal,
        }
    }
}

static DENSITY: AtomicU8 = AtomicU8::new(Density::Normal as u8);

pub fn density() -> Density {
    match DENSITY.load(Ordering::Relaxed) {
        1 => Density::Compact,
        2 => Density::Comfortable,
        _ => Density::Normal,
    }
}

/// Callers must invalidate laid-out text afterwards — the roles read this at
/// layout time, so a change reflows every window.
pub fn set_density(d: Density) {
    DENSITY.store(d as u8, Ordering::Relaxed);
}

// ----------------------------------------------------- default document view mode
//
// The global default a new document opens in, and the canvas colour it opens
// with. Process-globals for the same reason the theme is: a new document is
// built far from any `Settings` (the container's `open_node`), and the Settings
// layout row sets them live. Persisted by the shell from `Settings` on the
// click that chose them; loaded here at startup.

static DEFAULT_VIEW_MODE: AtomicU8 = AtomicU8::new(0); // 0=Full, 1=Page, 2=Narrow
/// Packed RGBA, or `0` for "Auto" (follow the theme canvas). Real colours are
/// opaque, so the low byte is `0xFF` and `0` is unambiguous.
static DEFAULT_CANVAS_BG: AtomicU32 = AtomicU32::new(0);

pub fn default_view_mode() -> ViewMode {
    match DEFAULT_VIEW_MODE.load(Ordering::Relaxed) {
        1 => ViewMode::Page,
        2 => ViewMode::Narrow,
        _ => ViewMode::Full,
    }
}

pub fn set_default_view_mode(mode: ViewMode) {
    let i = match mode {
        ViewMode::Page => 1,
        ViewMode::Narrow => 2,
        ViewMode::Full => 0,
    };
    DEFAULT_VIEW_MODE.store(i, Ordering::Relaxed);
}

pub fn default_canvas_bg() -> Option<[u8; 4]> {
    let v = DEFAULT_CANVAS_BG.load(Ordering::Relaxed);
    if v == 0 {
        None
    } else {
        Some([(v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8])
    }
}

pub fn set_default_canvas_bg(c: Option<[u8; 4]>) {
    let packed = c.map(|[r, g, b, a]| {
        (u32::from(r) << 24) | (u32::from(g) << 16) | (u32::from(b) << 8) | u32::from(a)
    });
    DEFAULT_CANVAS_BG.store(packed.unwrap_or(0), Ordering::Relaxed);
}

// ---------------------------------------------------------------- metrics

/// The stack the chrome is currently set in — tabs, the tree, pills, panels,
/// the status bar, the toolbars.
///
/// A function rather than a constant because the family is now the user's to
/// pick, and a **process-global** rather than a parameter for the same reason
/// the theme is one: it is read from a dozen leaf drawing calls that have no
/// business being handed a setting.
///
/// This is deliberately **not** what a document row is set in — see
/// [`FontChoice`]. `Paragraph::font` picks that, per row.
pub fn ui_stack() -> &'static str {
    ui_font().stack()
}

/// The weight the chrome asks for. See [`FontChoice::weight`] for why it is not
/// simply 500.
pub fn ui_weight() -> f32 {
    ui_font().weight()
}

/// Bundled variable fonts, registered into the font collection at startup so
/// they resolve identically on every platform rather than depending on what
/// happens to be installed.
///
/// Loaded from `assets/fonts/` rather than `include_bytes!`, so the fonts stay
/// out of git history (see `scripts/setup/fetch-fonts.*`). A missing file is not fatal: the
/// family simply does not register and the stacks in [`FontChoice::stack`] fall
/// through to whatever the OS provides.
///
/// **Each family is a pair.** None of them carries an `ital`/`slnt` axis, so
/// without the italic file `Ctrl+I` gets fontique's 14° faux skew — a slanted
/// roman rather than real italic letterforms. Registering both under one family
/// name is what lets it pick the right face instead.
pub const NOTO_KR_FILE: &str = "NotoSansKR-Regular.ttf";
pub const PHOSPHOR_FILE: &str = "Phosphor.ttf";
/// Phosphor's Fill weight. **A separate family, not a weight of the one above**
/// — it is a separate file at the *same* codepoints, which is how Phosphor
/// ships it, so asking the regular family for a heavier weight would only get
/// fontique's faux bold.
pub const PHOSPHOR_FILL_FILE: &str = "Phosphor-Fill.ttf";
pub const NOTO_KR_FAMILY: &str = "Noto Sans KR";

pub const INTER_FAMILY: &str = "Inter";
pub const SOURCE_SANS_FAMILY: &str = "Source Sans 3";
pub const JAKARTA_FAMILY: &str = "Plus Jakarta Sans";
pub const PLEX_FAMILY: &str = "IBM Plex Sans";
pub const POPPINS_FAMILY: &str = "Poppins";
pub const MANROPE_FAMILY: &str = "Manrope";
pub const SOURCE_SERIF_FAMILY: &str = "Source Serif 4";
pub const LORA_FAMILY: &str = "Lora";
pub const NUNITO_FAMILY: &str = "Nunito";
pub const JETBRAINS_MONO_FAMILY: &str = "JetBrains Mono";
pub const SOURCE_CODE_FAMILY: &str = "Source Code Pro";

/// A bundled family and the shape of its delivery.
///
/// Replaces a flat `(roman, italic, family)` tuple because not every family
/// ships that shape:
/// - **Variable** families ship one variable-roman file and one variable-italic
///   file. Both register under one family name with `style: None`, so fontique
///   picks the italic member for `Ctrl+I`.
/// - **Static** families ship a per-weight static instance set (Poppins); the
///   italic members are separate files. fontique matches the nearest weight.
/// - **RomanOnly** families ship a variable roman with no italic face at all
///   (Manrope). `Ctrl+I` then faux-skews — accepted and documented per family.
pub enum BundledFamily {
    Variable {
        roman: &'static str,
        italic: &'static str,
        family: &'static str,
    },
    Static {
        files: &'static [&'static str],
        family: &'static str,
    },
    RomanOnly {
        roman: &'static str,
        family: &'static str,
    },
}

impl BundledFamily {
    pub fn family(self) -> &'static str {
        match self {
            BundledFamily::Variable { family, .. }
            | BundledFamily::Static { family, .. }
            | BundledFamily::RomanOnly { family, .. } => family,
        }
    }
}

/// Every bundled text family.
///
/// One list, so registration and [`FontChoice`] cannot disagree about what is
/// available. Order is sans → serif → rounded → mono, matching the order
/// [`FontChoice::ALL`] groups them for a picker.
pub const BUNDLED_FAMILIES: &[BundledFamily] = &[
    // sans
    BundledFamily::Variable {
        roman: "Inter.ttf",
        italic: "Inter-Italic.ttf",
        family: INTER_FAMILY,
    },
    BundledFamily::Variable {
        roman: "SourceSans3.ttf",
        italic: "SourceSans3-Italic.ttf",
        family: SOURCE_SANS_FAMILY,
    },
    BundledFamily::Variable {
        roman: "PlusJakartaSans.ttf",
        italic: "PlusJakartaSans-Italic.ttf",
        family: JAKARTA_FAMILY,
    },
    BundledFamily::Variable {
        roman: "IBMPlexSans.ttf",
        italic: "IBMPlexSans-Italic.ttf",
        family: PLEX_FAMILY,
    },
    // Poppins ships only static instances per weight (no variable font); the
    // italic members are separate files, so Ctrl+I gets real italic letterforms.
    BundledFamily::Static {
        files: &[
            "Poppins-Regular.ttf",
            "Poppins-Italic.ttf",
            "Poppins-Medium.ttf",
            "Poppins-MediumItalic.ttf",
            "Poppins-SemiBold.ttf",
            "Poppins-SemiBoldItalic.ttf",
            "Poppins-Bold.ttf",
            "Poppins-BoldItalic.ttf",
        ],
        family: POPPINS_FAMILY,
    },
    // Manrope ships a variable roman but no italic face at all — Ctrl+I
    // faux-skews here. Documented in TYPOGRAPHY.md.
    BundledFamily::RomanOnly {
        roman: "Manrope.ttf",
        family: MANROPE_FAMILY,
    },
    // serif
    BundledFamily::Variable {
        roman: "SourceSerif4.ttf",
        italic: "SourceSerif4-Italic.ttf",
        family: SOURCE_SERIF_FAMILY,
    },
    BundledFamily::Variable {
        roman: "Lora.ttf",
        italic: "Lora-Italic.ttf",
        family: LORA_FAMILY,
    },
    // rounded
    BundledFamily::Variable {
        roman: "Nunito.ttf",
        italic: "Nunito-Italic.ttf",
        family: NUNITO_FAMILY,
    },
    // mono
    BundledFamily::Variable {
        roman: "JetBrainsMono.ttf",
        italic: "JetBrainsMono-Italic.ttf",
        family: JETBRAINS_MONO_FAMILY,
    },
    BundledFamily::Variable {
        roman: "SourceCodePro.ttf",
        italic: "SourceCodePro-Italic.ttf",
        family: SOURCE_CODE_FAMILY,
    },
];

/// Icon size in the document toolbar, where an icon *is* the control.
/// The caption row uses the smaller [`CAPTION_ICON`].
#[allow(dead_code)]
pub const ICON_SIZE: f32 = 17.0;

/// A typeface the user can pick.
///
/// **Four independent things choose from this list**, and they are deliberately
/// not the same setting:
///
/// - the **app font** — the chrome, via [`ui_font`] / [`set_ui_font`], persisted
///   in `Settings`;
/// - the **body** font of a document — `theme::body_font()`;
/// - the **headings** font of a document — `theme::headings_font()`;
/// - the **code** font (code blocks and inline code) — `theme::code_font()`.
///
/// Sharing the list is the point: whatever role a face can fill, any role can.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum FontChoice {
    // sans
    #[default]
    Inter,
    SourceSans3,
    PlusJakartaSans,
    IbmPlexSans,
    Poppins,
    Manrope,
    // serif
    SourceSerif4,
    Lora,
    // rounded
    Nunito,
    // mono
    JetBrainsMono,
    SourceCodePro,
    Mono,
    // The platform's own UI face — SF Pro on macOS, Segoe UI on Windows —
    // resolved through the `system-ui` generic every sans stack already
    // carries as its fallback. Appended, not inserted: the role globals store
    // an *index* into [`Self::ALL`], so a mid-list insertion silently reseats
    // every persisted choice.
    System,
}

/// The class a family belongs to, so a picker can group families and a role can
/// filter them (the code role offers the mono class by default).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FontClass {
    Sans,
    Serif,
    Rounded,
    Mono,
}

impl FontChoice {
    pub const ALL: [FontChoice; 13] = [
        Self::Inter,
        Self::SourceSans3,
        Self::PlusJakartaSans,
        Self::IbmPlexSans,
        Self::Poppins,
        Self::Manrope,
        Self::SourceSerif4,
        Self::Lora,
        Self::Nunito,
        Self::JetBrainsMono,
        Self::SourceCodePro,
        Self::Mono,
        Self::System,
    ];

    /// Which class a family belongs to — sans, serif, rounded, or mono. A picker
    /// groups by this; the code role offers the mono class by default.
    pub fn class(self) -> FontClass {
        match self {
            Self::Inter
            | Self::SourceSans3
            | Self::PlusJakartaSans
            | Self::IbmPlexSans
            | Self::Poppins
            | Self::Manrope => FontClass::Sans,
            Self::System => FontClass::Sans,
            Self::SourceSerif4 | Self::Lora => FontClass::Serif,
            Self::Nunito => FontClass::Rounded,
            Self::JetBrainsMono | Self::SourceCodePro | Self::Mono => FontClass::Mono,
        }
    }

    /// A CSS-style family list. parley resolves it **per character**, so the
    /// tail below adds script coverage the Latin faces do not have without
    /// touching a single Latin glyph: a family that is not installed simply
    /// does not match, and the next one is tried.
    ///
    /// **The Simplified Chinese faces come before Noto Sans KR**, and that
    /// order is the whole point. Noto Sans KR carries most Han and *not* the
    /// Simplified-only forms — 简 among them — so with KR first the word
    /// 简体中文 reached the screen as a tofu box followed by three correct
    /// characters, in the language picker, as the name of a language.
    ///
    /// What this cannot fix is **Han unification**: a static list has to choose
    /// one regional face for characters Chinese and Japanese share, so a long
    /// Japanese passage may take Simplified forms from the Chinese face. Doing
    /// that properly means ordering the stack by the language being drawn, and
    /// that is on the roadmap rather than here.
    pub fn stack(self) -> &'static str {
        /// Everything after the chosen family. Written once, appended by
        /// `concat!` so each arm below stays a single literal.
        macro_rules! with_scripts {
            ($head:literal) => {
                concat!(
                    $head,
                    ", Microsoft YaHei UI, Microsoft YaHei, PingFang SC, Noto Sans CJK SC",
                    ", Noto Sans SC, Yu Gothic UI, Hiragino Sans, Noto Sans KR",
                    ", Noto Sans Arabic, Segoe UI, Geeza Pro"
                )
            };
        }
        match self {
            Self::Inter => with_scripts!("Inter, system-ui, sans-serif"),
            Self::SourceSans3 => with_scripts!("Source Sans 3, system-ui, sans-serif"),
            Self::PlusJakartaSans => with_scripts!("Plus Jakarta Sans, system-ui, sans-serif"),
            Self::IbmPlexSans => with_scripts!("IBM Plex Sans, system-ui, sans-serif"),
            Self::Poppins => with_scripts!("Poppins, system-ui, sans-serif"),
            Self::Manrope => with_scripts!("Manrope, system-ui, sans-serif"),
            Self::SourceSerif4 => with_scripts!("Source Serif 4, Georgia, serif"),
            Self::Lora => with_scripts!("Lora, Georgia, serif"),
            Self::Nunito => with_scripts!("Nunito, system-ui, sans-serif"),
            Self::JetBrainsMono => with_scripts!(
                "JetBrains Mono, ui-monospace, Cascadia Mono, Menlo, Consolas, Segoe UI Symbol, Apple Symbols, Noto Sans Symbols 2, monospace"
            ),
            Self::SourceCodePro => with_scripts!(
                "Source Code Pro, ui-monospace, Cascadia Mono, Menlo, Consolas, Segoe UI Symbol, Apple Symbols, Noto Sans Symbols 2, monospace"
            ),
            Self::Mono => {
                with_scripts!(
                    "ui-monospace, Cascadia Mono, Menlo, Consolas, Segoe UI Symbol, Apple Symbols, Noto Sans Symbols 2, monospace"
                )
            }
            // The generic alone: fontique maps it to "System Font"/".SF NS" on
            // macOS and "Segoe UI" on Windows, so the arm needs no platform
            // names of its own.
            Self::System => with_scripts!("system-ui, sans-serif"),
        }
    }

    /// The weight to render this family at by default.
    ///
    /// **500, except for the mono class** — and the exception is not taste.
    /// vello does no gamma correction or stem darkening, so every face renders
    /// lighter here than the same face does in a native control; a real Medium
    /// is the main way back. The sans/serif/rounded families are variable, so
    /// fontique turns a request for 500 into a genuine `wght` axis position.
    ///
    /// The mono families ask for **400** even when bundled and variable:
    /// monospaced glyphs read heavy fast, and code is the thing rendered in
    /// them. `Mono` itself resolves to *system* faces with no `wght` axis, where
    /// 500 would instead set `embolden` and stroke a second pass — faux bold.
    /// See [`Self::weight_for`] for the heading case.
    pub fn weight(self) -> f32 {
        // **`System` asks for 400, not the sans 500**, for the `Mono` reason
        // in the paragraph above seen from the sans side: Segoe UI is a static
        // family, so 500 would set `embolden` and stroke a faux-bold second
        // pass — and 400 is what both platforms set their own chrome in, which
        // is the whole point of choosing the system face.
        if self == Self::System {
            return 400.0;
        }
        match self.class() {
            FontClass::Mono => 400.0,
            _ => 500.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Inter => "Inter",
            Self::SourceSans3 => "Source Sans",
            Self::PlusJakartaSans => "Jakarta",
            Self::IbmPlexSans => "Plex",
            Self::Poppins => "Poppins",
            Self::Manrope => "Manrope",
            Self::SourceSerif4 => "Source Serif",
            Self::Lora => "Lora",
            Self::Nunito => "Nunito",
            Self::JetBrainsMono => "JetBrains Mono",
            Self::SourceCodePro => "Source Code",
            Self::Mono => "System Mono",
            Self::System => "System",
        }
    }

    /// The name this is written as in the settings file. Stable — it survives a
    /// restart, so renaming one silently resets whoever had it chosen.
    pub fn key(self) -> &'static str {
        match self {
            Self::Inter => "inter",
            Self::SourceSans3 => "sourcesans3",
            Self::PlusJakartaSans => "plusjakartasans",
            Self::IbmPlexSans => "ibmplexsans",
            Self::Poppins => "poppins",
            Self::Manrope => "manrope",
            Self::SourceSerif4 => "sourceserif4",
            Self::Lora => "lora",
            Self::Nunito => "nunito",
            Self::JetBrainsMono => "jetbrainsmono",
            Self::SourceCodePro => "sourcecodepro",
            Self::Mono => "mono",
            Self::System => "system",
        }
    }

    /// An unknown name is the default rather than an error: the settings file is
    /// hand-editable and best-effort by design.
    pub fn parse(s: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|c| c.key() == s)
            .unwrap_or_default()
    }

    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|c| *c == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    /// The weight a block is drawn at: heavier for a heading, and the family's
    /// own default for body text.
    ///
    /// **Mono families get 700 at every heading level** rather than the ramp,
    /// and that is the same trap [`weight`](Self::weight) documents, seen from
    /// the other side. `Mono` resolves to *system* faces that ship 400 and 700
    /// and nothing between; the bundled variable monos could take the ramp, but
    /// asking for a weight that is really there is the only way to stay honest
    /// about `Mono`. The sans/serif/rounded families are variable and take the
    /// ramp verbatim.
    pub fn weight_for(self, heading: Option<u8>) -> f32 {
        match (self, heading) {
            (_, None) => self.weight(),
            (Self::Mono, Some(_)) => 700.0,
            // `System` takes the ramp: SF Pro is variable, and DirectWrite's
            // Segoe UI family carries real Semibold/Bold members for the
            // 600–700 the ramp asks for.
            (_, Some(level)) => HEADING_WEIGHTS[(level.clamp(1, 6) - 1) as usize],
        }
    }
}
/// A highlight colour by the name a document stores. Unknown names fall back to
/// the first swatch rather than vanishing — a colour this build does not know
/// about was still a deliberate highlight, and hiding it would lose the mark.
pub fn highlight(name: &str) -> Color {
    let set = p().highlights;
    set.iter()
        .find(|(n, _)| *n == name)
        .map(|(_, c)| *c)
        .unwrap_or(set[0].1)
}

/// The swatch names a picker offers, in order.
pub fn highlight_names() -> [&'static str; 4] {
    p().highlights.map(|(n, _)| n)
}

/// A workspace colour by the name the profile stores.
///
/// `None` for a name this build does not know — the opposite of
/// [`highlight`], and deliberately. A highlight falls back to the first swatch
/// because the alternative is losing a mark the user made in their text. A
/// workspace chip is an identity: showing the *wrong* colour would say this is a
/// different workspace than it is, which is worse than showing none. The caller
/// draws a neutral dot instead, and a build that knows the name draws it right.
pub fn workspace_color(name: &str) -> Option<Color> {
    p().workspace_colors
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, c)| *c)
}

/// The workspace swatch names, in the order the picker offers them.
pub fn workspace_color_names() -> [&'static str; 10] {
    p().workspace_colors.map(|(n, _)| n)
}

pub const FONT_SIZE: f32 = 16.0;
pub const LINE_HEIGHT: f32 = 1.5;

/// One step on the type scale: how big, how much leading, and how much space
/// opens above the block.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct TypeStep {
    pub size: f32,
    /// A multiple of [`size`](Self::size), the way [`LINE_HEIGHT`] is of
    /// [`FONT_SIZE`] — parley wants `LineHeight::FontSizeRelative`.
    pub line_height: f32,
    pub space_above: f32,
}

/// The heading ramp, H1 first.
///
/// Three numbers rather than one, because they do not move together. **Size**
/// steps by roughly a major third off the 16px body — 20, 24, 30 — and then
/// stops: H4 is one small notch above body and is carried by weight instead,
/// because a fifth size between 16 and 20 is a difference nobody sees. H5 is
/// body size and H6 is *below* it, which is what those levels are for.
/// **Leading tightens as size grows**: 30px at the body's 1.5 is a 45px line box
/// and reads double-spaced. **Space above is about the heading's own size** —
/// the classical rule — and there is none below, since the following
/// paragraph's own line box is already the gap.
const HEADINGS: [TypeStep; 6] = [
    TypeStep {
        size: 30.0,
        line_height: 1.25,
        space_above: 28.0,
    },
    TypeStep {
        size: 24.0,
        line_height: 1.30,
        space_above: 24.0,
    },
    TypeStep {
        size: 20.0,
        line_height: 1.35,
        space_above: 20.0,
    },
    TypeStep {
        size: 17.0,
        line_height: 1.45,
        space_above: 16.0,
    },
    TypeStep {
        size: 16.0,
        line_height: 1.50,
        space_above: 14.0,
    },
    TypeStep {
        size: 14.0,
        line_height: 1.50,
        space_above: 12.0,
    },
];

/// The `wght` axis position for each heading level, on a family that has the
/// axis. Body text's weight is the family's own — see [`FontChoice::weight`].
const HEADING_WEIGHTS: [f32; 6] = [700.0, 700.0, 650.0, 600.0, 600.0, 600.0];

/// The step a document's title row is drawn at.
///
/// A level on the ramp above rather than a size of its own, so the title sits
/// *on* the type scale instead of beside it. It is deliberately not H1: a
/// document that opens with an H1 heading of its own should not be competing
/// with its own name for the largest thing on the page. One constant, because
/// "H2 for now" is a decision somebody will want to revisit.
pub const TITLE_LEVEL: u8 = 2;

/// The air a document's title keeps between itself and the first paragraph.
///
/// Prose does not nest, so there is no depth to separate the name from what
/// follows it — this gap is the separation, and it belongs to the title the
/// way a heading's `space_above` belongs to the heading: the row after it
/// claims nothing.
pub const TITLE_GAP_BELOW: f32 = 40.0;

/// The step a block is set at: a heading level 1–6, or `None` for body text.
///
/// Keyed by the **level** rather than by a `Heading`, because this crate names
/// no nodes — `koru-core` is not a dependency and must not become one. An
/// out-of-range level cannot arrive (`Heading::from_level` clamps at the schema
/// boundary), but it clamps here too rather than panicking in a draw path.
pub fn type_step(heading: Option<u8>) -> TypeStep {
    match heading {
        Some(level) => HEADINGS[(level.clamp(1, 6) - 1) as usize],
        None => TypeStep {
            size: FONT_SIZE,
            line_height: LINE_HEIGHT,
            space_above: 0.0,
        },
    }
}

// ------------------------------------------------------------- the type system
//
// A two-layer system, the way Tailwind pairs a size scale with semantic names.
//
// Layer 1 is `text_metrics` — a `TextMetrics { size, line_height }` per tier,
// with absolute leading (not multipliers), so a tier carries its intended
// rhythm. Layer 2 is `role` — semantic names for *what a label is* (a hint, a
// row, the default UI, the document body), which is what call sites use. Roles
// return a `TextStyle` (metrics + a typed `FontWeight`), so a layout call never
// receives a bare `f32` size or a `weight_bold: bool`.
//
// `role::ui()` and `role::body()` are density-aware (they read `density()`);
// the rest are stable. See `docs/features/design/TYPOGRAPHY.md` for the full reference.

/// A size and an absolute line height, the way Tailwind pairs `text-sm` with
/// `leading`. A tier is a value of this type, not a bare `f32`.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct TextMetrics {
    pub size: f32,
    pub line_height: f32,
}

impl TextMetrics {
    pub const fn new(size: f32, line_height: f32) -> Self {
        Self { size, line_height }
    }
}

/// A typed font weight. Used in place of a bare `f32` so a call site reads
/// `FontWeight::SemiBold` rather than `600.0`. The numeric value is what
/// `push_axes` hands to parley at the one low-level seam.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum FontWeight {
    Regular,
    #[default]
    Medium,
    SemiBold,
    Bold,
}

impl FontWeight {
    pub const fn value(self) -> f32 {
        match self {
            Self::Regular => 400.0,
            Self::Medium => 500.0,
            Self::SemiBold => 600.0,
            Self::Bold => 700.0,
        }
    }
}

/// A complete chrome text style: metrics and a typed weight. Every role
/// returns one; `simple_layout` takes one.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct TextStyle {
    pub metrics: TextMetrics,
    pub weight: FontWeight,
}

impl TextStyle {
    pub const fn new(metrics: TextMetrics, weight: FontWeight) -> Self {
        Self { metrics, weight }
    }
    /// Override the weight, keeping the metrics (e.g. a Semibold title at the
    /// UI metrics).
    pub const fn with_weight(self, weight: FontWeight) -> Self {
        Self {
            metrics: self.metrics,
            weight,
        }
    }
}

/// Layer 1 — the size/leading scale.
///
/// Named by domain so the two "bases" cannot be confused: `UI_BODY` (the
/// default application UI, 14/20) and `DOC_BODY` (the document body, 16/24).
/// `DOC_BODY_COMPACT` and `DOC_BODY_COMFORTABLE` are the density variants of
/// the document body, named explicitly rather than inlined.
pub mod text_metrics {
    use super::TextMetrics;

    pub const MICRO: TextMetrics = TextMetrics::new(11.0, 14.0);
    pub const XS: TextMetrics = TextMetrics::new(12.0, 16.0);
    pub const SM: TextMetrics = TextMetrics::new(13.0, 18.0);
    pub const UI_BODY: TextMetrics = TextMetrics::new(14.0, 20.0);
    pub const DOC_BODY: TextMetrics = TextMetrics::new(16.0, 24.0);
    pub const DOC_BODY_COMPACT: TextMetrics = TextMetrics::new(14.0, 21.0);
    pub const DOC_BODY_COMFORTABLE: TextMetrics = TextMetrics::new(18.0, 27.0);
    pub const LG: TextMetrics = TextMetrics::new(18.0, 26.0);
    pub const XL: TextMetrics = TextMetrics::new(20.0, 28.0);
    pub const _2XL: TextMetrics = TextMetrics::new(24.0, 32.0);
    pub const _3XL: TextMetrics = TextMetrics::new(30.0, 38.0);
}

/// Layer 2 — semantic roles. Call sites name *what a label is*; the tier (and
/// thus the tuning knob) lives here.
///
/// `ui()` and `body()` are density-aware; the rest are stable. Chrome and body
/// both default to [`FontWeight::Medium`] (today's 500); `section_heading` is
/// [`FontWeight::SemiBold`].
pub mod role {
    use super::*;

    const MEDIUM: FontWeight = FontWeight::Medium;

    /// Keyboard hints, counts, timestamps, status indicators. 11/14.
    pub fn hint() -> TextStyle {
        TextStyle::new(text_metrics::MICRO, MEDIUM)
    }
    /// Metadata, breadcrumbs, secondary labels. 12/16.
    pub fn meta() -> TextStyle {
        TextStyle::new(text_metrics::XS, MEDIUM)
    }
    /// Sidebar rows, menus, buttons, tabs, compact inputs. 13/18.
    pub const fn row() -> TextStyle {
        TextStyle::new(text_metrics::SM, MEDIUM)
    }
    /// Large body, document subtitle. 18/26.
    pub fn subtitle() -> TextStyle {
        TextStyle::new(text_metrics::LG, MEDIUM)
    }
    /// A screen's title — *Welcome to Koru*. 24/32. Drawn through
    /// `koru_ui::KoruTextTitle`, so every title is this one size.
    pub fn title() -> TextStyle {
        TextStyle::new(text_metrics::_2XL, MEDIUM)
    }
    /// A section heading: 14/20 at [`FontWeight::SemiBold`]. Defined so the
    /// hierarchy is explicit (meta → row → section heading → subtitle); the
    /// welcome-tab headers are pointed at it in a follow-up, not by the
    /// migration.
    pub const fn section_heading() -> TextStyle {
        TextStyle::new(text_metrics::UI_BODY, FontWeight::SemiBold)
    }
    /// The default application UI — dialogs, settings, prominent controls.
    /// Compact steps it down to `SM` (13/18); Normal and Comfortable keep
    /// `UI_BODY` (14/20).
    pub fn ui() -> TextStyle {
        let m = match density() {
            Density::Compact => text_metrics::SM,
            Density::Normal | Density::Comfortable => text_metrics::UI_BODY,
        };
        TextStyle::new(m, MEDIUM)
    }
    /// Fixed-grid terminal cells. It follows application density but remains
    /// regular weight; SGR owns the four typed face combinations.
    pub fn terminal() -> TextStyle {
        let m = match density() {
            Density::Compact => text_metrics::SM,
            Density::Normal | Density::Comfortable => text_metrics::UI_BODY,
        };
        TextStyle::new(m, FontWeight::Regular)
    }
    /// The document body. Compact 14/21, Normal 16/24, Comfortable 18/27.
    pub fn body() -> TextStyle {
        let m = match density() {
            Density::Compact => text_metrics::DOC_BODY_COMPACT,
            Density::Normal => text_metrics::DOC_BODY,
            Density::Comfortable => text_metrics::DOC_BODY_COMFORTABLE,
        };
        TextStyle::new(m, MEDIUM)
    }
}

/// How a document lays out its content column. Documents only — the task
/// outline is always [`ViewMode::Full`], so the shared editor never enters a
/// page mode on the outline's behalf.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum ViewMode {
    /// Content on a centred page sheet over a grey canvas — the Google
    /// Docs/Word look.
    Page,
    /// Pageless: content clamped to the page width and centred, but no sheet
    /// and a uniform background.
    Narrow,
    /// Pageless and unrestricted: content fills the viewport. Today's
    /// behaviour, and the outline's only mode.
    #[default]
    Full,
}

/// The width of the page sheet in Page mode and the content column in Narrow.
/// A constant for now — paper sizes (A4, A3, Letter, custom) live in
/// `BACKLOG.md`.
pub const PAGE_W: f32 = 820.0;
/// Grey canvas between the toolbar and the page sheet (and between the sheet
/// and the status bar) in Page mode. The rows' `MARGIN_TOP` is then the
/// inside-page top padding, so the first line sits below it.
pub const PAGE_GAP: f32 = 16.0;
/// The page sheet's corner radius. Matches the compositor's window radius
/// (`DWMWCP_ROUND`, 8px on Windows 11) so the page rounds the same as the
/// window that holds it — see `crates/koru-window/src/frame.rs`.
pub const PAGE_RADIUS: f32 = 8.0;

/// The canvas-colour presets the swatch cycles, per theme. The canvas is the
/// area *around* the page, so every preset is darker than the page surface
/// (`bg`) by a visible margin — the page reads as sitting on it. The dark set
/// mirrors the light set's contrast: light presets sit ~20-30 below white, dark
/// presets ~10-15 below the dark `bg` (`0x19`), so neither theme has a canvas
/// that disappears into the page (which is what happened when a dark preset
/// landed lighter than the page).
pub const CANVAS_PRESETS_LIGHT: [[u8; 4]; 3] = [
    [0xE8, 0xE9, 0xED, 0xFF], // cool grey
    [0xE9, 0xE2, 0xD3, 0xFF], // warm cream
    [0xE0, 0xE7, 0xE5, 0xFF], // soft slate
];
pub const CANVAS_PRESETS_DARK: [[u8; 4]; 3] = [
    [0x0C, 0x0C, 0x0E, 0xFF], // cool dark
    [0x10, 0x0C, 0x09, 0xFF], // warm dark
    [0x0A, 0x0E, 0x12, 0xFF], // slate dark
];

/// The canvas-colour presets for the active theme.
pub fn canvas_presets() -> &'static [[u8; 4]; 3] {
    if is_dark() {
        &CANVAS_PRESETS_DARK
    } else {
        &CANVAS_PRESETS_LIGHT
    }
}

impl ViewMode {
    /// The width rows lay out against: the page width in Page/Narrow, the whole
    /// viewport in Full. Rows are laid out in this width starting at `x = 0`;
    /// [`content_x`](Self::content_x) is what centres the column at draw time.
    pub fn effective_w(self, vw: f32) -> f32 {
        match self {
            Self::Full => vw,
            Self::Page | Self::Narrow => vw.min(PAGE_W),
        }
    }

    /// How far the centred content column is inset from the viewport's left
    /// edge. Zero in Full.
    pub fn content_x(self, vw: f32) -> f32 {
        match self {
            Self::Full => 0.0,
            Self::Page | Self::Narrow => ((vw - self.effective_w(vw)) * 0.5).max(0.0),
        }
    }
}

/// Left edge of the page margin — where the affordance lane begins.
pub const MARGIN_X: f32 = 28.0;
/// The lane between the page margin and the checkbox column, reserved for a
/// row's affordances: the insert `+` and the drag grip, in that order.
///
/// It exists because those two do not fit in `MARGIN_X` alone. The grip sits at
/// `checkbox_x - GRIP_W - 6`, which is x ∈ [10, 22] without this — leaving the
/// `+` nowhere to go but negative x, which is off the viewport in Full mode
/// rather than merely in the margin. Folded into [`geometry::text_x`] and
/// [`geometry::checkbox_x`] so there is still one arithmetic, and subtracted
/// from the right in `rowlayout::build_row` so the column stays symmetric.
///
/// [`geometry::text_x`]: crate::geometry::text_x
/// [`geometry::checkbox_x`]: crate::geometry::checkbox_x
pub const AFFORDANCE_W: f32 = 28.0;
/// How far the block-selection chip extends past the text column, on both sides.
///
/// Named because drawing the chip and placing the title's trailing action both
/// need this edge; a local `6.0` in one of them is how the icon sat past the
/// highlight.
pub const ROW_SELECTED_PAD: f32 = 6.0;
/// A square affordance in that lane — the insert `+`, and the title row's
/// page-settings `…`.
pub const AFFORDANCE_BTN: f32 = 20.0;
/// Distance from the checkbox column to the text column.
pub const GUTTER: f32 = 24.0;
/// Horizontal offset added per indent level.
pub const INDENT_W: f32 = 24.0;
pub const MARGIN_TOP: f32 = 24.0;
/// Extra vertical breathing room between tasks.
pub const TASK_GAP: f32 = 2.0;

pub const CHECKBOX_SIZE: f32 = 13.0;
/// Radius of a list bullet, centred on the line's body height.
pub const BULLET_R: f32 = 2.4;
/// Gap between the right edge of a list marker (a number, letter or roman) and
/// the text column. A bullet ignores it — it is centred, not right-aligned.
pub const MARKER_GAP: f32 = 6.0;
/// Drag handle sits in the margin, left of the checkbox column.
/// Every border chrome draws: a panel, a menu, a text field, a page sheet, a
/// swatch, a chip.
///
/// **One number, because a border is one idea.** The panels were at 1.0 and the
/// fields inside them at 1.2, which is not a distinction anybody reads as
/// deliberate — it reads as a heavy box inside a light one. What separates a
/// field from a panel edge is its *colour* (`focus_ring` against
/// `panel_border`), not its weight.
///
/// The strokes that are deliberately *not* borders stay where they are: a
/// selected image's outline is a 2px indicator, and the link glyph's 1.1 is
/// part of an icon rather than a frame around one.
pub const BORDER_W: f64 = 1.0;

/// The outline of a control — a secondary button, a text input's box — at
/// half of [`BORDER_W`].
///
/// At 1.0 a control's outline read heavier on a scaled Windows desktop than
/// in the design it follows: at 150% a logical pixel is a pixel and a half,
/// and a stroke on a half-pixel lands across two device pixels. Half lands on
/// about one, which is the design's hairline. Panels keep `BORDER_W`; what
/// tells a control from a panel edge is still its colour, `control_border`.
pub const CONTROL_BORDER_W: f64 = 0.5;

/// The air between a code block's tint and the code inside it.
///
/// Applied on all four sides — the panel is drawn `CODE_PAD` beyond the glyphs
/// each way, and `reflow` adds the same above and below so the paragraphs
/// around it do not end up inside it.
pub const CODE_PAD: f32 = 8.0;
/// How far a code block's panel reaches past its glyphs sideways.
///
/// Less than [`CODE_PAD`]: the drag grip ends six pixels before the text,
/// and a panel reaching the full pad to the left sat two pixels over it.
/// Four puts the edge between the grip and the first glyph, with air on
/// both sides; the right edge keeps the same reach so the panel is
/// symmetric. Above and below, nothing sits that close, so the vertical pad
/// stays `CODE_PAD`.
pub const CODE_PAD_X: f32 = 4.0;
/// Vertical air above and below an image block.
///
/// An image is opaque, so without this it paints over anything that hangs past
/// the previous row's line box — a link pill's descent, its selection ring.
/// `reflow` adds it the same way as [`CODE_PAD`]: once above the row and once
/// after it, so neighbours never sit flush against the picture.
pub const IMAGE_PAD: f32 = 8.0;
/// Air between a diagram block's panel and the rows around it, outside the
/// `CODE_PAD` the panel keeps inside its border.
///
/// A code block's neighbours sit `TASK_GAP` from its border, and its text
/// starts another `CODE_PAD` in, so the border reads as part of the block.
/// A drawn diagram is a figure, the way an image is: a bordered picture two
/// pixels under a paragraph's descenders reads as pressed against it.
/// `reflow` adds this the way it adds [`IMAGE_PAD`], once above and once
/// below, so the block keeps the same distance from both neighbours.
pub const DIAGRAM_AIR: f32 = 8.0;
/// Quiet air on each vertical side of a rendered table.
///
/// Four pixels per side keeps a table attached to the surrounding prose while
/// ensuring two adjacent tables have an eight-pixel boundary. This is outside
/// the row's measured height, so selection and hit-testing still end at the
/// table border rather than including invisible padding.
pub const TABLE_PAD: f32 = 4.0;
pub const GRIP_W: f32 = 12.0;
pub const GRIP_H: f32 = 14.0;
pub const CARET_W: f32 = 0.8;
/// Fraction of the typographic ink height the caret uses.
///
/// Kept because a test still measures against it, and because it names the
/// height the caret ends up near. The rect itself is now built from the two
/// constants below, each anchored to the **baseline** rather than one of them
/// being anchored to the descender — see [`caret_rect`].
pub const CARET_H_SCALE: f64 = 0.72;

/// How much of the (font) ascent the caret rises above the baseline.
///
/// Ascent fed into [`caret_rect`] is already typographic (cap height + a
/// touch, not the strut-inflated line box), so this stays near 1.0.
pub const CARET_ASCENT_SCALE: f64 = 1.0;

/// How far below the baseline it drops, as a fraction of font descent.
///
/// A small foot under the baseline (OneNote-shaped), not the full `g`/`p`
/// descender — the bar should read as "a bit taller than a capital", not the
/// full typographic ink box.
pub const CARET_DESCENT_SCALE: f64 = 0.30;

/// Caret geometry from a parley cursor box and active-run metrics.
///
/// `rise` is capHeight (preferred) or ascent. Tall bar:
///
/// ```text
/// tallTop    = baseline - rise - descent
/// tallBottom = baseline + descent
/// tallHeight = rise + 2 × descent
/// ```
///
/// Medium/Short = that height × 0.85 / 0.70, scaled about **Tall’s center**
/// (not the baseline). Never includes line gap. Selection height is independent
/// (parley line boxes). Optional `slant_deg` makes a parallelogram.
pub fn caret_rect(
    bb: BoundingBox,
    baseline: f32,
    rise: f32,
    descent: f32,
    dx: f32,
    dy: f32,
) -> Rect {
    caret_bounds(bb, baseline, rise, descent, dx, dy, 0.0)
}

/// Axis-aligned bounds of the caret (useful for hit/scroll); same as
/// [`caret_rect`] when upright.
pub fn caret_bounds(
    bb: BoundingBox,
    baseline: f32,
    rise: f32,
    descent: f32,
    dx: f32,
    dy: f32,
    slant_deg: f64,
) -> Rect {
    let (y0, y1, x_bot, _x_top, w) = caret_metrics(bb, baseline, rise, descent, dx, dy, slant_deg);
    let shift = (y1 - y0) * slant_tan(slant_deg);
    let x_top = x_bot + shift;
    let x0 = x_bot.min(x_top);
    let x1 = (x_bot + w).max(x_top + w);
    Rect::new(x0, y0, x1, y1)
}

fn slant_tan(deg: f64) -> f64 {
    if deg.abs() < 0.01 {
        0.0
    } else {
        deg.to_radians().tan()
    }
}

fn caret_metrics(
    bb: BoundingBox,
    baseline: f32,
    rise: f32,
    descent: f32,
    dx: f32,
    dy: f32,
    _slant_deg: f64,
) -> (f64, f64, f64, f64, f64) {
    let x = bb.x0 + dx as f64;
    let w = caret_w() as f64;
    let base = baseline as f64 + dy as f64;
    let rise = rise as f64;
    let descent = descent as f64;
    // Tall: one full descent above the cap (or ascent) and one below baseline.
    let tall_top = base - rise - descent;
    let tall_bot = base + descent;
    let (y0, y1) = if tall_bot > tall_top + 0.5 {
        let tall_h = tall_bot - tall_top;
        let tall_c = (tall_top + tall_bot) * 0.5;
        let h = tall_h * caret_height().scale();
        let top = tall_c - h * 0.5;
        let bot = tall_c + h * 0.5;
        (top.round(), bot.round().max(top.round() + 1.0))
    } else {
        (
            (bb.y0 + dy as f64).round(),
            (bb.y1 + dy as f64)
                .round()
                .max((bb.y0 + dy as f64).round() + 1.0),
        )
    };
    let line_top = (bb.y0 + dy as f64).min(y0);
    let line_bot = (bb.y1 + dy as f64).max(y1);
    let y0 = y0.clamp(line_top, line_bot);
    let y1 = y1.clamp(y0 + 1.0, line_bot.max(y0 + 1.0));
    (y0, y1, x, x, w)
}

/// Four corners of the caret bar as a parallelogram (TL, TR, BR, BL), for
/// slanted italic carets. Upright when `slant_deg ≈ 0`.
pub fn caret_quad(
    bb: BoundingBox,
    baseline: f32,
    ascent: f32,
    descent: f32,
    dx: f32,
    dy: f32,
    slant_deg: f64,
) -> [(f64, f64); 4] {
    let (y0, y1, x_bot, _, w) = caret_metrics(bb, baseline, ascent, descent, dx, dy, slant_deg);
    // CSS-style: top shifts right for positive italic angle (letters lean right).
    let shift = (y1 - y0) * slant_tan(slant_deg);
    let x_top = x_bot + shift;
    [(x_top, y0), (x_top + w, y0), (x_bot + w, y1), (x_bot, y1)]
}

pub const PILL_FONT_SIZE: f32 = 13.0;
pub const PILL_PAD_X: f32 = 7.0;
/// Vertical inset inside a pill: above the **ascent line** and below the
/// **descent line** (not the line box). Height is
/// `2 × pad + ascent + descent`.
pub const PILL_PAD_Y: f32 = 2.0;
/// Icon slot beside a pill label. Sized to the label's body, not the pill
/// chrome — at the label's 13 px it is a mark next to the word, not a badge
/// competing with it.
pub const PILL_ICON_W: f32 = 8.0;
pub const PILL_ICON_GAP: f32 = 4.0;
/// Widest a link pill may draw. A URL is an opaque string that an inline box
/// cannot wrap, so an unbounded one overflows the line; longer than this and
/// the pill shows the host (scheme and path dropped) instead. The full URL
/// stays one hover away.
pub const LINK_PILL_MAX_W: f32 = 300.0;

/// Height of the bottom status bar.
pub const STATUS_H: f32 = 26.0;

/// Drawn width of the overlay scrollbar thumb. Fixed — hover darkens the tone
/// ([`scrollbar_thumb_color`]), it does not fatten the bar. Hit testing uses the
/// wider [`SCROLLBAR_GUTTER_W`] lane.
pub const SCROLLBAR_W: f32 = 4.0;
/// Clickable column for the scrollbar. Wider than the drawn thumb so it is
/// easy to grab without a fat bar on the page. Opens *inward* from the thumb.
pub const SCROLLBAR_GUTTER_W: f32 = 10.0;
/// Gap between the drawn thumb and the viewport's right edge. Small on purpose
/// so the bar sits close to the window frame; the hit gutter still opens
/// *inward*. The shell's resize band is 6px and wins only the outermost pixels
/// — a 2px clear leaves almost no empty stripe beside the thumb.
pub const SCROLLBAR_EDGE_CLEAR: f32 = 2.0;
pub const SCROLLBAR_MIN_THUMB: f32 = 28.0;

/// Scrollbar thumb colour: at rest, or clearly stronger while hovered/dragged.
///
/// Rest thumbs are low-alpha greys; multiplying RGB alone is nearly invisible.
/// Hover raises opacity and darkens RGB so the change reads as "active" without
/// a grab cursor or a wider bar.
pub fn scrollbar_thumb_color(active: bool) -> Color {
    let c = p().scrollbar_thumb;
    if !active {
        return c;
    }
    let [r, g, b, a] = c.components;
    Color::new([r * 0.7, g * 0.7, b * 0.7, (a * 2.0).min(0.95)])
}

pub const PANEL_W: f32 = 258.0;
pub const PANEL_PAD: f32 = 6.0;

/// Vertical breathing room above + below a row label inside a menu/list row.
/// Named, not a magic residue, so the row height reads as "label + padding".
const ROW_PAD_V: f32 = 8.0;
/// A menu/list row = the row label's line height + breathing room. Tracks
/// `role::row()` (`text_metrics::SM`, 13/18), so it stays in lockstep with the
/// row tier; `role::row()` is density-independent, so this does not move with
/// density. A pinned test fails loudly if `SM` ever moves.
pub const ROW_H: f32 = text_metrics::SM.line_height + ROW_PAD_V; // 18 + 8 = 26

// ------------------------------------------------------------- view container
//
// The window is cut into four regions: a caption row carrying the tabs and the
// window buttons, a collapsible navigation column on the left, the active tab's
// view, and a collapsible context column on the right. Per-site constants rather
// than a scale, the same as the toolbar above.

/// The caption row: tabs and the window buttons share it, so it is taller than
/// a toolbar and about the height of a native title bar.
///
/// A literal, **not** derived from a text tier: it is sized by the window
/// buttons, the drag area, and the tab shape, not by the label inside it. Do
/// not rewrite it as label-metrics + pad — the exemption is on purpose.
pub const CAPTION_H: f32 = 36.0;
/// A window button (minimise, maximise, close). Square, and full caption height
/// so the pointer can slam into the top-right corner and still hit close.
pub const CAPTION_BTN: f32 = 44.0;

/// Width of each side column when open. Zero when collapsed — a collapsed panel
/// is a zero-width rect rather than an absent one, so every caller does the same
/// arithmetic either way.
///
/// **240 so the explore heading holds seven mode icons and its search button.**
/// Seven icons at a 26 px pitch end at 188 px; the `…` and the search icon need
/// 24 px each beyond that, so 232 — the width before Canvases became the seventh
/// mode — dropped the search button at the default width.
pub const SIDEBAR_W: f32 = 240.0;
pub const CONTEXT_W: f32 = 268.0;
/// The narrowest the active view may become before a side column is dropped.
/// Content is what the window is for; the panels give way first.
pub const CONTENT_MIN_W: f32 = 320.0;
/// How long a side column takes to slide in or out.
pub const PANEL_SLIDE_MS: f32 = 140.0;

/// Icons in the caption row — the tab close box, the `+`, the two panel
/// toggles and the window buttons.
///
/// Smaller than [`ICON_SIZE`]: the caption is a title bar, where an icon is a
/// small affordance beside a label, rather than a command surface where it is
/// the whole control.
pub const CAPTION_ICON: f32 = 15.0;

/// The optical inset either side of a caption's contents.
///
/// A caption button is [`CAPTION_BTN`] wide with a [`CAPTION_ICON`] mark
/// centred in it, so the mark floats this far from the button's edge — and the
/// rightmost button's edge *is* the window's. A title set flush at `MARGIN_X`
/// therefore sat at a different distance from its edge than the icon did from
/// the other, which reads as a lopsided bar however carefully each side was
/// chosen on its own. Derived rather than typed, so it follows if either
/// constant moves.
pub const CAPTION_PAD_X: f32 = (CAPTION_BTN - CAPTION_ICON) * 0.5;

/// How long the caption's window-menu button takes to turn into an ✕, and back.
///
/// Shorter than [`PANEL_SLIDE_MS`] for the reason [`TAB_SLIDE_MS`] is: a glyph
/// turning about its own centre travels no distance, so the same duration reads
/// as hesitation rather than motion.
pub const GLYPH_MORPH_MS: f32 = 110.0;

/// Cadence shared by indicators that breathe while work is pending.
pub const PULSE_LOOP_MS: f32 = 1200.0;

/// Preferred tab width clamps. Each tab is measured from its label in the
/// current UI font, then clamped here. Below the minimum the strip scrolls
/// rather than shrinking further; above the maximum the label truncates.
pub const TAB_MIN_W: f32 = 96.0;
pub const TAB_MAX_W: f32 = 200.0;
pub const TAB_PINNED_W: f32 = 36.0;
pub const TAB_PIN_GAP: f32 = 6.0;
pub const TAB_GAP: f32 = 2.0;
/// The close box inside a tab, and the `+` that follows the last one.
pub const TAB_CLOSE: f32 = 16.0;
pub const TAB_NEW_W: f32 = 28.0;
/// Horizontal overflow thumb under the tab strip. Same hairline as
/// [`SCROLLBAR_W`]. Drawn only while the pointer is over the strip.
pub const TAB_SCROLLBAR_H: f32 = SCROLLBAR_W;

/// How far the tab *pill* is inset inside the caption row, top and bottom.
///
/// The pill is what is drawn and what the label centres in; the tab's hit area
/// stays the full row, so the pointer never has to find the pill. Keeping the
/// two apart is what makes the label's padding even — centring it in the row
/// while drawing the pill lower is what made the text look like it was sitting
/// on the floor of its tab.
pub const TAB_INSET_TOP: f32 = 4.0;
/// Deeper than the top by the caption's hairline plus a visible gap, so a tab
/// never sits *on* the line that separates the caption from the content.
pub const TAB_INSET_BOTTOM: f32 = 5.0;

/// How long a tab takes to slide into a new slot after a reorder.
///
/// Shorter than [`PANEL_SLIDE_MS`]: a column crossing 232 px reads as travel,
/// while a tab moving one slot is a nudge — the same duration over that
/// distance would feel like treacle.
pub const TAB_SLIDE_MS: f32 = 110.0;

/// How long an explore row takes to slide into a new slot during a drag.
///
/// The same duration as [`TAB_SLIDE_MS`]: a row moving one slot is the same
/// nudge a tab is, and two motions in one window should not move to different
/// rhythms.
pub const ROW_SLIDE_MS: f32 = 110.0;

/// How close to the top or bottom of the explore column the pointer may sit
/// before a drag in flight auto-scrolls the list, and the fastest it scrolls
/// when it does. Scaled by closeness, so a pointer on the very edge runs at the
/// max and one near the inner edge barely moves.
pub const AUTO_SCROLL_BAND: f32 = 40.0;
pub const AUTO_SCROLL_MAX_SPEED: f32 = 600.0;

/// How long a popover takes to appear or go, and how far it rises into place.
///
/// **Shorter than everything else that moves**, and deliberately so: a menu is
/// answering a click, and anything you can perceive as travel between clicking
/// and reading is a menu that feels slow. The rise is small for the same reason
/// — it is there to say "this came from the button", not to be watched.
///
/// The chevron's turn shares the duration, because it is the same event seen
/// from the other end.
pub const POPOVER_FADE_MS: f32 = 90.0;

pub mod motion;
pub mod overlay;

#[cfg(test)]
mod token_guards;

pub const POPOVER_RISE: f32 = 5.0;

/// One row of the explore tree or a context list, and the width one level of
/// nesting adds.
///
/// `NAV_ROW_H` is derived: the row label's line height plus a tighter vertical
/// pad than a menu row. Tracks `role::row()` (`text_metrics::SM`); like
/// [`ROW_H`] it does not move with density.
const NAV_PAD_V: f32 = 6.0;
pub const NAV_ROW_H: f32 = text_metrics::SM.line_height + NAV_PAD_V; // 18 + 6 = 24
pub const NAV_INDENT: f32 = 14.0;
/// Padding inside a side column.
pub const NAV_PAD: f32 = 8.0;

// ---------------------------------------------------------------- picker data

pub struct Contact {
    pub name: &'static str,
    pub role: &'static str,
}

pub const CONTACTS: &[Contact] = &[
    Contact {
        name: "Ben",
        role: "Growth",
    },
    Contact {
        name: "Mia",
        role: "Design",
    },
    Contact {
        name: "Raj",
        role: "Engineering",
    },
    Contact {
        name: "Kim",
        role: "Marketing",
    },
    Contact {
        name: "Sam",
        role: "Legal",
    },
    Contact {
        name: "Ava",
        role: "Data",
    },
    Contact {
        name: "Noah",
        role: "Support",
    },
];

pub const WEEKDAYS: [&str; 7] = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];

#[cfg(test)]
mod tests {
    use super::*;

    /// The defaults live twice — index consts for the atomics, value functions
    /// for `Settings::default` — and a variant inserted mid-list would move
    /// every index. Both hazards, one test.
    #[test]
    fn defaults_and_indices_agree() {
        assert_eq!(FontChoice::ALL[INTER as usize], FontChoice::Inter);
        assert_eq!(FontChoice::ALL[MANROPE as usize], FontChoice::Manrope);
        assert_eq!(
            FontChoice::ALL[JETBRAINS_MONO as usize],
            FontChoice::JetBrainsMono
        );
        assert_eq!(FontChoice::ALL[SYSTEM as usize], FontChoice::System);
        // The UI atomic's literal initial value is default_ui_font's index.
        let ui_index = FontChoice::ALL
            .iter()
            .position(|c| *c == default_ui_font())
            .unwrap();
        assert_eq!(
            ui_index as u8,
            if cfg!(target_os = "macos") { 12 } else { 0 }
        );
        assert_eq!(default_body_font(), FontChoice::ALL[INTER as usize]);
        assert_eq!(default_headings_font(), FontChoice::ALL[MANROPE as usize]);
        assert_eq!(
            default_code_font(),
            FontChoice::ALL[JETBRAINS_MONO as usize]
        );
    }

    /// Every choice keeps a stable settings key and a drawable stack, and the
    /// keys stay unique — `parse` answers the wrong family the day two collide.
    #[test]
    fn every_font_choice_round_trips_its_key() {
        let mut seen = std::collections::HashSet::new();
        for choice in FontChoice::ALL {
            assert_eq!(FontChoice::parse(choice.key()), choice);
            assert!(
                seen.insert(choice.key()),
                "{} is two families",
                choice.key()
            );
            assert!(!choice.stack().is_empty());
            assert!(!choice.label().is_empty());
        }
    }

    /// `density()` is a process global, so tests that flip it take this lock to
    /// avoid racing each other under cargo's parallel runner — the same shape
    /// the shell's theme tests use (`THEME_LOCK`).
    static DENSITY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_density() -> std::sync::MutexGuard<'static, ()> {
        DENSITY_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The size scale is monotonic: each tier's size and line height step up.
    /// `DOC_BODY_COMPACT` and `DOC_BODY_COMFORTABLE` are density variants of
    /// the document body, checked separately rather than as rungs on the ladder.
    #[test]
    fn text_metrics_form_a_monotonic_scale() {
        use text_metrics::*;
        let ladder = [MICRO, XS, SM, UI_BODY, DOC_BODY, LG, XL, _2XL, _3XL];
        for w in ladder.windows(2) {
            assert!(w[0].size < w[1].size, "size must increase across the scale");
            assert!(
                w[0].line_height < w[1].line_height,
                "line height must increase across the scale"
            );
        }
        // Document-body density variants: compact is smaller than normal,
        // comfortable is larger.
        assert!(DOC_BODY_COMPACT.size < DOC_BODY.size);
        assert!(DOC_BODY.size < DOC_BODY_COMFORTABLE.size);
    }

    /// The four named weights map to the numbers `push_axes` hands to parley.
    #[test]
    fn fontweight_values() {
        assert_eq!(FontWeight::Regular.value(), 400.0);
        assert_eq!(FontWeight::Medium.value(), 500.0);
        assert_eq!(FontWeight::SemiBold.value(), 600.0);
        assert_eq!(FontWeight::Bold.value(), 700.0);
    }

    /// `with_weight` overrides only the weight, leaving the metrics intact.
    #[test]
    fn with_weight_keeps_the_metrics() {
        let s = role::ui().with_weight(FontWeight::Bold);
        assert_eq!(s.metrics, role::ui().metrics);
        assert_eq!(s.weight, FontWeight::Bold);
    }

    /// The density-aware roles land on the documented tiers for each mode, and
    /// the stable roles never move.
    #[test]
    fn roles_match_their_documented_tiers() {
        let _g = lock_density();
        // Stable roles.
        assert_eq!(role::hint().metrics, text_metrics::MICRO);
        assert_eq!(role::meta().metrics, text_metrics::XS);
        assert_eq!(role::row().metrics, text_metrics::SM);
        assert_eq!(role::subtitle().metrics, text_metrics::LG);
        assert_eq!(role::section_heading().metrics, text_metrics::UI_BODY);
        assert_eq!(role::section_heading().weight, FontWeight::SemiBold);

        for d in [Density::Compact, Density::Normal, Density::Comfortable] {
            set_density(d);
            match d {
                Density::Compact => {
                    assert_eq!(role::ui().metrics, text_metrics::SM);
                    assert_eq!(role::terminal().metrics, text_metrics::SM);
                    assert_eq!(role::body().metrics, text_metrics::DOC_BODY_COMPACT);
                }
                Density::Normal => {
                    assert_eq!(role::ui().metrics, text_metrics::UI_BODY);
                    assert_eq!(role::terminal().metrics, text_metrics::UI_BODY);
                    assert_eq!(role::body().metrics, text_metrics::DOC_BODY);
                }
                Density::Comfortable => {
                    assert_eq!(role::ui().metrics, text_metrics::UI_BODY);
                    assert_eq!(role::terminal().metrics, text_metrics::UI_BODY);
                    assert_eq!(role::body().metrics, text_metrics::DOC_BODY_COMFORTABLE);
                }
            }
        }
        // Restore the default so this test does not leak density into others.
        set_density(Density::Normal);
    }

    /// Cycling density moves `body()` every step and `ui()` only Compact↔
    /// non-Compact; the stable roles never move.
    #[test]
    fn density_switch_moves_ui_and_body_only() {
        let _g = lock_density();
        let stable = [
            role::hint().metrics,
            role::meta().metrics,
            role::row().metrics,
            role::subtitle().metrics,
            role::section_heading().metrics,
        ];

        set_density(Density::Compact);
        let ui_c = role::ui().metrics;
        let body_c = role::body().metrics;

        set_density(Density::Normal);
        let ui_n = role::ui().metrics;
        let body_n = role::body().metrics;

        set_density(Density::Comfortable);
        let ui_f = role::ui().metrics;
        let body_f = role::body().metrics;

        // body changes at every step.
        assert_ne!(body_c, body_n);
        assert_ne!(body_n, body_f);
        assert_ne!(body_c, body_f);
        // ui differs only between Compact and the others.
        assert_ne!(ui_c, ui_n);
        assert_eq!(ui_n, ui_f);
        // Stable roles never moved (re-read after cycling).
        assert_eq!(
            [
                role::hint().metrics,
                role::meta().metrics,
                role::row().metrics,
                role::subtitle().metrics,
                role::section_heading().metrics,
            ],
            stable
        );
        set_density(Density::Normal);
    }

    /// Row heights are derived from the row-label tier plus named padding, and
    /// they pin to today's values. If `text_metrics::SM` ever moves, this fails
    /// loudly so the new geometry is re-accepted deliberately rather than
    /// silently shifting scroll math, culling, and hit targets.
    #[test]
    fn row_heights_pinned() {
        assert_eq!(ROW_H, 26.0, "ROW_H must stay 26 until SM is re-tuned");
        assert_eq!(
            NAV_ROW_H, 24.0,
            "NAV_ROW_H must stay 24 until SM is re-tuned"
        );
        // And the derivation is honest: SM's line height (18) + the named pads.
        assert_eq!(ROW_H, text_metrics::SM.line_height + 8.0);
        assert_eq!(NAV_ROW_H, text_metrics::SM.line_height + 6.0);
    }

    #[test]
    fn terminal_palette_resolves_xterm_cube_and_greyscale() {
        assert_eq!(terminal_indexed(16), rgb(0, 0, 0));
        assert_eq!(terminal_indexed(21), rgb(0, 0, 255));
        assert_eq!(terminal_indexed(196), rgb(255, 0, 0));
        assert_eq!(terminal_indexed(231), rgb(255, 255, 255));
        assert_eq!(terminal_indexed(232), rgb(8, 8, 8));
        assert_eq!(terminal_indexed(255), rgb(238, 238, 238));
    }

    /// The grid and the bounds, which four call sites used to each write out
    /// for themselves. Pure arithmetic — no global is touched, so this cannot
    /// race a test running beside it.
    #[test]
    fn a_zoom_factor_snaps_to_the_step_grid_and_stops_at_the_bounds() {
        assert_eq!((ZOOM_MIN, ZOOM_MAX, ZOOM_STEP), (0.5, 2.0, 0.1));
        assert_eq!(clamp_zoom(3.0), ZOOM_MAX, "above the ceiling");
        assert_eq!(clamp_zoom(0.1), ZOOM_MIN, "below the floor");
        assert_eq!(clamp_zoom(1.0), 1.0);
        // The drift a repeated `+ 0.1` accumulates, snapped back onto the grid.
        assert_eq!(zoom_percent(clamp_zoom(1.1999999)), "120%");
        assert_eq!(
            clamp_zoom(f32::NAN),
            1.0,
            "a factor that is not a number is not a zoom"
        );
    }

    /// A control at the end of its range is dim, and "the end" is asked on the
    /// grid rather than with a bare comparison against a repeated float sum.
    #[test]
    fn the_ends_of_the_zoom_range_are_answered_on_the_grid() {
        assert!(!can_zoom_out(ZOOM_MIN));
        assert!(can_zoom_in(ZOOM_MIN));
        assert!(can_zoom_out(ZOOM_MAX));
        assert!(!can_zoom_in(ZOOM_MAX));
        assert!(!can_zoom_out(0.4), "already past the floor");
        assert!(!can_zoom_in(2.4), "already past the ceiling");
        assert!(can_zoom_out(1.0) && can_zoom_in(1.0));
    }

    /// One percentage, three readers — the shell's flash, Koru's own, and the
    /// window menu's readout. Integer, because the grid is 10%.
    #[test]
    fn a_zoom_reads_as_a_whole_percentage() {
        assert_eq!(zoom_percent(0.5), "50%");
        assert_eq!(zoom_percent(1.0), "100%");
        assert_eq!(zoom_percent(1.25), "125%");
        assert_eq!(zoom_percent(2.0), "200%");
        assert_eq!(zoom_percent(1.12000001), "112%", "no rounding artefact");
    }

    #[test]
    fn terminal_rejects_a_proportional_code_preference() {
        let original = code_font();
        set_code_font(FontChoice::Inter);
        assert_eq!(terminal_code_font(), FontChoice::JetBrainsMono);
        set_code_font(FontChoice::SourceCodePro);
        assert_eq!(terminal_code_font(), FontChoice::SourceCodePro);
        set_code_font(original);
    }
}
