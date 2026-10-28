// SPDX-License-Identifier: MPL-2.0

//! **Who owns the keyboard**, said once so two views cannot drift apart about
//! it.
//!
//! ## Why this is two types and not one
//!
//! The obvious shape — a single "focus owner" a view answers with — cannot
//! reproduce what this app does, and the code says so in its own comments. An
//! open explore popover *"takes Esc, the arrows and Enter — and lets everything
//! else through"*; a focused sidebar takes *"only the navigation keys —
//! everything else falls through, so typing still reaches the document you are
//! writing in"*. `ExploreView::popover_key` answers an `Option` for exactly
//! that reason: it decides **per key**. So "who owns the keyboard" has no
//! answer until the key is known, and an enum whose names encoded the
//! fall-through (`SidebarThenTab`, `PopoverThenTab`) would only rename the
//! knowledge and leave every consumer to work out what *Then* meant.
//!
//! What *is* shared is the part that is key-independent, and the part a
//! dictation asks:
//!
//! - [`Blockers`] — the prefix of the chain that owns **everything** beneath
//!   it: the dialogs, the confirmations, the prompts, the open menu. This is
//!   where a mistake is dangerous, because a dictation that skipped a modal
//!   would type into the document behind a dialog.
//! - [`EditableTarget`] — where a typed character would land once nothing is
//!   blocking. Narrower than "who owns the keyboard", and unlike it, it has a
//!   single answer: neither a popover nor a focused sidebar accepts text.
//!
//! The per-key layering below `Blockers` — the popover, the tree's navigation
//! keys, the container chords, the active tab — is deliberately **left where it
//! is**. Factoring it would mean inventing a vocabulary for "claims some keys",
//! and there is no second consumer that needs one.
//!
//! ## Why it lives in the shell
//!
//! Two views answer these questions — the container (`MainView`) and the
//! built-in document, which is a whole window on its own in `koru-document` and
//! in a popped-out tab. `koru-workspace-host` is what both already depend on, and it
//! already carries the rest of the shell↔view vocabulary (`View`,
//! `WindowRequest`, `ImportPreview`). Putting the enums here is what makes
//! "exactly one implementation, asked by both paths" true rather than aspired
//! to.
//!
//! ## Who asks [`EditableTarget`]
//!
//! It was built beside the `Blockers` the key path *does* use, so that the two
//! answers were written and tested together rather than a second chain being
//! invented later, and for a while nothing consumed it. Two things do now, and
//! both ask the same question for the same reason:
//!
//! - **A dictation**, at `MainView::dictate`. Where the words go is decided
//!   once, at `Begin`, and every later action is delivered to what was
//!   recorded — so a surface that gained focus mid-utterance cannot take the
//!   rest of the sentence.
//! - **The document's accessibility projection**, at
//!   `apps/koru/src/mainview/accessibility/document.rs`. A paragraph publishes
//!   `read_only` exactly when the answer is not this tab's body, so a read-only
//!   document, an open document menu, a focused find bar and an open overlay
//!   each reach a screen reader as *not editable* — the same answer the
//!   keyboard gives rather than a second opinion about it. A request from an
//!   assistive technology to move the selection is refused while that is true,
//!   for the same reason.
//!
//! Both are consumers of *this* answer. Neither derives its own.

/// A modal surface: it is on top, and it swallows every key.
///
/// "Modal" here is behavioural, not visual — the open context menu is in this
/// list because it answers every key it is given, including the ones it does
/// nothing with. What they have in common is the only thing that matters to a
/// caller: nothing below them sees the press.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ModalKind {
    /// A failure the person at the keyboard has to acknowledge.
    Error,
    /// The legacy-workspace import preview.
    Import,
    /// The header's update-management panel.
    Updates,
    /// "This build is newer than the channel" — informational, one button.
    UpdateNewer,
    /// A question that must be answered before something is destroyed.
    Confirm,
    /// The right-edge All Reminders overlay.
    ///
    /// It is visually a panel, but behaviourally modal while open: neither a
    /// key nor a dictation may reach the document or AI composer underneath.
    ReminderOverlay,
    /// Exact-object reminder management has no editable field and blocks the
    /// document beneath it while open.
    ReminderManagement,
    /// A drawn diagram shown in its own view over the document: the page is
    /// dimmed, the drawing zooms and drags, and every key and press is the
    /// viewer's until `Esc` or its close control takes it down. Nothing to
    /// type into, so a modal and never a prompt.
    DiagramViewer,
    /// A right-click menu, a caption menu, or a submenu of one.
    ///
    /// Grouped with the modals rather than given a kind of its own because
    /// `menu_key` answers **true whatever the key was**: *"a chord must not,
    /// say, close a tab under a menu that is itself about to dismiss."*
    ContextMenu,
    /// The internal session recorder's setup modal (F9 / the caption menu).
    ///
    /// It *has* text fields — a prompt, by that token — but dictating into a
    /// developer tool's width box is not a case to support, so it is a modal:
    /// every key routes through the modal's own handler and an unhandled one
    /// is swallowed, and a dictation is declined rather than landed in the
    /// document beneath it.
    Recording,
}

