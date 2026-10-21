// SPDX-License-Identifier: MPL-2.0

//! The document's right-click surface: a formatting toolbar and edit menu.
//!
//! The toolbar is the existing selection bubble, not a second command strip;
//! the list is the existing [`menu::Menu`]. This module owns only their joint
//! geometry and the menu choices. Normally the toolbar is above the click
//! and the menu below it. When the menu flips above near the viewport floor,
//! the toolbar stacks above the flipped menu so the two never overlap.
//! A right-click on a document pill prepends Open / Open in new tab / Open in
//! new window — the same three destinations as click / Ctrl+click / Shift+click.

use koru_draw::{geometry::Rect, icons};

use crate::{bubble, menu};

const GAP: f32 = 4.0;
const EDGE_MARGIN: f32 = 8.0;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Choice {
    /// Drill into the corrections submenu — the full menu's one spelling row.
    SpellingMenu,
    /// Replace the misspelled word with suggestion `n` of the frozen snapshot.
    Spelling(usize),
    /// Stop flagging this word for the rest of the session.
    SpellingIgnore,
    /// Stop flagging this word in this document, durably, for this person.
    SpellingIgnoreDocument,
    /// Add this word to the personal dictionary — valid everywhere.
    SpellingAddWord,
    /// Add this word to the workspace dictionary — valid for collaborators.
    SpellingAddWorkspaceWord,
    Open,
    OpenInNewTab,
    OpenInNewWindow,
    /// Open the attachment in the OS-associated app.
    OpenFile,
    /// Switch Explore to Files and scroll the attachment into view.
    RevealFile,
    /// Save a copy of the attachment via the shell's save dialog.
    SaveFileAs,
    /// Delete the attachment pill's anchor from the body (not the file itself).
    RemoveAttachment,
    /// Put a trashed file back; the only action offered while it is in Trash.
    RestoreFile,
    // Missing-file menu is Remove only — handled via RemoveAttachment.
    Cut,
    Copy,
    Paste,
    PastePlain,
    /// Open the document picker in move mode over the selection — the same
    /// picker `@` over marked rows opens, hence the `@` hint on the row.
    MoveToDocument,
    /// **Hand this work to an agent** (ROADMAP D7): one of the four gestures
    /// behind the single delegation entry. Beside *Add reminder* because it is
    /// the same shape of thought — hand this line to something that will act
    /// on it later — and the view only *asks*: the modal belongs to the
    /// container.
    Delegate,
    /// Open the tag chooser over the selection (or the clicked row) — the
    /// same picker `/tag` and `#tag` open.
    Tags,
    /// Open the accent color picker, ditto `/color`.
    Color,
    /// Open the due date grid, ditto `/due` (roadmap A3).
    Due,
    /// Open the priority picker, ditto `/priority` (roadmap A4).
    Priority,
    /// **Show this task on the Tasks page**, on the queue it falls in, with its
    /// row selected. The return leg of *Go to origin*.
    GoToTask,
    /// **The chip's own two rows.** `EditChip` opens the picker the chip's
    /// kind owns, over the chip's own row — the same one a left-click on it
    /// opens, which is what makes the menu a second door rather than a second
    /// gesture. `ClearChip` empties that one value.
    ///
    /// Neither names the attribute: the chip under the pointer already does,
    /// and it is remembered on the view for the same reason the document and
    /// attachment pills are.
    EditChip,
    ClearChip,
    /// Complete this task outright, or reopen it — SPEC §5's **explicit
    /// action**, which is what the row's label already says it will do.
    ///
    /// One `Choice` rather than two: the row is a toggle over one canonical
    /// lifecycle, and splitting it would let a menu offer both at once on a
    /// state that has only one sensible move.
    CompleteTask,
    /// **Delete the paragraph itself**, and anything under it.
    ///
    /// The other half of the question *Remove from Tasks* cannot answer on its
    /// own: a task is a marked node, so taking the marker off leaves the
    /// writing behind, and sometimes the writing is what you wanted gone. Only
    /// on the due chip's menu, where the two sit together and the difference
    /// between them is visible.
    DeleteRow,
    /// Stop tracking this row as a task — SPEC §6, and the one row in this
    /// menu that takes something away.
    ///
    /// It opens a confirm rather than acting, because what it clears is not
    /// visible from the row it is clicked on: the marker goes, the canonical
    /// lifecycle goes, and so does the **due date**, which may be the only
    /// reason the row is in anyone's Today list. Priority and the checkbox
    /// survive, so neither is mentioned.
    RemoveFromTasks,
    /// Open the shared one-time reminder creation surface for this object.
    Remind,
    ReminderEdit,
    ReminderPause,
    ReminderResume,
    ReminderDelete,
    Table(koru_editor::table::Edit),
    /// Open a diagram's text for editing — the same thing Enter on the
    /// selected drawing does; on a block that does not draw, the caret lands
    /// on the line its reason names.
    DiagramEditText,
    /// Save a picture of a drawn diagram through the shell's save dialog. The
    /// view only asks (`koru_editor::SaveDiagramAs`): the dialog, the raster
    /// and the file are the shell's.
    SaveDiagramAs,
}

/// Which pill, if any, the context menu was opened on.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum PillContext {
    #[default]
    None,
    Document,
    /// A **tag, colour, due or priority chip** on a row.
    ///
    /// **A chip is not a run of text**, and the menu it was getting said
    /// otherwise: the formatting toolbar over it, then Cut / Copy / Paste /
    /// Paste as plain / Move to document, then the four attribute rows that act
    /// on the whole *paragraph*. Right-clicking a due pill offered to change
    /// the row's colour and to paste over it, and offered nothing at all about
    /// the date under the pointer.
    ///
    /// Two rows, on the attachment pill's precedent: edit this value, and clear
    /// it. What *Edit* opens is the same picker a left-click opens — the menu
    /// is a second door to one gesture, not a second gesture.
    Attribute(ChipMenu),
    Attachment,
    /// Attachment whose file is currently in the workspace Trash.
    AttachmentTrashed,
    /// Attachment whose file was permanently deleted / is unresolvable.
    AttachmentMissing,
    /// One exact inline reminder pill. Its occurrence state decides which
    /// lifecycle actions are still meaningful without changing the menu after
    /// it opens.
    Reminder {
        paused: bool,
        handled: bool,
    },
}

/// Which chip a right-click landed on, and therefore what its two rows say.
///
/// The kind rather than the value: a menu is built once and frozen, and the
/// value can be changed by the picker the menu itself opens.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ChipMenu {
    Tag,
    Color,
    Due,
    Priority,
}

impl ChipMenu {
    /// **Written out rather than composed.** A row's label is a `&'static str`,
    /// and the four pairs read better chosen than assembled: *Edit tags* is
    /// plural where the other three are singular, and *Clear tag* takes the one
    /// under the pointer where the other three take the row's only value.
    fn edit_label(self) -> &'static str {
        match self {
            Self::Tag => "Edit tags…",
            Self::Color => "Edit colour…",
            Self::Due => "Edit due date…",
            Self::Priority => "Edit priority…",
        }
    }

    fn clear_label(self) -> &'static str {
        match self {
            Self::Tag => "Remove tag",
            Self::Color => "Clear colour",
            Self::Due => "Clear due date",
            Self::Priority => "Clear priority",
        }
    }

    fn icon(self) -> char {
        match self {
            Self::Tag => icons::TAG,
            Self::Color => icons::PALETTE,
            Self::Due => icons::DATE,
            Self::Priority => icons::FLAG,
        }
    }
}

impl PillContext {
    /// Whether this pill's menu is **the whole menu**.
    ///
    /// A chip and an attachment pill each answer with their own rows and
    /// nothing else: no formatting toolbar above, no spelling block leading,
    /// no clipboard or attribute rows behind. A *document* pill is the other
    /// shape — its three open rows lead the ordinary menu rather than
    /// replacing it — and `None` is the ordinary menu itself.
    ///
    /// **Four places ask this question and they have to agree.** `rows` builds
    /// the list, `choice_at` maps an index back into it, and two more decide
    /// whether the toolbar is drawn. When they disagreed, the rows drawn and
    /// the rows mapped came apart: a chip's menu on a row with a spelling
    /// issue drew three rows while `choice_at` still subtracted the spelling
    /// block's length, so pressing the second row did nothing. That is what
    /// this predicate exists to make impossible — the answer is here, once.
    pub fn owns_the_menu(self) -> bool {
        matches!(
            self,
            Self::Attribute(_)
                | Self::Attachment
                | Self::AttachmentTrashed
                | Self::AttachmentMissing
                | Self::Reminder { .. }
        )
    }
}

const DOCUMENT_PILL_PREFIX: usize = 4;

/// What the row the menu was opened on is **as a task** — the one fact the
/// Complete row is built from.
///
/// It is the canonical lifecycle (`koru.task_state`), never the checkbox: SPEC
/// §5 lets the two disagree on a task with children, and this row is precisely
/// the gesture that exists because a rich task's box is only a box. A task with
/// no children keeps them in step, so the label is right either way.
///
/// `NotATask` dims the row rather than removing it, because
/// [`choice_at`]'s map is by index and dimming shifts none of them — the same
/// rule the four attribute rows follow on a mixed selection.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum TaskContext {
    #[default]
    NotATask,
    Open,
    Completed,
}

impl TaskContext {
    /// What a row's cached task state means for this menu.
    pub fn of(task: Option<koru_editor::TaskRow>) -> Self {
        match task {
            None => Self::NotATask,
            Some(task) if task.completed => Self::Completed,
            Some(_) => Self::Open,
        }
    }

    fn is_task(self) -> bool {
        !matches!(self, Self::NotATask)
    }
}

fn click_hint() -> menu::Chord {
    menu::Chord::Literal("Click")
}

fn primary_click_hint() -> menu::Chord {
    menu::Chord::Literal(if cfg!(target_os = "macos") {
        "⌘Click"
    } else {
        "Ctrl+Click"
    })
}

fn shift_click_hint() -> menu::Chord {
    menu::Chord::Literal(if cfg!(target_os = "macos") {
        "⇧Click"
    } else {
        "Shift+Click"
    })
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct TableContext {
    pub cell: (usize, usize),
    pub source_visible: bool,
}

/// The menu was opened on a `mermaid` code block. `drawn` is whether this
/// view currently holds a drawing of its text, which is what *Save as…* needs
/// and the only row of the group that comes and goes.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct DiagramContext {
    pub node: koru_core::NodeId,
    pub drawn: bool,
}

