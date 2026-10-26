// SPDX-License-Identifier: MPL-2.0

//! Trigger detection and the overlay state machine.
//!
//! A trigger arms when `/` or `@` is typed at a word boundary. The trigger text
//! stays in the document as literal text while the overlay is open, so you can
//! see `/doc` as you type it; the live query is `text[start + 1 .. caret]`.
//! Escaping leaves that text alone — nothing is mutated until you commit.

use chrono::{Local, NaiveDate};

use koru_core::text::ANCHOR_LEN;
use koru_core::{NodeId, StableTextRange};

use crate::components::Overlay;
use crate::components::command_menu::{self, Command, CommandMenu};
use crate::components::date_picker::DatePicker;
use crate::components::entity_picker::EntityPicker;
use crate::components::link_prompt::{self, LinkPrompt};
use crate::components::name_prompt::NamePrompt;
use crate::components::text_field::TextField;
use crate::paragraph::Paragraph;
use crate::utils::dates;
use koru_draw::pill::PillKind;
use koru_draw::text::TextEngine;
use koru_draw::theme;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum TriggerKind {
    Slash,
    At,
    Colon,
    /// The `#tag` grammar. Late-armed like [`Colon`](Self::Colon): the `#`
    /// alone is quiet, and the tag chooser opens only once the query after it
    /// earns one.
    Hash,
}

impl TriggerKind {
    pub fn ch(self) -> char {
        match self {
            Self::Slash => '/',
            Self::At => '@',
            Self::Colon => ':',
            Self::Hash => '#',
        }
    }
}

#[derive(Clone, Debug)]
pub struct Trigger {
    pub kind: TriggerKind,
    /// Byte index of the `/` or `@` itself — or, for the non-text anchors, of
    /// the existing pill or the insertion point.
    pub start: usize,
    /// The node the trigger lives in. A node id rather than a row index so a
    /// remote insert above the caret cannot silently re-point an open picker at
    /// somebody else's line.
    pub node: NodeId,
    /// What this overlay stands in for, which decides what commit replaces and
    /// whether [`refresh`] tracks the document text at all.
    pub anchor: Anchor,
    /// CRDT-stable endpoints for the text this overlay replaces. The byte
    /// offsets above remain a layout cache; this range is the authority when a
    /// peer edits the paragraph while the overlay is open.
    pub stable: Option<StableTextRange>,
}

