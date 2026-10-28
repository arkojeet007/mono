// SPDX-License-Identifier: MPL-2.0

//! Phosphor icon glyphs, by name.
//!
//! Icons are a **font**, not SVG: `assets/fonts/Phosphor.ttf` is fetched like
//! the text faces and registered into the same font collection, so an icon is
//! a character parley shapes and vello draws. No rasteriser, no second asset
//! pipeline, and an icon inherits the colour of the text around it.
//!
//! The codepoints below are lifted from the `@phosphor-icons/web` stylesheet
//! and are stable for a given release — `scripts/setup/fetch-fonts.sh` pins the
//! version that produced them. They live in the Private Use Area, so they carry
//! no meaning outside this font: rendering one without Phosphor registered
//! shows a missing-glyph box rather than a wrong character.

// Most of these are used by the document toolbar, which is the next thing
// built; a constant with no call site yet is vocabulary, not dead code.
#![allow(dead_code)]

/// The family name `layout.rs` registers the font under.
pub const FAMILY: &str = "Phosphor";

/// Phosphor's **Fill** weight, registered as its own family.
///
/// A separate family rather than a bolder weight of [`FAMILY`], because that is
/// how Phosphor ships it: a separate file carrying the *same* codepoints with
/// solid shapes. Asking the regular family for a heavier weight would get
/// fontique's faux bold — a stroked outline, not a filled glyph.
///
/// Use it for a state that is genuinely **on**, where the fill is the signal:
/// the caption's pin while the window is above the others. If the file is
/// missing the family is simply unregistered, so
/// [`TextEngine::icon_layout_filled`](crate::text::TextEngine::icon_layout_filled)
/// falls back to the regular face rather than drawing a missing-glyph box.
pub const FAMILY_FILL: &str = "Phosphor Fill";

/// undo — `ph-arrow-counter-clockwise`
/// A small connected graph — nodes, and the edges between them.
///
/// **The Koru Agent's mark.** It is the device's process: the one that knows
/// which window holds which workspace and, in phase 2, which devices trust
/// each other. Used as an *overlay* on the product icon rather than instead
/// of it, so the agent reads as Koru in the notification area rather than as
/// a second application.
///
/// U+EB58 is `.ph.ph-graph` in the pinned `@phosphor-icons/web@2.1.2`
/// stylesheet. **The font cannot tell you this**: `Phosphor.ttf` ships a
/// `post` table at version 3.0, which carries no glyph names at all, so the
/// stylesheet is the only source and a codepoint here is never derived from
/// the file beside it.
pub const GRAPH: char = '\u{EB58}';

