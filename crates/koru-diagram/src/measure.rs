// SPDX-License-Identifier: MPL-2.0

//! Text measurement: the host's measurer in Koru's vocabulary, and its
//! adapter to merman's.
//!
//! merman bundles no font. It asks a **host measurer** for the width of every
//! label as it lays a diagram out, one of twenty-two operations per request
//! (`getComputedTextLength`, `getBBox().width`, a wrapped block, …), each
//! carrying the font family, size and weight the diagram's text and config
//! ask for. Koru answers every one of them with its own font at [`TEXT_PX`]
//! and weight 400 and ignores the style in the request — which is how a
//! `classDef`/`style` font directive is neutralised without parsing it — and
//! answers `Ok(None)` for the operations it does not model, so merman falls
//! back to its own table for that one request. merman checks cancellation
//! before and after every request, which is what makes a layout cancellable
//! while the host measures.
//!
//! The painter's measurer lives in koru-draw on parley and the bundled Inter
//! face; this crate's tests use a deterministic stub.

use std::convert::Infallible;
use std::sync::{Arc, LazyLock, Mutex};

use merman::svg::{
    HostMeasurementResult, HostTextMeasurement, HostTextMeasurementRequest, HostTextMeasurer,
    MeasurementProfileId, TextMeasurementOperation, TextMeasurementPhase, TextMeasurementPolicy,
    TextMeasurementProfileIdentity, TextMetrics as MermanMetrics,
};
use merman_render::text::try_measure_wrapped_text;

/// The size every label is measured and laid out at, in logical pixels, at
/// weight 400 — whatever the diagram's text or merman's config asks for.
/// One step below the 16 px body on the type scale.
pub const TEXT_PX: f32 = 14.0;

/// The width and vertical metrics of one line of text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextMetrics {
    /// The advance of the line.
    pub width: f32,
    /// The line height: lines of a label stack at this distance.
    pub height: f32,
    /// The baseline's distance from the top of the line.
    pub ascent: f32,
}

/// One line of a wrapped text, with its width.
#[derive(Clone, Debug, PartialEq)]
pub struct MeasuredLine {
    pub text: String,
    pub width: f32,
}

/// The host's text measurer: the one font every diagram is laid out with.
///
/// Implemented by koru-draw on parley and Inter, and by a deterministic stub
/// in this crate's tests. Both are asked at [`TEXT_PX`]; the `px` argument
/// is passed so a measurer need not know the constant.
pub trait TextMeasure: Send + Sync {
    /// Width and vertical metrics of one line of `text` at `px` logical
    /// pixels.
    fn measure(&self, text: &str, px: f32) -> TextMetrics;
    /// `text` broken at word boundaries so no line exceeds `max_width`; one
    /// entry per line, each with its width.
    fn wrap(&self, text: &str, px: f32, max_width: f32) -> Vec<MeasuredLine>;
}

/// One request merman made of the adapter, as the tests record it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Request {
    pub operation: TextMeasurementOperation,
    pub phase: TextMeasurementPhase,
    pub text: String,
    pub max_width: Option<f64>,
    /// `false` when the adapter declined with `Ok(None)`.
    pub answered: bool,
}

/// Where the adapter records its requests when a test asks it to.
pub(crate) type RequestLog = Arc<Mutex<Vec<Request>>>;

/// The adapter: merman's [`HostTextMeasurer`] over the host's [`TextMeasure`].
pub(crate) struct HostMeasure {
    measure: Arc<dyn TextMeasure>,
    log: Option<RequestLog>,
}

/// The identity merman records for every measurement this adapter answers.
static IDENTITY: LazyLock<TextMeasurementProfileIdentity> = LazyLock::new(|| {
    let profile = MeasurementProfileId::new("koru.text-measure").expect("a non-empty profile id");
    TextMeasurementProfileIdentity::new(profile, env!("CARGO_PKG_VERSION"))
        .expect("a non-empty version")
});

impl HostMeasure {
    pub(crate) fn new(measure: Arc<dyn TextMeasure>, log: Option<RequestLog>) -> Self {
        Self { measure, log }
    }

    /// The measurement policy for a merman render environment: this adapter
    /// on every phase, merman's own table behind it for a declined request.
    pub(crate) fn policy(self) -> TextMeasurementPolicy {
        TextMeasurementPolicy::host_display(
            IDENTITY.clone(),
            Arc::new(self),
            TextMeasurementPhase::ALL,
        )
    }

    /// The width of one line at [`TEXT_PX`], never negative or NaN.
    fn line_width(&self, line: &str) -> f64 {
        finite(f64::from(self.measure.measure(line, TEXT_PX).width))
    }