/// What an open picker will replace when it commits.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Anchor {
    /// Typed trigger text, from `Trigger::start` to the caret. Only this variant
    /// is tied to the document, so only this one gets re-filtered and dismissed
    /// as the text changes.
    TriggerText,
    /// An existing pill's anchor character at `Trigger::start`.
    Pill,
    /// Nothing — a free-standing prompt (Ctrl+K) inserting at `Trigger::start`.
    /// Deliberately distinct from `TriggerText`: there is no `/` in the document
    /// to track, and treating it as trigger text made `refresh` dismiss the
    /// prompt the instant it opened.
    Insert,
    /// Text that was selected when the prompt opened, from `Trigger::start` to
    /// the offset here — the pill takes its place.
    ///
    /// The end is carried rather than read from the caret because a selection
    /// made by dragging backwards leaves the caret at its *start*, and the
    /// prompt takes the focus away the moment it opens.
    Selection(usize),
    /// A node's own text. Committing sets it rather than inserting anything —
    /// renaming a document is not a pill, even though it reuses the prompt.
    Rename(NodeId),
    /// An image's display name. Committing writes node metadata because the
    /// image's content is the blob itself, not a text container.
    RenameImage(NodeId),
    /// A page node's metadata icon, selected by the Unicode picker.
    PageIcon(NodeId),
    /// An attribute chip that was clicked. Nothing in the document stands in
    /// for the picker — the chip is presentation of node state, not text — so
    /// committing replaces nothing, exactly like [`Insert`](Self::Insert).
    ///
    /// It is a variant of its own rather than an `Insert` because the *panel*
    /// hangs off the chip rather than off a caret, and `overlay_panel_rect`
    /// has to be able to tell which: an `Insert` with `start: 0` anchors at
    /// the beginning of the row, which is the far end of the line the chips
    /// trail. The rect itself is `ViewState::chip_anchor`.
    Chip,
    /// A block that **does not exist yet**, opened from a row's `+`.
    ///
    /// `Trigger::node` is the row the `+` belongs to and `above` says which
    /// side of it the new block goes. Nothing is inserted while the menu is
    /// open: dismissing it has to leave the document exactly as it was, or the
    /// button would litter empty paragraphs every time somebody looked at it.
    /// The node is made the instant a kind is chosen, and the trigger is
    /// re-pointed at it so every `commit_*` path runs unchanged.
    NewBlock { above: bool },
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PillSpec {
    pub kind: PillKind,
    pub label: String,
    /// Set for date pills, whose label ("Aug 2") is lossy on its own.
    pub date: Option<NaiveDate>,
    /// Set for link pills: the target the name points at.
    pub url: Option<String>,
    /// Set for document pills selected by the entity picker.
    pub document: Option<DocumentTarget>,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct DocumentTarget {
    pub document: koru_core::DocumentId,
    /// Absent for an ordinary document link; set to a heading when the picker
    /// was taken into its heading stage with `#`.
    pub node: Option<NodeId>,
}

impl PillSpec {
    pub fn new(kind: PillKind, label: impl Into<String>) -> Self {
        Self {
            kind,
            label: label.into(),
            date: None,
            url: None,
            document: None,
        }
    }

    pub fn link(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            kind: PillKind::Link,
            label: name.into(),
            date: None,
            url: Some(url.into()),
            document: None,
        }
    }

    pub fn date(d: NaiveDate) -> Self {
        Self {
            kind: PillKind::Date,
            label: dates::date_label(d),
            date: Some(d),
            url: None,
            document: None,
        }
    }

    pub fn document(label: impl Into<String>, target: DocumentTarget) -> Self {
        Self {
            kind: PillKind::Document,
            label: label.into(),
            date: None,
            url: None,
            document: Some(target),
        }
    }

    pub fn file(label: impl Into<String>, target: DocumentTarget) -> Self {
        Self {
            kind: PillKind::File,
            label: label.into(),
            date: None,
            url: None,
            document: Some(target),
        }
    }
}

/// The result of feeding an event to the overlay.
pub fn is_trigger_position(row: &Paragraph, at: usize) -> bool {
    row.text[..at]
        .chars()
        .next_back()
        .is_none_or(char::is_whitespace)
}

/// The punctuation stripped from the end of a candidate URL — the characters a
/// sentence ends with, never a working URL. A URL contains dots and colons, so
/// these are **not** triggers (typing a `.` mid-URL must not link prematurely);
/// they are stripped only once a real terminator — whitespace, or Enter — has
/// already said the word is done.
fn is_sentence_punct(c: char) -> bool {
    matches!(c, '.' | ',' | ';' | ':' | '!' | '?')
}

/// A scheme-bearing URL that ends at `end` (an exclusive byte index), with
/// trailing whitespace and sentence punctuation stripped.
///
/// This is what auto-link uses: when the user types a **whitespace** terminator
/// (or presses Enter), the word just finished is tested here. A bare host
/// (`example.com`) is deliberately **not** recognised — only a word carrying a
/// scheme (`https://`, `http://`, `mailto:`, …) — so a filename like `file.txt`
/// or a version like `v1.2` is left as text.
///
/// Returns the URL's byte range within `text` and the URL itself. The range
/// excludes any trailing punctuation, which stays as text after the link.
pub fn url_ending_at(text: &str, end: usize) -> Option<(std::ops::Range<usize>, String)> {
    let end = end.min(text.len());
    // Strip the terminator and any trailing sentence punctuation. Whitespace is
    // the terminator the caller typed; a period or comma after the URL is not
    // part of it either. A real URL is never terminated by these.
    let mut e = end;
    while e > 0 {
        let c = text[..e].chars().next_back()?;
        if c.is_whitespace() || is_sentence_punct(c) {
            e -= c.len_utf8();
        } else {
            break;
        }
    }
    // The URL token: the maximal run of non-whitespace ending at `e`.
    let mut s = e;
    while s > 0 {
        let c = text[..s].chars().next_back()?;
        if c.is_whitespace() {
            break;
        }
        s -= c.len_utf8();
    }
    if e <= s {
        return None;
    }
    let candidate = &text[s..e];
    if !looks_like_a_scheme_url(candidate) {
        return None;
    }
    Some((s..e, candidate.to_string()))
}

