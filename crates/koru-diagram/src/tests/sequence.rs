// SPDX-License-Identifier: MPL-2.0

//! The sequence builder: the showcase's messages, notes, activations and
//! frames against the facts of its text; a self-message; the `box`, title and
//! `critical` of the checkout text; every label line held to a line merman
//! sized; and every scene held, element by element, to merman's own SVG of
//! the same text under the same measurer — the picture Koru draws is the one
//! merman draws.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use kurbo::{PathEl, Point, Rect, Shape};
use merman::entities::decode_mermaid_entities_to_unicode;
use merman::svg::TextMeasurementOperation;
use merman::{OperationControl, RenderOutput, RenderRequest, Renderer, SvgEnvironment, SvgRequest};

use super::layout::{ends, merman_bounds, sequence_of, sequence_showcase};
use super::{SEQUENCE_CHECKOUT, SEQUENCE_SHOWCASE, Stub, stub};
use crate::engine::engine;
use crate::layout::layout_uncounted;
use crate::measure::{HostMeasure, RequestLog};
use crate::{
    Cancel, DiagramScene, Frame, Label, LabelLine, Limits, Marker, Message, ParticipantKind, Role,
    SequenceScene, StrokeKind, TextMeasure, layout,
};

/// Two participants, a message from A to itself and one on to B.
const SELF_MESSAGE: &str = "sequenceDiagram
    participant A
    participant B
    A->>A: think
    A->>B: say
";

/// Labels Mermaid wraps or breaks: a `wrap:` participant name, message and
/// note, a `<br>` with characters Mermaid's sanitizer spells as entities,
/// and a loop title and a divider label longer than the width their frame
/// gives them. (A written `&amp;` or `#quot;` cannot be tested here: the
/// `;` ends a statement in Mermaid's sequence grammar.)
const WRAPPED: &str = "sequenceDiagram
    participant A as wrap: A participant with a rather long name
    participant B
    A->>B: wrap: a message long enough that Mermaid breaks it into several lines at the message width
    Note over A,B: wrap: a note long enough that Mermaid breaks it into several lines inside its box
    B-->>A: first<br>second & \"third\"
    loop a loop title long enough to be broken at the width Mermaid gives the loop
        A->>A: hmm
    end
    alt one
        A->>B: x
    else a divider label that is long enough to wrap at the block width too
        B->>A: y
    end
";

fn scene_of(text: &str) -> DiagramScene {
    layout(text, &stub(), &Limits::default(), &Cancel::new()).expect("the text draws")
}

/// The lifeline x of participant `id`.
fn lifeline_x(sequence: &SequenceScene, id: &str) -> f64 {
    let index = sequence
        .participants
        .iter()
        .position(|p| p.id == id)
        .unwrap_or_else(|| panic!("participant {id}"));
    sequence.lifelines[index].x
}

fn message<'a>(sequence: &'a SequenceScene, text: &str) -> &'a Message {
    sequence
        .messages
        .iter()
        .find(|m| m.label.text == text)
        .unwrap_or_else(|| panic!("message {text:?}"))
}

/// The y of a message's line.
fn line_y(message: &Message) -> f64 {
    ends(&message.path).0.y
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6
}

