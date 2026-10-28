// SPDX-License-Identifier: MPL-2.0

//! `layout` on the fixtures: the two showcases against their golden scenes,
//! the flowchart's facts, the directives that change nothing, cancellation
//! and the deadline, the sequence showcase's participants and lifelines (the
//! rest of its scene is in `sequence.rs`), and the statuses that never reach
//! merman.
//!
//! The goldens `tests/fixtures/flowchart-showcase.scene.json` and
//! `tests/fixtures/sequence-showcase.scene.json` are the showcases' scenes as
//! pretty JSON. Regenerate them with
//! `UPDATE_DIAGRAM_GOLDENS=1 cargo test -p koru-diagram`, which is the one
//! disk write in this suite and the only thing that reads that variable.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use kurbo::{BezPath, ParamCurve, ParamCurveNearest, PathEl, Point, Rect, Shape};
use merman::svg::TextMeasurementOperation;
use merman::{OperationControl, RenderOutput, RenderRequest, Renderer, SvgEnvironment, SvgRequest};
use serde_json::Value;

use super::{
    BAD_ARROW_LINE7, EMPTY, FLOWCHART_SHOWCASE, GANTT, SEQUENCE_SHOWCASE, Stub, chain, stub,
};
use crate::engine::engine;
use crate::layout::layout_uncounted;
use crate::measure::{HostMeasure, RequestLog};
use crate::{
    Cancel, Counts, DiagramScene, Fail, FlowchartScene, Kind, Limits, Marker, MeasuredLine,
    NodeShape, ParticipantKind, SceneBody, SequenceScene, Status, StrokeKind, TextMeasure,
    TextMetrics, layout,
};

const FLOWCHART_GOLDEN: &str = include_str!("../../tests/fixtures/flowchart-showcase.scene.json");
const FLOWCHART_GOLDEN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/flowchart-showcase.scene.json"
);
const SEQUENCE_GOLDEN: &str = include_str!("../../tests/fixtures/sequence-showcase.scene.json");
const SEQUENCE_GOLDEN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/sequence-showcase.scene.json"
);

fn limits() -> Limits {
    Limits::default()
}

/// The stub's line height at `TEXT_PX`, as the f64 the scene carries.
fn line_height() -> f64 {
    f64::from(Stub.measure("x", crate::TEXT_PX).height)
}

fn showcase() -> DiagramScene {
    layout(FLOWCHART_SHOWCASE, &stub(), &limits(), &Cancel::new()).expect("the showcase draws")
}

fn flowchart(scene: &DiagramScene) -> &FlowchartScene {
    match &scene.body {
        SceneBody::Flowchart(flowchart) => flowchart,
        SceneBody::Sequence(_) | SceneBody::Pie(_) => panic!("a flowchart scene"),
    }
}

pub(super) fn sequence_showcase() -> DiagramScene {
    layout(SEQUENCE_SHOWCASE, &stub(), &limits(), &Cancel::new())
        .expect("the sequence showcase draws")
}

pub(super) fn sequence_of(scene: &DiagramScene) -> &SequenceScene {
    match &scene.body {
        SceneBody::Sequence(sequence) => sequence,
        SceneBody::Flowchart(_) | SceneBody::Pie(_) => panic!("a sequence scene"),
    }
}

fn pretty(scene: &DiagramScene) -> String {
    serde_json::to_string_pretty(scene).expect("a scene serialises")
}

/// The first path where two JSON values differ beyond `tolerance` on a
/// number, or exactly on anything else.
fn first_difference(
    expected: &Value,
    actual: &Value,
    path: &str,
    tolerance: f64,
) -> Option<String> {
    match (expected, actual) {
        (Value::Number(a), Value::Number(b)) => {
            let (a, b) = (a.as_f64()?, b.as_f64()?);
            ((a - b).abs() > tolerance).then(|| format!("{path}: {a} vs {b}"))
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                return Some(format!("{path}: {} vs {} items", a.len(), b.len()));
            }
            a.iter()
                .zip(b)
                .enumerate()
                .find_map(|(i, (a, b))| first_difference(a, b, &format!("{path}[{i}]"), tolerance))
        }
        (Value::Object(a), Value::Object(b)) => {
            let keys: Vec<&String> = a.keys().chain(b.keys()).collect();
            keys.into_iter()
                .find_map(|key| match (a.get(key), b.get(key)) {
                    (Some(a), Some(b)) => {
                        first_difference(a, b, &format!("{path}.{key}"), tolerance)
                    }
                    _ => Some(format!("{path}.{key}: present on one side only")),
                })
        }
        (a, b) => (a != b).then(|| format!("{path}: {a} vs {b}")),
    }
}