pub const UNDO: char = '\u{E038}';
/// Put a stepped value back to its default — `ph-arrow-counter-clockwise`, the
/// same glyph [`UNDO`] wears.
///
/// An alias rather than a second codepoint, and a *name* rather than reusing
/// `UNDO` at the call site: the window menu's zoom row ends with a control that
/// returns to 100%, and a reader who found `icons::UNDO` there would spend a
/// moment deciding whether the row could undo something. One shape, two jobs,
/// and the job said out loud — the [`DIVIDER`]/[`WINDOW_MINIMISE`] precedent.
pub const RESET: char = UNDO;
/// redo — `ph-arrow-clockwise`
pub const REDO: char = '\u{E036}';
/// heading level — `ph-text-h`
pub const HEADING: char = '\u{E6BA}';
/// heading level one — `ph-text-h-one`
pub const HEADING_1: char = '\u{E6BC}';
/// heading level two — `ph-text-h-two`
pub const HEADING_2: char = '\u{E6BE}';
/// heading level three — `ph-text-h-three`
pub const HEADING_3: char = '\u{E6C0}';
/// heading level four — `ph-text-h-four`
pub const HEADING_4: char = '\u{E6C2}';
/// bulleted list — `ph-list-bullets`
pub const LIST: char = '\u{E2F2}';
/// three plain lines, no bullets — `ph-list`.
///
/// **Not [`LIST`]**, whose name is a misnomer: that one is `ph-list-bullets`
/// and draws three short bars with a dot beside each, which is right for a
/// bulleted-list command and wrong for a menu button. Verified against the
/// font's outlines rather than guessed — this codepoint is the only one in the
/// group with exactly three full-width contours and nothing else.
pub const LIST_PLAIN: char = '\u{E2F0}';
/// three dots in a row, an overflow menu — `ph-dots-three`.
///
/// Vocabulary. **The app's `…` is [`dots_three`]**, not this — the caption's
/// window-menu button wore this glyph briefly and it thinned out to almost
/// nothing at 15px, where the vector one's three 1.6px circles hold up. One
/// mark, drawn one way, wherever it appears.
///
/// Verified against the font's outlines: three small contours on one baseline.
///
/// [`dots_three`]: crate::scene::dots_three
pub const DOTS_THREE: char = '\u{E1FE}';
/// the same three dots stacked — `ph-dots-three-vertical`. Vocabulary; nothing
/// wears it today.
pub const DOTS_VERTICAL: char = '\u{E208}';
/// numbered list — `ph-list-numbers`
pub const LIST_NUM: char = '\u{E2F6}';
/// indent — `ph-text-indent`
pub const INDENT: char = '\u{EA1E}';
/// clear formatting — `ph-eraser`
pub const CLEAR: char = '\u{E21E}';
/// cut to the clipboard — `ph-scissors`
pub const CUT: char = '\u{EAE0}';
/// copy to the clipboard — `ph-copy`
pub const COPY: char = '\u{E1CA}';
/// Picking an element in the inspector — `ph-cursor`, pinned Phosphor 2.1.2.
pub const CURSOR: char = '\u{E1DC}';
/// Inspector spacing overlay — `ph-ruler`, pinned Phosphor 2.1.2.
pub const RULER: char = '\u{E6B8}';
/// negative feedback — `ph-thumbs-down`.
pub const THUMBS_DOWN: char = '\u{E48C}';
/// positive feedback — `ph-thumbs-up`.
pub const THUMBS_UP: char = '\u{E48E}';
/// branch a conversation — `ph-git-branch`.
pub const GIT_BRANCH: char = '\u{E278}';
/// store a response durably — `ph-floppy-disk`.
pub const FLOPPY_DISK: char = '\u{E248}';
/// edit a link — `ph-pencil-simple`
pub const PENCIL: char = '\u{E3B4}';
/// paste — `ph-clipboard`.
///
/// Phosphor 2.1.2 has no glyph named `paste`; its plain clipboard is the
/// nearest native verb and keeps this command in the same family as
/// [`PASTE_PLAIN`].
pub const PASTE: char = '\u{E196}';
/// paste without formatting — `ph-clipboard-text`
pub const PASTE_PLAIN: char = '\u{E198}';
/// bold — `ph-text-b`
pub const BOLD: char = '\u{E5BE}';
/// italic — `ph-text-italic`
pub const ITALIC: char = '\u{E5C0}';
/// strikethrough — `ph-text-strikethrough`
pub const STRIKE: char = '\u{E5C2}';
/// code block — `ph-code`
pub const CODE: char = '\u{E1BC}';
/// terminal — `ph-terminal`
pub const TERMINAL: char = '\u{E47E}';
/// an agent — `ph-robot`. Worn by the Agents button in the navigator's mode
/// switcher (Q-E7 P7); the per-row session marks are the products' own logos.
///
/// U+E762 is `.ph.ph-robot` in the pinned `@phosphor-icons/web@2.1.2`
/// stylesheet. **The font cannot tell you this** — `Phosphor.ttf` ships a
/// nameless `post` table — so the stylesheet is the only source, exactly as
/// [`GRAPH`] records.
pub const ROBOT: char = '\u{E762}';
/// underline — `ph-text-underline`
pub const UNDERLINE: char = '\u{E5C4}';
/// highlight colour — `ph-highlighter`
pub const HIGHLIGHT: char = '\u{EC76}';
/// link — `ph-link`
pub const LINK: char = '\u{E2E2}';
/// superscript — `ph-text-superscript`
pub const SUPERSCRIPT: char = '\u{EC9A}';
/// subscript — `ph-text-subscript`
pub const SUBSCRIPT: char = '\u{EC98}';
/// align to the start — `ph-text-align-left`
pub const ALIGN_LEFT: char = '\u{E484}';
/// centre — `ph-text-align-center`
pub const ALIGN_CENTER: char = '\u{E480}';
/// align to the end — `ph-text-align-right`
pub const ALIGN_RIGHT: char = '\u{E486}';
/// justify — `ph-text-align-justify`
pub const ALIGN_JUSTIFY: char = '\u{E482}';
/// insert an image — `ph-image`
pub const IMAGE: char = '\u{E2CA}';
/// add — `ph-plus`
pub const PLUS: char = '\u{E3D4}';
/// table — `ph-table`
pub const TABLE: char = '\u{E476}';
/// Fit a table back into the document measure — `ph-arrows-in-line-horizontal`.
/// Shown while a table is Extended, so the glyph names the action that clicking
/// it will perform rather than repeating the generic table symbol.
pub const TABLE_COMPACT: char = '\u{E530}';
/// Let a table use its content-driven width — `ph-arrows-out-line-horizontal`.
/// Shown while a table is Compact, as the inverse of [`TABLE_COMPACT`].
pub const TABLE_EXTEND: char = '\u{E534}';
/// a completed task — `ph-check-square`
pub const CHECK_SQUARE: char = '\u{E186}';
/// a bare tick — `ph-check`. For a checkbox's mark, without the box around it.
pub const CHECK: char = '\u{E182}';
/// an open task — `ph-square`
pub const SQUARE: char = '\u{E45E}';
/// drag handle — `ph-dots-six-vertical`
pub const GRIP: char = '\u{EAE2}';
/// a date pill — `ph-calendar-blank`
pub const DATE: char = '\u{E10A}';
/// A file's attached note — `ph-note`, a sticky note with a folded corner.
/// Worn by the note affordance on a file's row in the explore column.
///
/// U+E348 read from the pinned `@phosphor-icons/web@2.1.2` regular stylesheet,
/// the procedure DESIGN.md §Icons names — never guessed from a neighbouring
/// codepoint.
pub const NOTE: char = '\u{E348}';
/// a contact pill — `ph-user`
pub const CONTACT: char = '\u{E4C2}';
/// a document pill — `ph-file-text`
pub const DOCUMENT: char = '\u{E23A}';
/// Open in a new tab — `ph-tabs`
pub const TABS: char = '\u{E778}';
/// the trash — `ph-trash`. Worn by the pinned row at the foot of the explore
/// column and by the Trash tab. Not `ph-trash-simple`: the lidded can is the
/// glyph every desktop uses for this, and a row at the bottom of a list has one
/// chance to be recognised without a label.
pub const TRASH: char = '\u{E4A6}';
/// the notification area — `ph-tray`, a shallow box with a band across its
/// foot. Worn by the window menu's *Close to Tray*.
///
/// Identified by its outlines and its neighbours: it sits immediately after
/// `ph-trash` and `ph-trash-simple`, which is where the alphabet puts it.
pub const TRAY: char = '\u{E4AA}';
/// a horizontal rule — `ph-minus`. Deliberately **not** in [`ALL`]: it reuses
/// the `WINDOW_MINIMISE` shape (a minus *is* a rule) and a divider is never a
/// landmark, so the glyph never reaches a toolbar or the explore tree. It exists
/// only so the exhaustive `icon_for` match has a name to reach for.
pub const DIVIDER: char = MINUS;
/// Quoted prose and block quotations — `ph-quotes`.
pub const QUOTES: char = '\u{E660}';

