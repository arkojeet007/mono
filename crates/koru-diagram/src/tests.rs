// SPDX-License-Identifier: MPL-2.0

//! Smoke tests for the merman dependency: the pinned features compile, the
//! engine detects the two families Koru draws, and the patched `merman-render`
//! reports the sequence geometry the unpatched layout JSON leaves out
//! (`third-party/patches/merman-render.md`), with the same numbers its own
//! SVG renderer draws.

use merman::{
    Engine, MermaidConfig, OperationControl, RenderOutput, RenderRequest, Renderer, SvgRequest,
    diagram_type_family_id,
};
use merman_render::model::SequenceDiagramLayout;
use serde_json::Value;

/// The sequence showcase a later change reuses as a drawing fixture: every
/// kind of frame but `par` and `critical`, three activations, two notes,
/// autonumbering, no `box` and no title.
const SEQUENCE_SHOWCASE: &str = include_str!("../tests/fixtures/sequence-showcase.mmd");

/// What the showcase lacks: a title, a `box` group, and a `critical` frame
/// with an `option`, which Mermaid widens to the left.
const CHECKOUT: &str = "sequenceDiagram
    title Checkout
    box Shop
    participant A
    participant B
    end
    participant C
    A->>B: Hi
    critical Pay
        B->>C: Charge
    option Declined
        C-->>B: No
    end
";

fn detect(engine: &Engine, text: &str) -> (&'static str, &'static str) {
    let mut config = MermaidConfig::default();
    let diagram_type = engine
        .registry()
        .detect_type(text, &mut config)
        .expect("a diagram type is detected");
    let family = diagram_type_family_id(diagram_type)
        .expect("the detected type belongs to a family")
        .as_str();
    (diagram_type, family)
}

#[test]
fn detects_the_two_families_koru_draws() {
    let engine = Engine::new();

    let (diagram_type, family) = detect(&engine, "flowchart LR\n    A --> B");
    assert!(
        diagram_type.starts_with("flowchart"),
        "detected `{diagram_type}`"
    );
    assert_eq!(family, "flowchart");

    let (diagram_type, family) = detect(&engine, "sequenceDiagram\n    A->>B: Hi");
    assert_eq!(diagram_type, "sequence");
    assert_eq!(family, "sequence");

    // The families the features leave out are not detected as drawable kinds
    // Koru knows; detection still names them, which is what `Kind::Other`
    // will carry.
    let (diagram_type, family) = detect(&engine, "pie\n    \"A\" : 1");
    assert_eq!((diagram_type, family), ("pie", "pie"));
}

/// merman's layout JSON: `{ meta, semantic, layout: { SequenceDiagram: … } }`.
fn layout_json(source: &str) -> Value {
    let output = Renderer::new()
        .render(RenderRequest::layout_json(
            source,
            OperationControl::new(),
            SvgRequest::default(),
        ))
        .expect("merman lays the diagram out");
    let RenderOutput::LayoutJson(Some(layout)) = output else {
        panic!("no diagram in the source");
    };
    layout.layout().as_value().clone()
}

/// merman's own SVG of the same source, the reference the patch must agree with.
fn svg(source: &str) -> String {
    let output = Renderer::new()
        .render(RenderRequest::svg(
            source,
            OperationControl::new(),
            SvgRequest::default(),
        ))
        .expect("merman renders the diagram");
    let RenderOutput::Svg(Some(svg)) = output else {
        panic!("no diagram in the source");
    };
    svg.svg().to_string()
}

/// The id of the message whose text is `text`, from the semantic model.
fn message_id(json: &Value, text: &str) -> String {
    json["semantic"]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|message| message["message"].as_str() == Some(text))
        .unwrap_or_else(|| panic!("no message {text:?}"))["id"]
        .as_str()
        .expect("message id")
        .to_string()
}

/// The y of a message line and the x range it spans, from the `msg-<id>` edge.
fn message_line(layout: &Value, id: &str) -> (f64, f64, f64) {
    let edge_id = format!("msg-{id}");
    let edge = layout["edges"]
        .as_array()
        .expect("edges")
        .iter()
        .find(|edge| edge["id"].as_str() == Some(&edge_id))
        .unwrap_or_else(|| panic!("no edge {edge_id}"));
    let points = edge["points"].as_array().expect("points");
    let xs = points.iter().map(|point| point["x"].as_f64().unwrap());
    (
        points[0]["y"].as_f64().unwrap(),
        xs.clone().fold(f64::INFINITY, f64::min),
        xs.fold(f64::NEG_INFINITY, f64::max),
    )
}