#[test]
fn the_showcase_has_twelve_messages_in_order_numbered_with_their_markers_and_strokes() {
    let scene = sequence_showcase();
    let sequence = sequence_of(&scene);
    let texts: Vec<&str> = sequence
        .messages
        .iter()
        .map(|m| m.label.text.as_str())
        .collect();
    assert_eq!(
        texts,
        [
            "Sign in",
            "Request challenge",
            "Challenge",
            "Ask for passkey",
            "Touch sensor",
            "Signed challenge",
            "Create session",
            "Session id",
            "Session token",
            "Rejected",
            "Register device",
            "Signed in",
        ]
    );
    let numbers: Vec<&str> = sequence
        .messages
        .iter()
        .map(|m| m.number.as_ref().expect("numbered").text.as_str())
        .collect();
    assert_eq!(
        numbers,
        [
            "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12"
        ]
    );
    let ends_of = |m: &Message| (m.from.clone(), m.to.clone());
    assert_eq!(ends_of(&sequence.messages[0]), ("P".into(), "App".into()));
    assert_eq!(
        ends_of(&sequence.messages[6]),
        ("Server".into(), "DB".into())
    );
    assert_eq!(ends_of(&sequence.messages[11]), ("App".into(), "P".into()));

    let challenge = message(sequence, "Challenge");
    assert_eq!(
        (challenge.stroke, challenge.start, challenge.end),
        (StrokeKind::Dotted, Marker::None, Marker::Arrow)
    );
    // `--x`: a dotted line ending in a cross.
    let rejected = message(sequence, "Rejected");
    assert_eq!(
        (rejected.stroke, rejected.end),
        (StrokeKind::Dotted, Marker::Cross)
    );
    let register = message(sequence, "Register device");
    assert_eq!(
        (register.stroke, register.end),
        (StrokeKind::Normal, Marker::OpenArrow)
    );
    let sign_in = message(sequence, "Sign in");
    assert_eq!(
        (sign_in.stroke, sign_in.start, sign_in.end),
        (StrokeKind::Normal, Marker::None, Marker::Arrow)
    );

    let mut last_y = f64::NEG_INFINITY;
    for message in &sequence.messages {
        // A straight line at one y from the sender's lifeline to the
        // receiver's. Mermaid starts it 1 px off the lifeline's centre and
        // stops it at the edge of an activation bar (5 px) and 3 px short of
        // an arrowhead, so an end is within 9 px of its lifeline, between the
        // two.
        assert!(
            matches!(
                message.path.elements(),
                [PathEl::MoveTo(_), PathEl::LineTo(_)]
            ),
            "{}",
            message.id
        );
        let (start, end) = ends(&message.path);
        assert_eq!(start.y, end.y);
        assert!(
            start.y > last_y,
            "messages run down the page in source order"
        );
        last_y = start.y;
        let (from_x, to_x) = (
            lifeline_x(sequence, &message.from),
            lifeline_x(sequence, &message.to),
        );
        assert!(
            (start.x - from_x).abs() <= 9.0 && (end.x - to_x).abs() <= 9.0,
            "{}: {start:?} to {end:?} between the lifelines at {from_x} and {to_x}",
            message.id
        );
        assert!(
            (start.x - from_x) * (to_x - from_x) >= 0.0 && (end.x - to_x) * (from_x - to_x) >= 0.0,
            "{}: the line stays between its lifelines",
            message.id
        );
        // The label is centred over the line, just above it.
        assert_eq!(message.label.role, Role::Text);
        assert!(
            message.label.rect.y1 <= start.y && start.y - message.label.rect.y1 < 10.0,
            "{}: the label {:?} sits over the line at {}",
            message.id,
            message.label.rect,
            start.y
        );
        assert!((message.label.rect.center().x - (start.x + end.x) / 2.0).abs() <= 0.5);
        // The number sits on the line at the end it leaves, 1 px inside the
        // sender's activation stack.
        let number = message.number.as_ref().unwrap();
        assert_eq!((number.role, number.lines.len()), (Role::Accent, 1));
        assert!(near(number.rect.center().y, start.y));
        assert!(
            (number.rect.center().x - from_x).abs() <= 6.0,
            "{}: the number at {} by the lifeline at {from_x}",
            message.id,
            number.rect.center().x
        );
    }
}