// ------------------------------------------------------------- view container

/// Collapse or expand the explore column — `ph-sidebar-simple`
pub const PANEL_LEFT: char = '\u{EC24}';
/// Collapse or expand the context column — `ph-sidebar`
pub const PANEL_RIGHT: char = '\u{EAB6}';
/// Open search — `ph-magnifying-glass`.
pub const SEARCH: char = '\u{E30C}';
/// See a drawing larger — `ph-magnifying-glass-plus`, the glass with a `+`
/// in its lens. A drawn diagram's *Open* control wears it: the glyph says
/// "closer", which is what the viewer it opens is for, where [`SEARCH`]
/// would say "find". Read from the pinned Phosphor web 2.1.2 regular
/// stylesheet beside `ph-magnifying-glass` (U+E30C) and
/// `ph-magnifying-glass-minus` (U+E30E).
pub const MAGNIFYING_GLASS_PLUS: char = '\u{E310}';
/// Fit a picture to the window — `ph-arrows-in-simple`, two diagonal arrows
/// pointing at each other. The diagram viewer's *Fit* control: the inverse
/// of zooming, so the glyph points inward. Not `ph-arrows-in` (U+E09A), the
/// four-arrow form `ARROW_UP`'s note warns about.
pub const ARROWS_IN_SIMPLE: char = '\u{E09E}';
/// Search the web — `ph-globe`.
pub const GLOBE: char = '\u{E288}';
/// A minus — `ph-minus`. The shape three jobs share, named once so the other
/// two can point at it: [`WINDOW_MINIMISE`], [`DIVIDER`], and the `−` half of a
/// stepper.
pub const MINUS: char = '\u{E32A}';
/// Minimise the window — `ph-minus`
pub const WINDOW_MINIMISE: char = MINUS;
/// Close the window — `ph-x`
pub const WINDOW_CLOSE: char = '\u{E4F6}';
/// Restore a maximised window — `ph-corners-in`, four arrows pointing inward.
///
/// Phosphor has no overlapping-squares glyph, which is what Windows uses. Of
/// what it does have, arrows pointing *in* say "come back down" without
/// ambiguity — `ph-copy` looks closer to the Windows mark but reads as copying.
pub const WINDOW_RESTORE: char = '\u{E1CE}';
// Maximise reuses [`SQUARE`]: a plain square outline is the conventional glyph
// for "fill the screen".
/// Pop a tab out into its own window — `ph-arrow-square-out`, a square with an
/// arrow leaving its top-right corner. The conventional "open in new window"
/// glyph, used for tearing a document tab off the strip.
pub const POP_OUT: char = '\u{E5DE}';
/// Pin a window above the others — `ph-push-pin`. Drawn in the accent colour
/// while the window is pinned, dim when it is not; one glyph, tinted by state.
pub const PUSH_PIN: char = '\u{E3E2}';

