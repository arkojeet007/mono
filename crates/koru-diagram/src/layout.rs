// SPDX-License-Identifier: MPL-2.0

//! Laying a diagram out: merman's geometry, measured with the host's font,
//! built into a [`DiagramScene`].

use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;
use std::time::Instant;

use merman::{
    CancelReason, OperationCancelled, OperationControl, OperationLedgerError, RenderSemanticModel,
};
use merman_render::LayoutOptions;
use merman_render::environment::RenderEnvironment;
use merman_render::model::{FlowchartLayout, SequenceDiagramLayout};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::counters::Counters;
use crate::inspect::{self, Checked, PANIC_CODE, Refused, panic_message};
use crate::measure::{HostMeasure, RequestLog, TextMeasure};
use crate::scene::DiagramScene;
use crate::{Limits, Status, flowchart, sequence};

/// The code of a [`Fail::Internal`] for an error merman's layout returned.
pub(crate) const LAYOUT_CODE: &str = "koru.diagram.layout";
/// The code of a [`Fail::Internal`] for a layout the scene builder could not
/// read: a node or edge merman laid out but did not list, or the reverse.
pub(crate) const SCENE_CODE: &str = "koru.diagram.scene";

/// A handle to cancel a layout from another thread.
///
/// Cloning gives another handle to the same job: `cancel` on any clone
/// cancels every [`layout`] running under any of them, and the next check
/// merman makes — before and after every text measurement, every so many
/// layout steps — returns [`Fail::Cancelled`]. A handle may be reused for
/// several layouts; each gets its own deadline.
#[derive(Clone, Debug)]
pub struct Cancel(OperationControl);

impl Cancel {
    pub fn new() -> Self {
        Self(OperationControl::new())
    }

    /// Asks every layout under this handle to stop.
    pub fn cancel(&self) {
        self.0.cancel();
    }

    /// Whether [`cancel`](Self::cancel) was called on this handle or a clone.
    pub fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

impl Default for Cancel {
    fn default() -> Self {
        Self::new()
    }
}

/// Why [`layout`] gave no scene.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fail {
    /// The text does not draw: any answer of [`inspect`](crate::inspect) but
    /// [`Status::Draws`], found before any layout ran.
    Status(Status),
    /// [`Cancel::cancel`] was called while the layout ran.
    Cancelled,
    /// The layout passed [`Limits::deadline`], or exhausted merman's work
    /// budget for a diagram. The editor shows both as *too large*.
    TimedOut,
    /// merman failed or panicked, or handed back a layout this crate could
    /// not read. `code` is `koru.diagram.panic`, `koru.diagram.layout` or
    /// `koru.diagram.scene`; `message` is for the inspector and the logs,
    /// never for the page.
    Internal { code: String, message: String },
}

/// Lays `text` out as a [`DiagramScene`], measuring every label with
/// `measure` at [`TEXT_PX`](crate::TEXT_PX).
///
/// In order: the same detection, parse and count as [`inspect`](crate::inspect),
/// on one parse, so a text that does not draw never reaches a layout and is
/// answered as [`Fail::Status`]; then merman's layout under the host's
/// measurement policy and merman's `interactive` resource policy, with
/// `limits.deadline` running from the moment this function was entered and
/// `cancel` watched at every checkpoint; then the typed layout and the
/// semantic model built into the scene. The scene's origin is its top-left
/// corner. Never retried with a larger budget.
///
/// Never panics: merman runs under `catch_unwind`, and a panic is
/// [`Fail::Internal`] with the code `koru.diagram.panic`. Never waits on
/// anything but the CPU — the measurer is called on this thread. Every call
/// counts once in [`Counters`](crate::Counters), with its time.
pub fn layout(
    text: &str,
    measure: &Arc<dyn TextMeasure>,
    limits: &Limits,
    cancel: &Cancel,
) -> Result<DiagramScene, Fail> {
    let started = Instant::now();
    let result = layout_uncounted(text, measure, limits, cancel, None);
    Counters::global().record_layout(&result, started.elapsed());
    result
}

/// [`layout`] without the counting, with a log of merman's measurement
/// requests when a test asks for one.
pub(crate) fn layout_uncounted(
    text: &str,
    measure: &Arc<dyn TextMeasure>,
    limits: &Limits,
    cancel: &Cancel,
    log: Option<RequestLog>,
) -> Result<DiagramScene, Fail> {
    match panic::catch_unwind(AssertUnwindSafe(|| {
        layout_unguarded(text, measure, limits, cancel, log)
    })) {
        Ok(result) => result,
        Err(payload) => Err(Fail::Internal {
            code: PANIC_CODE.to_owned(),
            message: panic_message(&*payload),
        }),
    }
}