/// [`link_prompt::looks_like_a_url`] but a scheme is required, so a bare host
/// or a filename is not mistaken for a link.
fn looks_like_a_scheme_url(text: &str) -> bool {
    link_prompt::looks_like_a_url(text) && (text.contains("://") || text.starts_with("mailto:"))
}

/// Every scheme URL appearing in `text`, as `(byte range, url)`. Trailing
/// sentence punctuation is excluded from the range and left as text. Used by
/// paste, which can drop several URLs into a line at once — unlike typing,
/// which links the single word a terminator just finished.
pub fn urls_in(text: &str) -> Vec<(std::ops::Range<usize>, String)> {
    let mut out = Vec::new();
    let mut word_start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            if let Some(start) = word_start.take() {
                consider_word(text, start, i, &mut out);
            }
        } else if word_start.is_none() {
            word_start = Some(i);
        }
    }
    if let Some(start) = word_start {
        consider_word(text, start, text.len(), &mut out);
    }
    out
}

/// One whitespace-delimited word, with trailing sentence punctuation stripped:
/// if what remains is a scheme URL, record its range (excluding that punctuation).
fn consider_word(
    text: &str,
    start: usize,
    end: usize,
    out: &mut Vec<(std::ops::Range<usize>, String)>,
) {
    let mut e = end;
    while e > start {
        let c = text[..e].chars().next_back().unwrap();
        if is_sentence_punct(c) {
            e -= c.len_utf8();
        } else {
            break;
        }
    }
    if e <= start {
        return;
    }
    let candidate = &text[start..e];
    if looks_like_a_scheme_url(candidate) {
        out.push((start..e, candidate.to_owned()));
    }
}

/// The live query text between the trigger character and the caret, or `None`
/// if the trigger no longer makes sense (caret moved behind it, the character
/// was deleted, or whitespace ended the word).
pub fn query<'a>(row: &'a Paragraph, trigger: &Trigger) -> Option<&'a str> {
    let caret = row.caret();
    let after = trigger.start + trigger.kind.ch().len_utf8();
    if caret < after || after > row.text.len() || caret > row.text.len() {
        return None;
    }
    if row.text[trigger.start..].chars().next() != Some(trigger.kind.ch()) {
        return None;
    }
    let q = &row.text[after..caret];
    if trigger.kind != TriggerKind::At && q.chars().any(char::is_whitespace) {
        None
    } else {
        Some(q)
    }
}

pub fn open_for_trigger(
    ws: &koru_core::schema::WorkspaceDoc,
    // The `/` menu carries its own search field now, and a field needs a text
    // engine to lay itself out — the same reason `for_command` takes one.
    engine: &mut koru_draw::text::TextEngine,
    kind: TriggerKind,
    node: NodeId,
    start: usize,
    allows: command_menu::Allows,
) -> Overlay {
    let trigger = Trigger {
        kind,
        start,
        node,
        anchor: Anchor::TriggerText,
        stable: ws
            .capture_text_range(node, start, start + kind.ch().len_utf8())
            .ok(),
    };
    match kind {
        TriggerKind::Slash => Overlay::Command(CommandMenu::opened(engine, trigger, allows)),
        TriggerKind::At => Overlay::Entity(EntityPicker::new(trigger)),
        TriggerKind::Colon | TriggerKind::Hash => {
            unreachable!("late-armed completion opens after its first query character")
        }
    }
}