/// Leftward — `ph-caret-left`. Its alphabetical neighbours place it: down,
/// **left**, right, up, and it is the taller narrower one of that middle pair.
pub const CARET_LEFT: char = '\u{E138}';
/// A collapsed node in the explore tree — `ph-caret-right`
pub const CARET_RIGHT: char = '\u{E13A}';
/// An expanded node in the explore tree — `ph-caret-down`
pub const CARET_DOWN: char = '\u{E136}';
/// Upward - `ph-caret-up`.
pub const CARET_UP: char = '\u{E13C}';
/// Replace text — `ph-arrows-left-right`.
pub const REPLACE: char = '\u{E0A0}';
/// The return/Enter action - `ph-arrow-elbow-down-left`.
pub const ENTER: char = '\u{E044}';
/// Fold a whole tree — `ph-arrows-in-line-vertical`. Deliberately *not* a
/// doubled caret: a caret in this app means one node's own fold, and a control
/// that acts on every node has to look like a different verb.
pub const FOLD_ALL: char = '\u{E532}';
/// Unfold a whole tree — `ph-arrows-out-line-vertical`
pub const UNFOLD_ALL: char = '\u{E536}';
/// Import content — `ph-download-simple`.
pub const IMPORT: char = '\u{E20C}';
/// Save a picture to disk — a drawn diagram's *Save as…* control and the
/// diagram viewer's. The same tray-and-arrow as [`IMPORT`], read the other
/// way: there it is content coming in, here it is a file going down to
/// disk, which is the glyph every browser and viewer uses for the same
/// verb. An alias, so not in [`ALL`].
pub const DOWNLOAD: char = IMPORT;
/// A node with children — `ph-folder-simple`
pub const FOLDER: char = '\u{E25A}';
/// Work still going on — `ph-circle-notch`. An open ring, and the one glyph in
/// this set whose *shape* says "not finished": drawn turning, it is what a run
/// that is still working wears in the navigator, and the folder is what a
/// finished, failed or cancelled one keeps.
pub const CIRCLE_NOTCH: char = '\u{EB44}';
/// A task list — `ph-list-checks`. The icon a `Kind::Outline` node will wear.
pub const LIST_CHECKS: char = '\u{EADC}';
/// Comments and threads — `ph-chat-circle`
pub const CHAT_CIRCLE: char = '\u{E168}';
/// Send feedback — `ph-user-sound`. A person with sound waves; the same
/// glyph the overflow menu and the Feedback tab share.
pub const USER_SOUND: char = '\u{ECA8}';
/// Support — `ph-headset`. A person on the other end of a line: the overflow
/// menu's Support row, which opens the support chat window.
pub const HEADSET: char = '\u{E584}';
/// Help — `ph-question`. A question mark in its own ring, so a button wearing
/// it needs no badge circle of its own: the Help button on the acquisition and
/// account surfaces.
pub const QUESTION: char = '\u{E3E8}';
/// A bug report — `ph-bug`
pub const BUG: char = '\u{E0C4}';
/// An idea — `ph-lightbulb`
pub const LIGHTBULB: char = '\u{E2DC}';
/// Attachments — `ph-paperclip`
pub const PAPERCLIP: char = '\u{E39A}';
/// Recently opened — `ph-clock-counter-clockwise`
pub const CLOCK: char = '\u{E1A0}';
/// Reminder attention — `ph-bell` from the pinned Phosphor 2.1.2 regular set.
pub const BELL: char = '\u{E0CE}';
/// A snoozed reminder - `ph-bell-z` from the pinned Phosphor 2.1.2 regular set.
pub const BELL_Z: char = '\u{E5EE}';
/// A paused reminder — `ph-pause` from the pinned Phosphor 2.1.2 regular set.
pub const PAUSE: char = '\u{E39E}';
/// The welcome tab — `ph-house`
pub const HOUSE: char = '\u{E2C2}';
/// Record — `ph-record` from the pinned Phosphor 2.1.2 regular set. The
/// record dot: the internal session recorder's caption-menu row
/// (the internal session-recorder design).
pub const RECORD: char = '\u{E3EE}';
/// Stop — `ph-stop` from the same pinned set. The stop square: what the
/// record row becomes while a recording is live.
pub const STOP: char = '\u{E46C}';
/// `ph-play`: Settings' tone preview, and where a chain of agents begins. The canvas's start node wears it
/// in its header, beside the word for start, so the node that a Run begins
/// from reads as the beginning before its button is found.
///
/// U+E3D0 is `.ph.ph-play` in the pinned `@phosphor-icons/web@2.1.2` regular
/// stylesheet, read from the sheet and checked there against `ph-stop`
/// (U+E46C) and `ph-circle-notch` (U+EB44), per DESIGN.md §Icons.
pub const PLAY: char = '\u{E3D0}';

