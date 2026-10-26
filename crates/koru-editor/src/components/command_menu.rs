// SPDX-License-Identifier: MPL-2.0

//! The `/` menu: pick what kind of thing to insert.
//!
//! The only picker that does not produce a pill. Choosing an entry *becomes*
//! another picker — which is why [`Outcome::Become`] exists.

use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::Outcome;
use super::text_field::TextField;
use crate::trigger::Trigger;
use koru_core::ListStyle;
use koru_core::node::Heading;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme;

/// Which commands a view offers, gathered so the flags travel as one thing.
///
/// They were positional `bool`s on `matching`, on `open_for_trigger` and on
/// every construction of a menu — six chances to swap two of them and no way to
/// notice. `CommandMenu` holds one of these rather than a `bool` per gate, so
/// adding a seventh was one field rather than seven.
#[derive(Copy, Clone, Debug)]
pub struct Allows {
    pub image: bool,
    /// `/image` is offered here but cannot work at the moment, because the store
    /// that would hold the bytes is unreachable.
    ///
    /// **Listed and dim, not filtered out.** `image` above says whether images
    /// belong in this view at all; this says whether one could be stored right
    /// now. Hiding the command would answer "why is /image missing" with
    /// silence, and it is the sort of question somebody asks once and then stops
    /// trusting the menu over. See [`disabled`].
    pub image_offline: bool,
    pub divider: bool,
    pub quote: bool,
    pub headings: bool,
    pub code: bool,
    pub table: bool,
    pub tasks: bool,
    pub blocks: bool,
    /// Whether a *reference* may be inserted here — an attachment today, and
    /// whatever else grows an anchor character tomorrow.
    ///
    /// Separate from `blocks` because the two answers come apart in exactly one
    /// place: a document's **name**, which takes blocks from nothing and pills
    /// from nothing, but is ordinary text and so takes an emoji. A pill is
    /// anchored to a character, and a name is drawn as plain text everywhere it
    /// appears — a tab, a tree row, another document's pill — so one in a title
    /// would show as a stray anchor in all three.
    pub pills: bool,
    /// Whether a node **attribute** may be set here — `/attr`, `/tag`,
    /// `/color`, `/due`, `/priority`. Its own gate rather than `blocks` or
    /// `pills`, because an attribute is neither: a list item takes a tag even
    /// though it takes no block, and **so does a document's title row** — an
    /// attribute is metadata on the node, not content inside its text, so the
    /// thing that refuses a pill has no reason to refuse this
    /// ([ADR-187]).
    ///
    /// Nothing in the tree sets it `false` today; it stays a field because the
    /// question ("does this surface take node attributes at all") is one a
    /// second view will have to answer, and because [`matching`] is the only
    /// place it may be answered.
    ///
    /// [ADR-187]: ../../../../docs/decisions/ADR-201-225.md#adr-210-titleattrs--a-title-row-takes-attributes-but-not-pills-and-date-is-gone
    pub attrs: bool,
    /// Whether the row this menu was opened over is a **task-capable kind**
    /// ([ADR-285]'s closed set: paragraph, the legacy task kind, a document's
    /// title row, an image, a file).
    ///
    /// **Not `tasks`** — the field two lines up answers "does this view offer
    /// checkbox and bullet *lists*", which is a question about a block. This
    /// one is about the node under the caret, and the two are independent: a
    /// quote row in a document that offers checklists is still a quote, and a
    /// quote cannot carry a task marker. Naming them apart is deliberate; the
    /// whole reason [`Allows`] is a struct is that adjacent `bool`s get
    /// swapped.
    ///
    /// **False dims the row, it does not remove it.** `/task` is offered
    /// wherever attributes are (`attrs`), because task-ness *is* node state —
    /// `/due` creates a task through the same gate. What a non-capable kind
    /// gets is the [`disabled`] reason printed where the hint goes, which is
    /// this menu's house rule and the same answer `image_offline` gets. The
    /// model refuses underneath either way: `koru_core::tasks::ensure_task`
    /// calls `require_capable`, so a bypassed menu changes nothing.
    ///
    /// [ADR-285]: ../../../../docs/decisions/ADR-276-300.md#adr-285-taskmodel--a-task-is-a-marked-node-and-the-marker-or-a-due-date-is-the-whole-derivation
    pub task_capable: bool,
    /// Whether the row this menu was opened over **is a task** — what
    /// `/completed` needs, since completing a line that is not one is not a
    /// thing. False dims the row with the reason, as `task_capable` does.
    pub task: bool,
    /// Whether this installation offers dictation at all.
    ///
    /// **Absent, not dimmed**, when it does not: `image_offline`'s row is
    /// listed-and-dim because images *are* a thing here and one is temporarily
    /// unstorable, whereas `KORU_VOICE_DISABLED` means the feature is not part
    /// of this installation and a row explaining an environment variable is
    /// worse than no row. Set from `koru_voice::available()` where the menu is
    /// built — one predicate, so the four discovery surfaces cannot answer it
    /// four ways (Q-V44).
    pub voice: bool,
}

/// What `/` can offer. The vocabulary of the menu, in the order it lists them.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Command {
    /// Import a file and replace this command row with an attachment pill.
    Attachment,
    Document,
    Emoji,
    /// Add an image from disk. Document-only: the outline has no prose to drop
    /// one into, so the view never offers it.
    Image,
    /// Make this block a code block. Like a heading, nothing is left to choose
    /// once the command is picked, so it is applied directly. Document-only.
    Code,
    /// **Start a diagram**: a `mermaid` code block holding a three-line
    /// example that is drawn at once, its text open with the example selected
    /// so the first keystroke replaces it. Applied directly, like
    /// [`Code`](Self::Code), and offered wherever code blocks are. On a line
    /// that has words the words stay and the diagram is the block below.
    Diagram,
    /// **Complete the task this row is** — or reopen it, when it is done —
    /// through the canonical lifecycle command. Nothing left to choose, so
    /// it is applied the moment it is picked, as `/task` is. On a page whose
    /// title is the task, this is how the page is finished: the title strikes
    /// through.
    Complete,
    /// **Put this row in one of the workspace's lists**, or in a new one made
    /// here with its colour — and make the row a task first when it is not
    /// one, since a list holds tasks (SPEC §8). Opens the list picker; the
    /// rail is where the list is then found.
    TaskList,
    /// A GFM Markdown table rendered as an editable grid. Document-only.
    Table,
    /// A horizontal rule block. Document-only, like `Image`: it is prose
    /// furniture, and there is nothing left to choose once it is named.
    Divider,
    /// Make this block quoted prose. Document-only.
    Quote,
    /// Make this block a heading. The only entry that does not become another
    /// picker — there is nothing left to choose once the level is named.
    Heading(Heading),
    /// Wrap the current row into a `Kind::ListBlock` with `Checkbox` style — a
    /// to-do list. Document-only, like Image.
    Tasks,
    /// The same, with a marker style instead of checkboxes.
    ///
    /// **A list is a block, not a way of setting a line**, which is why all of
    /// these live here rather than in the selection bubble's block dropdown:
    /// that menu answers "what kind of text is this", and a list wraps lines
    /// rather than styling one.
    List(ListStyle),
    /// Browse this block's attributes — the engine `/tag` and `/color` are
    /// sugar over (D7). Becomes a small chooser whose rows `Become` the
    /// specific pickers below.
    Attr,
    /// Tag this block: becomes the tag chooser directly.
    Tag,
    /// Set this block's accent color: becomes the color picker directly.
    Color,
    /// Set this block's due date: becomes the date picker armed for
    /// `koru.due` — and the first command that takes a **typed argument**:
    /// `due friday` in the search field commits directly on Enter.
    Due,
    /// Create a Store-backed reminder for this entity. An argument is natural
    /// language input, but even a resolved argument opens confirmation.
    Remind,
    /// Set this block's priority: becomes the priority picker, or commits
    /// directly from `priority high`.
    Priority,
    /// **Track this row as a task.** Nothing is left to choose once it is
    /// named, so — like a heading — it is applied directly rather than
    /// becoming a picker: the editor runs `ensure_task` with
    /// [`TaskReason::Explicit`] in the same undo group that takes the typed
    /// `/task` back out.
    ///
    /// It is listed wherever the attribute commands are and **dimmed** on a
    /// kind that cannot be a task ([`Allows::task_capable`]).
    ///
    /// [`TaskReason::Explicit`]: koru_core::attributes::TaskReason::Explicit
    Task,
    /// **Start dictating here.** The chord's twin, for somebody who cannot
    /// remember it — and the row carries the chord in its second column, so
    /// choosing it three times is reading `Ctrl+Shift+D` three times.
    ///
    /// Like `Image` it has no panel to become: the row removes the typed
    /// `/dictate`, leaves the caret where it was, and asks the shell (through
    /// `ViewState::wants_dictation`) to begin a session — a microphone is no
    /// more a view's to open than a file dialog is.
    Dictate,
    /// **Hand this work to an agent** (ROADMAP D7, D18). One of the four
    /// gestures behind the single delegation entry, and the only one that is
    /// a slash command.
    ///
    /// Like `Dictate` it has no panel to become and writes nothing into the
    /// document: the row removes the typed `/delegate`, leaves the caret where
    /// it was, and asks the shell (through `ViewState::wants_delegate`) to open
    /// the modal with this row as the context it starts from. Starting an agent
    /// is no more a view's to do than opening a microphone is.
    ///
    /// **It carries no `Allows` gate**, unlike `Dictate`. Whether this device
    /// can delegate is the gateway's answer, and a view has no way to ask it
    /// without waiting on the world; the modal opens and says so in a sentence
    /// instead, which is the same shape as the empty agent list.
    Delegate,
}