#[test]
fn the_showcase_has_its_notes_activations_and_frames_where_the_text_puts_them() {
    let scene = sequence_showcase();
    let sequence = sequence_of(&scene);
    let x = |id: &str| lifeline_x(sequence, id);

    // --- notes
    let texts: Vec<&str> = sequence
        .notes
        .iter()
        .map(|n| n.label.text.as_str())
        .collect();
    assert_eq!(
        texts,
        ["The challenge expires after 60 s", "Shows \"Try again\""]
    );
    for note in &sequence.notes {
        assert!(note.rect.contains(note.label.rect.center()));
        assert!(note.label.rect.width() <= note.rect.width());
        assert_eq!(note.label.role, Role::Text);
    }
    let (over, right_of) = (&sequence.notes[0], &sequence.notes[1]);
    assert!(
        over.rect.x0 < x("App") && x("Server") < over.rect.x1,
        "over App and Server"
    );
    assert!(
        right_of.rect.x0 > x("App") && right_of.rect.x0 < x("Server"),
        "right of App"
    );
    assert!(
        line_y(message(sequence, "Challenge")) < over.rect.y0
            && over.rect.y1 < line_y(message(sequence, "Ask for passkey"))
    );

    // --- activations: App once, Server twice, each on its lifeline
    assert_eq!(sequence.activations.len(), 3);
    let bars: Vec<&Rect> = sequence.activations.iter().collect();
    for (bar, id) in bars.iter().zip(["App", "Server", "Server"]) {
        assert!(
            near(bar.center().x, x(id)),
            "{bar:?} on {id}'s lifeline at {}",
            x(id)
        );
        assert_eq!(bar.width(), 10.0);
    }
    // A bar runs from the line that activates to the line that deactivates;
    // Mermaid nudges every bar but the diagram's first one 2 px down.
    assert!(near(bars[0].y0, line_y(message(sequence, "Sign in"))));
    assert!(near(bars[0].y1, line_y(message(sequence, "Signed in"))));
    assert!(near(
        bars[1].y0,
        line_y(message(sequence, "Request challenge")) + 2.0
    ));
    assert!(near(bars[1].y1, line_y(message(sequence, "Challenge"))));
    assert!(near(
        bars[2].y0,
        line_y(message(sequence, "Signed challenge")) + 2.0
    ));
    assert!(near(bars[2].y1, line_y(message(sequence, "Session token"))));

    // --- frames
    let kinds: Vec<&str> = sequence.frames.iter().map(|f| f.kind.as_str()).collect();
    assert_eq!(kinds, ["loop", "alt", "opt"]);
    let [loop_, alt, opt] = sequence.frames.as_slice() else {
        panic!("three frames");
    };
    let encloses = |frame: &Frame, text: &str| {
        let m = message(sequence, text);
        let (start, end) = ends(&m.path);
        frame.rect.contains(start)
            && frame.rect.contains(end)
            && frame.rect.contains(m.label.rect.center())
    };

    assert_eq!(
        loop_.title.text.replace('\n', " "),
        "[Until the person confirms or cancels]"
    );
    // Mermaid wraps a frame's title to the width its contents give the
    // frame; this one takes two lines.
    assert_eq!(loop_.title.lines.len(), 2);
    assert!(loop_.sections.is_empty());
    assert!(encloses(loop_, "Ask for passkey") && encloses(loop_, "Touch sensor"));
    assert!(!encloses(loop_, "Challenge"));

    assert_eq!(alt.title.text, "[Signature valid]");
    assert_eq!(alt.sections.len(), 1);
    let section = &alt.sections[0];
    let label = section.label.as_ref().expect("the else is labelled");
    assert_eq!(
        (label.text.as_str(), label.role),
        ("[Signature rejected]", Role::TextDim)
    );
    let (token, rejected) = (
        line_y(message(sequence, "Session token")),
        line_y(message(sequence, "Rejected")),
    );
    assert!(
        token < section.y && section.y < rejected,
        "the divider at {} between {token} and {rejected}",
        section.y
    );
    assert!(
        section.y < label.rect.y0 && label.rect.y1 < rejected,
        "the label hangs under the divider"
    );
    assert!(near(label.rect.center().x, alt.rect.center().x));
    for text in [
        "Signed challenge",
        "Create session",
        "Session id",
        "Session token",
        "Rejected",
    ] {
        assert!(encloses(alt, text), "{text}");
    }

    // Also wrapped: the opt's one message gives it the same width.
    assert_eq!(opt.title.text.replace('\n', " "), "[Remember this device]");
    assert!(opt.sections.is_empty());
    assert!(encloses(opt, "Register device"));

    for frame in [loop_, alt, opt] {
        // The title sits right of the 50 px tag at the top-left corner, its
        // first baseline on the first text row under the top edge.
        assert_eq!(frame.title.role, Role::TextDim);
        assert!(
            frame.title.rect.x0 >= frame.rect.x0 + 50.0 && frame.title.rect.x1 <= frame.rect.x1,
            "{}: the title {:?} between the tag and the right edge",
            frame.kind,
            frame.title.rect
        );
        assert!(near(
            frame.title.rect.center().x,
            (frame.rect.x0 + 50.0 + frame.rect.x1) / 2.0
        ));
        let baseline = frame.title.lines[0].baseline;
        assert!(
            (baseline - (frame.rect.y0 + 17.5)).abs() <= 0.5,
            "{}: the first baseline {baseline} under the top at {}",
            frame.kind,
            frame.rect.y0
        );
    }
    assert!(sequence.boxes.is_empty() && sequence.title.is_none());
}