/// http (insecure) — `ph-link-simple-break`, a broken link
pub const LINK_SIMPLE_BREAK: char = '\u{E2E8}';
/// mailto — `ph-envelope-simple`
pub const ENVELOPE: char = '\u{E218}';
/// file — `ph-file`
pub const FILE: char = '\u{E230}';
/// PDF document — `ph-file-pdf`.
pub const FILE_PDF: char = '\u{E702}';
/// Word-processing document — `ph-file-doc`.
pub const FILE_DOC: char = '\u{EB1E}';
/// Spreadsheet — `ph-file-xls`.
pub const FILE_XLS: char = '\u{EB22}';
/// Presentation — `ph-file-ppt`.
pub const FILE_PPT: char = '\u{EB20}';
/// Markdown document — `ph-file-md`.
pub const FILE_MD: char = '\u{ED50}';
/// Image file — `ph-file-image`.
pub const FILE_IMAGE: char = '\u{EA24}';
/// Archive — `ph-file-zip`.
pub const FILE_ZIP: char = '\u{E958}';
/// Source or structured-data file — `ph-file-code`.
pub const FILE_CODE: char = '\u{E914}';
/// Audio file — `ph-file-audio`.
pub const FILE_AUDIO: char = '\u{EA20}';
/// Video file — `ph-file-video`.
pub const FILE_VIDEO: char = '\u{EA22}';
/// ftp — `ph-file-arrow-up`
pub const FILE_ARROW_UP: char = '\u{E61E}';
/// sftp / ssh — `ph-key`
pub const KEY: char = '\u{E2D6}';
/// Private / incognito mode — `ph-detective`.
pub const DETECTIVE: char = '\u{E83E}';
/// Send / submit — `ph-arrow-up`.
/// **Verified against the font, not the stylesheet.** This was `E09A`, which
/// is `ph-arrows-in` - four diagonal arrows pointing at each other - so the
/// AI composer's send button showed a collapse icon. Both codepoints exist
/// and both render, so `every_icon_is_in_the_private_use_area` and
/// `no_two_icons_share_a_codepoint` passed throughout: a wrong glyph is a
/// different failure from a missing one, and only the second is testable
/// from the codepoint alone.
pub const ARROW_UP: char = '\u{E08E}';
/// The four the tasks rail names its queues with, and the same four the due
/// chip carries in a document — `ph-sun`, `ph-arrow-right`,
/// `ph-caret-double-right`, `ph-clock-countdown`.
///
/// **Read out of the pinned stylesheet, not inferred from a neighbour.** The
/// values came from `@phosphor-icons/web@2.1.2`'s own `style.css`, read back
/// through `getComputedStyle(el, '::before')` on a page whose first act was to
/// re-read a codepoint this file already held and confirm it matched. Each was
/// then checked against `assets/fonts/Phosphor.ttf`'s cmap, which has 1513 glyphs
/// between U+E000 and U+EE83 and contains all four.
///
/// That is two independent confirmations, and it is what the comment on
/// [`ARROW_UP`] asks for: the tests here can only see a codepoint outside the
/// PUA or one used twice, never a plausible value that draws the wrong
/// picture. `docs/contributing/techdebt/README.md` tracks the hand-copying this replaces.
pub const SUN: char = '\u{E472}';
/// See [`SUN`] for how these four were verified. Next's arrow, and the reason
/// the rail no longer rotates [`ARROW_UP`] a quarter turn to fake one.
pub const ARROW_RIGHT: char = '\u{E06C}';
/// See [`SUN`]. Upcoming.
pub const CARET_DOUBLE_RIGHT: char = '\u{E12A}';
/// See [`SUN`]. Later.
pub const CLOCK_COUNTDOWN: char = '\u{ED2C}';
/// Keyboard Enter / Return — `ph-key-return`. Footer chords and shortcut
/// columns that name the key itself (Move to…, search open). Not the whole
/// keyboard (`ph-keyboard`) and not a bent arrow — those read as navigation.
pub const KEY_RETURN: char = '\u{E782}';
/// tel — `ph-phone`
pub const PHONE: char = '\u{E3B8}';
/// Dictation — `ph-microphone`. The `/dictate` command row's leading glyph, and
/// whatever a listening surface draws.
pub const MICROPHONE: char = '\u{E326}';
/// git — `ph-github-logo`
pub const GITHUB: char = '\u{E576}';
/// sparkle — `ph-sparkle`, the stable visual identity of Koru AI links.
pub const SPARKLE: char = '\u{E6A2}';
/// Tool execution — `ph-wrench`.
pub const WRENCH: char = '\u{E5D4}';
/// A tag — `ph-tag`. The `/tag` command row, the Tags… menu rows, and the
/// tag chooser's rows.
pub const TAG: char = '\u{E478}';
/// Attribute controls — `ph-faders`. The `/attr` command row: browse and set
/// what is true about this block.
pub const FADERS: char = '\u{E228}';
/// An accent color — `ph-palette`. The `/color` command row and the Color…
/// menu rows.
pub const PALETTE: char = '\u{E6C8}';
/// Priority — `ph-flag`. The `/priority` command row and the Priority… menu
/// rows: how important this is, relative to other work (never its order —
/// that is Rank, a different concept).
///
/// U+E244 read from the pinned `@phosphor-icons/web@2.1.2` regular
/// stylesheet, the procedure DESIGN.md §Icons names — never guessed from a
/// neighbouring codepoint.
pub const FLAG: char = '\u{E244}';
/// Something completed is still carrying unfinished work — `ph-warning`, the
/// triangle. Drawn in `notice_fg`, the warm ink `Palette` already reserves for
/// a notice band and its icon; it is a statement about the row, never an error.
///
/// U+E4E0 read from the pinned `@phosphor-icons/web@2.1.2` regular stylesheet,
/// the procedure DESIGN.md §Icons names — never guessed from a neighbouring
/// codepoint, and cross-checked against `ph-trash` (U+E4A6) and `ph-wrench`
/// (U+E5D4) in the same sheet before it was written down.
pub const WARNING: char = '\u{E4E0}';