impl Command {
    /// Every command, **in alphabetical order by [`label`](Self::label)**, which
    /// is the order the menu lists them.
    ///
    /// It was grouped by kind — headings, then lists, then blocks, then pills —
    /// which is a shape that makes sense to whoever wrote it and to nobody
    /// scanning for one entry. Sixteen rows is past the point where a reader
    /// can hold the grouping in their head, so it goes in the order they can
    /// already predict. `every_command_is_listed_alphabetically` keeps it that
    /// way as commands are added.
    ///
    /// Image, Code, Divider, the lists and the headings are filtered out by
    /// [`matching`] for views that do not offer them.
    pub const ALL: [Command; 29] = [
        Command::Attachment,
        Command::Attr,
        Command::Code,
        Command::Color,
        Command::Complete,
        Command::Delegate,
        Command::Diagram,
        Command::Dictate,
        Command::Divider,
        Command::Document,
        Command::Due,
        Command::Emoji,
        Command::Heading(Heading::H1),
        Command::Heading(Heading::H2),
        Command::Heading(Heading::H3),
        Command::Heading(Heading::H4),
        Command::Image,
        Command::List(ListStyle::Bullet),
        Command::Tasks,
        Command::List(ListStyle::Letter),
        Command::List(ListStyle::Numbered),
        Command::List(ListStyle::Roman),
        Command::Priority,
        Command::Quote,
        Command::Remind,
        Command::Table,
        Command::Tag,
        Command::Task,
        Command::TaskList,
    ];

    /// What the row reads.
    ///
    /// Separate from [`name`](Self::name), which is what you *type*: the menu
    /// showed `/header2` because the two were the same string, and a list of
    /// slash-prefixed tokens is a list of things to memorise rather than a list
    /// of things to pick.
    pub fn label(self) -> &'static str {
        match self {
            Self::Attachment => "Attachment",
            Self::Document => "Document",
            Self::Emoji => "Emoji",
            Self::Image => "Image",
            // `List - x`, so the five sort together in an alphabetical menu
            // and read as one family rather than five unrelated entries
            // scattered between Divider and Roman.
            Self::Tasks | Self::List(ListStyle::Checkbox) => "List - Checklist",
            Self::List(ListStyle::Bullet) => "List - Bulleted",
            Self::List(ListStyle::Numbered) => "List - Numbered",
            Self::List(ListStyle::Letter) => "List - Lettered",
            Self::List(ListStyle::Roman) => "List - Roman",
            Self::Code => "Code block",
            Self::Table => "Table",
            Self::Divider => "Divider",
            Self::Quote => "Quote",
            Self::Heading(Heading::H1) => "Heading 1",
            Self::Heading(Heading::H2) => "Heading 2",
            Self::Heading(Heading::H3) => "Heading 3",
            Self::Heading(_) => "Heading 4",
            Self::Attr => "Attributes",
            Self::Tag => "Tag",
            Self::Color => "Color",
            Self::Due => "Due",
            Self::Priority => "Priority",
            Self::Task => "Task",
            Self::TaskList => "Task list",
            Self::Complete => "Complete",
            Self::Remind => "Reminder",
            Self::Dictate => "Dictate",
            // **The one localized row**, and deliberately so: it is the
            // newest, it is the only one the roadmap requires in all eight
            // languages (standing requirement 2), and `koru_i18n::t`
            // answers `&'static str` so the signature does not move. The
            // twenty-one English rows above are a gap this does not widen;
            // `docs/contributing/techdebt/README.md` carries it.
            Self::Delegate => koru_i18n::t(koru_i18n::current(), koru_i18n::Key::DelegateCommand),
            // Localized for the reason `Delegate` is: a new row, and the
            // standing rule that a new string ships in every language.
            Self::Diagram => koru_i18n::t(koru_i18n::current(), koru_i18n::Key::DiagramCommand),
        }
    }

    /// The Phosphor glyph in the row's leading column.
    pub fn icon(self) -> char {
        use koru_draw::icons;
        match self {
            Self::Attachment => icons::FILE,
            Self::Document => icons::DOCUMENT,
            Self::Emoji => icons::HIGHLIGHT,
            Self::Image => icons::IMAGE,
            Self::Tasks | Self::List(ListStyle::Checkbox) => icons::LIST_CHECKS,
            Self::List(ListStyle::Numbered) => icons::LIST_NUM,
            Self::List(_) => icons::LIST,
            Self::Code => icons::CODE,
            Self::Diagram => icons::GRAPH,
            Self::Table => icons::TABLE,
            Self::Divider => icons::DIVIDER,
            Self::Quote => icons::QUOTES,
            Self::Heading(Heading::H1) => icons::HEADING_1,
            Self::Heading(Heading::H2) => icons::HEADING_2,
            Self::Heading(Heading::H3) => icons::HEADING_3,
            Self::Heading(_) => icons::HEADING_4,
            Self::Attr => icons::FADERS,
            Self::Tag => icons::TAG,
            Self::Color => icons::PALETTE,
            Self::Due => icons::DATE,
            Self::Priority => icons::FLAG,
            // **The Tasks identity glyph**, the one the context column's
            // `Section::Tasks` already wears — so the command that makes a task
            // and the column that lists them are the same mark. It is shared
            // with the checklist rows above, deliberately: a checklist and a
            // task are not the same thing (SPEC §4), but there is one Tasks
            // glyph in this product and inventing a second one to keep two rows
            // apart would cost more than the rows gain. They sit far apart in an
            // alphabetical list and read nothing alike.
            Self::Task => icons::LIST_CHECKS,
            // The rail's glyph for a list, so the command and the column agree.
            Self::TaskList => icons::LIST,
            Self::Complete => icons::CHECK,
            Self::Remind => icons::CLOCK,
            Self::Dictate => icons::MICROPHONE,
            // **The Agents identity glyph**, the one the explore column's
            // Agents navigator already wears, for the reason `Task` gives
            // above: the command that hands work to an agent and the column
            // that lists them are the same mark.
            Self::Delegate => icons::ROBOT,
        }
    }

    /// What you type after the `/`.
    ///
    /// **No spaces.** `trigger::query` gives up the moment the query contains
    /// whitespace — which is how a `/` typed mid-sentence un-arms itself — so
    /// the levels are `header1`, not `header 1`. Typing `/h` narrows to the
    /// four.
    pub fn name(self) -> &'static str {
        match self {
            Self::Attachment => "attachment",
            Self::Document => "document",
            Self::Emoji => "emoji",
            Self::Image => "image",
            Self::Tasks | Self::List(ListStyle::Checkbox) => "checklist",
            Self::List(ListStyle::Bullet) => "bullet",
            Self::List(ListStyle::Numbered) => "numbered",
            Self::List(ListStyle::Letter) => "lettered",
            Self::List(ListStyle::Roman) => "roman",
            Self::Code => "code",
            Self::Diagram => "diagram",
            Self::Table => "table",
            Self::Divider => "divider",
            Self::Quote => "quote",
            Self::Heading(Heading::H1) => "header1",
            Self::Heading(Heading::H2) => "header2",
            Self::Heading(Heading::H3) => "header3",
            Self::Heading(Heading::H4) => "header4",
            // Only four levels are offered; the rest exist so an imported
            // document renders rather than being flattened.
            Self::Heading(_) => "header",
            Self::Attr => "attr",
            Self::Tag => "tag",
            Self::Color => "color",
            Self::Due => "due",
            Self::Priority => "priority",
            Self::Task => "task",
            Self::TaskList => "task_list",
            // `completed`, as the owner typed it; `complete` is its prefix.
            Self::Complete => "completed",
            Self::Remind => "remind",
            Self::Dictate => "dictate",
            Self::Delegate => "delegate",
        }
    }

    /// Whether text after the name in the search field is an argument this
    /// command can commit from directly — `due friday`, `priority high`.
    /// D7's typed-argument sugar, and only these two: everything else either
    /// has nothing to type (`/color` is nine rows) or its own richer query
    /// (the tag chooser's field).
    pub fn takes_argument(self) -> bool {
        matches!(self, Self::Due | Self::Priority | Self::Remind)
    }

    /// The typing shorthand that does the same thing, set quietly at the row's
    /// right-hand end — the way the menus in the bubble carry their chords.
    /// Empty for the four that have none.
    ///
    /// **`Dictate` is the one that is a real chord**, so it comes from
    /// `koru_keys::show_hint` rather than being written out here: the label is
    /// right on macOS, it follows the registry if the chord is ever changed,
    /// and it goes quiet with the rest of the menu hints. `show_hint`'s `None`
    /// becomes the empty string every other hintless row already answers, so a
    /// silenced chord draws nothing rather than leaving a column behind.
    ///
    /// `Cow` rather than `&'static str` because that lookup is per-platform and
    /// cannot be a constant; every other arm is still borrowed.
    pub fn hint(self) -> std::borrow::Cow<'static, str> {
        use std::borrow::Cow;
        if self == Self::Dictate {
            return koru_keys::show_hint(koru_keys::ShortcutId::Dictate, koru_keys::Section::Menu)
                .map_or(Cow::Borrowed(""), Cow::Owned);
        }
        Cow::Borrowed(match self {
            Self::Attachment => "",
            Self::Document => "",
            Self::Emoji => ":",
            Self::Image => "",
            Self::Tasks | Self::List(ListStyle::Checkbox) => "[]",
            Self::List(ListStyle::Bullet) => "*",
            Self::List(ListStyle::Numbered) => "1.",
            Self::List(ListStyle::Letter) => "a.",
            Self::List(ListStyle::Roman) => "i.",
            Self::Code => "```",
            Self::Diagram => "```mermaid",
            Self::Table => "",
            Self::Divider => "---",
            Self::Quote => ">",
            Self::Heading(Heading::H1) => "#",
            Self::Heading(Heading::H2) => "##",
            Self::Heading(Heading::H3) => "###",
            Self::Heading(_) => "####",
            Self::Attr => "",
            // Mid-line, after a space — `#` at line start stays the heading
            // shorthand the arm above advertises.
            Self::Tag => "#",
            Self::Color => "",
            Self::Due => "",
            // **No chord and no typing shorthand** (owner decision). A task is
            // made by naming it; `[]` already means a checklist item, and a
            // second bracket grammar beside it would teach that the two are the
            // same thing.
            Self::Priority | Self::Task | Self::TaskList | Self::Complete => "",
            Self::Remind => "",
            // Answered above, before the borrowed table.
            Self::Dictate => "",
            // **No shorthand.** `Ctrl+Enter` belongs to the modal this row
            // opens, not to the row: printing it here would teach a chord
            // that does nothing until the surface is already up.
            Self::Delegate => "",
        })
    }
}