/// The spelling rows' input: what the snapshot holds for the clicked word.
/// `suggestions: None` is the cold cache — one dimmed placeholder row, filled
/// in place the moment the worker answers (D13 as amended at acceptance).
/// An empty slice is a word the engine genuinely has nothing for.
#[derive(Copy, Clone, Debug)]
pub struct SpellContext<'a> {
    pub suggestions: Option<&'a [String]>,
    /// A right-click on the word with nothing selected is *about* the word:
    /// the menu is the corrections alone — no edit rows, no toolbar.
    pub only: bool,
}

impl SpellContext<'_> {
    /// How many rows the spelling block prepends — the one number [`rows`]
    /// and [`choice_at`] must agree on, so it is written once.
    fn prefix_len(&self) -> usize {
        if self.only {
            // Suggestions (or the one placeholder) + Ignore + Ignore in this
            // document + Add to personal dictionary + Add to workspace
            // dictionary. No divider: the block is the whole menu.
            self.suggestions.map_or(1, |s| s.len().max(1)) + 4
        } else {
            // The full menu carries one drill-in row and its divider; the
            // corrections live a level down (acceptance: the inline block
            // made the selection menu too long).
            2
        }
    }
}

/// The spelling block. Corrections-only (`only`): suggestion rows (bold-free,
/// word-as-label) and the four word actions — the whole menu. Otherwise one
/// *Spelling…* drill-in row and a divider. Split out of [`rows`] so the
/// prefix arithmetic sits beside [`SpellContext::prefix_len`].
fn spelling_rows(spell: &SpellContext<'_>, lang: koru_i18n::Lang) -> Vec<menu::Row> {
    let mut rows = Vec::with_capacity(spell.prefix_len());
    if !spell.only {
        rows.push(
            menu::Row::item(koru_i18n::t(lang, koru_i18n::Key::SpellingMenu))
                .with_icon(icons::PENCIL),
        );
        rows.push(menu::Row::Divider);
        return rows;
    }
    match spell.suggestions {
        None => rows.push(
            menu::Row::item(koru_i18n::t(lang, koru_i18n::Key::SpellingNoSuggestionsYet))
                .enabled(false),
        ),
        Some([]) => rows.push(
            menu::Row::item(koru_i18n::t(lang, koru_i18n::Key::SpellingNoSuggestions))
                .enabled(false),
        ),
        Some(suggestions) => {
            // No icons: the suggestions should read as the words they are.
            for suggestion in suggestions {
                rows.push(menu::Row::text(suggestion.clone()));
            }
        }
    }
    rows.push(
        menu::Row::item(koru_i18n::t(lang, koru_i18n::Key::SpellingIgnore)).with_icon(icons::CLEAR),
    );
    rows.push(
        menu::Row::item(koru_i18n::t(lang, koru_i18n::Key::SpellingIgnoreInDocument))
            .with_icon(icons::DOCUMENT),
    );
    rows.push(
        menu::Row::item(koru_i18n::t(lang, koru_i18n::Key::SpellingAddToDictionary))
            .with_icon(icons::PLUS),
    );
    rows.push(
        menu::Row::item(koru_i18n::t(lang, koru_i18n::Key::SpellingAddToWorkspace))
            .with_icon(icons::TAG),
    );
    rows
}

/// Open a context menu at `at` with a **pre-measured** `width` (frozen for the
/// life of the menu). `with_toolbar` is true when the formatting bubble sits
/// above the menu — both share the toolbar's left edge. Attachment-only menus
/// have no bubble and hang from the click like a native cursor menu.
/// `chrome_h` is the window chrome's height: near the top of the viewport the
/// toolbar cannot fit above the click, and without the floor below it clamped
/// *down over the menu's first row* — invisibly, because the menu paints last,
/// but the toolbar answers the pointer first, so that row could not be picked.
pub fn open(
    at: (f32, f32),
    viewport_w: f32,
    chrome_h: f32,
    width: f32,
    with_toolbar: bool,
) -> menu::Menu {
    let width = width.clamp(menu::MIN_W, menu::MAX_W);
    if with_toolbar {
        // The pointer centres the wider toolbar, then both surfaces share that
        // resolved left edge. Encoding the toolbar's width in the opener rect lets
        // `Menu::panel` keep owning viewport clamping instead of introducing a
        // context-menu-only panel calculation.
        let toolbar_w = bubble::width();
        let x = (at.0 - toolbar_w * 0.5).clamp(
            EDGE_MARGIN,
            (viewport_w - toolbar_w - EDGE_MARGIN).max(EDGE_MARGIN),
        );
        let y = at.1.max(chrome_h + EDGE_MARGIN + bubble::height() + GAP);
        menu::Menu::new(Rect::new(x, y, toolbar_w, 0.0), width)
    } else {
        // Cursor-style: top-left near the press, clamped so the panel stays on screen.
        let x = at.0.clamp(
            EDGE_MARGIN,
            (viewport_w - width - EDGE_MARGIN).max(EDGE_MARGIN),
        );
        menu::Menu::new(Rect::new(x, at.1, 0.0, 0.0), width)
    }
}

/// Why the four attribute rows dim on a selection that spans the document's
/// name *and* rows of its body.
///
/// A dimmed row that does not explain itself is a dead end, and this one is
/// easy to hit by accident: a drag that begins one row too high picks up the
/// title. Applying to the body alone silently skipped the document, which was
/// the bug; applying to both writes a document-level fact nobody asked for. So
/// the rule is *unambiguous or nothing*, and the note says which way out
/// ([ADR-210](../../../docs/decisions/README.md)).
const MIXED_NOTE: &str = "Title or body, not both";

/// Fixed shape, dynamic enabled state. A context menu is anchored to the
/// current insertion point; without one it can teach the shortcuts but cannot
/// honestly claim any edit will run.
///
/// `mixed_attributes` is that second refusal: the selection covers the title
/// row and body rows together, so the four attribute rows dim and carry
/// [`MIXED_NOTE`]. The row *set* is unchanged either way — dimming shifts no
/// index, which is what keeps [`choice_at`]'s hard-coded map honest.
impl Choice {
    /// Whether this build shows the surface the choice belongs to
    /// (`koru_features`, guide/contributing/FEATURE-FLAGS.md): the task rows with
    /// tasks (Due stays — it still makes a task), the reminder rows with
    /// reminders, Delegate with Koru AI.
    pub fn shown_in_product(self) -> bool {
        match self {
            Self::GoToTask | Self::CompleteTask | Self::RemoveFromTasks | Self::Priority => {
                koru_features::tasks()
            }
            Self::Remind
            | Self::ReminderEdit
            | Self::ReminderPause
            | Self::ReminderResume
            | Self::ReminderDelete => koru_features::reminders(),
            Self::Delegate => koru_features::ai(),
            _ => true,
        }
    }
}

/// The menu's rows: [`full_rows`] minus every row whose choice this build
/// hides ([`Choice::shown_in_product`]), dividers tidied.
///
/// **Identity, not index arithmetic.** The menu below is mapped by position
/// ([`choice_at`]), so a row cannot simply be left out of the list: every
/// index after it would name its neighbour. Both halves instead go through
/// [`kept`] — which full rows survive, decided from each row's *choice* — so
/// the drawn list and the map are the same filter over the same full list.
#[allow(clippy::too_many_arguments)]
pub fn rows(
    has_target: bool,
    has_selection: bool,
    mixed_attributes: bool,
    task: TaskContext,
    has_reminder_target: bool,
    table: Option<TableContext>,
    diagram: Option<DiagramContext>,
    pill: PillContext,
    spell: Option<&SpellContext<'_>>,
    lang: koru_i18n::Lang,
) -> Vec<menu::Row> {
    let full = full_rows(
        has_target,
        has_selection,
        mixed_attributes,
        task,
        has_reminder_target,
        table,
        diagram,
        pill,
        spell,
        lang,
    );
    kept(&full, pill, spell, table.is_some())
        .into_iter()
        .map(|index| full[index].clone())
        .collect()
}

/// Which rows of `full` this build shows, as indices into it: a row whose
/// choice is hidden goes, and so does any divider that would then lead, trail
/// or double another. The identity when nothing is hidden. `has_table` says
/// whether `full` carries the table group, which decides where the rows after
/// it sit ([`choice_at_edit`]).
fn kept(
    full: &[menu::Row],
    pill: PillContext,
    spell: Option<&SpellContext<'_>>,
    has_table: bool,
) -> Vec<usize> {
    let shown = |index: usize| {
        full_choice_at(index, pill, spell, has_table).is_none_or(Choice::shown_in_product)
    };
    if (0..full.len()).all(shown) {
        return (0..full.len()).collect();
    }
    let divider = |index: usize| matches!(full[index], menu::Row::Divider);
    let mut out: Vec<usize> = Vec::with_capacity(full.len());
    for index in (0..full.len()).filter(|index| shown(*index)) {
        if divider(index) && out.last().is_none_or(|last| divider(*last)) {
            continue;
        }
        out.push(index);
    }
    while out.last().is_some_and(|last| divider(*last)) {
        out.pop();
    }
    out
}