fn f(value: &Value) -> f64 {
    value.as_f64().expect("a number")
}

/// A coordinate as merman's SVG writes it (JavaScript number formatting:
/// `64`, `241.5`); the fixtures stay on values this covers.
fn svg_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

fn frame_lines(frame: &Value) -> [String; 4] {
    let (x1, y1) = (f(&frame["x"]), f(&frame["y"]));
    let (x2, y2) = (x1 + f(&frame["width"]), y1 + f(&frame["height"]));
    let [x1, y1, x2, y2] = [x1, y1, x2, y2].map(svg_number);
    [
        format!(r#"<line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y1}" class="loopLine"/>"#),
        format!(r#"<line x1="{x2}" y1="{y1}" x2="{x2}" y2="{y2}" class="loopLine"/>"#),
        format!(r#"<line x1="{x1}" y1="{y2}" x2="{x2}" y2="{y2}" class="loopLine"/>"#),
        format!(r#"<line x1="{x1}" y1="{y1}" x2="{x1}" y2="{y2}" class="loopLine"/>"#),
    ]
}

fn assert_frame_encloses(frame: &Value, (line_y, min_x, max_x): (f64, f64, f64), what: &str) {
    let (x, y) = (f(&frame["x"]), f(&frame["y"]));
    let (right, bottom) = (x + f(&frame["width"]), y + f(&frame["height"]));
    assert!(
        y < line_y && line_y < bottom && x <= min_x && max_x <= right,
        "{what} at y={line_y} x={min_x}..{max_x} is not inside the frame {frame}"
    );
}

#[test]
fn the_patched_layout_carries_frames_and_activations() {
    let json = layout_json(SEQUENCE_SHOWCASE);
    let layout = &json["layout"]["SequenceDiagram"];
    let svg = svg(SEQUENCE_SHOWCASE);

    // The patched struct deserialises its own JSON: the fields exist on the
    // type Koru's scene builder will read, not only in the output.
    let typed: SequenceDiagramLayout =
        serde_json::from_value(layout.clone()).expect("the layout deserialises");
    assert_eq!(typed.frames.len(), 3);
    assert_eq!(typed.activations.len(), 3);
    assert!(typed.boxes.is_empty() && typed.title.is_none());

    // --- frames: loop, alt, opt, in source order, each where the SVG draws it.
    let frames = layout["frames"].as_array().expect("frames");
    let kinds: Vec<&str> = frames
        .iter()
        .map(|frame| frame["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["loop", "alt", "opt"]);
    for frame in frames {
        for line in frame_lines(frame) {
            assert!(svg.contains(&line), "the SVG lacks the frame line {line}");
        }
    }

    let (loop_frame, alt_frame, opt_frame) = (&frames[0], &frames[1], &frames[2]);
    assert_eq!(loop_frame["label"], "Until the person confirms or cancels");
    assert!(
        loop_frame.get("sections").is_none(),
        "a loop has no dividers"
    );
    for text in ["Ask for passkey", "Touch sensor"] {
        let line = message_line(layout, &message_id(&json, text));
        assert_frame_encloses(loop_frame, line, text);
    }
    let outside = message_line(layout, &message_id(&json, "Challenge"));
    assert!(
        outside.0 < f(&loop_frame["y"]),
        "the loop starts after its predecessor"
    );

    assert_eq!(alt_frame["label"], "Signature valid");
    let sections = alt_frame["sections"].as_array().expect("alt sections");
    assert_eq!(sections.len(), 1, "one `else`, one divider");
    assert_eq!(sections[0]["label"], "Signature rejected");
    assert_eq!(sections[0]["id"], message_id(&json, "Signature rejected"));
    let divider_y = f(&sections[0]["y"]);
    let (last_valid, _, _) = message_line(layout, &message_id(&json, "Session token"));
    let (first_rejected, _, _) = message_line(layout, &message_id(&json, "Rejected"));
    assert!(
        last_valid < divider_y && divider_y < first_rejected,
        "divider {divider_y} between {last_valid} and {first_rejected}"
    );
    let dashed = format!(
        r#"<line x1="{x1}" y1="{y}" x2="{x2}" y2="{y}" class="loopLine" style="stroke-dasharray: 3, 3;"/>"#,
        x1 = svg_number(f(&alt_frame["x"])),
        x2 = svg_number(f(&alt_frame["x"]) + f(&alt_frame["width"])),
        y = svg_number(divider_y),
    );
    assert!(svg.contains(&dashed), "the SVG lacks the divider {dashed}");
    for text in [
        "Signed challenge",
        "Create session",
        "Session id",
        "Session token",
        "Rejected",
    ] {
        assert_frame_encloses(
            alt_frame,
            message_line(layout, &message_id(&json, text)),
            text,
        );
    }

    assert_eq!(opt_frame["label"], "Remember this device");
    let register = message_line(layout, &message_id(&json, "Register device"));
    assert_frame_encloses(opt_frame, register, "Register device");

    // --- activations: App once, Server twice, each where the SVG draws it.
    let activations = layout["activations"].as_array().expect("activations");
    let actors: Vec<&str> = activations
        .iter()
        .map(|bar| bar["actor"].as_str().unwrap())
        .collect();
    assert_eq!(actors, ["App", "Server", "Server"]);
    for bar in activations {
        assert_eq!(bar["depth"], 0, "no bar nests in the showcase");
        let rect = format!(
            r##"<rect x="{x}" y="{y}" fill="#EDF2AE" stroke="#666" width="{w}" height="{h}" class="activation0""##,
            x = svg_number(f(&bar["x"])),
            y = svg_number(f(&bar["y"])),
            w = svg_number(f(&bar["width"])),
            h = svg_number(f(&bar["height"])),
        );
        assert!(svg.contains(&rect), "the SVG lacks the activation {rect}");
    }
    // App is active from "Sign in" to "Signed in".
    let (sign_in, _, _) = message_line(layout, &message_id(&json, "Sign in"));
    let (signed_in, _, _) = message_line(layout, &message_id(&json, "Signed in"));
    let app = &activations[0];
    assert_eq!(f(&app["y"]), sign_in);
    assert_eq!(f(&app["y"]) + f(&app["height"]), signed_in);

    // --- nothing the showcase lacks is invented.
    assert!(layout.get("boxes").is_none() && layout.get("title").is_none());
}

#[test]
fn the_patched_layout_carries_boxes_and_the_title() {
    let json = layout_json(CHECKOUT);
    let layout = &json["layout"]["SequenceDiagram"];
    let svg = svg(CHECKOUT);

    let typed: SequenceDiagramLayout =
        serde_json::from_value(layout.clone()).expect("the layout deserialises");
    assert_eq!(typed.boxes.len(), 1);
    assert_eq!(
        typed.title.as_ref().map(|title| title.text.as_str()),
        Some("Checkout")
    );

    let boxes = layout["boxes"].as_array().expect("boxes");
    let shop = &boxes[0];
    assert_eq!(shop["fill"], "transparent");
    assert_eq!(shop["title"]["text"], "Shop");
    let rect = format!(
        r#"<rect x="{x}" y="{y}" fill="transparent" stroke="rgb(0,0,0, 0.5)" width="{w}" height="{h}" class="rect"/>"#,
        x = svg_number(f(&shop["x"])),
        y = svg_number(f(&shop["y"])),
        w = svg_number(f(&shop["width"])),
        h = svg_number(f(&shop["height"])),
    );
    assert!(svg.contains(&rect), "the SVG lacks the box {rect}");
    // The box encloses A and B and leaves C out.
    let actor_x = |id: &str| {
        let node_id = format!("actor-top-{id}");
        f(&layout["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"].as_str() == Some(&node_id))
            .unwrap()["x"])
    };
    let (left, right) = (f(&shop["x"]), f(&shop["x"]) + f(&shop["width"]));
    assert!(left < actor_x("A") && actor_x("B") < right && right < actor_x("C"));

    let title = &layout["title"];
    assert_eq!(title["text"], "Checkout");
    let text = format!(
        r#"<text x="{x}" y="{y}">Checkout</text>"#,
        x = svg_number(f(&title["x"])),
        y = svg_number(f(&title["y"])),
    );
    assert!(svg.contains(&text), "the SVG lacks the title {text}");

    // A `critical` with an `option`: one divider, and the frame as drawn.
    let frames = layout["frames"].as_array().expect("frames");
    assert_eq!(frames.len(), 1);
    let critical = &frames[0];
    assert_eq!(critical["kind"], "critical");
    assert_eq!(critical["label"], "Pay");
    assert_eq!(critical["sections"].as_array().unwrap().len(), 1);
    assert_eq!(critical["sections"][0]["label"], "Declined");
    for line in frame_lines(critical) {
        assert!(svg.contains(&line), "the SVG lacks the frame line {line}");
    }
    assert!(layout.get("activations").is_none());
}