/// The commands whose name starts with `q`, minus whatever this view does not
/// offer — Image, Code, Divider, Tasks and the headings are all document-only.
///
/// Two gates cut across the rest. `blocks` is false inside a `ListBlock`
/// paragraph, where only inline commands make sense; `pills` is false on a
/// document's **name**, where a reference cannot go either. Emoji survives
/// both: it is a character, not a block and not a reference. So do the five
/// attribute commands, which are metadata on the node rather than anything
/// inside its text ([ADR-187]).
///
/// [ADR-187]: ../../../../docs/decisions/ADR-201-225.md#adr-210-titleattrs--a-title-row-takes-attributes-but-not-pills-and-date-is-gone
pub fn matching(q: &str, allows: &Allows) -> Vec<Command> {
    let q = q.to_lowercase();
    Command::ALL
        .iter()
        .copied()
        .filter(|c| match c {
            // **Emoji is text.** Not a block, not a reference — one character,
            // which is why it survives every gate: inside a list item, and in a
            // document's name, where nothing else here belongs.
            Command::Emoji => true,
            // **Dictation is text**, like Emoji and for the same reason: it
            // types where a keystroke would, so it survives the block gate,
            // the pill gate and a document's name alike. Its own gate is
            // whether this installation has the feature at all.
            Command::Dictate => allows.voice,
            // **Delegating is not a block and not a reference.** It writes
            // nothing into the document, so like Emoji and Dictate it
            // survives the block gate, the pill gate and a document's name
            // alike — handing a page's work over from its title is an
            // ordinary thing to want.
            Command::Delegate => true,
            Command::Attachment => allows.pills,
            // An attribute is node state, not a block and not a reference —
            // its own gate, so a list item still takes a tag.
            //
            // **`/task` rides that gate**, because task-ness *is* node state:
            // it is the `koru.task` marker, and `/due` on the same row creates
            // the same task through the same door (SPEC §3). Whether *this*
            // kind can carry one is a separate question, answered by
            // [`disabled`] rather than here — see [`Allows::task_capable`].
            Command::Attr
            | Command::Tag
            | Command::Color
            | Command::Due
            | Command::Priority
            | Command::Task
            | Command::TaskList
            | Command::Complete => allows.attrs,
            Command::Remind => allows.attrs,
            _ => allows.blocks,
        })
        .filter(|c| *c != Command::Image || allows.image)
        .filter(|c| !matches!(c, Command::Tasks | Command::List(_)) || allows.tasks)
        .filter(|c| !matches!(c, Command::Code | Command::Diagram) || allows.code)
        .filter(|c| *c != Command::Table || allows.table)
        .filter(|c| *c != Command::Divider || allows.divider)
        .filter(|c| *c != Command::Quote || allows.quote)
        .filter(|c| !matches!(c, Command::Heading(_)) || allows.headings)
        // A surface this build hides is absent, not dimmed — the same answer
        // `voice` gets, for the same reason: it is not part of this product.
        .filter(|c| product_shows(*c))
        // Matched on the label as well as the typed name, so "list" finds the
        // list rows even though none of them is *called* list. Attachment also
        // answers to the short prefix "attach".
        //
        // Word-prefix, not substring: "d" means Document, Date and Divider,
        // not every command with a `d` somewhere in it. A palette that answers
        // a single letter with most of its contents has not filtered anything.
        //
        // Diagram also answers to "mermaid", the word people type for one.
        .filter(|c| {
            c.name().starts_with(&q)
                || word_prefix(c.label(), &q)
                || (*c == Command::Attachment && "attach".starts_with(&q) && !q.is_empty())
                || (*c == Command::Diagram && "mermaid".starts_with(&q) && !q.is_empty())
        })
        .collect()
}

/// Whether this build shows the surface a command belongs to
/// (`koru_features`, guide/contributing/FEATURE-FLAGS.md).
///
/// **Not an [`Allows`] field**, because it is not a question about the view:
/// every view of this build answers it the same way, and a field would be one
/// more place for a construction to forget it. `/due` is deliberately not
/// here — it still creates a task with tasks hidden; only the ways *into* the
/// Tasks view and the task-management commands go.
pub fn product_shows(command: Command) -> bool {
    match command {
        Command::Task | Command::TaskList | Command::Complete | Command::Priority => {
            koru_features::tasks()
        }
        Command::Remind => koru_features::reminders(),
        Command::Delegate => koru_features::ai(),
        _ => true,
    }
}

/// Why this command cannot be chosen right now, if it cannot.
///
/// `None` means it is available. A command that is *never* available here has
/// already been filtered out by [`matching`]; this is only for the ones that are
/// offered and temporarily cannot work, so the menu can dim the row and say what
/// is wrong instead of leaving somebody to pick it and find out.
///
/// A `&'static str` rather than a bool, because the row has to print it and the
/// reason is the whole point of showing the row at all.
///
/// **`/task` on a kind that cannot be one is here rather than in [`matching`]**,
/// and that is this menu's house rule rather than a preference: a quote and a
/// code block are places somebody will reasonably try to make a task, and
/// answering that with a row that is not there is the same silence
/// `image_offline` exists to avoid. The model refuses underneath in either
/// case — `koru_core::tasks::ensure_task` calls `require_capable` — so the dim
/// row is an explanation, never the enforcement.
pub fn disabled(command: Command, allows: &Allows) -> Option<&'static str> {
    if matches!(command, Command::Image | Command::Attachment) && allows.image_offline {
        return Some("Offline");
    }
    if matches!(command, Command::Task | Command::TaskList) && !allows.task_capable {
        return Some("Can't be a task");
    }
    if command == Command::Complete && !allows.task {
        return Some("Not a task");
    }
    None
}

/// Whether any word of `label` starts with `q`. Hyphens split words too, so
/// "do" finds "To-do list".
fn word_prefix(label: &str, q: &str) -> bool {
    if q.is_empty() {
        return true;
    }
    label
        .split(|c: char| c.is_whitespace() || c == '-')
        .any(|word| word.to_lowercase().starts_with(q))
}

/// The panel's own metrics: a search box across the top, then the rows.
///
/// **The box's own numbers moved down to `koru-ui`** when the date grid took
/// its phrase field there (ADR-196): four panels wear the same box, two of them
/// now in different crates, and the only way two boxes stay one shape is for
/// both to ask one function. Re-exported here because every call site in this
/// crate reaches for it by this name.
pub use koru_ui::text_field::SEARCH_H;
/// The inside margin — above the box, between the box and the rows, and below
/// the last one.
///
/// **One number for all three.** It was the box's own 8px pad above and that
/// pad *plus* a separate 6px gap below, so the box sat 8 from the top and 14
/// from the first row, and read as hanging off the panel's edge.
const PAD: f32 = 8.0;
/// Where a row's label begins, past its icon.
pub const LABEL_X: f32 = 40.0;
/// The most rows the list shows before it scrolls.
///
/// Sixteen commands is a panel two thirds of a laptop's height, which covers
/// what is behind it and puts the last entry somewhere the pointer has to
/// travel to. Capped like the emoji grid, and scrolled the same way — a thumb
/// with no track, `docs/features/design/DESIGN.md`'s rule for every scrollbar here.
pub const MAX_ROWS: usize = 9;
pub(super) const SCROLLBAR_W: f32 = 5.0;

/// How many rows are actually shown, for `hits` of a given length.
pub fn visible_rows(hits: usize) -> usize {
    hits.clamp(1, MAX_ROWS)
}

/// The scrollbar's column inside a panel `panel_w` wide, from the rows' top.
///
/// Claimed before the rows, the way the document's own gutter is (ADR-28): a
/// press in that strip scrolls or is swallowed, never picks the row it happens
/// to be level with. Without this, clicking the thumb chose whatever entry was
/// beside it — or, past the last one, closed the menu.
pub fn scrollbar_column(panel_w: f32) -> (f32, f32) {
    (panel_w - SCROLLBAR_W - PAD, panel_w)
}

/// Where the search box sits inside a panel `panel_w` wide.
pub use koru_ui::text_field::search_rect;

/// How far down the panel the first row starts, measured from where the rows
/// would begin with no search box — the renderer and `item_at` both add
/// `PANEL_PAD` before this.
pub fn rows_top() -> f32 {
    koru_ui::text_field::search_band() - theme::PANEL_PAD
}

pub mod accessibility;