/// Every row the menu can have, before the product filter. The shape —
/// which rows exist and in what order — depends only on `pill`, `spell`,
/// whether there is a `table` and whether there is a drawn `diagram`; the
/// other arguments enable and annotate.
#[allow(clippy::too_many_arguments)]
fn full_rows(
    has_target: bool,
    has_selection: bool,
    mixed_attributes: bool,
    task: TaskContext,
    has_reminder_target: bool,
    table: Option<TableContext>,
    diagram: Option<DiagramContext>,
    pill: PillContext,
    spell: Option<&SpellContext<'_>>,
    lang: koru_i18n::Lang,
) -> Vec<menu::Row> {
    let mut rows = Vec::new();
    // The spelling block leads: the click was *on* the misspelled word, so
    // the fix is the nearest thing to the pointer. Frozen at open — the rows
    // come from the snapshot, never the live cache (D13).
    //
    // **Unless the pill owns the menu.** A chip's menu is about the value
    // under the pointer, and the pointer was not on a word: a chip is
    // presentation of node state hanging off the row, never text. The arms
    // below `return` their own rows, so a block added here would be discarded
    // and `choice_at` would still be counting it.
    let spell = spell.filter(|_| !pill.owns_the_menu());
    if let Some(spell) = spell {
        rows.extend(spelling_rows(spell, lang));
        if spell.only {
            // The corrections are the whole menu.
            return rows;
        }
    }
    match pill {
        PillContext::Reminder { paused, handled } => {
            let lang =
                koru_i18n::Lang::for_system(koru_window::system_locale::system_locale().as_deref());
            if handled {
                return vec![
                    menu::Row::item(koru_i18n::t(lang, koru_i18n::Key::ReminderEdit))
                        .with_icon(icons::PENCIL),
                    menu::Row::Divider,
                    menu::Row::item(koru_i18n::t(lang, koru_i18n::Key::ReminderDelete))
                        .with_icon(icons::TRASH),
                ];
            }
            return vec![
                menu::Row::item(koru_i18n::t(lang, koru_i18n::Key::ReminderEdit))
                    .with_icon(icons::PENCIL),
                menu::Row::item(koru_i18n::t(
                    lang,
                    if paused {
                        koru_i18n::Key::ReminderResume
                    } else {
                        koru_i18n::Key::ReminderPause
                    },
                ))
                .with_icon(icons::CLOCK),
                menu::Row::Divider,
                menu::Row::item(koru_i18n::t(lang, koru_i18n::Key::ReminderDelete))
                    .with_icon(icons::TRASH),
            ];
        }
        PillContext::Document => {
            rows.extend([
                menu::Row::item("Open")
                    .with_icon(icons::DOCUMENT)
                    .with_shortcut(click_hint()),
                menu::Row::item("Open in new tab")
                    .with_icon(icons::TABS)
                    .with_shortcut(primary_click_hint()),
                menu::Row::item("Open in new window")
                    .with_icon(icons::POP_OUT)
                    .with_shortcut(shift_click_hint()),
                menu::Row::Divider,
            ]);
        }
        // **A chip is not a text selection either**, and the same `return` is
        // the whole fix: nothing below this point — not the toolbar, not the
        // clipboard rows, not the four attribute rows that act on the whole
        // paragraph — is about the value under the pointer.
        PillContext::Attribute(chip) => {
            // **The other direction of the journey.** Following a task to its
            // document has been one gesture since the Tasks page shipped;
            // coming back was not offered at all, so a task you were reading
            // here could only be found again by remembering which queue it fell
            // in. Offered on every chip a task's row wears, because any of them
            // is a place you notice you want the agenda.
            let go_to_task = menu::Row::item("Go to task")
                .with_icon(icons::LIST_CHECKS)
                .with_shortcut(menu::Chord::Id(koru_keys::ShortcutId::ToggleExplore));
            // `CLEAR` rather than `TRASH`, for the reason *Remove from Tasks*
            // gives: nothing is deleted, one value on one row is emptied.
            let clear = menu::Row::item(chip.clear_label()).with_icon(icons::CLEAR);
            let edit = menu::Row::item(chip.edit_label()).with_icon(chip.icon());
            // **The due chip is the task's, and its menu is the task's menu.**
            // Its own click is *go to task* now, so that row leads and carries
            // the click hint; editing the date is the second row rather than
            // the gesture. The red row at the foot is the only one that takes
            // anything away, and it is a different kind of taking from *Clear
            // due date* above it — which empties one value — so it sits below a
            // rule rather than beside it.
            if chip == ChipMenu::Due {
                // **The verb the tick is, spelled as a sentence** — the same
                // row the Tasks page offers, and reading off the row's own
                // lifecycle so it says which direction it would go. A chip on
                // a row that is somehow not a task offers none: there is no
                // lifecycle to move.
                // **Always present, so the map below needs no offset.** A due
                // chip *is* the task marker (`is_task = marker OR due`), so a
                // row wearing one is a task and `NotATask` cannot arise here;
                // it reads as *Complete* if it ever did.
                let complete = if task == TaskContext::Completed {
                    menu::Row::item("Reopen").with_icon(icons::UNDO)
                } else {
                    menu::Row::item("Complete").with_icon(icons::CHECK)
                };
                return vec![
                    go_to_task.with_shortcut(click_hint()),
                    complete,
                    edit,
                    clear,
                    menu::Row::Divider,
                    // **Two ways to be rid of it, because they are two
                    // different things and only the reader knows which they
                    // mean.** A task is a marked *node* (ADR-285) — there is no
                    // second object — so "delete the task" is ambiguous in a
                    // way it would not be if a task were a record of its own:
                    // it can mean stop treating this paragraph as a task, or it
                    // can mean the paragraph goes. A row with children under it
                    // makes the difference expensive, so the menu asks instead
                    // of choosing.
                    menu::Row::item("Remove from Tasks").with_icon(icons::LIST_CHECKS),
                    menu::Row::item("Delete row")
                        .with_icon(icons::TRASH)
                        .destructive(),
                ];
            }
            // A tag, a colour and a priority are values on a row: their chip is
            // the control for that value, so its click still opens the picker
            // and the menu is a second door to it.
            return vec![
                edit.with_shortcut(click_hint()),
                go_to_task,
                menu::Row::Divider,
                clear.destructive(),
            ];
        }
        // An attachment pill is not a text selection: only file actions.
        PillContext::Attachment => {
            return vec![
                menu::Row::item("Open in app")
                    .with_icon(icons::FILE)
                    .with_shortcut(click_hint()),
                menu::Row::item("Reveal in File Explorer").with_icon(icons::FOLDER),
                menu::Row::item(koru_i18n::t(lang, koru_i18n::Key::MenuSaveAs))
                    .with_icon(icons::FILE_ARROW_UP),
                menu::Row::Divider,
                menu::Row::item("Remove").with_icon(icons::TRASH),
            ];
        }
        PillContext::AttachmentTrashed => {
            return vec![menu::Row::item("Restore from Trash").with_icon(icons::UNDO)];
        }
        PillContext::AttachmentMissing => {
            // Gone for good: only drop the pill. Open/Reveal/Save/Restore would
            // just report "not found".
            return vec![menu::Row::item("Remove").with_icon(icons::TRASH)];
        }
        PillContext::None => {}
    }
    rows.extend([
        menu::Row::item("Cut")
            .with_icon(icons::CUT)
            .with_shortcut(menu::Chord::Id(koru_keys::ShortcutId::Cut))
            .enabled(has_target),
        menu::Row::item(koru_i18n::t(lang, koru_i18n::Key::MenuCopy))
            .with_icon(icons::COPY)
            .with_shortcut(menu::Chord::Id(koru_keys::ShortcutId::Copy))
            .enabled(has_target),
        menu::Row::Divider,
        menu::Row::item("Paste")
            .with_icon(icons::PASTE)
            .with_shortcut(menu::Chord::Id(koru_keys::ShortcutId::Paste))
            .enabled(has_target),
        menu::Row::item("Paste as plain text")
            .with_icon(icons::PASTE_PLAIN)
            .with_shortcut(menu::Chord::Id(koru_keys::ShortcutId::PastePlain))
            .enabled(has_target),
        menu::Row::Divider,
        // `Literal` because `@` is a trigger *character*, not a chord: it is
        // what typing @ over marked rows does, and the registry models chords.
        menu::Row::item("Move to document…")
            .with_icon(icons::DOCUMENT)
            .with_shortcut(menu::Chord::Literal("@"))
            .enabled(has_selection),
        menu::Row::Divider,
    ]);
    // The attribute rows (roadmap A2): the same pickers `/tag`, `#tag` and
    // `/color` open, over the block selection when there is one. `#` is a
    // trigger character like `@` above, hence `Literal` — and it is dropped on
    // a mixed selection, because the note it would sit beside occupies the
    // same end of the row.
    let attrs = has_target && !mixed_attributes;
    let note = |row: menu::Row| {
        if mixed_attributes {
            row.with_trailing(menu::Trailing::Note(MIXED_NOTE))
        } else {
            row
        }
    };
    let tags = menu::Row::item("Tags…").with_icon(icons::TAG);
    let tags = if mixed_attributes {
        note(tags)
    } else {
        tags.with_shortcut(menu::Chord::Literal("#"))
    };
    // **Completing is the explicit action** (SPEC §5). A task with structural
    // children has a checkbox that is only a checkbox, so there has to be a
    // gesture that means *this whole thing is done*, and the menu is where it
    // lives — the design's row menu puts Complete in this group, above Due and
    // Priority.
    //
    // It carries **no shortcut hint**, deliberately. `ShortcutId::ToggleDone`
    // exists and is the obvious thing to print, but it is the *checkbox* chord:
    // on the rich task this row exists for, pressing it ticks the box and
    // completes nothing. A hint that is wrong on the one row that needs the row
    // is worse than no hint.
    let complete = if task == TaskContext::Completed {
        menu::Row::item("Reopen").with_icon(icons::SQUARE)
    } else {
        menu::Row::item("Complete").with_icon(icons::CHECK_SQUARE)
    };
    rows.extend([
        // Above *Complete* because it is a navigation and the rest of the group
        // is an edit — and dimmed on a plain row for [`TaskContext`]'s reason,
        // since the map in [`choice_at`] is by index.
        // **The same chord as *Reveal in Explore View***, because it is the
        // same gesture — *show me where this lives in the column* — and for a
        // task the column's answer is the Tasks rail's queue.
        menu::Row::item("View in Tasks")
            .with_icon(icons::LIST_CHECKS)
            .with_shortcut(menu::Chord::Id(koru_keys::ShortcutId::ToggleExplore))
            .enabled(has_target && task.is_task()),
        complete.enabled(has_target && task.is_task()),
        tags.enabled(attrs),
        note(menu::Row::item("Color…").with_icon(icons::PALETTE)).enabled(attrs),
        note(menu::Row::item("Due…").with_icon(icons::DATE)).enabled(attrs),
        note(menu::Row::item("Priority…").with_icon(icons::FLAG)).enabled(attrs),
        // **The reminder row closes the attribute group**, above the hairline:
        // it adds something to the row, the way the four pickers above it do.
        menu::Row::item(koru_i18n::t(
            koru_i18n::Lang::for_system(koru_window::system_locale::system_locale().as_deref()),
            koru_i18n::Key::ReminderAdd,
        ))
        .with_icon(icons::CLOCK)
        .enabled(has_reminder_target),
        // **Delegating is the reminder's sibling**: both hand this line to
        // something that will act on it later, which is why the row sits
        // beside it rather than up with Cut and Copy. The Agents identity
        // glyph, the one the slash command and the `+` menu wear, so four
        // gestures reaching one modal look like each other. No chord —
        // `Ctrl+Enter` belongs to the modal this row opens, not to the row.
        // Enabled whenever there is a row under the pointer: a delegation
        // needs an objective, not a selection, and the selection is only what
        // it starts with.
        menu::Row::item(koru_i18n::t(lang, koru_i18n::Key::DelegateMenuRow))
            .with_icon(icons::ROBOT)
            .enabled(has_target),
        // **The last row of the row menu, below a hairline, in red** — the
        // design's card, and the only place in this menu something is taken
        // away. `icons::CLEAR` rather than `icons::TRASH`, deliberately:
        // nothing is deleted here. The row, its text, its checkbox and its
        // priority all stay; what is cleared is the task state on it, which is
        // exactly what an eraser says and a bin does not.
        //
        // Dimmed on a plain row for [`TaskContext`]'s reason — the map in
        // [`choice_at`] is by index, so the row set is the same length whether
        // or not the row under the pointer is a task. The **table group is
        // appended after it**, because that group is its own headed section
        // about the cell rather than about the row; putting it before this row
        // would make every table index depend on a table being there.
        menu::Row::Divider,
        menu::Row::item("Remove from Tasks")
            .with_icon(icons::CLEAR)
            .destructive()
            .enabled(has_target && task.is_task()),
    ]);
    if let Some(table) = table {
        let (table_row, _) = table.cell;
        let structure_enabled = !table.source_visible;
        let view_item = menu::Row::item(if table.source_visible {
            "Show table"
        } else {
            "Edit Markdown"
        })
        .with_icon(if table.source_visible {
            icons::TABLE
        } else {
            icons::CODE
        });
        let view_item = if table.source_visible {
            view_item.with_shortcut(menu::Chord::Literal("Esc"))
        } else {
            view_item
        };
        rows.extend([
            menu::Row::Divider,
            menu::Row::Heading("TABLE"),
            view_item,
            menu::Row::item("Insert row above")
                .with_icon(icons::PLUS)
                .enabled(structure_enabled),
            menu::Row::item("Insert row below")
                .with_icon(icons::PLUS)
                .enabled(structure_enabled),
            menu::Row::item("Delete row")
                .with_icon(icons::TRASH)
                .enabled(structure_enabled && table_row > 0),
            menu::Row::Divider,
            menu::Row::item("Insert column before")
                .with_icon(icons::PLUS)
                .enabled(structure_enabled),
            menu::Row::item("Insert column after")
                .with_icon(icons::PLUS)
                .enabled(structure_enabled),
            menu::Row::item("Delete column")
                .with_icon(icons::TRASH)
                .enabled(structure_enabled),
        ]);
    }
    // **The diagram group follows the table group** for the same reason the
    // table group follows the Remove row: appended last, it shifts no index
    // anyone else relies on. A row is a table or a code block, never both, so
    // a real menu carries at most one of the two groups — `choice_at` is told
    // which. *Edit text* is always offered; *Save as…* only when there is a
    // drawing to save, so a block that does not draw never offers it. *Copy*
    // is the menu's ordinary Copy row above: on a selected drawing it takes
    // the whole block with its language, which is the only way a drawing is
    // copied, so the group adds no second one.
    if let Some(diagram) = diagram {
        rows.extend([
            menu::Row::Divider,
            menu::Row::item(koru_i18n::t(lang, koru_i18n::Key::DiagramEditText))
                .with_icon(icons::PENCIL),
        ]);
        if diagram.drawn {
            rows.push(
                menu::Row::item(koru_i18n::t(lang, koru_i18n::Key::MenuSaveAs))
                    .with_icon(icons::FILE_ARROW_UP)
                    .with_shortcut(menu::Chord::Id(koru_keys::ShortcutId::SaveDiagramAs)),
            );
        }
    }
    rows
}