#[test]
fn a_self_message_loops_right_of_its_lifeline_and_back() {
    let scene = scene_of(SELF_MESSAGE);
    let sequence = sequence_of(&scene);
    let a = lifeline_x(sequence, "A");
    let think = message(sequence, "think");
    assert_eq!((think.from.as_str(), think.to.as_str()), ("A", "A"));
    let [PathEl::MoveTo(start), PathEl::CurveTo(c1, c2, end)] = think.path.elements() else {
        panic!("a move and one cubic, not {:?}", think.path.elements());
    };
    // Mermaid leaves the lifeline 1 px right of its centre, as every message
    // does, bows 60 px to the right and returns to the same x 20 px lower.
    assert!((start.x - a).abs() <= 1.0 && (end.x - a).abs() <= 1.0);
    assert_eq!(end.y, start.y + 20.0);
    assert_eq!((c1.x, c2.x), (start.x + 60.0, start.x + 60.0));
    assert_eq!((c1.y, c2.y), (start.y - 10.0, start.y + 30.0));
    assert!(think.path.bounding_box().x1 > a + 30.0);
    // Its text is centred on the lifeline above the loop; no autonumber.
    assert!((think.label.rect.center().x - a).abs() <= 1.0);
    assert!(think.label.rect.y1 <= start.y);
    assert!(think.number.is_none());
    // The next message starts below the loop.
    let say = message(sequence, "say");
    assert!(line_y(say) > end.y);
    assert!(matches!(
        say.path.elements(),
        [PathEl::MoveTo(_), PathEl::LineTo(_)]
    ));
}

#[test]
fn the_checkout_text_has_a_box_a_title_and_a_critical_frame_with_an_option() {
    let scene = scene_of(SEQUENCE_CHECKOUT);
    let sequence = sequence_of(&scene);
    let x = |id: &str| lifeline_x(sequence, id);

    assert_eq!(sequence.boxes.len(), 1);
    let shop = &sequence.boxes[0];
    let shop_title = shop.title.as_ref().expect("the box is named");
    assert_eq!(
        (shop_title.text.as_str(), shop_title.role),
        ("Shop", Role::TextDim)
    );
    assert!(
        shop.rect.x0 < x("A") && x("B") < shop.rect.x1 && shop.rect.x1 < x("C"),
        "the box holds A and B and leaves C out"
    );
    let first = &sequence.participants[0];
    assert!(
        shop.rect.contains(shop_title.rect.center()) && shop_title.rect.y1 <= first.top.y0,
        "named above its participants"
    );
    assert!(
        shop.rect.y1 >= first.bottom.y1 - 1e-6,
        "down past the footer"
    );

    let title = sequence.title.as_ref().expect("a title");
    assert_eq!((title.text.as_str(), title.role), ("Checkout", Role::Text));
    assert_eq!(title.lines.len(), 1);
    assert!(
        title.rect.y0 >= 0.0 && title.rect.y1 <= shop.rect.y0,
        "the title {:?} sits above everything, inside the bounds that grew for it",
        title.rect
    );

    assert_eq!(sequence.frames.len(), 1);
    let critical = &sequence.frames[0];
    assert_eq!(critical.kind, "critical");
    assert_eq!(critical.title.text, "[Pay]");
    assert_eq!(critical.sections.len(), 1);
    assert_eq!(
        critical.sections[0].label.as_ref().map(|l| l.text.as_str()),
        Some("[Declined]")
    );
    // Mermaid widens a `critical` with options to 9 px left of the leftmost
    // participant it touches, B.
    let b_left = sequence.participants[1].top.x0;
    assert!(
        critical.rect.x0 <= b_left - 9.0 + 1e-6,
        "{} vs {b_left}",
        critical.rect.x0
    );
    assert_eq!(sequence.messages.len(), 3);
    assert!(sequence.messages.iter().all(|m| m.number.is_none()));
}