pub struct CommandMenu {
    /// One opening; filtering preserves it and reopening never reuses it.
    pub instance: u64,
    pub trigger: Trigger,
    /// The query, typed into the panel rather than into the document.
    ///
    /// **The query used to be the text after the `/`.** That worked while the
    /// menu was a completion of something you were already typing, but the `+`
    /// in the margin opens the same menu with no `/` and no line to type into —
    /// and the field is the honest answer for both. It also means the document
    /// never carries a half-written command: `/head` is a search, not four
    /// characters somebody has to take back out.
    ///
    /// `None` only in the suite, where a menu is built by hand without a text
    /// engine to give a field.
    pub query: Option<TextField>,
    pub hits: Vec<Command>,
    pub sel: usize,
    /// Offset from the thumb's top where a drag was grabbed.
    ///
    /// Kept on the menu rather than in the caller so the drag stays captured
    /// once the pointer wanders off the panel — the same shape the emoji
    /// picker's scrollbar uses, and the same reason.
    pub scrollbar_drag: Option<f32>,
    /// The first row shown, when the list is longer than the window.
    ///
    /// Real state rather than something derived from `sel`, because the two
    /// move independently: the arrows move the selection and drag the window
    /// after it, while the **wheel moves the window and leaves the selection
    /// alone** — which is what scrolling means everywhere else, and what it did
    /// not mean while `first` was `sel` minus a row count.
    pub first: usize,
    /// What this menu may offer. Set at arming time from the view and carried
    /// through `refilter`, so typing does not bring back a command that was
    /// filtered out — one field rather than seven `bool`s, which is the whole
    /// reason [`Allows`] exists.
    pub allows: Allows,
}

impl CommandMenu {
    /// A menu with its own search field, filtering `Command::ALL` down by the
    /// same rules a typed query does.
    pub fn opened(engine: &mut TextEngine, trigger: Trigger, allows: Allows) -> Self {
        Self {
            instance: accessibility::next_instance(),
            trigger,
            query: Some(TextField::new(engine, theme::role::ui())),
            hits: matching("", &allows),
            sel: 0,
            first: 0,
            scrollbar_drag: None,
            allows,
        }
    }

    /// Whether the panel carries a search box. Always, outside the suite.
    pub fn has_search(&self) -> bool {
        self.query.is_some()
    }

    pub fn first_visible(&self) -> usize {
        self.first.min(self.max_first())
    }

    /// The furthest the window can scroll: the last full page.
    fn max_first(&self) -> usize {
        self.hits.len().saturating_sub(MAX_ROWS)
    }

    /// Drag the window to wherever the selection went. Called after every
    /// keyboard move, and never by the wheel.
    fn scroll_selection_into_view(&mut self) {
        let rows = visible_rows(self.hits.len());
        if self.sel < self.first {
            self.first = self.sel;
        } else if self.sel >= self.first + rows {
            self.first = self.sel + 1 - rows;
        }
        self.first = self.first.min(self.max_first());
    }

    /// Whether the list is taller than the window it is shown in.
    pub fn scrolls(&self) -> bool {
        self.hits.len() > MAX_ROWS
    }

    pub fn height(&self) -> f32 {
        let rows = visible_rows(self.hits.len()) as f32 * theme::ROW_H;
        if self.has_search() {
            // `PAD` three times over: above the box, between it and the rows
            // (both inside `rows_top`), and below the last row. Computing the
            // three independently is how a panel ends up with twice the air at
            // one end as the other.
            theme::PANEL_PAD + rows_top() + rows + PAD
        } else {
            theme::PANEL_PAD * 2.0 + rows
        }
    }

    /// Re-filter from the field, in place.
    fn refilter_from_field(&mut self) {
        let q = self
            .query
            .as_ref()
            .map(TextField::value)
            .unwrap_or_default();
        // Trimmed so `due ` — a space typed on the way to an argument —
        // keeps the row it obviously means instead of flashing "No matches".
        self.hits = matching(q.trim(), &self.allows);
        if self.hits.is_empty()
            && let Some((cmd, _)) = self.argument()
        {
            // `due friday`: no command is *called* that, but the head names
            // one that takes arguments — keep its row on screen so the menu
            // says what Enter will do instead of "No matches".
            self.hits = vec![cmd];
        }
        self.sel = self.sel.min(self.hits.len().saturating_sub(1));
    }

    /// The typed-argument form, when the field holds one: a command name (or
    /// unambiguous prefix) that takes arguments, whitespace, and a non-empty
    /// remainder — `due friday`, `pri high`. What the remainder *means* is
    /// the editor's (it has the clock and the document); this only says
    /// which command owns it.
    fn argument(&self) -> Option<(Command, String)> {
        let text = self.query.as_ref().map(TextField::value)?;
        let (head, rest) = text.split_once(char::is_whitespace)?;
        let rest = rest.trim();
        if head.is_empty() || rest.is_empty() {
            return None;
        }
        let candidates: Vec<Command> = matching(head, &self.allows)
            .into_iter()
            .filter(|c| c.takes_argument() && c.name().starts_with(&head.to_lowercase()))
            .collect();
        match candidates.as_slice() {
            [only] => Some((*only, rest.to_owned())),
            _ => None,
        }
    }

    /// Re-filter as the query behind the menu changes. `None` means the query
    /// matches nothing and the menu should close.
    pub fn refilter(&self, trigger: Trigger, q: &str) -> Option<Self> {
        let hits = matching(q, &self.allows);
        if hits.is_empty() {
            return None;
        }
        let sel = self.sel.min(hits.len() - 1);
        Some(Self {
            instance: self.instance,
            trigger,
            // A document-driven re-filter never has a field; one with a field
            // re-filters itself and never comes through here.
            query: None,
            hits,
            sel,
            first: 0,
            scrollbar_drag: None,
            allows: self.allows,
        })
    }

    pub fn key(
        &mut self,
        engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Outcome {
        let named = match key {
            Key::Named(n) => Some(*n),
            _ => None,
        };
        match named {
            Some(NamedKey::ArrowDown) => {
                self.sel = (self.sel + 1) % self.hits.len().max(1);
                self.scroll_selection_into_view();
                Outcome::Consumed
            }
            Some(NamedKey::ArrowUp) => {
                self.sel = (self.sel + self.hits.len().saturating_sub(1)) % self.hits.len().max(1);
                self.scroll_selection_into_view();
                Outcome::Consumed
            }
            Some(NamedKey::Enter) | Some(NamedKey::Tab) => self.choose(self.sel),
            _ => {
                let Some(field) = &mut self.query else {
                    // No field: printable keys keep flowing into the document,
                    // which is how the suite's hand-built menus behave.
                    return Outcome::Ignored;
                };
                // Backspace on an empty field leaves the menu rather than
                // sitting there with nothing to erase — the way it would if the
                // query were still the typed `/command` it replaced.
                if named == Some(NamedKey::Backspace) && field.is_empty() {
                    return Outcome::Close;
                }
                field.key(engine, key, text, mods);
                self.refilter_from_field();
                // A narrower list has to start from the top, or the window is
                // parked past the end of what is left.
                self.first = 0;
                Outcome::Consumed
            }
        }
    }

    /// Put the caret in the search field at a panel-local x.
    pub fn click_field(&mut self, engine: &mut TextEngine, x: f32, panel_w: f32) {
        let rect = search_rect(panel_w);
        if let Some(field) = &mut self.query {
            field.click(engine, x - rect.x + field.scroll());
        }
    }

    pub fn drag_field(&mut self, engine: &mut TextEngine, x: f32, panel_w: f32) {
        let rect = search_rect(panel_w);
        if let Some(field) = &mut self.query {
            field.drag_to(engine, x - rect.x + field.scroll());
        }
    }

    /// Which row a panel-local point is over, past the search box.
    ///
    /// In *list* indices, not screen ones: a scrolled panel shows rows
    /// `first_visible..` in the same boxes, so the offset is added back here
    /// rather than at each call site.
    pub fn item_at(&self, local_y: f32) -> Option<usize> {
        let top = if self.has_search() { rows_top() } else { 0.0 };
        let row = ((local_y - top) / theme::ROW_H).floor();
        if row < 0.0 || row as usize >= visible_rows(self.hits.len()) {
            return None;
        }
        Some(self.first_visible() + row as usize)
    }

    /// Move the *window* by whole rows. The wheel's, so the selection stays
    /// where it is — dragging it along would make scrolling a way of changing
    /// what Enter does, which is not what a wheel means.
    pub fn scroll_rows(&mut self, delta: isize) {
        if self.hits.is_empty() {
            return;
        }
        self.first = (self.first as isize + delta).clamp(0, self.max_first() as isize) as usize;
    }

    /// What the empty field says it is for.
    pub fn placeholder(&self) -> &'static str {
        match self.trigger.anchor {
            crate::trigger::Anchor::NewBlock { above: true } => "Insert above",
            crate::trigger::Anchor::NewBlock { above: false } => "Insert below",
            _ => "Filter blocks",
        }
    }

    /// A click lands on whichever row it is over, or nowhere.
    pub fn click(&self, local_y: f32) -> Outcome {
        match self.item_at(local_y) {
            Some(row) => self.choose(row),
            None => Outcome::Consumed,
        }
    }

    /// The track the thumb runs in, in panel-local coordinates.
    pub fn scrollbar_track(&self, panel_w: f32) -> Rect {
        let (x, _) = scrollbar_column(panel_w);
        let top = theme::PANEL_PAD + if self.has_search() { rows_top() } else { 0.0 };
        let h = visible_rows(self.hits.len()) as f32 * theme::ROW_H;
        Rect::new(x + 4.0, top, SCROLLBAR_W, h)
    }