/// Show the password — `ph-eye`. Trails a masked password field; pressing it
/// shows what was typed.
///
/// U+E220 read from the pinned `@phosphor-icons/web@2.1.2` regular stylesheet,
/// beside `ph-eye-closed` (U+E222) and `ph-eye-slash` (U+E224).
pub const EYE: char = '\u{E220}';
/// Hide the password again — `ph-eye-slash`, U+E224 from the same sheet. What
/// the same button wears while the password is showing.
pub const EYE_SLASH: char = '\u{E224}';

/// The Windows mark — `ph-windows-logo`. The *System* shell profile's icon on
/// Windows: the row launches whatever the OS calls its own shell, so the OS's
/// mark is the honest picture of what you get (TERMINATAB Q-E6 P4).
///
/// All three OS logos below read from the pinned `@phosphor-icons/web@2.1.2`
/// regular stylesheet, per DESIGN.md §Icons.
pub const WINDOWS_LOGO: char = '\u{E692}';
/// The Apple mark — `ph-apple-logo`. The *System* shell profile on macOS.
pub const APPLE_LOGO: char = '\u{E516}';
/// Tux — `ph-linux-logo`. The *System* shell profile everywhere else.
pub const LINUX_LOGO: char = '\u{EB02}';

/// A cloud — `ph-cloud`. Cloud sync is **on** for a Profile.
///
/// U+E1AA is `.ph.ph-cloud` in the pinned `@phosphor-icons/web@2.1.2`
/// stylesheet, read the way [`GRAPH`] says every codepoint here is read: from
/// the stylesheet, never from `Phosphor.ttf`, whose version-3.0 `post` table
/// carries no glyph names to check against.
pub const CLOUD: char = '\u{E1AA}';
/// A cloud with a stroke through it — `ph-cloud-slash`, U+E1B6 in the same
/// pinned stylesheet. Cloud sync is **off** for a Profile.
///
/// The pair is deliberately two glyphs rather than one glyph plus a colour:
/// the toggle beside it already carries the on/off colour, and a colour-only
/// difference is the one difference a person cannot see in greyscale.
pub const CLOUD_SLASH: char = '\u{E1B6}';

/// A **canvas** — `ph-squares-four`: Explore's Canvases section, the `+`
/// menu's Canvases flyout, and the tab over a canvas.
///
/// U+E464 is `.ph.ph-squares-four` in the pinned `@phosphor-icons/web@2.1.2`
/// regular stylesheet, read the way [`GRAPH`] says every codepoint here is
/// read, and cross-checked against `ph-square` (U+E45E, [`SQUARE`]) and
/// `ph-robot` (U+E762, [`ROBOT`]) in the same sheet before it was written down.
/// Four tiles rather than `ph-frame-corners` (U+E626): a canvas is a surface
/// things are laid out on, not a viewport onto something else.
pub const CANVAS: char = '\u{E464}';

