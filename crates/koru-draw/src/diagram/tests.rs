// SPDX-License-Identifier: MPL-2.0

use std::sync::Arc;

use koru_diagram::{
    Cancel, Counts, DiagramScene, Frame, FrameSection, Kind, Label, LabelLine, Lifeline, Limits,
    Marker, Message, Note, Participant, ParticipantKind, Role, SceneBody, SequenceScene,
    StrokeKind, TEXT_PX, TextMeasure,
};
use kurbo::{Affine, BezPath, Point, Rect};
use quick_xml::events::Event;

use super::*;
use crate::theme::{DARK, LIGHT};

const FLOWCHART: &str = include_str!("../../../koru-diagram/tests/fixtures/flowchart-showcase.mmd");

/// The flowchart showcase laid out with the real measurer. The fixture is
/// part of the proof: this is the text the acceptance checks paste.
fn flowchart() -> Arc<DiagramScene> {
    let measure: Arc<dyn TextMeasure> = Arc::new(InterMeasure);
    Arc::new(
        koru_diagram::layout(FLOWCHART, &measure, &Limits::default(), &Cancel::new())
            .expect("the showcase lays out"),
    )
}

fn every_label_line(scene: &DiagramScene) -> usize {
    let SceneBody::Flowchart(flowchart) = &scene.body else {
        panic!("a flowchart");
    };
    let lines = |label: &Label| label.lines.len();
    flowchart
        .nodes
        .iter()
        .map(|n| lines(&n.label))
        .sum::<usize>()
        + flowchart
            .edges
            .iter()
            .filter_map(|e| e.label.as_ref())
            .map(lines)
            .sum::<usize>()
        + flowchart
            .clusters
            .iter()
            .filter_map(|c| c.title.as_ref())
            .map(lines)
            .sum::<usize>()
}