/// `scene` against the golden at `path`, whose text is `golden`; rewrites
/// it instead under `UPDATE_DIAGRAM_GOLDENS`.
fn assert_matches_golden(scene: &DiagramScene, golden: &str, path: &str) {
    let actual = pretty(scene);
    if std::env::var_os("UPDATE_DIAGRAM_GOLDENS").is_some() {
        std::fs::write(path, format!("{actual}\n")).expect("the golden is written");
        return;
    }
    if actual.trim_end() == golden.trim_end() {
        return;
    }
    // Not byte-identical: a number may differ in its last places between
    // machines, which is not a change of scene.
    let expected: Value = serde_json::from_str(golden).expect("the golden is JSON");
    let actual: Value = serde_json::from_str(&actual).expect("the scene is JSON");
    if let Some(difference) = first_difference(&expected, &actual, "scene", 1e-6) {
        panic!(
            "the showcase scene differs from its golden at {difference}; \
             if the change is intended, UPDATE_DIAGRAM_GOLDENS=1 cargo test -p koru-diagram"
        );
    }
}

#[test]
fn the_flowchart_showcase_scene_matches_its_golden() {
    assert_matches_golden(&showcase(), FLOWCHART_GOLDEN, FLOWCHART_GOLDEN_PATH);
}

#[test]
fn the_sequence_showcase_scene_matches_its_golden() {
    assert_matches_golden(&sequence_showcase(), SEQUENCE_GOLDEN, SEQUENCE_GOLDEN_PATH);
}

#[test]
fn the_flowchart_showcase_has_its_boxes_arrows_and_groups() {
    let scene = showcase();
    assert_eq!(scene.kind, Kind::Flowchart);
    assert_eq!(
        scene.counts,
        Counts::Flowchart {
            nodes: 10,
            edges: 12,
            groups: 2,
        }
    );
    assert_eq!(scene.acc_title.as_deref(), Some("Checkout flow"));
    assert!(
        scene
            .acc_descr
            .as_deref()
            .unwrap()
            .starts_with("How an order moves")
    );
    assert_eq!(scene.bounds.origin(), Point::ZERO);
    assert!(scene.bounds.width() > 1000.0 && scene.bounds.height() > 400.0);

    let flowchart = flowchart(&scene);
    assert_eq!(flowchart.nodes.len(), 10);
    assert_eq!(flowchart.edges.len(), 12);
    assert_eq!(flowchart.clusters.len(), 2);

    let shapes: Vec<(&str, NodeShape)> = flowchart
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.shape))
        .collect();
    assert_eq!(
        shapes,
        [
            ("Cart", NodeShape::Stadium),
            ("Validate", NodeShape::Diamond),
            ("Fix", NodeShape::LeanRight),
            ("Pay", NodeShape::Subroutine),
            ("Gateway", NodeShape::Cylinder),
            ("Retry", NodeShape::Circle),
            ("Alt", NodeShape::Hexagon),
            ("Done", NodeShape::Asymmetric),
            ("Notify", NodeShape::Rect),
            ("Audit", NodeShape::Cylinder),
        ]
    );
    for node in &flowchart.nodes {
        assert!(
            !node.outline.elements().is_empty(),
            "{} has an outline",
            node.id
        );
        assert!(
            node.rect.contains(node.label.rect.center()),
            "{}'s label sits in its box",
            node.id
        );
        assert!(!node.label.lines.is_empty(), "{} has a label", node.id);
    }
    // The label stays at the size Koru paints with, whatever merman asked.
    let cart = &flowchart.nodes[0];
    assert_eq!(cart.label.text, "Cart");
    assert_eq!(cart.label.lines[0].width, 28.0, "four characters at 7 px");
    assert_eq!(cart.label.rect.height(), line_height());

    let edge = |id: &str| {
        flowchart
            .edges
            .iter()
            .find(|e| e.id == id)
            .unwrap_or_else(|| panic!("edge {id}"))
    };
    assert_eq!(edge("L_Fix_Cart_0").stroke, StrokeKind::Dotted);
    assert_eq!(edge("L_Pay_Gateway_0").stroke, StrokeKind::Thick);
    assert_eq!(edge("L_Cart_Validate_0").stroke, StrokeKind::Normal);
    let both = edge("L_Done_Notify_0");
    assert_eq!((both.start, both.end), (Marker::Arrow, Marker::Arrow));
    let one = edge("L_Cart_Validate_0");
    assert_eq!((one.start, one.end), (Marker::None, Marker::Arrow));
    assert_eq!(
        edge("L_Validate_Fix_0")
            .label
            .as_ref()
            .map(|l| l.text.as_str()),
        Some("no")
    );
    assert!(edge("L_Fix_Cart_0").label.is_none());
    // The arrow from the group leaves the group, not the box inside it.
    let from_group = edge("L_Payments_Audit_0");
    assert_eq!(
        (from_group.from.as_str(), from_group.to.as_str()),
        ("Payments", "Audit")
    );

    let depths: Vec<(&str, usize)> = flowchart
        .clusters
        .iter()
        .map(|c| (c.id.as_str(), c.depth))
        .collect();
    assert_eq!(
        depths,
        [("Payments", 0), ("Fallback", 1)],
        "outermost first"
    );
    assert_eq!(
        flowchart.clusters[1]
            .title
            .as_ref()
            .map(|t| t.text.as_str()),
        Some("Fallback provider")
    );
    let payments = &flowchart.clusters[0];
    let fallback = &flowchart.clusters[1];
    assert!(payments.rect.contains(fallback.rect.center()));
    let title = payments.title.as_ref().unwrap();
    assert!(title.rect.y0 >= payments.rect.y0 && title.rect.y1 < payments.rect.y0 + 40.0);
}