    /// The thumb, in the same coordinates. **One function**, so the drawn pill
    /// and the press that grabs it cannot disagree about where it is.
    pub fn scrollbar_thumb(&self, panel_w: f32) -> Rect {
        let track = self.scrollbar_track(panel_w);
        let shown = visible_rows(self.hits.len()) as f32;
        let h = (track.h * shown / self.hits.len().max(1) as f32).clamp(20.0, track.h);
        let travel = (track.h - h).max(0.0);
        let t = self.first_visible() as f32 / self.max_first().max(1) as f32;
        Rect::new(track.x, track.y + travel * t, track.w, h)
    }

    /// A press in the scrollbar column.
    ///
    /// **On the thumb it grabs**, remembering where along it the press landed
    /// so the pill does not jump under the pointer; above or below it, the
    /// window scrubs there and *then* grabs, so one press-and-drag both moves
    /// the list and keeps hold of it.
    pub fn press_scrollbar(&mut self, panel_w: f32, local_y: f32) {
        if !self.scrolls() {
            return;
        }
        let thumb = self.scrollbar_thumb(panel_w);
        if local_y >= thumb.y && local_y <= thumb.y + thumb.h {
            self.scrollbar_drag = Some(local_y - thumb.y);
            return;
        }
        self.scrollbar_drag = Some(thumb.h * 0.5);
        self.drag_scrollbar(panel_w, local_y);
    }

    pub fn is_dragging_scrollbar(&self) -> bool {
        self.scrollbar_drag.is_some()
    }

    /// Carry the grabbed thumb to a panel-local y. Does **not** hit-test: a
    /// drag that has wandered off the panel is still that thumb's drag.
    pub fn drag_scrollbar(&mut self, panel_w: f32, local_y: f32) {
        let Some(grab) = self.scrollbar_drag else {
            return;
        };
        let track = self.scrollbar_track(panel_w);
        let thumb = self.scrollbar_thumb(panel_w);
        let travel = (track.h - thumb.h).max(0.0);
        let top = (local_y - track.y - grab).clamp(0.0, travel);
        self.first = if travel > 0.0 {
            (top / travel * self.max_first() as f32).round() as usize
        } else {
            0
        };
    }

    pub fn stop_scrollbar_drag(&mut self) -> bool {
        self.scrollbar_drag.take().is_some()
    }

    fn choose(&self, i: usize) -> Outcome {
        match self.hits.get(i) {
            // **A disabled row does nothing at all — it does not even close.**
            // Closing would look like the command ran, which is the confusion
            // the dim row and its reason exist to prevent. The menu stays up
            // with the explanation still on screen.
            Some(&cmd) if disabled(cmd, &self.allows).is_some() => Outcome::Consumed,
            // The typed-argument form — `due friday` — commits through the
            // command rather than opening its picker. Checked in `choose`
            // rather than only on Enter so a click on the row the argument
            // kept visible does the same thing the key does.
            Some(&cmd) if cmd.takes_argument() => match self.argument() {
                Some((owner, arg)) if owner == cmd => Outcome::CommandArg(cmd, arg),
                _ => Outcome::Become(cmd),
            },
            Some(&cmd) => Outcome::Become(cmd),
            None => Outcome::Close,
        }
    }
}

/// The first row of whichever command menu is open, or zero.
pub fn first_visible(overlay: &super::Overlay) -> usize {
    match overlay {
        super::Overlay::Command(m) => m.first_visible(),
        _ => 0,
    }
}