/// What row `index` of the menu [`rows`] drew means.
///
/// Maps through [`kept`] over the menu's *shape*, built with a table group so
/// the map covers every row a menu with one has. A menu without a table is a
/// prefix of that list — the table group is appended after every row the
/// product can hide — so one map serves both. A menu on a `diagram` has the
/// diagram group where the table group would be (a row is never both), so
/// the shape is built the way that menu was drawn: no table, and the group
/// at its full length.
pub fn choice_at(
    index: usize,
    pill: PillContext,
    spell: Option<&SpellContext<'_>>,
    diagram: Option<DiagramContext>,
) -> Option<Choice> {
    let table = diagram.is_none().then_some(TableContext {
        cell: (1, 0),
        source_visible: false,
    });
    let shape = full_rows(
        true,
        true,
        false,
        TaskContext::Open,
        true,
        table,
        diagram.map(|diagram| DiagramContext {
            drawn: true,
            ..diagram
        }),
        pill,
        spell,
        koru_i18n::Lang::En,
    );
    let has_table = table.is_some();
    let full = *kept(&shape, pill, spell, has_table).get(index)?;
    full_choice_at(full, pill, spell, has_table)
}

/// [`choice_at`] over the full, unfiltered list.
fn full_choice_at(
    index: usize,
    pill: PillContext,
    spell: Option<&SpellContext<'_>>,
    has_table: bool,
) -> Option<Choice> {
    // The same gate `rows` applies, for the reason it applies it: a menu the
    // pill owns has no spelling block in it, so subtracting one here would map
    // every row to whatever sits that many places earlier.
    let spell = spell.filter(|_| !pill.owns_the_menu());
    let index = match spell {
        Some(spell) => {
            let prefix = spell.prefix_len();
            if index < prefix {
                if !spell.only {
                    // The drill-in row, then its divider.
                    return (index == 0).then_some(Choice::SpellingMenu);
                }
                let suggestions = spell.suggestions.map_or(0, <[String]>::len);
                // The four action rows close the block, whether the rows
                // above them hold suggestions or a placeholder.
                return if index < suggestions {
                    Some(Choice::Spelling(index))
                } else if index == prefix - 4 {
                    Some(Choice::SpellingIgnore)
                } else if index == prefix - 3 {
                    Some(Choice::SpellingIgnoreDocument)
                } else if index == prefix - 2 {
                    Some(Choice::SpellingAddWord)
                } else if index == prefix - 1 {
                    Some(Choice::SpellingAddWorkspaceWord)
                } else {
                    // The placeholder does nothing.
                    None
                };
            }
            index - prefix
        }
        None => index,
    };
    match pill {
        PillContext::Reminder {
            paused,
            handled: false,
        } => match index {
            0 => Some(Choice::ReminderEdit),
            1 if paused => Some(Choice::ReminderResume),
            1 => Some(Choice::ReminderPause),
            3 => Some(Choice::ReminderDelete),
            _ => None,
        },
        PillContext::Reminder { handled: true, .. } => match index {
            0 => Some(Choice::ReminderEdit),
            2 => Some(Choice::ReminderDelete),
            _ => None,
        },
        PillContext::Document => match index {
            0 => Some(Choice::Open),
            1 => Some(Choice::OpenInNewTab),
            2 => Some(Choice::OpenInNewWindow),
            i => choice_at_edit(i.checked_sub(DOCUMENT_PILL_PREFIX)?, has_table),
        },
        // The due chip's rows lead with *Go to task* and end with the red
        // *Delete task*; the other three keep the order they shipped with. Two
        // arms rather than one arithmetic expression, because the two menus are
        // different lists and an offset that happened to line up would be a
        // coincidence waiting to be broken by a fifth row.
        PillContext::Attribute(ChipMenu::Due) => match index {
            0 => Some(Choice::GoToTask),
            1 => Some(Choice::CompleteTask),
            2 => Some(Choice::EditChip),
            3 => Some(Choice::ClearChip),
            5 => Some(Choice::RemoveFromTasks),
            6 => Some(Choice::DeleteRow),
            _ => None,
        },
        PillContext::Attribute(_) => match index {
            0 => Some(Choice::EditChip),
            1 => Some(Choice::GoToTask),
            3 => Some(Choice::ClearChip),
            _ => None,
        },
        // Attachment menu is file-only (no edit rows after the divider).
        PillContext::Attachment => match index {
            0 => Some(Choice::OpenFile),
            1 => Some(Choice::RevealFile),
            2 => Some(Choice::SaveFileAs),
            4 => Some(Choice::RemoveAttachment),
            _ => None,
        },
        PillContext::AttachmentTrashed => match index {
            0 => Some(Choice::RestoreFile),
            _ => None,
        },
        PillContext::AttachmentMissing => match index {
            0 => Some(Choice::RemoveAttachment),
            _ => None,
        },
        PillContext::None => choice_at_edit(index, has_table),
    }
}

/// The first row of a block group — the table's or the diagram's — after
/// the eighteen rows every row menu has.
const BLOCK_GROUP_START: usize = 18;
/// The table group's length: its divider, heading, four row commands, a
/// divider and three column commands.
const TABLE_GROUP_LEN: usize = 10;

fn choice_at_edit(index: usize, has_table: bool) -> Option<Choice> {
    match index {
        0 => Some(Choice::Cut),
        1 => Some(Choice::Copy),
        3 => Some(Choice::Paste),
        4 => Some(Choice::PastePlain),
        6 => Some(Choice::MoveToDocument),
        8 => Some(Choice::GoToTask),
        9 => Some(Choice::CompleteTask),
        10 => Some(Choice::Tags),
        11 => Some(Choice::Color),
        12 => Some(Choice::Due),
        13 => Some(Choice::Priority),
        14 => Some(Choice::Remind),
        15 => Some(Choice::Delegate),
        17 => Some(Choice::RemoveFromTasks),
        20 if has_table => Some(Choice::Table(koru_editor::table::Edit::ToggleView)),
        21 if has_table => Some(Choice::Table(koru_editor::table::Edit::InsertRowAbove)),
        22 if has_table => Some(Choice::Table(koru_editor::table::Edit::InsertRowBelow)),
        23 if has_table => Some(Choice::Table(koru_editor::table::Edit::DeleteRow)),
        25 if has_table => Some(Choice::Table(koru_editor::table::Edit::InsertColumnBefore)),
        26 if has_table => Some(Choice::Table(koru_editor::table::Edit::InsertColumnAfter)),
        27 if has_table => Some(Choice::Table(koru_editor::table::Edit::DeleteColumn)),
        // The diagram group: after the table group when the shape has one,
        // in its place when it does not. Its divider is the first index.
        _ => {
            let start = if has_table {
                BLOCK_GROUP_START + TABLE_GROUP_LEN
            } else {
                BLOCK_GROUP_START
            };
            match index.checked_sub(start)? {
                1 => Some(Choice::DiagramEditText),
                2 => Some(Choice::SaveDiagramAs),
                _ => None,
            }
        }
    }
}