/// Every line a sequence scene's labels carry, but the numbers and the
/// diagram title, which merman never measures: Mermaid places both at a
/// fixed offset.
fn label_lines(sequence: &SequenceScene) -> Vec<String> {
    let mut labels: Vec<&Label> = sequence.participants.iter().map(|p| &p.label).collect();
    labels.extend(sequence.messages.iter().map(|m| &m.label));
    labels.extend(sequence.notes.iter().map(|n| &n.label));
    labels.extend(sequence.frames.iter().flat_map(|f| {
        std::iter::once(&f.title).chain(f.sections.iter().filter_map(|s| s.label.as_ref()))
    }));
    labels.extend(sequence.boxes.iter().filter_map(|b| b.title.as_ref()));
    labels
        .iter()
        .flat_map(|label| label.lines.iter().map(|line| line.text.clone()))
        .collect()
}

#[test]
fn every_sequence_label_line_is_a_line_merman_sized() {
    for text in [SEQUENCE_SHOWCASE, SEQUENCE_CHECKOUT, WRAPPED] {
        let log: RequestLog = Arc::new(Mutex::new(Vec::new()));
        let scene = layout_uncounted(
            text,
            &stub(),
            &Limits::default(),
            &Cancel::new(),
            Some(Arc::clone(&log)),
        )
        .expect("the text draws");
        // Every text merman asked the measurer to size, as a person reads it.
        let sized: HashSet<String> = log
            .lock()
            .unwrap()
            .iter()
            .filter(|request| {
                matches!(
                    request.operation,
                    TextMeasurementOperation::RawBBoxWidth
                        | TextMeasurementOperation::RawBBoxHeight
                        | TextMeasurementOperation::TspanBBoxWidth
                        | TextMeasurementOperation::TspanBBoxHeight
                        | TextMeasurementOperation::MermaidCalculateTextDimensions
                )
            })
            .flat_map(|request| {
                request
                    .text
                    .split('\n')
                    .map(|line| decode_mermaid_entities_to_unicode(line).into_owned())
                    .collect::<Vec<_>>()
            })
            .collect();
        let lines = label_lines(sequence_of(&scene));
        assert!(lines.len() > 6);
        for line in lines {
            assert!(
                line.is_empty() || sized.contains(&line),
                "{line:?} is not a line merman sized: {sized:#?}"
            );
        }
    }
}

/// merman's own SVG of `text`, measured by the stub through the same adapter,
/// on the crate's engine.
fn merman_svg(text: &str) -> Svg {
    let environment = SvgEnvironment::deterministic()
        .with_text_measurement_policy(HostMeasure::new(stub(), None).policy());
    let output = Renderer::new()
        .with_engine(engine().clone())
        .render(RenderRequest::svg(
            text,
            OperationControl::new(),
            SvgRequest {
                environment,
                ..SvgRequest::default()
            },
        ))
        .expect("merman renders the diagram");
    let RenderOutput::Svg(Some(svg)) = output else {
        panic!("no diagram in the text");
    };
    Svg(svg.svg().to_string())
}

/// merman's SVG read as elements: a tag's text between `<` and `>`, and the
/// text that follows it up to the next tag.
struct Svg(String);

impl Svg {
    fn elements(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.match_indices('<').filter_map(|(start, _)| {
            let rest = &self.0[start..];
            let close = rest.find('>')?;
            let after = &rest[close + 1..];
            let content = &after[..after.find('<').unwrap_or(after.len())];
            Some((&rest[..=close], content))
        })
    }

    /// The first element containing `needle` whose numeric attributes are
    /// within `tolerance` of `attrs`.
    fn find(&self, needle: &str, attrs: &[(&str, f64)], tolerance: f64) -> Option<(&str, &str)> {
        self.elements().find(|(element, _)| {
            element.contains(needle)
                && attrs.iter().all(|(name, value)| {
                    attr(element, name).is_some_and(|actual| (actual - value).abs() <= tolerance)
                })
        })
    }