    /// The height of one line at [`TEXT_PX`], never negative or NaN.
    fn line_height(&self, line: &str) -> f64 {
        finite(f64::from(self.measure.measure(line, TEXT_PX).height))
    }

    /// merman's metrics of `lines` stacked: the widest line, the heights
    /// summed, and the count. No lines measure as one empty line.
    fn metrics<'a>(&self, lines: impl IntoIterator<Item = &'a str>) -> MermanMetrics {
        let mut metrics = MermanMetrics {
            width: 0.0,
            height: 0.0,
            line_count: 0,
        };
        for line in lines {
            metrics.width = metrics.width.max(self.line_width(line));
            metrics.height += self.line_height(line);
            metrics.line_count += 1;
        }
        if metrics.line_count == 0 {
            metrics.height = self.line_height("");
            metrics.line_count = 1;
        }
        metrics
    }

    /// The text of a request broken into merman's own lines — at
    /// `max_width` by merman's wrapping rules with this measurer's widths, or
    /// at its `'\n'`s alone when there is no width — and measured. Using
    /// merman's wrapping keeps the lines it sizes the ones it later places.
    fn wrapped(&self, request: &HostTextMeasurementRequest<'_>) -> (MermanMetrics, Option<f64>) {
        let layout = try_measure_wrapped_text(
            request.text,
            request.style,
            request.max_width,
            request.wrap_mode,
            0.0,
            |candidate, _| Ok::<f64, Infallible>(self.line_width(candidate)),
        );
        let layout = match layout {
            Ok(layout) => layout,
            Err(never) => match never {},
        };
        let metrics = self.metrics(layout.lines.iter().map(String::as_str));
        let raw_width = layout
            .raw_width
            .map(finite)
            .or_else(|| Some(self.metrics(request.text.split('\n')).width));
        (metrics, raw_width)
    }

    fn answer(&self, request: &HostTextMeasurementRequest<'_>) -> Option<HostTextMeasurement> {
        use TextMeasurementOperation as Op;
        let lines = || request.text.split('\n');
        match request.operation {
            // One block of text, lines at its `'\n'`s.
            Op::Measure | Op::MermaidCalculateTextDimensions => {
                Some(HostTextMeasurement::Metrics(self.metrics(lines())))
            }
            // A block wrapped at the request's width by merman's rules.
            Op::Wrapped => Some(HostTextMeasurement::Metrics(self.wrapped(request).0)),
            Op::WrappedWithRawWidth => {
                let (metrics, raw_width) = self.wrapped(request);
                Some(HostTextMeasurement::WrappedWithRawWidth { metrics, raw_width })
            }
            // Every width merman asks for — the advance along the baseline,
            // a bounding box, a canvas measurement — is the advance of the
            // widest line here: Koru draws the text itself, so there is no
            // glyph overhang to model.
            Op::ComputedLength
            | Op::SimpleBBoxWidth
            | Op::RawBBoxWidth
            | Op::TspanBBoxWidth
            | Op::WrapProbeBBoxWidth
            | Op::BoundingClientRectWidth
            | Op::CanvasMeasureTextWidth => {
                Some(HostTextMeasurement::Length(self.metrics(lines()).width))
            }
            Op::SimpleBBoxHeight | Op::RawBBoxHeight | Op::TspanBBoxHeight => {
                Some(HostTextMeasurement::Length(self.metrics(lines()).height))
            }
            // The extents of a centred `<text>` around its anchor: symmetric.
            Op::BBoxX | Op::BBoxXWithAsciiOverhang | Op::TitleBBoxX => {
                let half = self.metrics(lines()).width / 2.0;
                Some(HostTextMeasurement::HorizontalExtents {
                    left: half,
                    right: half,
                })
            }
            // The y offset of a browser `getBBox()` for a given DOM shape and
            // baseline rule: a fact about a font in a browser that Koru does
            // not draw. Declined; merman uses its own table for it.
            Op::CreateTextBBoxYOffset
            | Op::CreateTextMiddleBBoxYOffset
            | Op::RawBBoxYOffset
            | Op::RawMiddleBBoxYOffset
            | Op::RawHangingBBoxYOffset => None,
        }
    }
}

impl HostTextMeasurer for HostMeasure {
    fn measure(&self, request: HostTextMeasurementRequest<'_>) -> HostMeasurementResult {
        let answer = self.answer(&request);
        if let Some(log) = &self.log
            && let Ok(mut log) = log.lock()
        {
            log.push(Request {
                operation: request.operation,
                phase: request.phase,
                text: request.text.to_owned(),
                max_width: request.max_width,
                answered: answer.is_some(),
            });
        }
        Ok(answer)
    }
}

/// `value`, or `0.0` when it is negative or not a number — merman rejects
/// such a measurement and would fall back for it.
fn finite(value: f64) -> f64 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}