/// A blocking surface that is also somewhere to type.
///
/// The distinction from [`ModalKind`] is not decoration: a prompt owns
/// everything beneath it *and* is the answer to [`EditableTarget`], which is
/// what lets a dictation reach a rename field instead of being refused.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PromptKind {
    /// The centred *Move to…* picker.
    MoveTo,
    /// The global-search overlay.
    Search,
    /// The taskbar's *New document* name prompt.
    NewDocument,
    /// The device-linking modal, which has a code to type into.
    LinkDevice,
    /// The emoji-and-colour picker, which has a search field.
    Appearance,
    /// Renaming a workspace, a profile, or a file in its row.
    Rename,
    /// The save-snapshot prompt (Ctrl+S), which has a name to type into.
    Snapshot,
    /// The tasks rail's inline list-name field — creating or renaming a list.
    NewList,
    /// The focused natural-language field in one-time reminder creation.
    ReminderCreation,
    /// The delegation modal's objective — the one field in it, and the reason
    /// the surface is a prompt rather than a modal.
    Delegate,
}

/// The key-independent prefix of a view's input chain: what owns **everything**
/// beneath it.
///
/// `None` does not mean "the document has the keyboard" — it means nothing is
/// blocking, and the per-key layering below decides the rest.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Blockers {
    None,
    Modal(ModalKind),
    Prompt(PromptKind),
}

impl Blockers {
    /// Whether anything at all is standing over the view's own content.
    pub fn is_blocking(self) -> bool {
        !matches!(self, Self::None)
    }

    /// Where a typed character goes **because of the blocker**, if it is one
    /// that accepts text. `None` for every modal: a confirmation has nothing
    /// to type into, which is the whole reason it is a modal.
    pub fn editable(self) -> EditableTarget {
        match self {
            Self::None => EditableTarget::None,
            Self::Modal(_) => EditableTarget::None,
            Self::Prompt(prompt) => EditableTarget::Field(FieldId::from(prompt)),
        }
    }
}

/// A one-line text field a character can be routed into.
///
/// Deliberately a closed list rather than an index: a caller has to be able to
/// *name* what it is about to dictate into, and "field 3" names nothing.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum FieldId {
    /// The global-search query box.
    Search,
    /// Renaming a workspace, a profile, or a file in its row.
    Rename,
    /// The device-linking code.
    LinkDevice,
    /// The *Move to…* picker's filter.
    MoveTo,
    /// The taskbar prompt's document name.
    NewDocument,
    /// The appearance picker's search.
    Appearance,
    /// The Koru AI composer, while the pane has focus.
    Ai,
    /// A document's find bar.
    Find,
    /// A document's replace bar.
    Replace,
    /// A prompt inside a document — the rename prompt, a slash-command name.
    DocumentPrompt,
    /// The save-snapshot prompt's name box.
    Snapshot,
    /// The tasks rail's inline list-name field.
    NewList,
    /// The natural-language one-time reminder field.
    ReminderCreation,
    /// The delegation modal's objective.
    Delegate,
}

impl From<PromptKind> for FieldId {
    fn from(prompt: PromptKind) -> Self {
        match prompt {
            PromptKind::MoveTo => Self::MoveTo,
            PromptKind::Search => Self::Search,
            PromptKind::NewDocument => Self::NewDocument,
            PromptKind::LinkDevice => Self::LinkDevice,
            PromptKind::Appearance => Self::Appearance,
            PromptKind::Rename => Self::Rename,
            PromptKind::Snapshot => Self::Snapshot,
            PromptKind::NewList => Self::NewList,
            PromptKind::ReminderCreation => Self::ReminderCreation,
            PromptKind::Delegate => Self::Delegate,
        }
    }
}

/// Which tab's body would receive the character.
///
/// A container addresses its tabs by position — `TabStrip::active` is a
/// `usize` and every chord that names one carries an index — so this is that
/// index, named.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct TabId(pub usize);

impl TabId {
    /// A window that **is** one document has no tab strip to index into.
    /// `koru-document` and a popped-out tab both answer with this.
    pub const STANDALONE: Self = Self(usize::MAX);
}

/// Where a typed character would land, once [`Blockers::None`].
///
/// This is the question dictation asks, and the one the tail of the key chain
/// already answers implicitly. `None` is a real answer and the safe one: a
/// welcome screen, a read-only document and a confirmation all accept no text,
/// and offering to dictate into them would be worse than declining.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum EditableTarget {
    /// A one-line field.
    Field(FieldId),
    /// The body of the document showing in this tab.
    Tab(TabId),
    /// Nothing here accepts text.
    None,
}

impl EditableTarget {
    /// Whether there is anywhere for a character to go.
    pub fn accepts_text(self) -> bool {
        !matches!(self, Self::None)
    }
}
