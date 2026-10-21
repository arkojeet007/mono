// SPDX-License-Identifier: MPL-2.0

//! The page's rows, by stable id.
//!
//! **Ids, not indices.** A tick is stored machine-wide under
//! `welcome.done.<key>`, so the key is storage: renaming one forgets every
//! tick made under the old name, and reordering the page must not move a tick
//! to a different row. That is why rows are an enum with a written-out key
//! rather than a position in a list.

/// One row of the page.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RowId {
    FirstDocument,
    Documents,
    Files,
    Learn,
    /// Drawn, with nothing behind it yet.
    TaskList,
    Reminder,
    Feedback,
    Phone,
    Computer,
    /// Drawn, with nothing behind it yet.
    Watch,
    Ai,
    /// *Copy* puts the MCP command on the clipboard.
    Mcp,
    /// Opens the `+` menu's list of agents; a pick starts its tab.
    TerminalAgent,
    /// Drawn, with nothing behind it yet.
    OwnApps,
}

impl RowId {
    pub const ALL: [Self; 14] = [
        Self::FirstDocument,
        Self::Documents,
        Self::Files,
        Self::Learn,
        Self::TaskList,
        Self::Reminder,
        Self::Feedback,
        Self::Phone,
        Self::Computer,
        Self::Watch,
        Self::Ai,
        Self::Mcp,
        Self::TerminalAgent,
        Self::OwnApps,
    ];

    /// The stable storage key. **Never change one.**
    pub const fn key(self) -> &'static str {
        match self {
            Self::FirstDocument => "first-document",
            Self::Documents => "documents",
            Self::Files => "files",
            Self::Learn => "learn",
            Self::TaskList => "task-list",
            Self::Reminder => "reminder",
            Self::Feedback => "feedback",
            Self::Phone => "phone",
            Self::Computer => "computer",
            Self::Watch => "watch",
            Self::Ai => "ai",
            Self::Mcp => "mcp",
            Self::TerminalAgent => "terminal-agent",
            Self::OwnApps => "own-apps",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|row| row.key() == key)
    }

    /// Whether the row has something behind it. A row without one is drawn
    /// so the page reads as the design does, but has no tick circle, is never
    /// counted and is never a target.
    pub const fn actionable(self) -> bool {
        !matches!(self, Self::TaskList | Self::Watch | Self::OwnApps)
    }

    /// Whether this build shows the row (`koru_features`,
    /// guide/contributing/FEATURE-FLAGS.md): the task list with tasks, the
    /// reminder with reminders, Koru AI with Koru AI, the agent row with
    /// agents. *Phone*, *Watch* and
    /// *Write your own apps* are hidden for now, whatever the build: there is
    /// no phone or watch app to pair yet, and nothing to point at for apps.
    pub fn shown_in_product(self) -> bool {
        match self {
            Self::TaskList => koru_features::tasks(),
            Self::Reminder => koru_features::reminders(),
            Self::Ai => koru_features::ai(),
            Self::TerminalAgent => koru_features::agents(),
            Self::Phone | Self::Watch | Self::OwnApps => false,
            _ => true,
        }
    }

    /// Whether the row carries a tick circle and can be ticked.
    pub const fn ticks(self) -> bool {
        self.actionable()
    }

    /// Whether the row is part of its section's *n of m done*. The device
    /// rows tick but are not counted — their section has no count.
    pub const fn counted(self) -> bool {
        self.ticks() && !matches!(self, Self::Phone | Self::Computer)
    }

    /// Every id the container asks the Store about.
    pub fn persisted() -> Vec<&'static str> {
        Self::ALL
            .into_iter()
            .filter(|row| row.ticks())
            .map(Self::key)
            .collect()
    }
}

/// The three bordered cards below *Customize*.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Section {
    FirstMinutes,
    Devices,
    Further,
}

impl Section {
    pub const ALL: [Self; 3] = [Self::FirstMinutes, Self::Devices, Self::Further];

    /// The section's rows this build shows, in order — what is laid out,
    /// counted and targeted. A hidden row ([`RowId::shown_in_product`]) is not
    /// on the page at all; its stored tick is kept.
    pub fn rows(self) -> Vec<RowId> {
        self.all_rows()
            .iter()
            .copied()
            .filter(|row| row.shown_in_product())
            .collect()
    }

    /// Every row the section can hold, whatever this build shows.
    const fn all_rows(self) -> &'static [RowId] {
        match self {
            Self::FirstMinutes => &[
                RowId::FirstDocument,
                RowId::Documents,
                RowId::Files,
                RowId::Learn,
                RowId::TaskList,
                RowId::Reminder,
                RowId::Feedback,
            ],
            Self::Devices => &[RowId::Phone, RowId::Computer, RowId::Watch],
            Self::Further => &[RowId::Ai, RowId::Mcp, RowId::TerminalAgent, RowId::OwnApps],
        }
    }

    /// Whether the header shows *n of m done* and the card folds when done.
    /// Not a section left with nothing to count — *Further* when this build
    /// hides Koru AI — whose *0 of 0* would say nothing.
    pub fn counts(self) -> bool {
        !matches!(self, Self::Devices) && self.rows().iter().any(|row| row.counted())
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::FirstMinutes => super::FIRST_MINUTES_LABEL,
            Self::Devices => super::DEVICES_LABEL,
            Self::Further => super::FURTHER_LABEL,
        }
    }

    /// The one line a folded section shows.
    pub const fn done_line(self) -> &'static str {
        match self {
            Self::FirstMinutes => super::FIRST_MINUTES_DONE,
            Self::Devices => "",
            Self::Further => super::FURTHER_DONE,
        }
    }
}