    fn expect(&self, needle: &str, attrs: &[(&str, f64)]) -> (&str, &str) {
        self.find(needle, attrs, 1e-6)
            .unwrap_or_else(|| panic!("merman's SVG has no element with {needle} at {attrs:?}"))
    }

    /// Within a pixel: Mermaid rounds every row of text to a whole pixel
    /// from the unrounded first row, while a scene stacks its rows at the
    /// exact line height from the rounded first.
    fn expect_near(&self, needle: &str, attrs: &[(&str, f64)]) -> (&str, &str) {
        self.find(needle, attrs, 1.0)
            .unwrap_or_else(|| panic!("merman's SVG has no element with {needle} near {attrs:?}"))
    }
}

/// The value of attribute `name` of `element`.
fn attr_str<'a>(element: &'a str, name: &str) -> Option<&'a str> {
    let key = format!(" {name}=\"");
    let start = element.find(&key)? + key.len();
    let rest = &element[start..];
    Some(&rest[..rest.find('"')?])
}

fn attr(element: &str, name: &str) -> Option<f64> {
    attr_str(element, name)?.parse().ok()
}

/// The numbers in a path's `d` or a polygon's `points`.
fn numbers(text: &str) -> Vec<f64> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect()
}

fn all_near(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| (a - b).abs() <= 1e-6)
}

/// JavaScript's `Math.round`.
fn js_round(value: f64) -> f64 {
    let floor = value.floor();
    floor + if value - floor >= 0.5 { 1.0 } else { 0.0 }
}

/// The middle of a placed line, in the stub's metrics.
fn line_middle(line: &LabelLine) -> f64 {
    let metrics = Stub.measure("x", crate::TEXT_PX);
    line.baseline - f64::from(metrics.ascent) + f64::from(metrics.height) / 2.0
}