/// Re-open the picker that made an existing pill, seeded with what it holds, so
/// clicking a pill edits it rather than starting over.
#[allow(clippy::too_many_arguments)]
pub fn open_for_pill(
    ws: &koru_core::schema::WorkspaceDoc,
    engine: &mut TextEngine,
    node: NodeId,
    anchor: usize,
    kind: PillKind,
    label: &str,
    date: Option<NaiveDate>,
    url: Option<&str>,
) -> Overlay {
    let trigger = Trigger {
        // The trigger character is irrelevant here; Slash is a placeholder.
        kind: TriggerKind::Slash,
        start: anchor,
        node,
        anchor: Anchor::Pill,
        stable: ws
            .capture_text_range(node, anchor, anchor + ANCHOR_LEN)
            .ok(),
    };
    match kind {
        PillKind::Document => Overlay::Name(named_prompt(engine, trigger, label)),
        // Attachments are opened/saved/removed, never renamed in place — a
        // name prompt would overwrite the reference with dead text.
        PillKind::File => Overlay::None,
        // Legacy contact pills remain readable, but people are deliberately no
        // longer offered by `@`. Clicking one leaves it unchanged.
        PillKind::Contact => Overlay::None,
        // A run pill is a record of what was delegated, not a field. Clicking
        // it opens the run (`follow_run_pill`); nothing about it is editable
        // here, and a caption prompt would overwrite the reference with text.
        PillKind::Run | PillKind::Schedule | PillKind::SchedulePaused => Overlay::None,
        // Fall back to today only if the pill somehow has no stored date.
        PillKind::Date => Overlay::Date(DatePicker::for_pill(
            trigger,
            date.unwrap_or_else(|| Local::now().date_naive()),
        )),
        PillKind::Link => {
            let mut prompt = link_prompt(engine, trigger);
            prompt.name.set_text(engine, label);
            prompt.url.set_text(engine, url.unwrap_or_default());
            // Selected, so typing replaces the caption rather than extending it
            // — and `set_text` has already cleared the history, so Ctrl+Z
            // cannot erase what the pill came in with.
            prompt.name.select_all(engine);
            Overlay::Link(prompt)
        }
    }
}

/// A prompt seeded with a node's current name, ready to replace it.
///
/// Reuses the name prompt, because "type a name and press Enter" is
/// the same interaction — only what happens on commit differs, and
/// [`Anchor::Rename`] carries that.
pub fn open_rename(
    engine: &mut TextEngine,
    node: NodeId,
    current: &str,
    page_icon: Option<String>,
) -> Overlay {
    let trigger = Trigger {
        kind: TriggerKind::Slash,
        start: 0,
        node,
        anchor: Anchor::Rename(node),
        stable: None,
    };
    let mut prompt = named_prompt(engine, trigger, current);
    prompt.page_icon = page_icon;
    Overlay::Name(prompt)
}

/// Open the selected-text rename prompt for an image's display name.
pub fn open_image_rename(engine: &mut TextEngine, node: NodeId, current: &str) -> Overlay {
    let trigger = Trigger {
        kind: TriggerKind::Slash,
        start: 0,
        node,
        anchor: Anchor::RenameImage(node),
        stable: None,
    };
    Overlay::Name(named_prompt(engine, trigger, current))
}

/// Build a fresh, empty link prompt anchored at `trigger`.
pub fn open_link_prompt(engine: &mut TextEngine, trigger: Trigger) -> Overlay {
    Overlay::Link(link_prompt(engine, trigger))
}

/// A link prompt seeded from whatever was selected when it opened.
///
/// A link is two answers — a caption and a target — and a selection is always
/// one of them. Which one is legible from the text itself: `https://…` is a
/// target, and a word is a caption. So the half the selection supplies is
/// filled in and **the other half takes the focus**, because that is the one
/// still to be answered. Selecting a word and pressing `Ctrl+K` should leave
/// you typing the URL, not retyping the word.
pub fn open_link_prompt_for(engine: &mut TextEngine, trigger: Trigger, selected: &str) -> Overlay {
    let mut prompt = link_prompt(engine, trigger);
    let selected = selected.trim();
    if !selected.is_empty() {
        let (filled, focus) = if link_prompt::looks_like_a_url(selected) {
            (&mut prompt.url, link_prompt::Field::Name)
        } else {
            (&mut prompt.name, link_prompt::Field::Url)
        };
        // `set_text` leaves the caret at the end and clears the history, so
        // Ctrl+Z cannot erase the text the prompt opened with.
        filled.set_text(engine, selected);
        prompt.field = focus;
    }
    Overlay::Link(prompt)
}

