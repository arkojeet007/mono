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
//! No worker in the process — a test — means no client, and every call is a
//! cheap no-op: the block keeps its placeholder, the spelling rule.

use koru_editor::{Editing, Editor};
use koru_workspace_host::diagram::{DiagramClient, client};

pub struct Diagrams {
    client: Option<DiagramClient>,
}

impl Default for Diagrams {
    fn default() -> Self {
        Self::new()
    }
}

impl Diagrams {
    pub fn new() -> Self {
        Self { client: client() }
    }

    /// Hand the editor's pending requests to the worker and land its answers.
    /// Returns whether a drawing or a reason changed, so the view's `maintain`
    /// can ask for a redraw. One empty `try_recv` when idle.
    pub fn maintain(&mut self, editor: &mut Editor) -> bool {
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
