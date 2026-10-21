// SPDX-License-Identifier: MPL-2.0

//! The document's line to the shell's diagram worker.
//!
//! The editor asks for every layout and never computes one
//! (`koru_editor::diagram`); the shell lays each text out on a worker it owns
//! (`koru_workspace_host::diagram`). This is the few lines between them, run
//! once per maintenance pass: the requests the last reflow queued go to the
//! worker, and whatever the worker has answered lands on the editor, which
//! says whether a redraw is owed. The editor already keeps the version
//! stamps (the text hash), drops a stale answer and shows the old drawing
//! until the new one lands, so there is no controller state here beyond the
//! client itself.
//!
//! **The client is looked up until there is one, not once.** A view the
//! shell restores at launch is built before the shell starts the worker, so a
//! line taken at construction would be none for the life of that view and its
//! diagrams would wait forever. Until the worker exists, every pass is one
//! empty `OnceLock` read and the editor keeps its requests; the first pass
//! after it starts sends them. No worker in the process at all — a test —
//! is the same cheap no-op: the block keeps its placeholder, the spelling rule.

use koru_editor::{Editing, Editor};
use koru_workspace_host::diagram::{DiagramClient, client};

pub struct Diagrams {
    client: Option<DiagramClient>,
    connect: fn() -> Option<DiagramClient>,
}

impl Default for Diagrams {
    fn default() -> Self {
        Self::new()
    }
}

impl Diagrams {
    pub fn new() -> Self {
        Self::connecting(client)
    }

    /// A line to whatever worker `connect` finds, asked again on every pass
    /// until it finds one.
    pub(crate) fn connecting(connect: fn() -> Option<DiagramClient>) -> Self {
        Self {
            client: connect(),
            connect,
        }
    }

    /// Hand the editor's pending requests to the worker and land its answers.
    /// Returns whether a drawing or a reason changed, so the view's `maintain`
    /// can ask for a redraw. One empty `try_recv` when idle.
    pub fn maintain(&mut self, editor: &mut Editor) -> bool {
        if self.client.is_none() {
            self.client = (self.connect)();
        }
        let Some(client) = &self.client else {
            return false;
        };
        for request in editor.take_diagram_requests() {
            client.request(request.node, request.text_hash, request.text);
        }
        let mut changed = false;
        for reply in client.drain() {
            changed |= editor.diagram_result(reply.node, reply.text_hash, reply.result);
        }
        changed
    }
}