/// The panel a chosen `/` command opens.
pub fn for_command(engine: &mut TextEngine, trigger: Trigger, cmd: Command) -> Overlay {
    match cmd {
        Command::Document => Overlay::Entity(EntityPicker::new(trigger)),
        Command::Emoji => Overlay::Emoji(crate::components::emoji_picker::EmojiPicker::standalone(
            engine, trigger,
        )),
        // Image, Code, Diagram, Divider, the lists and a heading have no sub-picker —
        // all are intercepted in `Editing::choose_command` before this is
        // reached. The attribute commands are intercepted there too,
        // because their pickers open seeded with document state (the node's
        // current tags, color, due and priority) and this function
        // deliberately has no `ws`.
        Command::Image
        | Command::Attachment
        | Command::Code
        | Command::Diagram
        | Command::Table
        | Command::Divider
        | Command::Quote
        | Command::Tasks
        | Command::List(_)
        | Command::Attr
        | Command::Tag
        | Command::Color
        | Command::Due
        | Command::Priority
        | Command::TaskList
        | Command::Complete
        // `/task` has nothing left to choose either — `choose_command` runs
        // `ensure_task` and closes.
        | Command::Task
        | Command::Remind
        // `Dictate` has no sub-picker for a different reason again: it opens no
        // panel at all, ever. It is intercepted with Image and the rest.
        | Command::Dictate
        // `Delegate` the same: the modal it opens is the container's, not a
        // panel over the document, and `choose_command` intercepts it.
        | Command::Delegate
        | Command::Heading(_) => Overlay::None,
    }
}

/// A name prompt showing `current`, selected so typing replaces it — which is
/// what a rename affordance should do.
fn named_prompt(engine: &mut TextEngine, trigger: Trigger, current: &str) -> NamePrompt {
    let mut input = TextField::with_text(engine, theme::role::ui(), current);
    input.select_all(engine);
    NamePrompt {
        trigger,
        input,
        page_icon: None,
    }
}

fn link_prompt(engine: &mut TextEngine, trigger: Trigger) -> LinkPrompt {
    LinkPrompt {
        trigger,
        name: TextField::new(engine, theme::role::ui()),
        url: TextField::new(engine, theme::role::ui()),
        field: link_prompt::Field::Name,
    }
}