/// The Phosphor glyph for a link's scheme, so a glance tells you what kind of
/// thing a link is before you open it: a link for `https`, a broken link for
/// plain `http`, an envelope for `mailto`, and so on.
///
/// The scheme is the lowercase prefix before the first `:`, collapsed at the
/// first `+` (so `git+ssh:` reads as `git`). An unknown scheme, a non-URL
/// label, or no scheme at all falls back to the generic [`LINK`].
pub fn for_scheme(url: Option<&str>) -> char {
    let Some(raw) = url else { return LINK };
    let scheme = raw
        .trim()
        .split(':')
        .next()
        .and_then(|s| s.split('+').next())
        .unwrap_or("");
    match scheme.to_ascii_lowercase().as_str() {
        "https" => LINK,
        "http" => LINK_SIMPLE_BREAK,
        "mailto" => ENVELOPE,
        "file" => FILE,
        "ftp" => FILE_ARROW_UP,
        "sftp" | "ssh" => KEY,
        "tel" => PHONE,
        "git" => GITHUB,
        "koru" if raw.get(7..).is_some_and(|rest| rest.starts_with("chat/")) => SPARKLE,
        _ => LINK,
    }
}

/// Socials navigator — `ph-megaphone`, verified in the pinned Phosphor web
/// 2.1.2 regular stylesheet (`src/regular/style.css`).
pub const MEGAPHONE: char = '\u{e324}';

