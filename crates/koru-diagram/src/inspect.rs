// SPDX-License-Identifier: MPL-2.0

//! Detecting a diagram's kind, and inspecting whether its text draws.

use std::any::Any;
use std::panic::{self, AssertUnwindSafe};

use merman::diagrams::sequence::SequenceMessageKind;
use merman::resources::InputResourcePolicy;
use merman::{
    Engine, Error, MermaidConfig, OperationControl, ParseOptions, RenderSemanticModel,
    TerminalDiagnostic, diagram_type_family_id,
};

use crate::counters::Counters;
use crate::engine::engine;
use crate::line::line_for_span;
use crate::{Counts, Kind, Limits, Status};

/// The diagnostic code of a [`Status::NotParsed`] that stands for a panic
/// inside the engine, caught so that Koru keeps running. The message is the
/// panic's own.
pub(crate) const PANIC_CODE: &str = "koru.diagram.panic";

/// The kind of diagram `text` declares, or `None` when no diagram keyword is
/// found.
///
/// This is merman's type detection alone — front matter, directives and
/// comments stripped, then the header matched — without parsing the body, so
/// it is cheap enough to ask on every keystroke. A text that detects as a
/// kind may still fail to parse; [`inspect`] says.
pub fn detect(text: &str) -> Option<Kind> {
    detect_type(engine(), text).map(kind_of)
}

/// merman's type id for `text`: `flowchart-v2`, `sequence`, `pie`, …
///
/// Detection is a pass of pattern matches over untrusted text. It is not
/// expected to panic, and a panic is still answered as nothing detected.
fn detect_type(engine: &Engine, text: &str) -> Option<&'static str> {
    panic::catch_unwind(AssertUnwindSafe(|| {
        engine
            .registry()
            .detect_type(text, &mut MermaidConfig::default())
            .ok()
    }))
    .ok()
    .flatten()
}

/// The [`Kind`] of merman's type id, by family, so that every flowchart
/// header is one kind.
fn kind_of(type_id: &'static str) -> Kind {
    match diagram_type_family_id(type_id).map(|family| family.as_str()) {
        Some("flowchart") => Kind::Flowchart,
        Some("sequence") => Kind::Sequence,
        _ => Kind::Other(type_id.to_owned()),
    }
}

/// Whether `text` draws and, if not, why.
///
/// In order: an empty or whitespace text is [`Status::Empty`]; a text whose
/// header is no diagram, or a kind Koru does not draw, is
/// [`Status::KindNotDrawn`]; a text merman cannot parse is
/// [`Status::NotParsed`] at the line Koru names; a parsed diagram with more
/// nodes or messages than `limits` allow is [`Status::TooLarge`]; the rest
/// is [`Status::Draws`] with its counts. The parse runs under merman's
/// `interactive` resource policy, the one its own renderer applies, so a text
/// over that policy's source or model budget is too large as well.
///
/// **No layout and no text measurement happen here.** The cost is a parse and
/// a count — a fraction of a millisecond for a showcase diagram — so the MCP
/// server asks once per code block on every read, and the editor asks before
/// it requests a layout.
///
/// The line of a [`Status::NotParsed`] is the 1-based line, in the text as
/// given, of the start of merman's span; merman maps spans back through the
/// front matter and directives it strips. When there is no span, when merman
/// marks it `Fallback`, or when it starts at or beyond the last
/// non-whitespace byte — a missing `end` — Koru names the **last non-empty
/// line**, where the person has to type. [`line_of`](crate::line_of) is the
/// line arithmetic.
///
/// Never panics: the engine runs under `catch_unwind`, and a panic is a
/// [`Status::NotParsed`] at line 1 with the code `koru.diagram.panic`. Never
/// waits on anything but the CPU. Every call counts once in [`Counters`].
pub fn inspect(text: &str, limits: &Limits) -> Status {
    let status = inspect_uncounted(text, limits);
    Counters::global().record(&status);
    status
}