/// The first and last points of a path.
pub(super) fn ends(path: &BezPath) -> (Point, Point) {
    let start = match path.elements().first() {
        Some(PathEl::MoveTo(p)) => *p,
        other => panic!("a path starts with a move, not {other:?}"),
    };
    let end = path.segments().last().expect("a segment").end();
    (start, end)
}

/// How far `point` is from the nearest point of `path`.
fn distance_to(path: &BezPath, point: Point) -> f64 {
    path.segments()
        .map(|segment| segment.nearest(point, 1e-9).distance_sq.sqrt())
        .fold(f64::INFINITY, f64::min)
}

#[test]
fn every_arrow_starts_and_ends_on_the_border_it_touches() {
    let scene = showcase();
    let flowchart = flowchart(&scene);
    let boundary = |id: &str| -> BezPath {
        if let Some(node) = flowchart.nodes.iter().find(|n| n.id == id) {
            // The silhouette is the first subpath; the inner strokes follow a
            // second move.
            let mut silhouette = BezPath::new();
            for element in node.outline.elements() {
                if matches!(element, PathEl::MoveTo(_)) && !silhouette.elements().is_empty() {
                    break;
                }
                silhouette.push(*element);
            }
            if silhouette.elements().is_empty() {
                node.rect.to_path(0.0)
            } else {
                silhouette
            }
        } else if let Some(cluster) = flowchart.clusters.iter().find(|c| c.id == id) {
            cluster.rect.to_path(0.0)
        } else {
            panic!("{id} is neither a node nor a group")
        }
    };
    for edge in &flowchart.edges {
        let (start, end) = ends(&edge.path);
        let from = distance_to(&boundary(&edge.from), start);
        let to = distance_to(&boundary(&edge.to), end);
        assert!(
            from <= 0.5,
            "{}: starts {from} px from {}",
            edge.id,
            edge.from
        );
        assert!(to <= 0.5, "{}: ends {to} px from {}", edge.id, edge.to);
    }
}

#[test]
fn the_curve_is_a_basis_spline_through_the_route() {
    // Two points: a straight line. More: a move, a short line, cubics, a
    // short line, ending exactly on the last point.
    let two = crate::edges::basis_spline(&[Point::new(0.0, 0.0), Point::new(10.0, 0.0)]);
    assert_eq!(
        two.elements(),
        [
            PathEl::MoveTo(Point::ZERO),
            PathEl::LineTo(Point::new(10.0, 0.0))
        ]
    );
    let route = [
        Point::new(0.0, 0.0),
        Point::new(60.0, 0.0),
        Point::new(60.0, 60.0),
        Point::new(120.0, 60.0),
    ];
    let path = crate::edges::basis_spline(&route);
    let (start, end) = ends(&path);
    assert_eq!((start, end), (route[0], route[3]));
    assert_eq!(path.elements()[1], PathEl::LineTo(Point::new(10.0, 0.0)));
    assert_eq!(
        path.elements()[2],
        PathEl::CurveTo(
            Point::new(20.0, 0.0),
            Point::new(40.0, 0.0),
            Point::new(50.0, 10.0)
        )
    );
    assert_eq!(path.segments().count(), 5);
    // The curve bends through the corner instead of touching it.
    let corner = Point::new(60.0, 0.0);
    assert!(distance_to(&path, corner) > 5.0);
}