/// Every icon, for tests and for a future picker.
pub const ALL: [(&str, char); 126] = [
    ("MEGAPHONE", MEGAPHONE),
    ("FOLD_ALL", FOLD_ALL),
    ("UNFOLD_ALL", UNFOLD_ALL),
    ("IMPORT", IMPORT),
    ("UNDO", UNDO),
    ("REDO", REDO),
    ("HEADING", HEADING),
    ("HEADING_1", HEADING_1),
    ("HEADING_2", HEADING_2),
    ("HEADING_3", HEADING_3),
    ("HEADING_4", HEADING_4),
    ("LIST", LIST),
    ("LIST_PLAIN", LIST_PLAIN),
    ("DOTS_THREE", DOTS_THREE),
    ("DOTS_VERTICAL", DOTS_VERTICAL),
    ("LIST_NUM", LIST_NUM),
    ("INDENT", INDENT),
    ("CLEAR", CLEAR),
    ("CUT", CUT),
    ("COPY", COPY),
    ("CURSOR", CURSOR),
    ("RULER", RULER),
    ("THUMBS_DOWN", THUMBS_DOWN),
    ("THUMBS_UP", THUMBS_UP),
    ("GIT_BRANCH", GIT_BRANCH),
    ("FLOPPY_DISK", FLOPPY_DISK),
    ("PENCIL", PENCIL),
    ("PASTE", PASTE),
    ("PASTE_PLAIN", PASTE_PLAIN),
    ("BOLD", BOLD),
    ("ITALIC", ITALIC),
    ("STRIKE", STRIKE),
    ("CODE", CODE),
    ("UNDERLINE", UNDERLINE),
    ("HIGHLIGHT", HIGHLIGHT),
    ("LINK", LINK),
    ("SUPERSCRIPT", SUPERSCRIPT),
    ("SUBSCRIPT", SUBSCRIPT),
    ("ALIGN_LEFT", ALIGN_LEFT),
    ("ALIGN_CENTER", ALIGN_CENTER),
    ("ALIGN_RIGHT", ALIGN_RIGHT),
    ("ALIGN_JUSTIFY", ALIGN_JUSTIFY),
    ("IMAGE", IMAGE),
    ("PLUS", PLUS),
    ("TABLE", TABLE),
    ("CHECK_SQUARE", CHECK_SQUARE),
    ("CHECK", CHECK),
    ("SQUARE", SQUARE),
    ("GRIP", GRIP),
    ("DATE", DATE),
    ("NOTE", NOTE),
    ("CONTACT", CONTACT),
    ("DOCUMENT", DOCUMENT),
    ("ROBOT", ROBOT),
    ("TABS", TABS),
    ("TRASH", TRASH),
    ("TRAY", TRAY),
    ("PANEL_LEFT", PANEL_LEFT),
    ("PANEL_RIGHT", PANEL_RIGHT),
    ("SEARCH", SEARCH),
    ("GLOBE", GLOBE),
    ("WINDOW_MINIMISE", WINDOW_MINIMISE),
    ("WINDOW_CLOSE", WINDOW_CLOSE),
    ("WINDOW_RESTORE", WINDOW_RESTORE),
    ("POP_OUT", POP_OUT),
    ("PUSH_PIN", PUSH_PIN),
    ("CARET_LEFT", CARET_LEFT),
    ("CARET_RIGHT", CARET_RIGHT),
    ("CARET_DOWN", CARET_DOWN),
    ("FOLDER", FOLDER),
    ("CIRCLE_NOTCH", CIRCLE_NOTCH),
    ("LIST_CHECKS", LIST_CHECKS),
    ("CHAT_CIRCLE", CHAT_CIRCLE),
    ("USER_SOUND", USER_SOUND),
    ("HEADSET", HEADSET),
    ("QUESTION", QUESTION),
    ("BUG", BUG),
    ("LIGHTBULB", LIGHTBULB),
    ("PAPERCLIP", PAPERCLIP),
    ("CLOCK", CLOCK),
    ("BELL", BELL),
    ("BELL_Z", BELL_Z),
    ("PAUSE", PAUSE),
    ("HOUSE", HOUSE),
    ("RECORD", RECORD),
    ("STOP", STOP),
    ("PLAY", PLAY),
    ("LINK_SIMPLE_BREAK", LINK_SIMPLE_BREAK),
    ("ENVELOPE", ENVELOPE),
    ("FILE", FILE),
    ("FILE_PDF", FILE_PDF),
    ("FILE_DOC", FILE_DOC),
    ("FILE_XLS", FILE_XLS),
    ("FILE_PPT", FILE_PPT),
    ("FILE_MD", FILE_MD),
    ("FILE_IMAGE", FILE_IMAGE),
    ("FILE_ZIP", FILE_ZIP),
    ("FILE_CODE", FILE_CODE),
    ("FILE_AUDIO", FILE_AUDIO),
    ("FILE_VIDEO", FILE_VIDEO),
    ("FILE_ARROW_UP", FILE_ARROW_UP),
    ("KEY", KEY),
    ("DETECTIVE", DETECTIVE),
    ("ARROW_UP", ARROW_UP),
    ("SUN", SUN),
    ("ARROW_RIGHT", ARROW_RIGHT),
    ("CARET_DOUBLE_RIGHT", CARET_DOUBLE_RIGHT),
    ("CLOCK_COUNTDOWN", CLOCK_COUNTDOWN),
    ("KEY_RETURN", KEY_RETURN),
    ("PHONE", PHONE),
    ("MICROPHONE", MICROPHONE),
    ("GITHUB", GITHUB),
    ("WRENCH", WRENCH),
    ("TAG", TAG),
    ("PALETTE", PALETTE),
    ("FADERS", FADERS),
    ("FLAG", FLAG),
    ("WARNING", WARNING),
    ("EYE", EYE),
    ("EYE_SLASH", EYE_SLASH),
    ("WINDOWS_LOGO", WINDOWS_LOGO),
    ("APPLE_LOGO", APPLE_LOGO),
    ("LINUX_LOGO", LINUX_LOGO),
    ("CLOUD", CLOUD),
    ("CLOUD_SLASH", CLOUD_SLASH),
    ("CANVAS", CANVAS),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Every glyph sits in the Private Use Area. One that did not would collide
    /// with real text — an icon constant holding `A` would render as an `A`
    /// wherever Phosphor happened not to be registered.
    #[test]
    fn every_icon_is_in_the_private_use_area() {
        for (name, ch) in ALL {
            let cp = ch as u32;
            assert!(
                (0xE000..=0xF8FF).contains(&cp),
                "{name} is U+{cp:04X}, outside the PUA"
            );
        }
    }

    /// Two icons sharing a codepoint would draw the same glyph, which is the
    /// sort of thing nobody notices until a toolbar has two identical buttons.
    #[test]
    fn no_two_icons_share_a_codepoint() {
        let mut seen: Vec<char> = ALL.iter().map(|(_, c)| *c).collect();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), before, "duplicate codepoint among the icons");
    }

    /// Each scheme a link can carry maps to a distinct, recognisable glyph, and
    /// anything unrecognised falls back to the generic link.
    #[test]
    fn for_scheme_picks_the_right_icon() {
        assert_eq!(for_scheme(Some("https://example.com")), LINK);
        assert_eq!(for_scheme(Some("HTTPS://x")), LINK, "case-insensitive");
        assert_eq!(for_scheme(Some("http://example.com")), LINK_SIMPLE_BREAK);
        assert_eq!(for_scheme(Some("mailto:a@b.com")), ENVELOPE);
        assert_eq!(for_scheme(Some("file:///etc/hosts")), FILE);
        assert_eq!(for_scheme(Some("ftp://host/path")), FILE_ARROW_UP);
        assert_eq!(for_scheme(Some("sftp://host/path")), KEY);
        assert_eq!(for_scheme(Some("ssh://host")), KEY);
        assert_eq!(for_scheme(Some("tel:+155512345")), PHONE);
        assert_eq!(for_scheme(Some("git://host/repo")), GITHUB);
        assert_eq!(
            for_scheme(Some("git+ssh://host/repo")),
            GITHUB,
            "git+ssh is git"
        );
        assert_eq!(for_scheme(Some("spotify:track:1")), LINK, "unknown scheme");
        assert_eq!(for_scheme(None), LINK, "no url at all");
    }
}