/// The semantic model of a diagram Koru draws, cloned out of merman's parse
/// before the layout consumes it. A flowchart comes with the parser's render
/// context, which holds the labels as merman measures them.
enum Model {
    Flowchart(
        Box<merman::diagrams::flowchart::FlowchartModel>,
        Box<merman::diagrams::flowchart::FlowchartRenderContext>,
    ),
    Sequence(Box<merman::diagrams::sequence::SequenceDiagramRenderModel>),
}

fn layout_unguarded(
    text: &str,
    measure: &Arc<dyn TextMeasure>,
    limits: &Limits,
    cancel: &Cancel,
    log: Option<RequestLog>,
) -> Result<DiagramScene, Fail> {
    // A child of the host's handle: cancelled with it, and carrying this
    // job's own deadline, which merman's control sets once per control.
    let control = cancel.0.child().with_deadline(limits.deadline);
    let Checked { counts, parsed } = match inspect::check(text, limits, &control) {
        Ok(checked) => checked,
        Err(Refused::Status(status)) => return Err(Fail::Status(status)),
        Err(Refused::Cancelled(_, cancelled)) => return Err(fail_for_cancelled(cancelled)),
    };
    let wrapping_width = parsed
        .metadata()
        .effective_config
        .as_value()
        .pointer("/flowchart/wrappingWidth")
        .and_then(Value::as_f64)
        .unwrap_or(flowchart::DEFAULT_WRAPPING_WIDTH);
    let model = match parsed.model() {
        RenderSemanticModel::Flowchart(model) => Model::Flowchart(
            Box::new(model.clone()),
            Box::new(
                parsed
                    .flowchart_render_context()
                    .cloned()
                    .unwrap_or_default(),
            ),
        ),
        RenderSemanticModel::Sequence(model) => Model::Sequence(Box::new(model.clone())),
        // `check` answers a kind Koru does not draw before this.
        _ => {
            return Err(internal(
                SCENE_CODE,
                "the parse gave a model of a kind Koru does not draw",
            ));
        }
    };
    let policy = HostMeasure::new(Arc::clone(measure), log).policy();
    let session = RenderEnvironment::deterministic()
        .with_text_measurement_policy(policy)
        .begin_session_with_control(control)
        .map_err(|error| internal(LAYOUT_CODE, error))?;
    let artifact =
        merman_render::family::prepare(parsed, &LayoutOptions::headless_svg_defaults(), session)
            .map_err(fail_for_render)?;
    let json = artifact.layout_json().map_err(fail_for_render)?;
    let layout = json
        .as_value()
        .get("layout")
        .cloned()
        .unwrap_or(Value::Null);
    match model {
        Model::Flowchart(model, context) => {
            let typed: FlowchartLayout = typed_layout(&layout, "FlowchartV2")?;
            flowchart::build(
                &model,
                &context,
                typed,
                counts,
                measure.as_ref(),
                wrapping_width,
            )
        }
        Model::Sequence(model) => {
            let typed: SequenceDiagramLayout = typed_layout(&layout, "SequenceDiagram")?;
            sequence::build(&model, typed, counts, measure.as_ref())
        }
    }
}

/// The typed layout of `family` out of merman's `layout` JSON member.
fn typed_layout<T: DeserializeOwned>(layout: &Value, family: &str) -> Result<T, Fail> {
    let value = layout
        .get(family)
        .ok_or_else(|| internal(LAYOUT_CODE, format!("merman gave no {family} layout")))?;
    serde_json::from_value(value.clone()).map_err(|error| internal(LAYOUT_CODE, error))
}

pub(crate) fn internal(code: &str, message: impl ToString) -> Fail {
    Fail::Internal {
        code: code.to_owned(),
        message: message.to_string(),
    }
}

/// A cancellation merman reported, by its reason.
fn fail_for_cancelled(cancelled: OperationCancelled) -> Fail {
    match cancelled.reason {
        CancelReason::Requested => Fail::Cancelled,
        CancelReason::DeadlineExceeded => Fail::TimedOut,
    }
}

/// An error merman's layout returned. A cancellation keeps its reason; a
/// resource ceiling is a diagram too large to draw; the rest is internal.
fn fail_for_render(error: merman_render::Error) -> Fail {
    use merman_render::Error;
    match error {
        Error::Cancelled(cancelled) => fail_for_cancelled(cancelled),
        Error::ResourceLimitExceeded(_) => Fail::TimedOut,
        Error::OperationResourceTerminal(OperationLedgerError::Cancelled(cancelled)) => {
            fail_for_cancelled(cancelled)
        }
        Error::OperationResourceTerminal(_) => Fail::TimedOut,
        other => internal(LAYOUT_CODE, other),
    }
}