const DIRECTIVES: [&str; 5] = [
    "%%{init: {\"theme\": \"forest\"}}%%\n",
    "    classDef warn fill:#f96,stroke:#333\n",
    "    class Retry warn\n",
    "    style Done stroke-width:4px\n",
    "    click Done \"https://example.com\" \"ignored by Koru\"\n",
];

#[test]
fn the_directive_lines_leave_the_scene_byte_identical() {
    let mut plain = FLOWCHART_SHOWCASE.to_owned();
    for directive in DIRECTIVES {
        assert!(
            plain.contains(directive),
            "{directive:?} is in the showcase"
        );
        plain = plain.replace(directive, "");
    }
    let with = pretty(&showcase());
    let without = pretty(
        &layout(&plain, &stub(), &limits(), &Cancel::new()).expect("the plain showcase draws"),
    );
    assert_eq!(with, without);
}

#[test]
fn the_statuses_that_do_not_draw_never_reach_a_layout() {
    let cases = [
        (EMPTY, Status::Empty),
        (
            GANTT,
            Status::KindNotDrawn {
                kind: Some("gantt".to_owned()),
            },
        ),
        ("hello world", Status::KindNotDrawn { kind: None }),
    ];
    for (text, status) in cases {
        assert_eq!(
            layout(text, &stub(), &limits(), &Cancel::new()),
            Err(Fail::Status(status)),
            "{text:?}"
        );
    }
    match layout(BAD_ARROW_LINE7, &stub(), &limits(), &Cancel::new()) {
        Err(Fail::Status(Status::NotParsed { line: 7, .. })) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(
        layout(&chain(1000), &stub(), &limits(), &Cancel::new()),
        Err(Fail::Status(Status::TooLarge {
            kind: Kind::Flowchart,
            count: 1000,
        }))
    );
}

fn unlimited(deadline: Duration) -> Limits {
    Limits {
        max_nodes: usize::MAX,
        max_messages: usize::MAX,
        deadline,
    }
}

/// A measurer that reports its first call and then waits to be let go, so a
/// test can act in the middle of a layout.
struct Gate {
    started: Mutex<Option<mpsc::Sender<()>>>,
    go: Mutex<Option<mpsc::Receiver<()>>>,
}

impl TextMeasure for Gate {
    fn measure(&self, text: &str, px: f32) -> TextMetrics {
        if let Some(started) = self.started.lock().unwrap().take() {
            let _ = started.send(());
            if let Some(go) = self.go.lock().unwrap().take() {
                let _ = go.recv_timeout(Duration::from_secs(10));
            }
        }
        Stub.measure(text, px)
    }

    fn wrap(&self, text: &str, px: f32, max_width: f32) -> Vec<MeasuredLine> {
        Stub.wrap(text, px, max_width)
    }
}

#[test]
fn a_cancel_pulled_from_another_thread_mid_layout_is_cancelled() {
    let (started_tx, started_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel();
    let measure: Arc<dyn TextMeasure> = Arc::new(Gate {
        started: Mutex::new(Some(started_tx)),
        go: Mutex::new(Some(go_rx)),
    });
    let cancel = Cancel::new();
    let handle = cancel.clone();
    let text = chain(1000);
    let worker = thread::spawn(move || {
        layout(
            &text,
            &measure,
            &unlimited(Duration::from_secs(30)),
            &handle,
        )
    });
    started_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the layout reaches its first measurement");
    assert!(!cancel.is_cancelled());
    cancel.cancel();
    assert!(cancel.is_cancelled());
    go_tx.send(()).expect("the measurer is waiting");
    assert_eq!(
        worker.join().expect("the worker returns"),
        Err(Fail::Cancelled)
    );
}

#[test]
fn a_one_millisecond_deadline_times_out() {
    assert_eq!(
        layout(
            &chain(1000),
            &stub(),
            &unlimited(Duration::from_millis(1)),
            &Cancel::new()
        ),
        Err(Fail::TimedOut)
    );
}

#[test]
fn a_cancel_handle_is_reusable_and_its_clones_share_the_job() {
    let cancel = Cancel::new();
    let first = layout(FLOWCHART_SHOWCASE, &stub(), &limits(), &cancel);
    let second = layout(FLOWCHART_SHOWCASE, &stub(), &limits(), &cancel.clone());
    assert!(first.is_ok() && first == second);
    cancel.clone().cancel();
    assert!(cancel.is_cancelled());
    assert_eq!(
        layout(FLOWCHART_SHOWCASE, &stub(), &limits(), &cancel),
        Err(Fail::Cancelled)
    );
}

/// merman's own bounds for `text` under the same measurer, through its
/// facade rather than this crate's pipeline.
pub(super) fn merman_bounds(text: &str) -> Rect {
    let environment = SvgEnvironment::deterministic()
        .with_text_measurement_policy(HostMeasure::new(stub(), None).policy());
    let output = Renderer::new()
        .with_engine(engine().clone())
        .render(RenderRequest::layout_json(
            text,
            OperationControl::new(),
            SvgRequest {
                environment,
                ..SvgRequest::default()
            },
        ))
        .expect("merman lays the diagram out");
    let RenderOutput::LayoutJson(Some(output)) = output else {
        panic!("no diagram");
    };
    let layout = &output.layout().as_value()["layout"];
    let bounds = layout
        .as_object()
        .and_then(|families| families.values().next())
        .map(|family| &family["bounds"])
        .expect("a family layout with bounds");
    Rect::new(
        bounds["min_x"].as_f64().unwrap(),
        bounds["min_y"].as_f64().unwrap(),
        bounds["max_x"].as_f64().unwrap(),
        bounds["max_y"].as_f64().unwrap(),
    )
}

#[test]
fn the_sequence_showcase_lays_out_its_participants_and_lifelines() {
    let scene = sequence_showcase();
    assert_eq!(scene.kind, Kind::Sequence);
    assert_eq!(
        scene.counts,
        Counts::Sequence {
            participants: 4,
            messages: 12,
            notes: 2,
        }
    );
    assert_eq!(scene.acc_title.as_deref(), Some("Sign-in with a passkey"));
    let expected = merman_bounds(SEQUENCE_SHOWCASE);
    assert_eq!(
        scene.bounds,
        Rect::new(0.0, 0.0, expected.width(), expected.height())
    );
    let sequence = sequence_of(&scene);
    assert_eq!(sequence.participants.len(), 4);
    assert_eq!(sequence.lifelines.len(), 4);
    let names: Vec<&str> = sequence
        .participants
        .iter()
        .map(|p| p.label.text.as_str())
        .collect();
    assert_eq!(names, ["Person", "Koru app", "Server", "Session store"]);
    assert_eq!(sequence.participants[0].kind, ParticipantKind::Actor);
    assert_eq!(sequence.participants[1].kind, ParticipantKind::Box);
    for (participant, lifeline) in sequence.participants.iter().zip(&sequence.lifelines) {
        assert_eq!(participant.top.center().x, lifeline.x);
        assert!(participant.top.y1 <= lifeline.y0 && lifeline.y1 <= participant.bottom.y0);
        let name = participant.label.rect.center();
        assert!(
            (name.x - lifeline.x).abs() < 1e-6,
            "{} is named on its lifeline",
            participant.id
        );
        match participant.kind {
            // A box is named in its centre.
            ParticipantKind::Box => assert!(participant.top.contains(name)),
            // An actor is named under its 65 px figure, in the gap before the
            // lifeline starts, as Mermaid draws it: `height / 2 + 35` below
            // the figure's top.
            ParticipantKind::Actor => {
                assert!((name.y - (participant.top.y0 + 65.0 / 2.0 + 35.0)).abs() < 1e-6);
                assert!(participant.top.y1 < name.y && participant.label.rect.y0 < lifeline.y0);
            }
        }
    }

    // The flowchart's bounds are merman's too.
    let flow = showcase();
    let expected = merman_bounds(FLOWCHART_SHOWCASE);
    assert_eq!(
        flow.bounds,
        Rect::new(0.0, 0.0, expected.width(), expected.height())
    );
}

/// The lines merman asked the measurer to size, from its `wrapped` requests.
fn wrapped_requests(text: &str) -> Vec<String> {
    let log: RequestLog = Arc::new(Mutex::new(Vec::new()));
    layout_uncounted(
        text,
        &stub(),
        &limits(),
        &Cancel::new(),
        Some(Arc::clone(&log)),
    )
    .expect("the text draws");
    log.lock()
        .unwrap()
        .iter()
        .filter(|r| r.operation == TextMeasurementOperation::Wrapped)
        .map(|r| r.text.clone())
        .collect()
}

/// Every label line placed is a line merman sized, decoded for display.
fn assert_lines_are_mermans(text: &str) {
    let sized: Vec<String> = wrapped_requests(text)
        .into_iter()
        .map(|sized| {
            crate::flowchart::display_lines(sized.split('\n').map(str::to_owned).collect())
                .join("\n")
        })
        .collect();
    let scene = layout(text, &stub(), &limits(), &Cancel::new()).expect("the text draws");
    let flowchart = flowchart(&scene);
    let mut labels: Vec<&str> = flowchart
        .nodes
        .iter()
        .map(|n| n.label.text.as_str())
        .collect();
    labels.extend(
        flowchart
            .edges
            .iter()
            .filter_map(|e| e.label.as_ref())
            .map(|l| l.text.as_str()),
    );
    labels.extend(
        flowchart
            .clusters
            .iter()
            .filter_map(|c| c.title.as_ref())
            .map(|t| t.text.as_str()),
    );
    assert!(!labels.is_empty());
    for label in labels {
        assert!(
            sized.iter().any(|s| s == label),
            "the placed lines {label:?} are not among the lines merman sized: {sized:#?}"
        );
    }
}

#[test]
fn label_lines_are_the_lines_merman_sized() {
    assert_lines_are_mermans(FLOWCHART_SHOWCASE);
    // Breaks written as `<br>`, entities and entity codes, tags, a word
    // wider than the box, an escaped colon, a group title with a break, an
    // edge label that wraps, runs of spaces.
    let text = concat!(
        "flowchart LR\n",
        "    A[\"first<br>second line\"] --> B[\"a &lt;b&gt; &amp; c &quot;d&quot; #quot;e#quot; <b>bold</b>\"]\n",
        "    B --> C[averyveryverylongwordthatexceedsthewrappingwidthbyfar words after]\n",
        "    C -- \"this is an edge label that is long enough to wrap at two hundred pixels\" --> D[\"colon\\: here #35; &#9829;\"]\n",
        "    subgraph G [\"Group<br/>title &quot;q&quot;\"]\n",
        "        D --> E[\"one  two   three\"]\n",
        "    end\n",
    );
    assert_lines_are_mermans(text);
    let scene = layout(text, &stub(), &limits(), &Cancel::new()).unwrap();
    let flowchart = flowchart(&scene);
    let label = |id: &str| {
        flowchart
            .nodes
            .iter()
            .find(|n| n.id == id)
            .unwrap()
            .label
            .text
            .as_str()
    };
    assert_eq!(label("A"), "first\nsecond line");
    // Broken where merman broke the entity text (`&quot;d&quot;` is a wide
    // word there; a tag is a word of its own), then decoded for display.
    assert_eq!(label("B"), "a <b> & c\n\"d\"\n\"e\" <b>\nbold </b>");
    assert_eq!(
        label("C"),
        "averyveryverylong\nwordthatexceedsth\newrappingwidthbyf\nar\nwords after"
    );
    // Mermaid's own `#35;` code reads as `#`; an authored `&#9829;` gains an
    // ampersand in Mermaid's sanitizer, which is what its SVG shows too.
    assert_eq!(label("D"), "colon: here #\n&♥");
    assert_eq!(label("E"), "one two three");
    assert_eq!(
        flowchart.clusters[0].title.as_ref().unwrap().text,
        "Group\ntitle \"q\""
    );
}

#[test]
fn a_showcase_layout_is_fast() {
    let measure = stub();
    let _ = showcase();
    for (name, text) in [
        ("flowchart", FLOWCHART_SHOWCASE),
        ("sequence", SEQUENCE_SHOWCASE),
    ] {
        let best = (0..5)
            .map(|_| {
                let started = std::time::Instant::now();
                layout(text, &measure, &limits(), &Cancel::new()).expect("draws");
                started.elapsed()
            })
            .min()
            .unwrap();
        eprintln!("layout({name} showcase), best of five: {best:?}");
        assert!(best < Duration::from_millis(200), "{name}: {best:?}");
    }
}