/// Text as merman's SVG escapes it in content: the ampersand, the angle
/// bracket that opens a tag, and both quotes.
fn escaped(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// The scene of `text` is merman's SVG of it, element by element: the same
/// lifelines, names, boxes and figures; every message line, loop, marker,
/// label row and number; every note, bar, frame edge, tag, title, divider
/// and divider label; every `box` and the title. Coordinates are compared
/// in merman's space, the scene shifted back by its origin.
fn assert_is_mermans_picture(text: &str) {
    let scene = scene_of(text);
    let sequence = sequence_of(&scene);
    let svg = merman_svg(text);
    let origin = merman_bounds(text).origin().to_vec2();
    let m = |p: Point| p + origin;
    let rect = |r: &Rect| *r + origin;
    // Mermaid's font size: a message or note row is drawn `1em` under its anchor.
    let em = 16.0;

    for (participant, lifeline) in sequence.participants.iter().zip(&sequence.lifelines) {
        let (top, bottom) = (
            m(Point::new(lifeline.x, lifeline.y0)),
            m(Point::new(lifeline.x, lifeline.y1)),
        );
        svg.expect(
            "class=\"actor-line",
            &[
                ("x1", top.x),
                ("y1", top.y),
                ("x2", bottom.x),
                ("y2", bottom.y),
            ],
        );
        let name = m(participant.label.rect.center());
        let top_rect = rect(&participant.top);
        match participant.kind {
            ParticipantKind::Box => {
                svg.expect("class=\"actor actor-box\"", &[("x", name.x), ("y", name.y)]);
                svg.expect(
                    "class=\"actor actor-top\"",
                    &[
                        ("x", top_rect.x0),
                        ("y", top_rect.y0),
                        ("width", top_rect.width()),
                        ("height", top_rect.height()),
                    ],
                );
            }
            ParticipantKind::Actor => {
                svg.expect("class=\"actor actor-man\"", &[("x", name.x), ("y", name.y)]);
                // The head of the figure: 15 px round, 10 px under the top.
                svg.expect(
                    "r=\"15\"",
                    &[("cx", top_rect.center().x), ("cy", top_rect.y0 + 10.0)],
                );
            }
        }
        for line in &participant.label.lines {
            let row = format!(">{}</tspan>", escaped(&line.text));
            assert!(svg.0.contains(&row), "{row} of {}", participant.id);
        }
    }

    for message in &sequence.messages {
        let (start, end) = ends(&message.path);
        let (s, e) = (m(start), m(end));
        let stroke_class = match message.stroke {
            StrokeKind::Dotted => "class=\"messageLine1\"",
            StrokeKind::Normal | StrokeKind::Thick => "class=\"messageLine0\"",
        };
        let (element, _) = if message.from == message.to {
            let [PathEl::MoveTo(_), PathEl::CurveTo(c1, c2, _)] = message.path.elements() else {
                panic!("{}: a self-message is one cubic", message.id);
            };
            let (c1, c2) = (m(*c1), m(*c2));
            let expected = [s.x, s.y, c1.x, c1.y, c2.x, c2.y, e.x, e.y];
            svg.elements()
                .find(|(element, _)| {
                    element.starts_with("<path")
                        && element.contains(stroke_class)
                        && attr_str(element, "d").is_some_and(|d| all_near(&numbers(d), &expected))
                })
                .unwrap_or_else(|| panic!("{}: no self-message path {expected:?}", message.id))
        } else {
            svg.expect(
                stroke_class,
                &[("x1", s.x), ("y1", s.y), ("x2", e.x), ("y2", e.y)],
            )
        };
        for (attribute, marker) in [("marker-start", message.start), ("marker-end", message.end)] {
            let url = attr_str(element, attribute);
            let suffix = match marker {
                Marker::None => None,
                Marker::Arrow => Some("-arrowhead)"),
                Marker::Cross => Some("-crosshead)"),
                Marker::OpenArrow => Some("-filled-head)"),
                Marker::Circle => panic!("a sequence diagram draws no circle"),
            };
            match (url, suffix) {
                (None, None) => {}
                (Some(url), Some(suffix)) => assert!(
                    url.ends_with(suffix),
                    "{}: {attribute} {url} is not {suffix}",
                    message.id
                ),
                (url, suffix) => panic!("{}: {attribute} {url:?} vs {suffix:?}", message.id),
            }
        }
        // The rows of text: the first at the label's anchor, each with its
        // line's text; the rest within Mermaid's per-row rounding.
        let x = m(message.label.rect.center()).x;
        for (index, line) in message.label.lines.iter().enumerate() {
            let anchor_y = m(Point::new(0.0, line_middle(line))).y - em;
            let attrs = [("x", x), ("y", anchor_y)];
            let (_, content) = if index == 0 {
                svg.expect("class=\"messageText\"", &attrs)
            } else {
                svg.expect_near("class=\"messageText\"", &attrs)
            };
            assert_eq!(content, escaped(&line.text), "{}", message.id);
        }
        if let Some(number) = &message.number {
            let c = m(number.rect.center());
            svg.expect(
                "stroke-width=\"0\" marker-start=",
                &[("x1", c.x), ("y1", c.y), ("x2", c.x), ("y2", c.y)],
            );
            let (_, content) =
                svg.expect("class=\"sequenceNumber\"", &[("x", c.x), ("y", c.y + 4.0)]);
            assert_eq!(content, number.text);
        }
    }

    for note in &sequence.notes {
        let r = rect(&note.rect);
        svg.expect(
            "class=\"note\"",
            &[
                ("x", r.x0),
                ("y", r.y0),
                ("width", r.width()),
                ("height", r.height()),
            ],
        );
        let x = m(note.label.rect.center()).x;
        for (index, line) in note.label.lines.iter().enumerate() {
            let anchor_y = m(Point::new(0.0, line_middle(line))).y - em;
            let attrs = [("x", x), ("y", anchor_y)];
            if index == 0 {
                svg.expect("class=\"noteText\"", &attrs);
            } else {
                svg.expect_near("class=\"noteText\"", &attrs);
            }
            let row = format!(">{}</tspan>", escaped(&line.text));
            assert!(svg.0.contains(&row), "{row}");
        }
    }

    for bar in &sequence.activations {
        let r = rect(bar);
        svg.expect(
            "class=\"activation0\"",
            &[
                ("x", r.x0),
                ("y", r.y0),
                ("width", r.width()),
                ("height", r.height()),
            ],
        );
    }

    for frame in &sequence.frames {
        let r = rect(&frame.rect);
        for (x1, y1, x2, y2) in [
            (r.x0, r.y0, r.x1, r.y0),
            (r.x1, r.y0, r.x1, r.y1),
            (r.x0, r.y1, r.x1, r.y1),
            (r.x0, r.y0, r.x0, r.y1),
        ] {
            svg.expect(
                "class=\"loopLine\"/>",
                &[("x1", x1), ("y1", y1), ("x2", x2), ("y2", y2)],
            );
        }
        // The tag: a 50 by 20 box at the top-left corner, named by the kind
        // in its middle.
        let tag = svg
            .elements()
            .find(|(element, _)| {
                element.contains("class=\"labelBox\"")
                    && attr_str(element, "points").is_some_and(|points| {
                        all_near(&numbers(points)[..4], &[r.x0, r.y0, r.x0 + 50.0, r.y0])
                    })
            })
            .map(|(element, _)| numbers(attr_str(element, "points").unwrap()))
            .unwrap_or_else(|| panic!("{}: no tag at {:?}", frame.kind, r.origin()));
        assert!(
            all_near(&tag[8..], &[r.x0, r.y0 + 20.0]),
            "{}: the tag is 20 px tall",
            frame.kind
        );
        let (_, kind) = svg.expect(
            "class=\"labelText\"",
            &[("x", (r.x0 + 25.0).round()), ("y", js_round(r.y0 + 12.5))],
        );
        assert_eq!(kind, frame.kind);
        if let Some(first) = frame.title.lines.first() {
            let x = m(frame.title.rect.center()).x;
            svg.expect(
                "class=\"loopText\"",
                &[("x", x), ("y", m(Point::new(0.0, first.baseline)).y)],
            );
            for line in &frame.title.lines {
                let row = format!(">{}</tspan>", escaped(&line.text));
                assert!(svg.0.contains(&row), "{row} of the {} title", frame.kind);
            }
        }
        for section in &frame.sections {
            let y = m(Point::new(0.0, section.y)).y;
            svg.expect(
                "style=\"stroke-dasharray: 3, 3;\"",
                &[("x1", r.x0), ("y1", y), ("x2", r.x1), ("y2", y)],
            );
            if let Some(label) = &section.label {
                let x = m(label.rect.center()).x;
                for (index, line) in label.lines.iter().enumerate() {
                    let attrs = [("x", x), ("y", m(Point::new(0.0, line.baseline)).y)];
                    let (_, content) = if index == 0 {
                        svg.expect("class=\"sectionTitle\"", &attrs)
                    } else {
                        svg.expect_near("class=\"sectionTitle\"", &attrs)
                    };
                    assert_eq!(content, escaped(&line.text));
                }
            }
        }
    }

    for group in &sequence.boxes {
        let r = rect(&group.rect);
        svg.expect(
            "class=\"rect\"/>",
            &[
                ("x", r.x0),
                ("y", r.y0),
                ("width", r.width()),
                ("height", r.height()),
            ],
        );
        if let Some(title) = &group.title {
            let c = m(title.rect.center());
            svg.expect("class=\"text\"", &[("x", c.x), ("y", c.y)]);
            let row = format!(">{}</tspan>", escaped(&title.text));
            assert!(svg.0.contains(&row), "{row}");
        }
    }

    if let Some(title) = &sequence.title {
        let (_, content) = svg.expect(
            "y=\"-25\"",
            &[
                ("x", m(Point::new(title.rect.x0, 0.0)).x),
                ("y", m(Point::new(0.0, title.lines[0].baseline)).y),
            ],
        );
        assert_eq!(content, escaped(&title.text));
    }
}

#[test]
fn a_sequence_scene_is_mermans_own_picture_of_the_text() {
    for text in [SEQUENCE_SHOWCASE, SEQUENCE_CHECKOUT, SELF_MESSAGE, WRAPPED] {
        assert_is_mermans_picture(text);
    }
}