/// The thumb down the right of a scrolled list. No track — the rest of the
/// interface wears none either (`docs/features/design/DESIGN.md`, ADR-28).
pub fn draw_scrollbar(
    scene: &mut vello::Scene,
    root: vello::kurbo::Affine,
    view: &crate::view_state::ViewState,
    px: f32,
    rows_y: f32,
    pw: f32,
    hits: usize,
) {
    let super::Overlay::Command(menu) = &view.overlay else {
        return;
    };
    if !menu.scrolls() {
        return;
    }
    let shown = visible_rows(hits) as f32;
    let band = shown * theme::ROW_H;
    let thumb_h = (band * shown / hits as f32).max(20.0);
    let travel = band - thumb_h;
    let max_first = hits - MAX_ROWS;
    let t = menu.first_visible() as f32 / max_first.max(1) as f32;
    koru_draw::scene::fill(
        scene,
        root,
        theme::p().scrollbar_thumb,
        &koru_draw::scene::rrect(
            px + pw - SCROLLBAR_W - 4.0,
            rows_y + theme::PANEL_PAD + travel * t,
            SCROLLBAR_W,
            thumb_h,
            SCROLLBAR_W * 0.5,
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::ModifiersState;

    /// Every command on the table. Tests that care about one gate clone this
    /// and turn that one off, which reads better than eight positional `bool`s.
    const ALL_ALLOWED: Allows = Allows {
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

    /// Serialises the two tests that read the process-global hint switch.
    static HINTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// `ALL_ALLOWED` with the named gates shut.
    fn without(f: impl FnOnce(&mut Allows)) -> Allows {
        let mut a = ALL_ALLOWED;
        f(&mut a);
        a
    }

    /// **The simplified product's `/` menu** (koru-features all off): no
    /// task-management commands, no reminder, no delegate — and `/due` and the
    /// checklist stay, because the first still makes a task and the second is
    /// a document block.
    #[test]
    fn hidden_surfaces_leave_the_menu_and_due_and_checklist_stay() {
        koru_features::testing::with(koru_features::Features::ALL_OFF, || {
            let all = matching("", &ALL_ALLOWED);
            for hidden in [
                Command::Task,
                Command::TaskList,
                Command::Complete,
                Command::Priority,
                Command::Remind,
                Command::Delegate,
            ] {
                assert!(!all.contains(&hidden), "{hidden:?} is still offered");
            }
            for kept in [
                Command::Due,
                Command::Tasks,
                Command::Attr,
                Command::Dictate,
            ] {
                assert!(all.contains(&kept), "{kept:?} went missing");
            }
            assert_eq!(matching("due", &ALL_ALLOWED), vec![Command::Due]);
            assert!(matching("remind", &ALL_ALLOWED).is_empty());
            assert!(matching("delegate", &ALL_ALLOWED).is_empty());
        });
        // Each flag gates only its own commands.
        koru_features::testing::with(
            koru_features::Features {
                tasks: true,
                ..koru_features::Features::ALL_OFF
            },
            || {
                let all = matching("", &ALL_ALLOWED);
                assert!(all.contains(&Command::Task) && all.contains(&Command::Priority));
                assert!(!all.contains(&Command::Remind) && !all.contains(&Command::Delegate));
            },
        );
    }

    #[test]
    fn the_filter_is_prefix_and_case_insensitive() {
        assert_eq!(
            matching("d", &ALL_ALLOWED),
            // Alphabetical. `List - Checklist` no longer matches: its words are
            // "List" and "Checklist", and the hyphen is a separator with spaces
            // around it rather than part of a word.
            vec![
                Command::Delegate,
                Command::Diagram,
                Command::Dictate,
                Command::Divider,
                Command::Document,
                Command::Due,
            ]
        );
        assert_eq!(matching("DOC", &ALL_ALLOWED), vec![Command::Document]);
        assert!(matching("zz", &ALL_ALLOWED).is_empty());
        assert_eq!(matching("", &ALL_ALLOWED).len(), Command::ALL.len());
        assert_eq!(
            matching(
                "",
                &without(|a| {
                    a.image = false;
                    a.divider = false;
                    a.quote = false;
                    a.headings = false;
                    a.code = false;
                    a.tasks = false;
                })
            ),
            // Alphabetical by label. Attachment is an inline reference, and
            // the attribute commands are node state, so all remain
            // available when block commands are gated off.
            vec![
                Command::Attachment,
                Command::Attr,
                Command::Color,
                Command::Complete,
                Command::Delegate,
                Command::Dictate,
                Command::Document,
                Command::Due,
                Command::Emoji,
                Command::Priority,
                Command::Remind,
                Command::Table,
                Command::Tag,
                Command::Task,
                Command::TaskList,
            ]
        );
        assert_eq!(
            matching("i", &without(|a| a.image = false)),
            vec![],
            "image stays hidden when disallowed"
        );
        assert_eq!(
            matching(
                "i",
                &without(|a| {
                    a.divider = false;
                    a.headings = false;
                })
            ),
            vec![Command::Image]
        );
        assert_eq!(
            matching("t", &ALL_ALLOWED),
            vec![
                Command::Table,
                Command::Tag,
                Command::Task,
                Command::TaskList
            ],
            "Checklist no longer claims the task prefix; Table, Tag and Task do"
        );
        // Inside a ListBlock: the two references survive, so does Emoji —
        // it is a character, not a block — and so do the attribute commands,
        // because a list item takes a tag. Dictation is text as well: it types
        // where a keystroke would.
        assert_eq!(
            matching("", &without(|a| a.blocks = false)),
            vec![
                Command::Attachment,
                Command::Attr,
                Command::Color,
                Command::Complete,
                Command::Delegate,
                Command::Dictate,
                Command::Due,
                Command::Emoji,
                Command::Priority,
                Command::Remind,
                Command::Tag,
                Command::Task,
                Command::TaskList,
            ],
            "block commands hidden inside a list"
        );
        // On a document's name — blocks and references off, attributes on
        // ([ADR-187]) — the emoji and the five attribute commands survive.
        assert_eq!(
            matching(
                "",
                &without(|a| {
                    a.blocks = false;
                    a.pills = false;
                })
            ),
            vec![
                Command::Attr,
                Command::Color,
                Command::Complete,
                Command::Delegate,
                Command::Dictate,
                Command::Due,
                Command::Emoji,
                Command::Priority,
                Command::Remind,
                Command::Tag,
                Command::Task,
                Command::TaskList,
            ],
            "a name takes an emoji and its own attributes"
        );
        // With attributes off as well, exactly one row survives — the gate
        // still cuts, even though no surface currently turns it off.
        assert_eq!(
            matching(
                "",
                &without(|a| {
                    a.blocks = false;
                    a.pills = false;
                    a.attrs = false;
                })
            ),
            // And Dictate, whose only gate is whether this installation has
            // dictation at all, and Delegate, which has no gate here at all:
            // the three things you can do to a document's name that are
            // neither a block nor a reference.
            vec![Command::Delegate, Command::Dictate, Command::Emoji],
        );
    }

    /// The six attribute commands share one gate, and shutting it offers
    /// none of them — typing the full name must not bring one back.
    ///
    /// **No surface sets `attrs` false today.** A title row used to, and that
    /// was the bug [ADR-187] fixed; the gate survives as the one place the
    /// question is answered, so this drives it directly.
    #[test]
    fn attribute_commands_are_gated_together() {
        let off = without(|a| a.attrs = false);
        for q in ["attr", "tag", "color", "due", "priority", "task", "remind"] {
            assert!(
                matching(q, &off).is_empty(),
                "/{q} offered where attributes are refused"
            );
        }
        assert_eq!(matching("attr", &ALL_ALLOWED), vec![Command::Attr]);
        assert_eq!(matching("tag", &ALL_ALLOWED), vec![Command::Tag]);
        assert_eq!(matching("color", &ALL_ALLOWED), vec![Command::Color]);
        assert_eq!(matching("due", &ALL_ALLOWED), vec![Command::Due]);
        assert_eq!(matching("priority", &ALL_ALLOWED), vec![Command::Priority]);
        assert_eq!(
            matching("task", &ALL_ALLOWED),
            vec![Command::Task, Command::TaskList]
        );
        assert_eq!(matching("remind", &ALL_ALLOWED), vec![Command::Remind]);
        // The row teaches the typing grammar: `#`, mid-line — heading keeps
        // line start.
        assert_eq!(Command::Tag.hint(), "#");
    }

    /// Code is document-only like image, so a view that does not offer it never
    /// lists it, and typing further must not bring it back.
    #[test]
    fn code_is_hidden_from_a_view_that_does_not_offer_it() {
        assert!(
            !matching("code", &without(|a| a.code = false)).contains(&Command::Code),
            "a view with no code blocks never offers one"
        );
        assert_eq!(matching("code", &ALL_ALLOWED), vec![Command::Code]);
    }

    #[test]
    fn quote_is_document_only_and_carries_its_typing_hint() {
        assert_eq!(matching("quote", &ALL_ALLOWED), vec![Command::Quote]);
        assert!(matching("quote", &without(|a| a.quote = false)).is_empty());
        assert_eq!(Command::Quote.hint(), ">");
    }

    /// The list is alphabetical by label, and stays that way as commands are
    /// added. Sixteen rows grouped by kind is a list you have to read twice.
    #[test]
    fn every_command_is_listed_alphabetically() {
        let labels: Vec<&str> = Command::ALL.iter().map(|c| c.label()).collect();
        let mut sorted = labels.clone();
        sorted.sort_by_key(|l| l.to_lowercase());
        assert_eq!(labels, sorted);
    }

    /// **`/dictate` teaches the chord rather than replacing it**, and the chord
    /// comes from the registry, so the row is right on macOS and follows the
    /// binding if it is ever changed.
    #[test]
    fn the_dictate_row_carries_the_chord_in_its_second_column() {
        let _quiet = HINTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            Command::Dictate.hint(),
            koru_keys::registry::label_of(koru_keys::ShortcutId::Dictate)
        );
        assert!(matching("dictate", &ALL_ALLOWED).contains(&Command::Dictate));
    }

    /// **`None` draws nothing rather than an empty column.** `show_hint`
    /// answers `None` when menu hints are silenced, and the row then reads like
    /// every other hintless one instead of leaving the gap the chord used to
    /// fill.
    ///
    /// The switch is a process global, so both tests that read it take
    /// [`HINTS`] -- otherwise the one that turns `Menu` off fails the other at
    /// random, which is the hazard `koru-keys`
    /// `a_section_can_be_turned_off_on_its_own` records. `Command::hint` is the
    /// only thing in this crate that prints a chord.
    #[test]
    fn a_silenced_chord_leaves_no_column_behind() {
        let _quiet = HINTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        koru_keys::hint::set_shown(koru_keys::Section::Menu, false);
        let quiet = Command::Dictate.hint();
        koru_keys::hint::set_shown(koru_keys::Section::Menu, true);
        assert_eq!(quiet, "", "a silenced chord has to draw like no chord");
        assert!(!Command::Dictate.hint().is_empty(), "and come back");
    }

    /// **Dictation is text.** It types where a keystroke would, so it survives
    /// the gates that refuse blocks and references -- a list item, and a
    /// document's own name -- exactly as Emoji does. Its own gate is whether
    /// this installation has the feature at all.
    #[test]
    fn the_dictate_row_survives_every_gate_but_its_own() {
        for shut in [
            without(|a| a.blocks = false),
            without(|a| a.pills = false),
            without(|a| a.attrs = false),
        ] {
            assert!(matching("", &shut).contains(&Command::Dictate));
        }
        assert!(
            !matching("", &without(|a| a.voice = false)).contains(&Command::Dictate),
            "an installation with dictation switched off must not offer it"
        );
        assert!(
            !matching("dictate", &without(|a| a.voice = false)).contains(&Command::Dictate),
            "and typing its name does not bring it back"
        );
    }

    /// The query matches a row's **label** as well as the token you type, so
    /// "list" finds the three list rows even though none of them is called
    /// `list`. Before the rows had labels there was nothing else to match on.
    #[test]
    fn the_query_matches_the_label_as_well_as_the_name() {
        let hits = matching("list", &ALL_ALLOWED);
        // `List - x` for all five, so one word finds the whole family and they
        // sit together in an alphabetical menu.
        assert_eq!(
            hits,
            vec![
                Command::List(ListStyle::Bullet),
                Command::Tasks,
                Command::List(ListStyle::Letter),
                Command::List(ListStyle::Numbered),
                Command::List(ListStyle::Roman),
                // ...and the list that is a task's, which is a different
                // thing and reads so beside them.
                Command::TaskList,
            ]
        );
    }

    /// The outline structures itself with the tree, so it offers no levels —
    /// and typing further must not bring one back.
    #[test]
    fn the_heading_commands_are_hidden_from_a_view_that_does_not_offer_them() {
        assert_eq!(matching("h", &without(|a| a.headings = false)), vec![]);
        assert_eq!(
            matching("h", &ALL_ALLOWED),
            vec![
                Command::Heading(Heading::H1),
                Command::Heading(Heading::H2),
                Command::Heading(Heading::H3),
                Command::Heading(Heading::H4),
            ]
        );
        assert_eq!(
            matching("header2", &ALL_ALLOWED),
            vec![Command::Heading(Heading::H2)],
            "the level can be named outright"
        );
    }

    /// Every name is one word: `trigger::query` gives up on whitespace, so a
    /// command called "header 1" could never be typed in full.
    #[test]
    fn no_command_name_contains_a_space() {
        for cmd in Command::ALL {
            assert!(
                !cmd.name().contains(char::is_whitespace),
                "{cmd:?} is unreachable behind a space"
            );
        }
    }

    /// A real menu, with the search field every one outside a test has.
    fn menu() -> (TextEngine, CommandMenu) {
        let mut engine = TextEngine::new();
        let menu = CommandMenu::opened(
            &mut engine,
            Trigger::at_start(),
            Allows {
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
            },
        );
        (engine, menu)
    }

    /// The same fixture as [`menu`], with the gates a test wants to vary.
    fn menu_allowing(allows: Allows) -> (TextEngine, CommandMenu) {
        let mut engine = TextEngine::new();
        let menu = CommandMenu::opened(&mut engine, Trigger::at_start(), allows);
        (engine, menu)
    }

    fn press(engine: &mut TextEngine, m: &mut CommandMenu, key: NamedKey) -> Outcome {
        m.key(engine, &Key::Named(key), None, ModifiersState::empty())
    }

    fn typed(engine: &mut TextEngine, m: &mut CommandMenu, text: &str) {
        for ch in text.chars() {
            let s = ch.to_string();
            m.key(
                engine,
                &Key::Character(s.as_str().into()),
                Some(&s),
                ModifiersState::empty(),
            );
        }
    }

    /// Wrapping both ways, because `sel - 1` on an unsigned index at zero is
    /// how this goes wrong.
    #[test]
    fn the_selection_wraps_in_both_directions() {
        let (mut engine, mut m) = menu();
        let last = m.hits.len() - 1;
        press(&mut engine, &mut m, NamedKey::ArrowUp);
        assert_eq!(m.sel, last, "up from the first entry lands on the last");
        press(&mut engine, &mut m, NamedKey::ArrowDown);
        assert_eq!(m.sel, 0);
    }

    /// The query is typed into the panel, not into the document.
    ///
    /// It used to be the text after the `/`, which the `+` in the margin has no
    /// equivalent of — and which left half-written commands in the line until
    /// something took them out again.
    #[test]
    fn typing_filters_through_the_field() {
        let (mut engine, mut m) = menu();
        let all = m.hits.len();
        typed(&mut engine, &mut m, "div");
        assert!(m.hits.len() < all);
        assert_eq!(m.hits, vec![Command::Divider]);
        assert_eq!(
            m.query.as_ref().map(TextField::value).as_deref(),
            Some("div")
        );

        press(&mut engine, &mut m, NamedKey::Backspace);
        press(&mut engine, &mut m, NamedKey::Backspace);
        press(&mut engine, &mut m, NamedKey::Backspace);
        assert_eq!(m.hits.len(), all, "erasing it brings everything back");
    }

    /// A query that matches nothing leaves the menu open and empty rather than
    /// dismissing it. With the query in the document, closing was the only way
    /// to stop showing a menu with no rows; with a field, closing would take
    /// away the box you were typing into.
    #[test]
    fn a_query_matching_nothing_leaves_the_menu_open() {
        let (mut engine, mut m) = menu();
        typed(&mut engine, &mut m, "zzzz");
        assert!(m.hits.is_empty());
        assert_eq!(m.sel, 0, "and the selection does not run off the end");
    }

    /// Backspace with nothing left to erase leaves, rather than sitting on an
    /// empty box with no way out but Escape.
    #[test]
    fn backspace_on_an_empty_field_closes_the_menu() {
        let (mut engine, mut m) = menu();
        assert!(matches!(
            press(&mut engine, &mut m, NamedKey::Backspace),
            Outcome::Close
        ));
    }

    #[test]
    fn re_filtering_clamps_a_selection_that_no_longer_exists() {
        let (_engine, mut m) = menu();
        m.sel = 3;
        let narrowed = m.refilter(m.trigger.clone(), "d").expect("hits");
        assert!(narrowed.hits.len() >= 2);
        assert_eq!(
            narrowed.sel,
            3.min(narrowed.hits.len() - 1),
            "clamped rather than left dangling"
        );
        assert!(
            m.refilter(m.trigger.clone(), "zz").is_none(),
            "no hits closes it"
        );
    }

    /// Choosing does not commit a pill — it swaps the menu for the picker that
    /// does. Getting this wrong inserts a pill labelled "date".
    #[test]
    fn choosing_becomes_another_picker() {
        let (mut engine, mut m) = menu();
        assert!(matches!(
            press(&mut engine, &mut m, NamedKey::Enter),
            Outcome::Become(_)
        ));
    }

    /// The rows sit below the search box, and a click has to know it.
    #[test]
    fn every_row_hit_tests_back_to_itself_below_the_search_box() {
        let (_engine, m) = menu();
        assert_eq!(m.item_at(rows_top() - 1.0), None, "in the search box");
        for i in 0..visible_rows(m.hits.len()) {
            let y = rows_top() + (i as f32 + 0.5) * theme::ROW_H;
            assert_eq!(m.item_at(y), Some(i));
        }
        let past = rows_top() + (visible_rows(m.hits.len()) as f32 + 0.5) * theme::ROW_H;
        assert_eq!(m.item_at(past), None, "past the last visible row");
    }

    /// A long list scrolls rather than growing past the window, and the rows it
    /// shows are still hit-tested by their place in the *list* — the offset is
    /// added back in `item_at` rather than at every call site.
    #[test]
    fn a_scrolled_list_hit_tests_by_list_index() {
        let (mut engine, mut m) = menu();
        assert!(
            m.scrolls(),
            "the full command set is longer than the window"
        );
        assert_eq!(m.first_visible(), 0);
        assert_eq!(m.item_at(rows_top() + 0.5 * theme::ROW_H), Some(0));

        // Walk past the bottom of the window: the list scrolls under it.
        for _ in 0..MAX_ROWS {
            press(&mut engine, &mut m, NamedKey::ArrowDown);
        }
        assert_eq!(m.sel, MAX_ROWS);
        assert_eq!(m.first_visible(), 1);
        assert_eq!(m.item_at(rows_top() + 0.5 * theme::ROW_H), Some(1));
    }

    /// **The wheel moves the window, not the selection.** Scrolling a list is
    /// how you look at the rest of it; if it dragged the selection along, a
    /// wheel would be a way of changing what `Enter` does.
    #[test]
    fn the_wheel_scrolls_the_window_and_leaves_the_selection_alone() {
        let (_engine, mut m) = menu();
        assert_eq!((m.sel, m.first_visible()), (0, 0));

        m.scroll_rows(3);
        assert_eq!(m.first_visible(), 3, "the window moved");
        assert_eq!(m.sel, 0, "and the selection did not");

        // It stops at the last full page rather than scrolling into blank space.
        m.scroll_rows(999);
        assert_eq!(m.first_visible(), m.hits.len() - MAX_ROWS);
        m.scroll_rows(-999);
        assert_eq!(m.first_visible(), 0);
    }

    /// **The thumb is grabbed and dragged**, not only pressed. The press
    /// remembers where along the pill it landed, so the thumb does not jump
    /// under the pointer, and the drag stays captured wherever the pointer goes.
    #[test]
    fn the_thumb_can_be_grabbed_and_dragged_to_either_end() {
        const PW: f32 = theme::PANEL_W;
        let (_engine, mut m) = menu();
        let (bar_x, right) = scrollbar_column(PW);
        assert!(bar_x < right && bar_x > PW * 0.5);

        // Grab the thumb a third of the way down it, and it stays a third of
        // the way down as the drag moves.
        let thumb = m.scrollbar_thumb(PW);
        let grab = thumb.y + thumb.h / 3.0;
        m.press_scrollbar(PW, grab);
        assert!(m.is_dragging_scrollbar());
        assert_eq!(m.first_visible(), 0, "grabbing alone moves nothing");

        let track = m.scrollbar_track(PW);
        m.drag_scrollbar(PW, track.y + track.h + 100.0);
        assert_eq!(m.first_visible(), m.max_first(), "dragged to the end");
        assert_eq!(m.sel, 0, "and the selection is untouched");

        m.drag_scrollbar(PW, track.y - 100.0);
        assert_eq!(m.first_visible(), 0, "and back to the start");
        assert!(m.stop_scrollbar_drag());
        assert!(!m.is_dragging_scrollbar());

        // A drag with nothing grabbed does nothing.
        m.drag_scrollbar(PW, track.y + track.h);
        assert_eq!(m.first_visible(), 0);
    }

    /// A press in the track *above or below* the thumb jumps there and then
    /// grabs, so one press-and-drag both moves the list and keeps hold of it.
    #[test]
    fn a_press_off_the_thumb_jumps_there_and_grabs() {
        const PW: f32 = theme::PANEL_W;
        let (_engine, mut m) = menu();
        let track = m.scrollbar_track(PW);
        m.press_scrollbar(PW, track.y + track.h);
        assert_eq!(m.first_visible(), m.max_first());
        assert!(m.is_dragging_scrollbar());
    }

    /// A menu that fits has no scrollbar, so a press there is nothing at all.
    #[test]
    fn a_short_list_has_no_scrollbar_to_press() {
        let (mut engine, mut m) = menu();
        typed(&mut engine, &mut m, "head");
        assert!(!m.scrolls());
        m.press_scrollbar(theme::PANEL_W, rows_top() + 40.0);
        assert!(!m.is_dragging_scrollbar());
        assert_eq!(m.first_visible(), 0);
    }

    /// Filtering puts the window back at the top, or it is parked past the end
    /// of what is left.
    #[test]
    fn narrowing_the_list_scrolls_back_to_the_top() {
        let (mut engine, mut m) = menu();
        m.scroll_rows(4);
        assert_eq!(m.first_visible(), 4);
        typed(&mut engine, &mut m, "h");
        assert_eq!(m.first_visible(), 0);
    }

    /// The panel never grows past its cap, however many commands there are.
    #[test]
    fn the_panel_stops_growing_at_its_cap() {
        let (_engine, m) = menu();
        let capped = m.height();
        assert!(m.hits.len() > MAX_ROWS);
        assert!(capped < rows_top() + (m.hits.len() as f32) * theme::ROW_H);
    }

    /// The panel is tall enough for the box it draws.
    #[test]
    fn the_panel_reserves_room_for_its_search_box() {
        let (_engine, m) = menu();
        let rows = visible_rows(m.hits.len()) as f32 * theme::ROW_H;
        assert!(m.height() > rows + SEARCH_H);
    }

    /// A click that misses every row chooses nothing — above the first, and
    /// below the last visible one.
    #[test]
    fn a_click_that_misses_every_row_chooses_nothing() {
        let (_engine, m) = menu();
        assert!(matches!(m.click(-4.0), Outcome::Consumed));
        assert!(matches!(m.click(9999.0), Outcome::Consumed));
    }

    /// **Offline, `/image` is still listed** — dimmed and explained, not gone.
    ///
    /// `allows.image` answers "do images belong in this view", and turning it
    /// off *removes* the row, which is right for a place they can never go.
    /// Offline is a different question, and hiding the command would answer
    /// "why is /image missing" with silence.
    #[test]
    fn an_offline_image_command_is_listed_rather_than_hidden() {
        let offline = Allows {
            image_offline: true,
            ..ALL_ALLOWED
        };
        assert!(
            matching("", &offline).contains(&Command::Image),
            "the command vanished instead of going dim"
        );
        assert_eq!(disabled(Command::Image, &offline), Some("Offline"));

        // The other half: a view with no prose still drops it entirely.
        let no_images = Allows {
            image: false,
            ..ALL_ALLOWED
        };
        assert!(!matching("", &no_images).contains(&Command::Image));
    }

    /// Nothing else is disabled by the store being away. Images and attachments
    /// need blob storage at insertion time; a heading is a local edit.
    #[test]
    fn being_offline_disables_blob_commands_and_nothing_else() {
        let offline = Allows {
            image_offline: true,
            ..ALL_ALLOWED
        };
        for command in matching("", &offline) {
            let why = disabled(command, &offline);
            if matches!(command, Command::Image | Command::Attachment) {
                assert_eq!(why, Some("Offline"));
            } else {
                assert_eq!(why, None, "{command:?} was disabled by being offline");
            }
        }
    }

    /// **Choosing it does nothing, and does not close the menu.** Closing would
    /// read as the command having run, which is exactly the confusion the dim
    /// row exists to prevent — the explanation stays on screen instead.
    #[test]
    fn choosing_a_disabled_command_neither_runs_nor_closes() {
        let (mut engine, mut m) = menu_allowing(Allows {
            image_offline: true,
            ..ALL_ALLOWED
        });
        typed(&mut engine, &mut m, "image");
        assert_eq!(
            m.hits,
            vec![Command::Image],
            "the query did not narrow to it"
        );

        assert!(matches!(
            press(&mut engine, &mut m, NamedKey::Enter),
            Outcome::Consumed
        ));

        // ...and it is choosable again the moment the store is back.
        let (mut engine, mut m) = menu_allowing(ALL_ALLOWED);
        typed(&mut engine, &mut m, "image");
        assert!(matches!(
            press(&mut engine, &mut m, NamedKey::Enter),
            Outcome::Become(Command::Image)
        ));
    }

    /// **`due friday` commits through the command, not into its picker.**
    /// The typed-argument form (D7): a command name — or an unambiguous
    /// prefix — a space, and a remainder. The menu hands the pair up as
    /// [`Outcome::CommandArg`]; what the remainder means (a date, a level)
    /// is the editor's, which has the clock.
    #[test]
    fn delegate_is_one_row_that_opens_no_panel_and_is_offered_everywhere() {
        // Typed as `/delegate`, listed as the localized word, and reached by a
        // prefix of either — the same two doors every other row has.
        let (mut engine, mut m) = menu();
        typed(&mut engine, &mut m, "deleg");
        assert_eq!(m.hits, vec![Command::Delegate]);
        assert!(matches!(
            press(&mut engine, &mut m, NamedKey::Enter),
            Outcome::Become(Command::Delegate)
        ));

        // **No gate of its own.** Whether this device can delegate is the
        // gateway's answer and a view cannot ask it without waiting on the
        // world, so the row is always offered and the modal says so instead.
        for allows in [
            without(|a| a.blocks = false),
            without(|a| a.pills = false),
            without(|a| a.attrs = false),
            without(|a| a.voice = false),
        ] {
            assert!(
                matching("delegate", &allows).contains(&Command::Delegate),
                "delegation writes nothing into the document, so no block, \
                 pill, attribute or dictation gate can hide it"
            );
        }

        // It opens no panel: the editor intercepts it and asks the shell.
        let mut engine = TextEngine::new();
        assert!(matches!(
            crate::trigger::for_command(&mut engine, Trigger::at_start(), Command::Delegate),
            crate::components::Overlay::None
        ));

        // The typed name is ASCII whatever the language, because it is what
        // you type; the label is what you read, and it is localized.
        assert_eq!(Command::Delegate.name(), "delegate");
        assert_eq!(
            Command::Delegate.label(),
            koru_i18n::t(koru_i18n::Lang::En, koru_i18n::Key::DelegateCommand)
        );
        assert!(
            Command::Delegate.hint().is_empty(),
            "the chord belongs to the modal, not to the row"
        );
    }

    #[test]
    fn a_typed_argument_commits_through_the_command() {
        let (mut engine, mut m) = menu();
        typed(&mut engine, &mut m, "due friday");
        // The owning row stays visible while the argument is typed, instead
        // of "No matches".
        assert_eq!(m.hits, vec![Command::Due]);
        assert!(matches!(
            press(&mut engine, &mut m, NamedKey::Enter),
            Outcome::CommandArg(Command::Due, arg) if arg == "friday"
        ));

        // An unambiguous prefix works — `pri high` can only mean Priority.
        let (mut engine, mut m) = menu();
        typed(&mut engine, &mut m, "pri high");
        assert!(matches!(
            press(&mut engine, &mut m, NamedKey::Enter),
            Outcome::CommandArg(Command::Priority, arg) if arg == "high"
        ));

        // A click on the kept row does the same thing the key does.
        let (mut engine, mut m) = menu();
        typed(&mut engine, &mut m, "due tomorrow");
        assert!(matches!(
            m.click(rows_top() + 0.5 * theme::ROW_H),
            Outcome::CommandArg(Command::Due, arg) if arg == "tomorrow"
        ));
    }

    /// The head resolves among **argument-taking** commands only: `d 31`
    /// can only mean Due, because Divider and Document take no
    /// argument — and a command that takes none never grows one, so
    /// `color red` is just a query matching nothing.
    #[test]
    fn the_head_resolves_among_argument_taking_commands_only() {
        let (mut engine, mut m) = menu();
        typed(&mut engine, &mut m, "d 31");
        assert_eq!(m.hits, vec![Command::Due], "only Due takes an argument");
        assert!(matches!(
            press(&mut engine, &mut m, NamedKey::Enter),
            Outcome::CommandArg(Command::Due, arg) if arg == "31"
        ));

        // `color red` — Color takes no argument, so this is just a query
        // that matches nothing.
        let (mut engine, mut m) = menu();
        typed(&mut engine, &mut m, "color red");
        assert!(m.hits.is_empty());
        assert!(matches!(
            press(&mut engine, &mut m, NamedKey::Enter),
            Outcome::Close
        ));

        // `due` with no remainder is the ordinary query it always was.
        let (mut engine, mut m) = menu();
        typed(&mut engine, &mut m, "due ");
        assert_eq!(m.hits, vec![Command::Due]);
        assert!(matches!(
            press(&mut engine, &mut m, NamedKey::Enter),
            Outcome::Become(Command::Due)
        ));
    }

    /// **`/task` on a kind that cannot be one is listed and dim**, with the
    /// reason where the hint goes — this menu's house rule, the same answer
    /// `/image` gets when the store is away. Hiding it would answer "why is
    /// /task missing on this row" with silence, and a quote is exactly the row
    /// somebody reasonably tries it on (SPEC §4's escape hatch is a nearby
    /// checklist item, and nothing can say so if the row is not there).
    ///
    /// The enforcement half is not here at all: `koru_core::tasks::ensure_task`
    /// refuses a non-capable kind whatever a menu drew — pinned by
    /// `task_is_dimmed_and_refused_on_a_kind_that_cannot_be_one` and
    /// `the_excluded_kinds_all_refuse_the_marker` in `editor.rs`, and by
    /// `koru-core`'s own suite.
    #[test]
    fn task_is_listed_and_dimmed_on_a_kind_that_cannot_be_one() {
        let incapable = without(|a| a.task_capable = false);
        assert!(
            matching("", &incapable).contains(&Command::Task),
            "the row vanished instead of going dim"
        );
        assert_eq!(disabled(Command::Task, &incapable), Some("Can't be a task"));
        assert_eq!(disabled(Command::Task, &ALL_ALLOWED), None);

        // And nothing *else* is dimmed by the row's kind: an incapable row
        // still takes a tag, a color, a heading and an emoji.
        for command in matching("", &incapable) {
            if !matches!(command, Command::Task | Command::TaskList) {
                assert_eq!(
                    disabled(command, &incapable),
                    None,
                    "{command:?} was disabled by the row's kind"
                );
            }
        }
    }

    /// Choosing the dim row does nothing and leaves the menu up — the same
    /// contract the offline `/image` row has, so the reason stays on screen.
    #[test]
    fn choosing_task_on_an_incapable_row_neither_runs_nor_closes() {
        let (mut engine, mut m) = menu_allowing(without(|a| a.task_capable = false));
        typed(&mut engine, &mut m, "task");
        assert_eq!(m.hits, vec![Command::Task, Command::TaskList]);
        assert!(matches!(
            press(&mut engine, &mut m, NamedKey::Enter),
            Outcome::Consumed
        ));

        // ...and it commits the moment the row underneath can carry one.
        let (mut engine, mut m) = menu_allowing(ALL_ALLOWED);
        typed(&mut engine, &mut m, "task");
        assert!(matches!(
            press(&mut engine, &mut m, NamedKey::Enter),
            Outcome::Become(Command::Task)
        ));
    }

    /// `task` is not an argument-taking command: there is nothing to type after
    /// it, so `task friday` is a query that matches nothing rather than a
    /// silent commit.
    #[test]
    fn task_takes_no_argument() {
        assert!(!Command::Task.takes_argument());
        assert_eq!(Command::Task.hint(), "", "no chord and no shorthand");
        let (mut engine, mut m) = menu();
        typed(&mut engine, &mut m, "task friday");
        assert!(m.hits.is_empty());
    }

    /// Where attributes are refused (a title), the argument form is refused
    /// with them — the gate cannot be typed around.
    #[test]
    fn the_argument_form_respects_the_attrs_gate() {
        let (mut engine, mut m) = menu_allowing(without(|a| a.attrs = false));
        typed(&mut engine, &mut m, "due friday");
        assert!(m.hits.is_empty());
        assert!(matches!(
            press(&mut engine, &mut m, NamedKey::Enter),
            Outcome::Close
        ));
    }
}