/// [`inspect`] without the counting, for the counters' own tests.
pub(crate) fn inspect_uncounted(text: &str, limits: &Limits) -> Status {
    if text.trim().is_empty() {
        return Status::Empty;
    }
    let kind = match detect(text) {
        None => return Status::KindNotDrawn { kind: None },
        Some(Kind::Other(type_id)) => {
            return Status::KindNotDrawn {
                kind: Some(type_id),
            };
        }
        Some(kind) => kind,
    };
    match panic::catch_unwind(AssertUnwindSafe(|| parse_and_count(text, &kind, limits))) {
        Ok(status) => status,
        Err(payload) => Status::NotParsed {
            line: 1,
            message: panic_message(&*payload),
            code: PANIC_CODE.to_owned(),
        },
    }
}

/// Parses `text`, already detected as `kind`, and counts it against `limits`.
fn parse_and_count(text: &str, kind: &Kind, limits: &Limits) -> Status {
    // merman's engine takes no resource policy; its renderer applies this one
    // around the same parse — the source budget before, the model budget
    // after — and so does this.
    let policy = InputResourcePolicy::default();
    if policy.check_source_bytes(text).is_err() {
        return too_large_uncounted(kind);
    }
    let control = OperationControl::new();
    let parsed = match engine().parse_diagram_for_render_model_controlled_sync(
        text,
        ParseOptions::strict(),
        &control,
    ) {
        // Nobody else holds the control and it has no deadline, so this does
        // not happen; it is answered the way a cancelled layout is.
        Err(_cancelled) => return too_large_uncounted(kind),
        Ok(Err(error)) => return status_for_error(text, kind, error),
        // Nothing was left after preprocessing; detection says so first.
        Ok(Ok(None)) => return Status::KindNotDrawn { kind: None },
        Ok(Ok(Some(parsed))) => parsed,
    };
    let counts = match parsed.model() {
        RenderSemanticModel::Flowchart(model) => Counts::Flowchart {
            // An arrow to or from a group gives merman a node for the group,
            // marked as its anchor and drawn as nothing; the boxes are the
            // authored nodes.
            nodes: model
                .nodes
                .iter()
                .filter(|node| !node.is_subgraph_anchor())
                .count(),
            edges: model.edges.len(),
            groups: model.subgraphs.len(),
        },
        RenderSemanticModel::Sequence(model) => Counts::Sequence {
            participants: model.actors.len(),
            // Mermaid's message list also holds the notes, the activations,
            // autonumbering and every control statement (`loop`, `alt`,
            // `else`, `end`, …) in source order; a message is a signal, an
            // arrow between participants.
            messages: model
                .messages
                .iter()
                .filter(|message| matches!(message.semantic_kind(), SequenceMessageKind::Signal))
                .count(),
            notes: model.notes.len(),
        },
        // A header detected as a kind Koru draws, parsed into something else.
        _ => {
            return Status::KindNotDrawn {
                kind: Some(parsed.metadata().diagram_type.clone()),
            };
        }
    };
    let (count, max) = match counts {
        Counts::Flowchart { nodes, .. } => (nodes, limits.max_nodes),
        Counts::Sequence { messages, .. } => (messages, limits.max_messages),
    };
    if count > max || policy.check_parsed_render(&parsed).is_err() {
        return Status::TooLarge {
            kind: counts.kind(),
            count,
        };
    }
    Status::Draws(counts)
}

/// Too large before anything was counted.
fn too_large_uncounted(kind: &Kind) -> Status {
    Status::TooLarge {
        kind: kind.clone(),
        count: 0,
    }
}

/// The status for an error merman returned from the parse.
fn status_for_error(text: &str, kind: &Kind, error: Error) -> Status {
    match error {
        Error::DetectType(_) => Status::KindNotDrawn { kind: None },
        Error::UnsupportedDiagram { diagram_type } => Status::KindNotDrawn {
            kind: Some(diagram_type),
        },
        Error::OperationCancelled(_) => too_large_uncounted(kind),
        // A syntax error, a malformed front matter or directive, a theme
        // color merman cannot read: its terminal-safe projection gives the
        // stable code, the span, and a bounded message.
        error => {
            let diagnostic = TerminalDiagnostic::from(error);
            let details = diagnostic.terminal_diagnostic_details();
            Status::NotParsed {
                line: line_for_span(text, details.span, details.span_kind),
                message: diagnostic.terminal_safe_message(),
                code: details.code,
            }
        }
    }
}

/// The text of a panic payload: what `panic!` was given, when that was text.
fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "the diagram engine panicked".to_owned()
    }
}