/// The same text, the same numbers — on this thread twice and on two others.
/// Everything downstream (the host's cache keyed by text, a PNG that matches
/// the screen) assumes it.
#[test]
fn the_measurer_gives_the_same_numbers_on_every_call_and_every_thread() {
    let texts = [
        "Payment gateway",
        "Email & push — café ☕",
        "a",
        "",
        "Valid order?",
    ];
    let here: Vec<_> = texts
        .iter()
        .map(|t| InterMeasure.measure(t, TEXT_PX))
        .collect();
    let again: Vec<_> = texts
        .iter()
        .map(|t| InterMeasure.measure(t, TEXT_PX))
        .collect();
    assert_eq!(here, again, "two calls on one thread");
    let measure: Arc<dyn TextMeasure> = Arc::new(InterMeasure);
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let measure = Arc::clone(&measure);
            std::thread::spawn(move || {
                texts
                    .iter()
                    .map(|t| measure.measure(t, TEXT_PX))
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    for thread in threads {
        assert_eq!(thread.join().expect("a measuring thread"), here);
    }
    assert!(here[0].width > 50.0, "a real width: {:?}", here[0]);
    assert!(here[0].height > here[0].ascent && here[0].ascent > 0.0);
    assert_eq!(here[3].width, 0.0, "an empty line has no width");
    assert_eq!(here[3].height, here[2].height, "but the line's height");
}

/// The measurer's width is parley's advance for the same stack and size: the
/// numbers a diagram is laid out from are the numbers the glyphs are drawn
/// with, and the size it is asked at is the size the painter shapes at.
#[test]
fn the_measurer_width_is_the_advance_of_the_same_stack_and_size() {
    let mut engine = crate::text::TextEngine::new();
    let advance = engine.measure_advance("Payment gateway", STACK, TEXT_PX, WEIGHT);
    let measured = InterMeasure.measure("Payment gateway", TEXT_PX).width;
    assert!(
        (advance - measured).abs() < 1.0,
        "measurer {measured} vs advance {advance}"
    );
    assert_eq!(super::measure::px(), TEXT_PX);
}

/// Wrapped lines never exceed the width; a break lands on a word boundary
/// when one fits, and inside a word only when that word alone is too wide.
#[test]
fn wrapping_breaks_on_words_and_never_exceeds_the_width() {
    let sentence = "The quick brown fox jumps over the lazy dog";
    let max = 90.0;
    let lines = InterMeasure.wrap(sentence, TEXT_PX, max);
    assert!(lines.len() > 1, "{lines:?}");
    for line in &lines {
        assert!(line.width <= max + 0.01, "{line:?} is wider than {max}");
        assert!(
            sentence.contains(&line.text) && !line.text.starts_with(' '),
            "{line:?} is a run of whole words"
        );
    }
    let rejoined: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(rejoined.join(" "), sentence, "every word, once, in order");

    let two_words = InterMeasure.measure("Payment gateway", TEXT_PX).width;
    let first = InterMeasure.measure("Payment", TEXT_PX).width;
    let lines = InterMeasure.wrap("Payment gateway", TEXT_PX, (two_words + first) / 2.0);
    let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(texts, ["Payment", "gateway"], "broken at the space");

    let word = "Supercalifragilisticexpialidocious";
    let lines = InterMeasure.wrap(word, TEXT_PX, 40.0);
    assert!(
        lines.len() > 1,
        "an over-long word is broken inside: {lines:?}"
    );
    for line in &lines {
        assert!(line.width <= 40.01, "{line:?}");
    }
    assert_eq!(
        lines.iter().map(|l| l.text.as_str()).collect::<String>(),
        word
    );
}

/// Every placed line is shaped exactly once; the painter never shapes.
#[test]
fn preparing_shapes_each_label_line_once() {
    let scene = flowchart();
    let prepared = PreparedDiagram::new(Arc::clone(&scene));
    let expected = every_label_line(&scene);
    assert!(expected >= 10, "the showcase has ten boxes: {expected}");
    assert_eq!(prepared.shaped_lines(), expected);
    assert_eq!(prepared.natural_size(), scene.bounds.size());
    assert!(Arc::ptr_eq(prepared.scene(), &scene));
}

/// The SVG is well-formed XML, carries no text and no font, and its `<path`
/// count is exactly the shapes plus the visible glyphs the writer counted —
/// asserted against the writer's own tally so the two cannot drift.
#[test]
fn the_flowchart_svg_is_well_formed_and_made_only_of_paths() {
    let prepared = PreparedDiagram::new(flowchart());
    let (document, counts) = svg_counted(&prepared, &LIGHT);
    assert_eq!(
        document,
        svg(&prepared, &LIGHT),
        "the public writer is the counted one"
    );
    for forbidden in ["<text", "font-family", "@font-face", "<image"] {
        assert!(!document.contains(forbidden), "{forbidden} in the file");
    }
    assert!(document.starts_with(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 "#));
    assert!(document.contains(r##"<rect width="##), "a background rect");
    assert!(
        document.contains(r#"stroke-dasharray="3 3""#),
        "the two dotted arrows"
    );
    assert!(document.contains(r#"stroke-width="3""#), "the thick arrow");

    let mut reader = quick_xml::Reader::from_str(&document);
    let mut paths = 0;
    let mut depth = 0_usize;
    let mut saw_svg_end = false;
    loop {
        match reader.read_event().expect("well-formed XML") {
            Event::Start(start) => {
                depth += 1;
                assert_eq!(start.name().as_ref(), b"svg", "only the root has children");
            }
            Event::End(end) => {
                depth -= 1;
                saw_svg_end |= end.name().as_ref() == b"svg";
            }
            Event::Empty(empty) => {
                assert_eq!(depth, 1, "every shape is a child of the root");
                if empty.name().as_ref() == b"path" {
                    paths += 1;
                    let d = empty
                        .try_get_attribute("d")
                        .expect("attributes parse")
                        .expect("every path has a d");
                    assert!(!d.value.is_empty());
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    assert!(saw_svg_end && depth == 0, "balanced");
    assert!(counts.shapes > 20 && counts.glyphs > 50, "{counts:?}");
    assert_eq!(paths, counts.shapes + counts.glyphs);
    assert_eq!(
        document.matches("<path").count(),
        paths,
        "the textual count agrees with the parser's"
    );
}

/// The same scene gives the same bytes, and a theme flip changes colors only.
#[test]
fn the_svg_is_stable_and_the_theme_changes_only_its_colors() {
    let prepared = PreparedDiagram::new(flowchart());
    assert_eq!(svg(&prepared, &LIGHT), svg(&prepared, &LIGHT));
    let light = svg(&prepared, &LIGHT);
    let dark = svg(&prepared, &DARK);
    assert_ne!(light, dark);
    let strip = |s: &str| {
        s.split('#')
            .map(|piece| piece.get(6..).unwrap_or(""))
            .collect::<String>()
    };
    assert_eq!(strip(&light), strip(&dark), "geometry identical");
    assert!(light.contains(&format!(r#"fill="{}""#, super::svg::hex(LIGHT.bg))));
    assert!(dark.contains(&format!(r#"fill="{}""#, super::svg::hex(DARK.bg))));
}

/// The painter records one inspector scope with the natural size, the scale
/// and the counts, and its text lines nest under it.
#[test]
fn painting_records_an_inspector_scope() {
    let prepared = PreparedDiagram::new(flowchart());
    let mut scene = vello::Scene::new();
    let root = Affine::scale(2.0);
    let frame = inspect::Frame::begin(1, root, Rect::new(0.0, 0.0, 4000.0, 2000.0));
    paint(
        &prepared,
        &LIGHT,
        root * Affine::scale(0.5),
        0.5,
        &mut scene,
    );
    let snapshot = frame.finish();
    let diagram = snapshot
        .records
        .iter()
        .find(|r| r.name == "Diagram")
        .expect("one Diagram scope");
    let property = |name: &str| {
        diagram
            .properties
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.value.clone())
            .unwrap_or_default()
    };
    assert_eq!(property("scale"), "0.500");
    assert_eq!(property("counts"), "10 nodes, 12 edges, 2 groups");
    assert!(property("natural size").contains('×'));
    assert!(
        snapshot.records.iter().any(|r| r.name == "text line"),
        "the labels are drawn through draw_layout"
    );
}

/// A painted scene has vello content: shapes and glyphs were encoded.
#[test]
fn painting_encodes_something() {
    let prepared = PreparedDiagram::new(flowchart());
    let mut scene = vello::Scene::new();
    paint(&prepared, &DARK, Affine::IDENTITY, 1.0, &mut scene);
    assert!(!scene.encoding().path_tags.is_empty());
    let mut off = vello::Scene::new();
    paint(&prepared, &DARK, Affine::IDENTITY, f64::NAN, &mut off);
    assert_eq!(
        scene.encoding().path_tags.len(),
        off.encoding().path_tags.len(),
        "a scale that is not a number paints as 1"
    );
}

// ---------------------------------------------------------------- sequence

/// A label placed by hand the way the builder places one: lines centered on
/// `center`, measured by the real measurer.
fn label(text: &str, center: Point, role: Role) -> Label {
    let m = InterMeasure.measure(text, TEXT_PX);
    let (w, h) = (f64::from(m.width), f64::from(m.height));
    let rect = Rect::from_center_size(center, (w, h));
    Label {
        text: text.to_owned(),
        lines: vec![LabelLine {
            text: text.to_owned(),
            x: rect.x0,
            baseline: rect.y0 + f64::from(m.ascent),
            width: w,
        }],
        rect,
        role,
    }
}

fn horizontal(x0: f64, x1: f64, y: f64) -> BezPath {
    let mut path = BezPath::new();
    path.move_to((x0, y));
    path.line_to((x1, y));
    path
}

/// A sequence scene with one of everything the painter draws, built by hand
/// because the builder that fills these vectors lands separately.
fn sequence_scene() -> Arc<DiagramScene> {
    let top_a = Rect::new(20.0, 10.0, 120.0, 75.0);
    let top_b = Rect::new(220.0, 10.0, 320.0, 60.0);
    let shift = |r: Rect| r + kurbo::Vec2::new(0.0, 300.0);
    let sequence = SequenceScene {
        participants: vec![
            Participant {
                id: "P".into(),
                kind: ParticipantKind::Actor,
                top: top_a,
                bottom: shift(top_a),
                label: label("Person", Point::new(70.0, 65.0), Role::Text),
            },
            Participant {
                id: "App".into(),
                kind: ParticipantKind::Box,
                top: top_b,
                bottom: shift(top_b),
                label: label("Koru app", top_b.center(), Role::Text),
            },
        ],
        lifelines: vec![
            Lifeline {
                x: 70.0,
                y0: 75.0,
                y1: 310.0,
            },
            Lifeline {
                x: 270.0,
                y0: 60.0,
                y1: 310.0,
            },
        ],
        messages: vec![Message {
            id: "m1".into(),
            from: "P".into(),
            to: "App".into(),
            path: horizontal(70.0, 270.0, 120.0),
            start: Marker::None,
            end: Marker::OpenArrow,
            stroke: StrokeKind::Dotted,
            label: label("Sign in", Point::new(170.0, 108.0), Role::Text),
            number: Some(label("1", Point::new(80.0, 120.0), Role::Text)),
        }],
        notes: vec![Note {
            rect: Rect::new(120.0, 140.0, 220.0, 170.0),
            label: label("Expires", Point::new(170.0, 155.0), Role::Text),
        }],
        activations: vec![Rect::new(265.0, 120.0, 275.0, 200.0)],
        frames: vec![Frame {
            kind: "loop".into(),
            rect: Rect::new(40.0, 180.0, 300.0, 280.0),
            title: label("Until done", Point::new(120.0, 192.0), Role::TextDim),
            sections: vec![FrameSection {
                y: 230.0,
                label: Some(label("else", Point::new(70.0, 242.0), Role::TextDim)),
            }],
        }],
        boxes: vec![],
        title: Some(label("Sign-in", Point::new(170.0, 4.0), Role::Text)),
    };
    Arc::new(DiagramScene {
        kind: Kind::Sequence,
        bounds: Rect::new(0.0, 0.0, 340.0, 390.0),
        counts: Counts::Sequence {
            participants: 2,
            messages: 1,
            notes: 1,
        },
        acc_title: None,
        acc_descr: None,
        body: SceneBody::Sequence(sequence),
    })
}

/// Everything a sequence diagram has paints without panicking, in both
/// themes, and the file dashes the lifelines the way the screen does.
#[test]
fn a_sequence_scene_paints_and_its_svg_dashes_the_lifelines() {
    let prepared = PreparedDiagram::new(sequence_scene());
    // Title, frame title, section label, two participants, note, message,
    // number: eight lines, each once.
    assert_eq!(prepared.shaped_lines(), 8, "every label line once");
    assert_eq!(prepared.frame_tags.len(), 1, "the loop keyword");
    for palette in [&LIGHT, &DARK] {
        let mut scene = vello::Scene::new();
        paint(&prepared, palette, Affine::scale(0.5), 0.5, &mut scene);
        assert!(!scene.encoding().path_tags.is_empty());
    }
    let (document, counts) = svg_counted(&prepared, &LIGHT);
    assert_eq!(
        document.matches(r#"stroke-dasharray="5 5""#).count(),
        2,
        "two lifelines at [5, 5]"
    );
    assert!(
        document.contains(r#"stroke-dasharray="3 3""#),
        "the section divider and the dotted message"
    );
    assert!(
        document.contains(r#"fill-opacity="0.25""#),
        "the activation bar"
    );
    assert!(counts.shapes > 10 && counts.glyphs > 20, "{counts:?}");
    assert_eq!(
        document.matches("<path").count(),
        counts.shapes + counts.glyphs
    );
}

/// A frame's title and its divider's label sit on page-colored patches, so
/// a lifeline does not run through the words — and the patches go down
/// before the activation bars, which they must not notch.
#[test]
fn frame_titles_sit_on_page_patches_under_the_activation_bars() {
    // Dark, where the number's ink is not the page color: in light both are
    // white, and the glyph would count as a patch.
    assert_ne!(DARK.on_accent, DARK.bg);
    let prepared = PreparedDiagram::new(sequence_scene());
    let (document, _) = svg_counted(&prepared, &DARK);
    let patch = format!(r#" fill="{}"/>"#, super::svg::hex(DARK.bg));
    let paths: Vec<&str> = document
        .lines()
        .filter(|line| line.starts_with("<path"))
        .collect();
    let patches: Vec<usize> = (0..paths.len())
        .filter(|&i| paths[i].ends_with(&patch))
        .collect();
    assert_eq!(patches.len(), 2, "the loop's title and its `else` label");
    let bar = paths
        .iter()
        .position(|line| line.contains(r#"fill-opacity="0.25""#))
        .expect("the activation bar");
    assert!(patches.iter().all(|&i| i < bar), "{patches:?} before {bar}");
}

/// One digit wears the circle; a number wider than the circle leaves room
/// for stretches it into a capsule of the same height, and either keeps its
/// size on screen when the column scales the drawing down.
#[test]
fn a_two_digit_number_gets_a_badge_it_fits_in() {
    let one = label("1", Point::new(50.0, 50.0), Role::Accent);
    let badge = super::walk::number_badge(&one, 1.0);
    assert_eq!(
        (badge.width(), badge.height()),
        (NUMBER_R * 2.0, NUMBER_R * 2.0)
    );
    assert_eq!(badge.center(), one.rect.center());

    let twelve = label("12", Point::new(50.0, 50.0), Role::Accent);
    assert!(
        twelve.rect.width() + NUMBER_PAD_X * 2.0 > NUMBER_R * 2.0,
        "the case at hand"
    );
    let badge = super::walk::number_badge(&twelve, 1.0);
    assert_eq!(badge.width(), twelve.rect.width() + NUMBER_PAD_X * 2.0);
    assert_eq!(badge.height(), NUMBER_R * 2.0);
    assert_eq!(badge.radii().top_left, NUMBER_R, "a capsule, not a box");

    let half = super::walk::number_badge(&one, 0.5);
    assert_eq!(
        half.height(),
        NUMBER_R * 4.0,
        "the same 16 px once scaled by 0.5"
    );
}

/// The keyword on a frame's tag has to be legible against the tag: of the
/// three candidates, the one with the most contrast on `panel_border`.
#[test]
fn the_frame_tag_ink_has_contrast_in_both_themes() {
    for palette in [&LIGHT, &DARK] {
        let ink = super::walk::frame_tag_ink(palette);
        let contrast = inspect::contrast(ink, palette.panel_border).expect("opaque");
        assert!(contrast >= 4.5, "{contrast} on the tag box");
    }
}

/// Roles map to the palette as the design says; the two fills share one
/// color and the frame's is the border's.
#[test]
fn roles_map_to_the_palette() {
    let p = &LIGHT;
    assert_eq!(ink(p, Role::Text), p.text);
    assert_eq!(ink(p, Role::TextDim), p.text_dim);
    assert_eq!(ink(p, Role::Line), p.text);
    assert_eq!(ink(p, Role::Fill), p.panel_bg);
    assert_eq!(ink(p, Role::Panel), p.panel_bg);
    assert_eq!(ink(p, Role::Accent), p.accent);
    assert_eq!(ink(p, Role::Frame), p.panel_border);
}