/// Place the formatting toolbar relative to the context menu's *resolved*
/// panel. `menu::Menu::panel` already owns flip/clamp behavior; reading that
/// answer here prevents a second implementation from disagreeing near the
/// bottom edge.
pub fn toolbar_panel(
    menu: &menu::Menu,
    rows: &[menu::Row],
    viewport: (f32, f32),
    chrome_h: f32,
) -> Rect {
    let context = menu.panel(rows, viewport);
    let w = bubble::width();
    let h = bubble::height();
    let x = context.x;
    let preferred_y = if context.y < menu.anchor.y {
        // The context menu flipped above the click: stack the toolbar above it.
        context.y - h - GAP
    } else {
        // Ordinary placement: toolbar above the click, context menu below.
        menu.anchor.y - h - GAP
    };
    let top = chrome_h + EDGE_MARGIN;
    let floor = (viewport.1 - h - EDGE_MARGIN).max(top);
    Rect::new(x, preferred_y.clamp(top, floor), w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pre-spelling signatures, shadowing the module's. Every test below
    /// this pair exercises a menu with **no** spelling block, and their
    /// hard-coded indices are exactly what these two lines keep meaning.
    fn rows(
        has_target: bool,
        has_selection: bool,
        mixed_attributes: bool,
        task: TaskContext,
        has_reminder_target: bool,
        table: Option<TableContext>,
        pill: PillContext,
    ) -> Vec<menu::Row> {
        super::rows(
            has_target,
            has_selection,
            mixed_attributes,
            task,
            has_reminder_target,
            table,
            None,
            pill,
            None,
            koru_i18n::Lang::En,
        )
    }

    fn choice_at(index: usize, pill: PillContext) -> Option<Choice> {
        super::choice_at(index, pill, None, None)
    }

    fn diagram(drawn: bool) -> Option<DiagramContext> {
        Some(DiagramContext {
            node: koru_core::NodeId::new(),
            drawn,
        })
    }

    fn label_of(row: &menu::Row) -> Option<&str> {
        match row {
            menu::Row::Item { label, .. } => Some(label.as_ref()),
            _ => None,
        }
    }

    /// The menu drawn on a diagram: the ordinary rows, then its group.
    fn diagram_rows(drawn: bool) -> Vec<menu::Row> {
        super::rows(
            true,
            true,
            false,
            TaskContext::Open,
            true,
            None,
            diagram(drawn),
            PillContext::None,
            None,
            koru_i18n::Lang::En,
        )
    }

    /// **The diagram group sits where the table group would**, so every
    /// index before it keeps its meaning, and the map resolves its rows by
    /// the context the menu was drawn with. *Save as…* is the last row and
    /// comes only with a drawing, so a block that does not draw is a prefix.
    #[test]
    fn the_diagram_group_follows_the_ordinary_rows_and_resolves() {
        let drawn = diagram_rows(true);
        let plain = rows(
            true,
            true,
            false,
            TaskContext::Open,
            true,
            None,
            PillContext::None,
        );
        assert_eq!(&drawn[..plain.len()], &plain[..], "the ordinary rows first");
        assert_eq!(plain.len(), 18);
        assert!(matches!(drawn[18], menu::Row::Divider));
        let label = |row: &menu::Row| match row {
            menu::Row::Item { label, .. } => label.to_string(),
            other => panic!("an item, not {other:?}"),
        };
        assert_eq!(label(&drawn[19]), "Edit text");
        assert_eq!(label(&drawn[20]), "Save as…");
        assert_eq!(drawn.len(), 21);
        // One Copy row, the menu's own at index 1: the group adds no second.
        assert_eq!(
            drawn
                .iter()
                .filter(|row| label_of(row) == Some("Copy"))
                .count(),
            1
        );
        let ctx = diagram(true);
        assert_eq!(
            super::choice_at(19, PillContext::None, None, ctx),
            Some(Choice::DiagramEditText)
        );
        assert_eq!(
            super::choice_at(20, PillContext::None, None, ctx),
            Some(Choice::SaveDiagramAs)
        );
        assert_eq!(super::choice_at(18, PillContext::None, None, ctx), None);
        assert_eq!(super::choice_at(21, PillContext::None, None, ctx), None);
        // Every ordinary index still means what it meant.
        for index in 0..18 {
            assert_eq!(
                super::choice_at(index, PillContext::None, None, ctx),
                choice_at(index, PillContext::None),
                "row {index}"
            );
        }

        // Not drawn: no *Save as…*, nothing else moves.
        let undrawn = diagram_rows(false);
        assert_eq!(undrawn.len(), 20);
        assert_eq!(&undrawn[..], &drawn[..20]);
        assert_eq!(
            super::choice_at(19, PillContext::None, None, diagram(false)),
            Some(Choice::DiagramEditText)
        );
    }

    /// With both groups in the full shape — which no real menu has — the
    /// diagram group follows the table group, so `choice_at` for a table menu
    /// is untouched by its existence.
    #[test]
    fn the_table_menu_does_not_see_the_diagram_group() {
        let with_both = super::rows(
            true,
            true,
            false,
            TaskContext::Open,
            true,
            Some(TableContext {
                cell: (1, 0),
                source_visible: false,
            }),
            diagram(true),
            PillContext::None,
            None,
            koru_i18n::Lang::En,
        );
        assert_eq!(with_both.len(), 31);
        assert!(matches!(with_both[28], menu::Row::Divider));
        assert_eq!(
            choice_at(27, PillContext::None),
            Some(Choice::Table(koru_editor::table::Edit::DeleteColumn))
        );
        assert_eq!(choice_at(28, PillContext::None), None);
        assert_eq!(
            choice_at(29, PillContext::None),
            None,
            "no diagram on a table menu"
        );
        assert_eq!(
            super::full_choice_at(29, PillContext::None, None, true),
            Some(Choice::DiagramEditText),
            "but the full map knows where it would be"
        );
    }

    /// **A menu the pill owns is drawn and mapped from the same list**, and a
    /// spelling issue on the row cannot come between them.
    ///
    /// `rows` returns a chip's rows and nothing else, so a spelling block added
    /// ahead of them would be discarded — while `choice_at` went on
    /// subtracting its length. That is the arithmetic that made *Go to task*
    /// press nothing: the second row mapped to whatever sat two places earlier
    /// in a list that was never built.
    #[test]
    fn a_pill_that_owns_its_menu_is_mapped_from_the_rows_it_draws() {
        let suggestions = ["definitely".to_owned()];
        let spell = SpellContext {
            suggestions: Some(&suggestions),
            only: false,
        };
        let pill = PillContext::Attribute(ChipMenu::Due);

        let with_spell = super::rows(
            true,
            false,
            false,
            TaskContext::NotATask,
            true,
            None,
            None,
            pill,
            Some(&spell),
            koru_i18n::Lang::En,
        );
        let without = super::rows(
            true,
            false,
            false,
            TaskContext::NotATask,
            true,
            None,
            None,
            pill,
            None,
            koru_i18n::Lang::En,
        );
        assert_eq!(
            with_spell.len(),
            without.len(),
            "a spelling issue on the row adds nothing to a chip's menu"
        );
        assert_eq!(
            with_spell.len(),
            7,
            "go to task, complete, edit, clear, a divider, and the two ways to \
             be rid of it"
        );

        // And the map agrees with the list, spelling snapshot or not.
        for spell in [Some(&spell), None] {
            assert_eq!(
                super::choice_at(0, pill, spell, None),
                Some(Choice::GoToTask),
                "row 0 — and once the one that pressed nothing"
            );
            assert_eq!(
                super::choice_at(1, pill, spell, None),
                Some(Choice::CompleteTask),
                "row 1 — the verb the tick is"
            );
            assert_eq!(
                super::choice_at(2, pill, spell, None),
                Some(Choice::EditChip),
                "row 2"
            );
            assert_eq!(
                super::choice_at(3, pill, spell, None),
                Some(Choice::ClearChip),
                "row 3"
            );
            assert_eq!(super::choice_at(4, pill, spell, None), None, "the rule");
            assert_eq!(
                super::choice_at(5, pill, spell, None),
                Some(Choice::RemoveFromTasks),
                "row 5 — the marker goes, the writing stays"
            );
            assert_eq!(
                super::choice_at(6, pill, spell, None),
                Some(Choice::DeleteRow),
                "row 6 — the red one, and the writing goes"
            );
        }
    }

    /// **Only the due chip's menu was reordered.** A tag, a colour and a
    /// priority are values on a row, their chip is the control for that value,
    /// and their click still opens the picker — so their menu still leads with
    /// *Edit* and ends at *Clear*, with no red row: there is no task to delete.
    #[test]
    fn the_other_three_chips_keep_the_order_they_shipped_with() {
        for chip in [ChipMenu::Tag, ChipMenu::Color, ChipMenu::Priority] {
            let pill = PillContext::Attribute(chip);
            let drawn = rows(true, false, false, TaskContext::NotATask, true, None, pill);
            assert_eq!(drawn.len(), 4, "{chip:?}: edit, go to task, rule, clear");
            assert_eq!(choice_at(0, pill), Some(Choice::EditChip), "{chip:?}");
            assert_eq!(choice_at(1, pill), Some(Choice::GoToTask), "{chip:?}");
            assert_eq!(choice_at(3, pill), Some(Choice::ClearChip), "{chip:?}");
            assert_eq!(
                choice_at(4, pill),
                None,
                "{chip:?}: no Delete task on a chip that is not the task marker"
            );
        }
    }

    /// The predicate itself: a chip and an attachment own their menus, a
    /// document pill leads the ordinary one, and `None` *is* the ordinary one.
    #[test]
    fn only_the_pills_that_replace_the_menu_own_it() {
        assert!(PillContext::Attribute(ChipMenu::Tag).owns_the_menu());
        assert!(PillContext::Attachment.owns_the_menu());
        assert!(PillContext::AttachmentTrashed.owns_the_menu());
        assert!(PillContext::AttachmentMissing.owns_the_menu());
        assert!(!PillContext::Document.owns_the_menu());
        assert!(!PillContext::None.owns_the_menu());
    }

    /// With a selection the full menu keeps its length sane: one *Spelling…*
    /// drill-in row and a divider lead it, and everything behind shifts by
    /// exactly that prefix — the same contract the pill prefixes keep.
    #[test]
    fn a_selection_menu_leads_with_one_spelling_drill_in_row() {
        let suggestions = vec!["the".to_string(), "then".to_string()];
        let spell = SpellContext {
            suggestions: Some(&suggestions),
            only: false,
        };
        let with = super::rows(
            true,
            false,
            false,
            TaskContext::NotATask,
            true,
            None,
            None,
            PillContext::None,
            Some(&spell),
            koru_i18n::Lang::En,
        );
        let plain = rows(
            true,
            false,
            false,
            TaskContext::NotATask,
            true,
            None,
            PillContext::None,
        );
        // One drill-in row + divider, then the menu — never the suggestions
        // inline, however many there are.
        assert_eq!(with.len(), plain.len() + 2);
        assert!(
            matches!(&with[0], menu::Row::Item { label, enabled: true, .. } if label.as_ref() == "Spelling…")
        );
        assert!(matches!(with[1], menu::Row::Divider));

        assert_eq!(
            super::choice_at(0, PillContext::None, Some(&spell), None),
            Some(Choice::SpellingMenu)
        );
        assert_eq!(
            super::choice_at(1, PillContext::None, Some(&spell), None),
            None
        );
        // Cut sits at 0 in the plain menu; behind the drill-in it is 0 + 2.
        assert_eq!(
            super::choice_at(2, PillContext::None, Some(&spell), None),
            Some(Choice::Cut)
        );
    }

    /// A right-click on the word with nothing selected: the corrections *are*
    /// the menu — no divider trailing them, no edit rows behind them, and the
    /// prefix arithmetic unchanged.
    #[test]
    fn without_a_selection_the_menu_is_the_corrections_alone() {
        let suggestions = vec!["the".to_string()];
        let spell = SpellContext {
            suggestions: Some(&suggestions),
            only: true,
        };
        let with = super::rows(
            true,
            false,
            false,
            TaskContext::NotATask,
            true,
            None,
            None,
            PillContext::None,
            Some(&spell),
            koru_i18n::Lang::En,
        );
        // One suggestion + the four word actions, and nothing else.
        assert_eq!(with.len(), 5);
        assert!(matches!(&with[0], menu::Row::Item { label, .. } if label.as_ref() == "the"));
        assert!(
            matches!(&with[4], menu::Row::Item { label, .. } if label.as_ref() == "Add to workspace dictionary")
        );
        assert!(!with.iter().any(|row| matches!(row, menu::Row::Divider)));
        assert_eq!(
            super::choice_at(0, PillContext::None, Some(&spell), None),
            Some(Choice::Spelling(0))
        );
        assert_eq!(
            super::choice_at(1, PillContext::None, Some(&spell), None),
            Some(Choice::SpellingIgnore)
        );
        assert_eq!(
            super::choice_at(4, PillContext::None, Some(&spell), None),
            Some(Choice::SpellingAddWorkspaceWord)
        );
    }

    /// Near the top of the viewport the toolbar cannot sit above the click, so
    /// the anchor is floored far enough down that the clamped toolbar never
    /// covers the menu's first row — the row a click could otherwise not pick.
    #[test]
    fn near_the_ceiling_the_toolbar_never_covers_the_first_row() {
        let chrome = 32.0;
        let menu = open((400.0, chrome + 2.0), 900.0, chrome, 280.0, true);
        let rows = rows(
            true,
            true,
            false,
            TaskContext::NotATask,
            true,
            None,
            PillContext::None,
        );
        let context = menu.panel(&rows, (900.0, 700.0));
        let toolbar = toolbar_panel(&menu, &rows, (900.0, 700.0), chrome);
        assert!(
            toolbar.y + toolbar.h <= context.y,
            "toolbar {toolbar:?} must end above the panel {context:?}"
        );
    }

    /// Cold cache in the corrections menu: one dimmed placeholder, Ignore
    /// still live, indices still honest. An answered-but-empty lookup reads
    /// differently but has the same shape.
    #[test]
    fn a_cold_cache_shows_a_placeholder_and_keeps_indices_honest() {
        for (suggestions, label) in [
            (None, "No suggestions yet"),
            (Some(&[][..]), "No suggestions"),
        ] {
            let spell = SpellContext {
                suggestions,
                only: true,
            };
            let with = super::rows(
                true,
                false,
                false,
                TaskContext::NotATask,
                true,
                None,
                None,
                PillContext::None,
                Some(&spell),
                koru_i18n::Lang::En,
            );
            assert_eq!(with.len(), 5);
            assert!(
                matches!(&with[0], menu::Row::Item { label: l, enabled: false, .. } if l.as_ref() == label)
            );
            assert!(
                matches!(&with[1], menu::Row::Item { label, enabled: true, .. } if label.as_ref() == "Ignore")
            );
            assert_eq!(
                super::choice_at(0, PillContext::None, Some(&spell), None),
                None
            );
            assert_eq!(
                super::choice_at(1, PillContext::None, Some(&spell), None),
                Some(Choice::SpellingIgnore)
            );
            assert_eq!(
                super::choice_at(2, PillContext::None, Some(&spell), None),
                Some(Choice::SpellingIgnoreDocument)
            );
            assert_eq!(
                super::choice_at(3, PillContext::None, Some(&spell), None),
                Some(Choice::SpellingAddWord)
            );
            assert_eq!(
                super::choice_at(4, PillContext::None, Some(&spell), None),
                Some(Choice::SpellingAddWorkspaceWord)
            );
        }
    }

    #[test]
    fn a_selection_enables_move_to_document_and_maps_to_its_choice() {
        let with = rows(
            true,
            true,
            false,
            TaskContext::NotATask,
            true,
            None,
            PillContext::None,
        );
        assert!(matches!(
            &with[6],
            menu::Row::Item {
                label,
                shortcut: Some(menu::Chord::Literal("@")),
                enabled: true,
                ..
            } if label.as_ref() == "Move to document…"
        ));
        assert_eq!(
            choice_at(6, PillContext::None),
            Some(Choice::MoveToDocument)
        );

        let without = rows(
            true,
            false,
            false,
            TaskContext::NotATask,
            true,
            None,
            PillContext::None,
        );
        assert!(matches!(without[6], menu::Row::Item { enabled: false, .. }));
    }

    /// The two attribute rows sit beside Move to document…, map to their
    /// choices, and dim without a caret to act on.
    #[test]
    fn the_attribute_rows_map_to_their_choices() {
        let with = rows(
            true,
            false,
            false,
            TaskContext::NotATask,
            true,
            None,
            PillContext::None,
        );
        assert!(matches!(
            &with[10],
            menu::Row::Item {
                label,
                shortcut: Some(menu::Chord::Literal("#")),
                enabled: true,
                ..
            } if label.as_ref() == "Tags…"
        ));
        assert!(matches!(
            &with[11],
            menu::Row::Item {
                label,
                enabled: true,
                ..
            } if label.as_ref() == "Color…"
        ));
        assert!(matches!(
            &with[12],
            menu::Row::Item {
                label,
                enabled: true,
                ..
            } if label.as_ref() == "Due…"
        ));
        assert!(matches!(
            &with[13],
            menu::Row::Item {
                label,
                enabled: true,
                ..
            } if label.as_ref() == "Priority…"
        ));
        assert_eq!(choice_at(10, PillContext::None), Some(Choice::Tags));
        assert_eq!(choice_at(11, PillContext::None), Some(Choice::Color));
        assert_eq!(choice_at(12, PillContext::None), Some(Choice::Due));
        assert_eq!(choice_at(13, PillContext::None), Some(Choice::Priority));
        // Behind a document pill's three open rows they shift with the rest.
        assert_eq!(choice_at(14, PillContext::Document), Some(Choice::Tags));
        assert_eq!(choice_at(16, PillContext::Document), Some(Choice::Due));
        assert_eq!(choice_at(17, PillContext::Document), Some(Choice::Priority));

        let without = rows(
            false,
            false,
            false,
            TaskContext::NotATask,
            true,
            None,
            PillContext::None,
        );
        for i in [10, 11, 12, 13] {
            assert!(matches!(without[i], menu::Row::Item { enabled: false, .. }));
        }
    }

    #[test]
    fn add_reminder_requires_an_exact_object_and_keeps_existing_choices_stable() {
        let label = koru_i18n::t(
            koru_i18n::Lang::for_system(koru_window::system_locale::system_locale().as_deref()),
            koru_i18n::Key::ReminderAdd,
        );
        let with = rows(
            true,
            false,
            false,
            TaskContext::NotATask,
            true,
            None,
            PillContext::None,
        );
        assert!(matches!(
            &with[14],
            menu::Row::Item {
                label: row_label,
                icon: Some(icons::CLOCK),
                enabled: true,
                ..
            } if row_label == label
        ));
        assert_eq!(choice_at(12, PillContext::None), Some(Choice::Due));
        assert_eq!(choice_at(13, PillContext::None), Some(Choice::Priority));
        assert_eq!(choice_at(14, PillContext::None), Some(Choice::Remind));

        // A preserved caret is not permission to attach a reminder when the
        // right-click itself landed outside every object. Existing clipboard
        // choices remain available from that caret.
        let without = rows(
            true,
            false,
            false,
            TaskContext::NotATask,
            false,
            None,
            PillContext::None,
        );
        assert!(matches!(without[0], menu::Row::Item { enabled: true, .. }));
        assert!(matches!(
            without[14],
            menu::Row::Item { enabled: false, .. }
        ));

        // A document-pill prefix shifts every edit action together.
        assert_eq!(choice_at(16, PillContext::Document), Some(Choice::Due));
        assert_eq!(choice_at(17, PillContext::Document), Some(Choice::Priority));
        assert_eq!(choice_at(18, PillContext::Document), Some(Choice::Remind));
    }

    #[test]
    fn an_exact_reminder_has_only_its_phase_one_actions() {
        let active = rows(
            true,
            true,
            false,
            TaskContext::NotATask,
            true,
            None,
            PillContext::Reminder {
                paused: false,
                handled: false,
            },
        );
        assert_eq!(active.len(), 4);
        assert!(matches!(active[2], menu::Row::Divider));
        assert_eq!(
            choice_at(
                0,
                PillContext::Reminder {
                    paused: false,
                    handled: false,
                }
            ),
            Some(Choice::ReminderEdit)
        );
        assert_eq!(
            choice_at(
                1,
                PillContext::Reminder {
                    paused: false,
                    handled: false,
                }
            ),
            Some(Choice::ReminderPause)
        );
        assert_eq!(
            choice_at(
                1,
                PillContext::Reminder {
                    paused: true,
                    handled: false,
                }
            ),
            Some(Choice::ReminderResume)
        );
        assert_eq!(
            choice_at(
                3,
                PillContext::Reminder {
                    paused: false,
                    handled: false,
                }
            ),
            Some(Choice::ReminderDelete)
        );
        assert_eq!(
            choice_at(
                2,
                PillContext::Reminder {
                    paused: false,
                    handled: false,
                }
            ),
            None
        );

        let handled = rows(
            true,
            true,
            false,
            TaskContext::NotATask,
            true,
            None,
            PillContext::Reminder {
                paused: false,
                handled: true,
            },
        );
        assert_eq!(handled.len(), 3);
        assert_eq!(
            choice_at(
                0,
                PillContext::Reminder {
                    paused: false,
                    handled: true,
                }
            ),
            Some(Choice::ReminderEdit)
        );
        assert_eq!(
            choice_at(
                1,
                PillContext::Reminder {
                    paused: false,
                    handled: true,
                }
            ),
            None
        );
        assert_eq!(
            choice_at(
                2,
                PillContext::Reminder {
                    paused: false,
                    handled: true,
                }
            ),
            Some(Choice::ReminderDelete)
        );
    }

    /// **A selection spanning the name and the body dims all four**, and each
    /// says why. It is dimmed rather than hidden because a row that vanishes
    /// teaches nothing; and the row *set* is the same length either way, so
    /// `choice_at`'s hard-coded indices still land where they did (ADR-187).
    #[test]
    fn a_mixed_title_and_body_selection_dims_the_attribute_rows() {
        let plain = rows(
            true,
            true,
            false,
            TaskContext::NotATask,
            true,
            None,
            PillContext::None,
        );
        let mixed = rows(
            true,
            true,
            true,
            TaskContext::NotATask,
            true,
            None,
            PillContext::None,
        );
        assert_eq!(plain.len(), mixed.len(), "dimming shifts no index");

        for i in [10, 11, 12, 13] {
            assert!(
                matches!(
                    mixed[i],
                    menu::Row::Item {
                        enabled: false,
                        trailing: menu::Trailing::Note(MIXED_NOTE),
                        ..
                    }
                ),
                "row {i} dims and explains itself: {:?}",
                mixed[i]
            );
        }
        // The `#` hint goes with it: the note occupies that end of the row.
        assert!(matches!(
            &mixed[10],
            menu::Row::Item {
                label,
                shortcut: None,
                ..
            } if label.as_ref() == "Tags…"
        ));
        assert_eq!(choice_at(10, PillContext::None), Some(Choice::Tags));
        assert_eq!(choice_at(13, PillContext::None), Some(Choice::Priority));

        // Nothing else in the menu is touched by it.
        for i in [0, 1, 3, 4, 6] {
            assert_eq!(
                std::mem::discriminant(&plain[i]),
                std::mem::discriminant(&mixed[i])
            );
            assert!(matches!(mixed[i], menu::Row::Item { enabled: true, .. }));
        }
    }

    /// **The design's card: a red last row, under a hairline, dimmed off a
    /// task.** All three facts in one test, because the row is only honest
    /// with all three: red says it takes something away, the hairline says it
    /// is not one of the attribute rows above it, and dimming rather than
    /// vanishing is what keeps [`choice_at`]'s map by index true.
    #[test]
    fn remove_from_tasks_is_the_red_last_row_below_a_hairline() {
        const DIVIDER: usize = 16;
        const REMOVE: usize = 17;

        let on_task = rows(
            true,
            false,
            false,
            TaskContext::Open,
            true,
            None,
            PillContext::None,
        );
        assert!(matches!(on_task[DIVIDER], menu::Row::Divider));
        assert!(
            matches!(
                &on_task[REMOVE],
                menu::Row::Item {
                    label,
                    icon: Some(icons::CLEAR),
                    enabled: true,
                    destructive: true,
                    ..
                } if label.as_ref() == "Remove from Tasks"
            ),
            "the row: {:?}",
            on_task[REMOVE]
        );
        assert_eq!(on_task.len(), REMOVE + 1, "and nothing follows it");
        assert_eq!(
            choice_at(REMOVE, PillContext::None),
            Some(Choice::RemoveFromTasks)
        );
        // Behind a document pill's three open rows it shifts with the rest.
        assert_eq!(
            choice_at(REMOVE + DOCUMENT_PILL_PREFIX, PillContext::Document),
            Some(Choice::RemoveFromTasks)
        );

        // A completed task can still be taken out of Tasks.
        let completed = rows(
            true,
            false,
            false,
            TaskContext::Completed,
            true,
            None,
            PillContext::None,
        );
        assert!(matches!(
            completed[REMOVE],
            menu::Row::Item { enabled: true, .. }
        ));

        // A plain row dims it, and shifts nothing.
        let plain = rows(
            true,
            false,
            false,
            TaskContext::NotATask,
            true,
            None,
            PillContext::None,
        );
        assert_eq!(plain.len(), on_task.len(), "dimming shifts no index");
        assert!(
            matches!(
                plain[REMOVE],
                menu::Row::Item {
                    enabled: false,
                    destructive: true,
                    ..
                }
            ),
            "a plain row dims it: {:?}",
            plain[REMOVE]
        );
        assert_eq!(
            choice_at(REMOVE, PillContext::None),
            Some(Choice::RemoveFromTasks),
            "and the index still means the same thing"
        );
    }

    /// The table group is appended **after** the Remove row, so a cell's
    /// commands sit at fixed indices whether or not the row is a task.
    #[test]
    fn the_table_group_follows_the_remove_row() {
        let with_table = |task| {
            rows(
                true,
                true,
                false,
                task,
                true,
                Some(TableContext {
                    cell: (1, 0),
                    source_visible: false,
                }),
                PillContext::None,
            )
        };
        let off = with_table(TaskContext::NotATask);
        let on = with_table(TaskContext::Open);
        assert_eq!(off.len(), on.len());
        assert!(matches!(off[17], menu::Row::Item { .. }));
        assert!(matches!(off[18], menu::Row::Divider));
        assert!(matches!(off[19], menu::Row::Heading("TABLE")));
        assert_eq!(
            choice_at(20, PillContext::None),
            Some(Choice::Table(koru_editor::table::Edit::ToggleView))
        );
        assert_eq!(
            choice_at(27, PillContext::None),
            Some(Choice::Table(koru_editor::table::Edit::DeleteColumn))
        );
    }

    #[test]
    fn clipboard_commands_use_their_phosphor_icons() {
        let rows = rows(
            true,
            true,
            false,
            TaskContext::NotATask,
            true,
            None,
            PillContext::None,
        );
        let icon_at = |i| match rows[i] {
            menu::Row::Item { icon, .. } => icon,
            _ => None,
        };

        assert_eq!(icon_at(0), Some(icons::CUT));
        assert_eq!(icon_at(1), Some(icons::COPY));
        assert_eq!(icon_at(3), Some(icons::PASTE));
        assert_eq!(icon_at(4), Some(icons::PASTE_PLAIN));
    }

    #[test]
    fn ordinary_context_puts_toolbar_above_click_and_menu_below() {
        // **Higher up the page than it used to be**, because the menu grew a
        // row: *Delegate…* joined the group the reminder closes, and a
        // seven-hundred-pixel window no longer has room below a click at 300
        // for the whole of it. What the test is about — toolbar above the
        // click, menu below, both left-aligned — is unchanged.
        let menu = open((400.0, 180.0), 900.0, 0.0, 280.0, true);
        let rows = rows(
            true,
            true,
            false,
            TaskContext::NotATask,
            true,
            None,
            PillContext::None,
        );
        let context = menu.panel(&rows, (900.0, 700.0));
        let toolbar = toolbar_panel(&menu, &rows, (900.0, 700.0), 0.0);

        assert!(toolbar.y + toolbar.h <= menu.anchor.y);
        assert!(context.y >= menu.anchor.y);
        assert_eq!(toolbar.x, context.x);
    }

    #[test]
    fn near_the_floor_toolbar_stacks_above_the_flipped_menu() {
        let menu = open((400.0, 680.0), 900.0, 0.0, 280.0, true);
        let rows = rows(
            true,
            true,
            false,
            TaskContext::NotATask,
            true,
            None,
            PillContext::None,
        );
        let context = menu.panel(&rows, (900.0, 700.0));
        let toolbar = toolbar_panel(&menu, &rows, (900.0, 700.0), 0.0);

        assert!(context.y < menu.anchor.y, "the context menu flipped above");
        assert!(toolbar.y + toolbar.h <= context.y);
        assert_eq!(toolbar.x, context.x);
    }

    #[test]
    fn a_short_attachment_menu_is_narrower_than_the_edit_menu() {
        let mut engine = koru_draw::text::TextEngine::new();
        let edit = rows(
            true,
            true,
            false,
            TaskContext::NotATask,
            true,
            None,
            PillContext::None,
        );
        let short = rows(
            true,
            false,
            false,
            TaskContext::NotATask,
            true,
            None,
            PillContext::AttachmentTrashed,
        );
        let edit_w = menu::measure_width(&mut engine, &edit);
        let short_w = menu::measure_width(&mut engine, &short);
        assert!(
            short_w < edit_w,
            "restore-only {short_w} should be narrower than edit {edit_w}"
        );
        assert!(short_w >= menu::MIN_W);
        assert!(edit_w <= menu::MAX_W);
    }

    #[test]
    fn attachment_menu_anchors_at_the_click_not_the_toolbar() {
        let at = (400.0, 300.0);
        let menu = open(at, 900.0, 0.0, 200.0, false);
        assert_eq!(menu.anchor.x, at.0);
        assert_eq!(menu.anchor.w, 0.0);
        assert_eq!(menu.width, 200.0);
    }

    #[test]
    fn a_table_cell_adds_source_and_structure_commands() {
        let body_rows = rows(
            true,
            true,
            false,
            TaskContext::NotATask,
            true,
            Some(TableContext {
                cell: (1, 0),
                source_visible: false,
            }),
            PillContext::None,
        );
        assert!(body_rows.iter().any(|row| matches!(
            row,
            menu::Row::Item { label, .. } if label.as_ref() == "Edit Markdown"
        )));
        assert_eq!(
            choice_at(25, PillContext::None),
            Some(Choice::Table(koru_editor::table::Edit::InsertColumnBefore))
        );

        let header = rows(
            true,
            true,
            false,
            TaskContext::NotATask,
            true,
            Some(TableContext {
                cell: (0, 0),
                source_visible: false,
            }),
            PillContext::None,
        );
        assert!(matches!(header[23], menu::Row::Item { enabled: false, .. }));
    }

    #[test]
    fn source_mode_offers_show_table_and_disables_structure() {
        let rows = rows(
            true,
            true,
            false,
            TaskContext::NotATask,
            true,
            Some(TableContext {
                cell: (1, 0),
                source_visible: true,
            }),
            PillContext::None,
        );
        assert!(matches!(
            &rows[20],
            menu::Row::Item {
                label,
                shortcut: Some(menu::Chord::Literal("Esc")),
                enabled: true,
                ..
            } if label.as_ref() == "Show table"
        ));
        for index in [21, 22, 23, 25, 26, 27] {
            assert!(matches!(
                rows[index],
                menu::Row::Item { enabled: false, .. }
            ));
        }
    }

    #[test]
    fn a_document_pill_prepends_the_three_open_destinations() {
        let rows = rows(
            true,
            false,
            false,
            TaskContext::NotATask,
            true,
            None,
            PillContext::Document,
        );
        assert!(matches!(
            &rows[0],
            menu::Row::Item {
                label,
                shortcut: Some(menu::Chord::Literal("Click")),
                enabled: true,
                ..
            } if label.as_ref() == "Open"
        ));
        assert!(matches!(
            &rows[1],
            menu::Row::Item {
                label,
                enabled: true,
                ..
            } if label.as_ref() == "Open in new tab"
        ));
        assert!(matches!(
            &rows[2],
            menu::Row::Item {
                label,
                enabled: true,
                ..
            } if label.as_ref() == "Open in new window"
        ));
        assert_eq!(choice_at(0, PillContext::Document), Some(Choice::Open));
        assert_eq!(
            choice_at(1, PillContext::Document),
            Some(Choice::OpenInNewTab)
        );
        assert_eq!(
            choice_at(2, PillContext::Document),
            Some(Choice::OpenInNewWindow)
        );
        assert_eq!(
            choice_at(10, PillContext::Document),
            Some(Choice::MoveToDocument)
        );
        assert!(matches!(rows[10], menu::Row::Item { enabled: false, .. }));
    }

    #[test]
    fn an_attachment_pill_prepends_open_reveal_save_and_remove() {
        let rows = rows(
            true,
            false,
            false,
            TaskContext::NotATask,
            true,
            None,
            PillContext::Attachment,
        );
        assert!(matches!(
            &rows[0],
            menu::Row::Item {
                label,
                enabled: true,
                ..
            } if label.as_ref() == "Open in app"
        ));
        assert!(matches!(
            &rows[1],
            menu::Row::Item {
                label,
                enabled: true,
                ..
            } if label.as_ref() == "Reveal in File Explorer"
        ));
        assert!(matches!(
            &rows[2],
            menu::Row::Item {
                label,
                enabled: true,
                ..
            } if label.as_ref() == "Save as\u{2026}"
        ));
        assert!(matches!(rows[3], menu::Row::Divider));
        assert!(matches!(
            &rows[4],
            menu::Row::Item {
                label,
                enabled: true,
                ..
            } if label.as_ref() == "Remove"
        ));
        assert_eq!(
            choice_at(0, PillContext::Attachment),
            Some(Choice::OpenFile)
        );
        assert_eq!(
            choice_at(1, PillContext::Attachment),
            Some(Choice::RevealFile)
        );
        assert_eq!(
            choice_at(2, PillContext::Attachment),
            Some(Choice::SaveFileAs)
        );
        assert_eq!(
            choice_at(4, PillContext::Attachment),
            Some(Choice::RemoveAttachment)
        );
        assert_eq!(rows.len(), 5, "attachment menu is file actions only");
    }

    #[test]
    fn a_trashed_attachment_offers_only_restore() {
        let rows = rows(
            true,
            false,
            false,
            TaskContext::NotATask,
            true,
            None,
            PillContext::AttachmentTrashed,
        );
        assert_eq!(rows.len(), 1);
        assert!(matches!(
            &rows[0],
            menu::Row::Item {
                label,
                enabled: true,
                ..
            } if label.as_ref() == "Restore from Trash"
        ));
        assert_eq!(
            choice_at(0, PillContext::AttachmentTrashed),
            Some(Choice::RestoreFile)
        );
    }

    #[test]
    fn a_missing_attachment_offers_only_remove() {
        let rows = rows(
            true,
            false,
            false,
            TaskContext::NotATask,
            true,
            None,
            PillContext::AttachmentMissing,
        );
        assert_eq!(rows.len(), 1);
        assert!(matches!(
            &rows[0],
            menu::Row::Item {
                label,
                enabled: true,
                ..
            } if label.as_ref() == "Remove"
        ));
        assert_eq!(
            choice_at(0, PillContext::AttachmentMissing),
            Some(Choice::RemoveAttachment)
        );
    }
}

#[cfg(test)]
mod chip_menu_tests {
    use super::*;

    /// **The tick, as a sentence, reading the row's own lifecycle.** The same
    /// row the Tasks page offers, so one gesture is not spelled two ways.
    #[test]
    fn the_due_chips_verb_says_which_direction_it_would_go() {
        let pill = PillContext::Attribute(ChipMenu::Due);
        let labels = |task| -> Vec<String> {
            super::rows(
                true,
                false,
                false,
                task,
                true,
                None,
                None,
                pill,
                None,
                koru_i18n::Lang::En,
            )
            .into_iter()
            .filter_map(|row| match row {
                menu::Row::Item { label, .. } => Some(label.to_string()),
                _ => None,
            })
            .collect()
        };
        assert!(labels(TaskContext::Open).contains(&"Complete".to_owned()));
        assert!(labels(TaskContext::Completed).contains(&"Reopen".to_owned()));
        assert!(
            !labels(TaskContext::Completed).contains(&"Complete".to_owned()),
            "one direction at a time -- the menu is frozen when it opens"
        );
        // A due chip is the task marker, so this state cannot arise; it reads
        // as Complete rather than as a hole in the list, which is what keeps
        // `choice_at`'s offsets constant.
        assert!(labels(TaskContext::NotATask).contains(&"Complete".to_owned()));
    }

    /// **The simplified product's row menu** (koru-features all off): the
    /// task, reminder and delegate rows are gone, no divider leads, trails or
    /// doubles, and every row that is left — table group included — still
    /// maps to the choice its label names. This is the index trap the
    /// identity-based [`kept`] exists for.
    #[test]
    fn hidden_rows_leave_the_menu_and_every_index_still_names_its_row() {
        let table = Some(TableContext {
            cell: (1, 0),
            source_visible: false,
        });
        koru_features::testing::with(koru_features::Features::ALL_OFF, || {
            for table in [None, table] {
                for pill in [PillContext::None, PillContext::Document] {
                    let drawn = rows(
                        true,
                        true,
                        false,
                        TaskContext::Open,
                        true,
                        table,
                        None,
                        pill,
                        None,
                        koru_i18n::Lang::En,
                    );
                    let label = |row: &menu::Row| match row {
                        menu::Row::Item { label, .. } => Some(label.to_string()),
                        _ => None,
                    };
                    for hidden in [
                        "View in Tasks",
                        "Complete",
                        "Priority…",
                        "Remove from Tasks",
                    ] {
                        assert!(
                            !drawn
                                .iter()
                                .any(|row| label(row).as_deref() == Some(hidden)),
                            "{hidden} is still in the menu"
                        );
                    }
                    for pair in drawn.windows(2) {
                        assert!(
                            !matches!(pair, [menu::Row::Divider, menu::Row::Divider]),
                            "two rules in a row"
                        );
                    }
                    assert!(!matches!(drawn.first(), Some(menu::Row::Divider)));
                    assert!(!matches!(drawn.last(), Some(menu::Row::Divider)));
                    let expect = |text: &str| -> Option<Choice> {
                        Some(match text {
                            "Open" => Choice::Open,
                            "Open in new tab" => Choice::OpenInNewTab,
                            "Open in new window" => Choice::OpenInNewWindow,
                            "Cut" => Choice::Cut,
                            "Copy" => Choice::Copy,
                            "Paste" => Choice::Paste,
                            "Paste as plain text" => Choice::PastePlain,
                            "Move to document…" => Choice::MoveToDocument,
                            "Tags…" => Choice::Tags,
                            "Color…" => Choice::Color,
                            "Due…" => Choice::Due,
                            "Edit Markdown" => Choice::Table(koru_editor::table::Edit::ToggleView),
                            "Insert row above" => {
                                Choice::Table(koru_editor::table::Edit::InsertRowAbove)
                            }
                            "Insert row below" => {
                                Choice::Table(koru_editor::table::Edit::InsertRowBelow)
                            }
                            "Delete row" => Choice::Table(koru_editor::table::Edit::DeleteRow),
                            "Insert column before" => {
                                Choice::Table(koru_editor::table::Edit::InsertColumnBefore)
                            }
                            "Insert column after" => {
                                Choice::Table(koru_editor::table::Edit::InsertColumnAfter)
                            }
                            "Delete column" => {
                                Choice::Table(koru_editor::table::Edit::DeleteColumn)
                            }
                            _ => return None,
                        })
                    };
                    let mut items = 0;
                    for (index, row) in drawn.iter().enumerate() {
                        let Some(text) = label(row) else {
                            assert_eq!(
                                choice_at(index, pill, None, None),
                                None,
                                "row {index} is not an item"
                            );
                            continue;
                        };
                        let choice = choice_at(index, pill, None, None);
                        assert!(
                            choice.is_some_and(Choice::shown_in_product),
                            "row {index} ({text}) maps to {choice:?}"
                        );
                        if let Some(expected) = expect(&text) {
                            assert_eq!(choice, Some(expected), "row {index} ({text})");
                            items += 1;
                        }
                    }
                    assert!(items >= 8, "the ordinary rows are all there");
                    assert_eq!(
                        choice_at(drawn.len(), pill, None, None),
                        None,
                        "past the end"
                    );
                }
            }
            // The due chip keeps editing its date and clearing it, and the
            // ways into the Tasks view go.
            let due = PillContext::Attribute(ChipMenu::Due);
            let drawn = rows(
                true,
                false,
                false,
                TaskContext::Open,
                true,
                None,
                None,
                due,
                None,
                koru_i18n::Lang::En,
            );
            let mapped: Vec<Option<Choice>> = (0..drawn.len())
                .map(|i| choice_at(i, due, None, None))
                .collect();
            assert_eq!(
                mapped,
                [
                    Some(Choice::EditChip),
                    Some(Choice::ClearChip),
                    None,
                    Some(Choice::DeleteRow)
                ]
            );
        });
    }
}