#[cfg(test)]
impl Trigger {
    /// A trigger a component test can hold. Components never read it — they hand
    /// it back untouched — so a throwaway node id is enough.
    pub fn at_start() -> Self {
        Self {
            kind: TriggerKind::Slash,
            start: 0,
            node: NodeId::new(),
            anchor: Anchor::TriggerText,
            stable: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every command on the table: these two are about which *picker* a trigger
    /// opens, not about what the picker lists.
    const ALL_ALLOWED: command_menu::Allows = command_menu::Allows {
        image: true,
        image_offline: false,
        divider: true,
        quote: true,
        headings: true,
        code: true,
        table: true,
        tasks: true,
        blocks: true,
        pills: true,
        attrs: true,
        task_capable: true,
        task: true,
        voice: true,
    };
    use crate::body::Body;
    use koru_core::WorkspaceId;
    use koru_core::node::Node;
    use koru_core::schema::{Parent, WorkspaceDoc};

    fn empty_ws() -> WorkspaceDoc {
        WorkspaceDoc::create(WorkspaceId::new(), "emoji tests").unwrap()
    }

    fn row_with(text: &str) -> Body {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        ws.insert(Parent::Root, Node::task(text).unwrap()).unwrap();
        Body::new(&ws)
    }

    #[test]
    fn triggers_only_arm_at_word_boundaries() {
        let o = row_with("see http:");
        let row = &o.rows[0];
        assert!(is_trigger_position(row, 0));
        assert!(is_trigger_position(row, 4)); // after the space
        assert!(!is_trigger_position(row, 9)); // mid-word, after ':'
    }

    /// `urls_in` finds every scheme URL in a line, leaves trailing punctuation
    /// as text, and ignores bare hosts and words.
    #[test]
    fn urls_in_finds_every_scheme_url() {
        let text = "see https://a.com/x and mailto:b@c.com, plus file.txt";
        let found: Vec<String> = urls_in(text).into_iter().map(|(_, u)| u).collect();
        assert_eq!(
            found,
            vec!["https://a.com/x".to_string(), "mailto:b@c.com".into()]
        );
        // Ranges exclude the trailing comma after the mailto.
        let ranges = urls_in(text);
        let comma = text.find(',').unwrap();
        assert!(ranges[1].0.end <= comma);
        assert!(text[ranges[1].0.clone()].starts_with("mailto:"));
    }

    /// A prompt field is a parley `PlainEditor` behind a `TextField` — no
    /// inline boxes needed, which is exactly what `PlainEditor` can do. This
    /// drives it without a window.
    #[test]
    fn document_prompt_accepts_typing_and_lays_out() {
        let mut engine = TextEngine::new();
        let trigger = Trigger {
            kind: TriggerKind::Slash,
            start: 0,
            node: NodeId::new(),
            anchor: Anchor::TriggerText,
            stable: None,
        };

        assert!(matches!(
            for_command(&mut engine, trigger, Command::Document),
            Overlay::Entity(_)
        ));
    }

    #[test]
    fn slash_commands_route_to_the_right_picker() {
        let mut engine = TextEngine::new();
        let trigger = Trigger {
            kind: TriggerKind::Slash,
            start: 0,
            node: NodeId::new(),
            anchor: Anchor::TriggerText,
            stable: None,
        };
        assert!(matches!(
            for_command(&mut engine, trigger.clone(), Command::Document),
            Overlay::Entity(_)
        ));
        assert!(matches!(
            for_command(&mut engine, trigger, Command::Emoji),
            Overlay::Emoji(_)
        ));
    }

    /// The Ctrl+K prompt drives two fields. This exercises the whole flow
    /// headlessly: open, type a name, switch field, type a URL, commit.
    #[test]
    fn link_prompt_collects_both_fields_and_commits() {
        let mut engine = TextEngine::new();
        let trigger = Trigger {
            kind: TriggerKind::Slash,
            start: 0,
            node: NodeId::new(),
            anchor: Anchor::TriggerText,
            stable: None,
        };

        let mut overlay = open_link_prompt(&mut engine, trigger);
        let Overlay::Link(LinkPrompt {
            name, url, field, ..
        }) = &mut overlay
        else {
            panic!("Ctrl+K should open the link prompt");
        };
        assert_eq!(
            *field,
            link_prompt::Field::Name,
            "name field is focused first"
        );

        name.set_text(&mut engine, "Terms page");
        *field = field.other();
        assert_eq!(
            *field,
            link_prompt::Field::Url,
            "Tab moves to the URL field"
        );
        url.set_text(&mut engine, "example.com/terms");

        assert_eq!(name.raw_text(), "Terms page");
        let spec = PillSpec::link(name.raw_text(), link_prompt::normalize_url(url.raw_text()));
        assert_eq!(spec.kind, PillKind::Link);
        assert_eq!(spec.label, "Terms page");
        assert_eq!(spec.url.as_deref(), Some("https://example.com/terms"));
    }

    #[test]
    fn re_editing_a_link_pill_prefills_both_fields() {
        let mut engine = TextEngine::new();
        let ws = empty_ws();
        let overlay = open_for_pill(
            &ws,
            &mut engine,
            NodeId::new(),
            4,
            PillKind::Link,
            "Terms page",
            None,
            Some("https://example.com/terms"),
        );
        let Overlay::Link(LinkPrompt {
            name, url, trigger, ..
        }) = &overlay
        else {
            panic!("clicking a link pill should open the link prompt");
        };
        assert_eq!(name.raw_text(), "Terms page");
        assert_eq!(url.raw_text(), "https://example.com/terms");
        // Commit must replace the pill's anchor, not a run of typed text.
        assert_eq!(trigger.anchor, Anchor::Pill);
        assert_eq!(trigger.start, 4);
    }

    #[test]
    fn re_editing_a_date_pill_seeds_the_stored_date() {
        let mut engine = TextEngine::new();
        let ws = empty_ws();
        let when = NaiveDate::from_ymd_opt(2026, 8, 2).unwrap();
        let overlay = open_for_pill(
            &ws,
            &mut engine,
            NodeId::new(),
            0,
            PillKind::Date,
            "Aug 2",
            Some(when),
            None,
        );
        match overlay {
            Overlay::Date(p) => {
                assert_eq!(p.grid.selected, when, "picker opens on the pill's own date");
                assert_eq!(p.trigger.anchor, Anchor::Pill);
            }
            _ => panic!("expected the date picker"),
        }
    }

    #[test]
    fn at_arms_the_entity_picker_directly() {
        let ws = empty_ws();
        assert!(matches!(
            open_for_trigger(
                &ws,
                &mut TextEngine::new(),
                TriggerKind::At,
                NodeId::new(),
                0,
                ALL_ALLOWED
            ),
            Overlay::Entity(_)
        ));
        assert!(matches!(
            open_for_trigger(
                &ws,
                &mut TextEngine::new(),
                TriggerKind::Slash,
                NodeId::new(),
                0,
                ALL_ALLOWED
            ),
            Overlay::Command(_)
        ));
    }
}
