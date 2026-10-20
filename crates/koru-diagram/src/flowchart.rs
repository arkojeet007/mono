// SPDX-License-Identifier: MPL-2.0

//! The flowchart scene builder: merman's typed layout and semantic model into
//! a [`FlowchartScene`].
//!
//! The layout gives every node's box (centre and size), every edge's route
//! and label box, and every group's box and title box; the semantic model
//! gives the shapes, the markers, the stroke kinds, the group nesting and
//! the accessibility text; the parser's render context gives each label as
//! merman measured it. `classDef`, `style`, `class`, `linkStyle`, `click`
//! and `href` are fields of the same model that are never read here: Koru's
//! theme decides how a diagram looks.
//!
//! **Labels.** merman measures the label Mermaid's sanitizer hands its
//! renderer: `&amp;`, `&quot;`, `&#35;` for `&`, `"`, `#`, a `#quot;` code
//! already turned into its entity, and tags kept as text. Its SVG shows
//! `&lt;`, `&gt;` and `&amp;` decoded and every other entity literally — a
//! Mermaid trait of labels drawn as SVG text. The lines here break where
//! merman's did, measured on that same text, and are then decoded for
//! display, so `#quot;` reads as the quote the person meant and the box
//! merman sized still holds the lines.

use std::collections::HashMap;

use kurbo::{Point, Rect, Shape, Vec2};
use merman::diagrams::flowchart::{
    FlowEdgeMarker, FlowEdgeStroke, FlowEdgeVisibility, FlowchartModel, FlowchartRenderContext,
};
use merman::entities::decode_html_entities_to_unicode;
use merman_render::model::{FlowchartLayout, LayoutCluster, LayoutNode};

use crate::layout::{Fail, SCENE_CODE, internal};
use crate::measure::{TEXT_PX, TextMeasure};
use crate::scene::{
    DiagramScene, FlowchartScene, Label, Marker, Role, SceneBody, SceneCluster, SceneEdge,
    SceneNode, StrokeKind,
};
use crate::{Counts, Kind, edges, shapes};

/// Mermaid's `flowchart.wrappingWidth` when the config has none; merman's
/// pinned config carries 120. Node labels wrap at this width.
pub(crate) const DEFAULT_WRAPPING_WIDTH: f64 = 200.0;
/// Mermaid wraps edge labels at a fixed 200 px, whatever the config says.
const EDGE_LABEL_WRAP_WIDTH: f64 = 200.0;

/// Builds the scene. `context` is the parser's render context for `model`;
/// `wrapping_width` is the effective `flowchart.wrappingWidth`.
pub(crate) fn build(
    model: &FlowchartModel,
    context: &FlowchartRenderContext,
    layout: FlowchartLayout,
    counts: Counts,
    measure: &dyn TextMeasure,
    wrapping_width: f64,
) -> Result<DiagramScene, Fail> {
    let (origin, bounds) = match layout.bounds {
        Some(b) => (
            Vec2::new(b.min_x, b.min_y),
            Rect::new(0.0, 0.0, b.max_x - b.min_x, b.max_y - b.min_y),
        ),
        None => (Vec2::ZERO, Rect::ZERO),
    };
    let shift = |x: f64, y: f64| Point::new(x, y) - origin;
    let layout_nodes: HashMap<&str, &LayoutNode> =
        layout.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let layout_clusters: HashMap<&str, &LayoutCluster> =
        layout.clusters.iter().map(|c| (c.id.as_str(), c)).collect();
    let width = |line: &str| f64::from(measure.measure(line, TEXT_PX).width);

    // --- nodes
    let mut nodes = Vec::new();
    for node in model.nodes.iter().filter(|n| !n.is_subgraph_anchor()) {
        let laid = layout_nodes
            .get(node.id.as_str())
            .ok_or_else(|| internal(SCENE_CODE, format!("node {:?} was not laid out", node.id)))?;
        let shape = shapes::node_shape(node.layout_shape.as_deref().or(node.shape.as_deref()));
        let rect = Rect::from_center_size(shift(laid.x, laid.y), (laid.width, laid.height));
        let text = context
            .node_label_for_render(node)
            .or(node.label.as_deref())
            .unwrap_or("");
        let label = Label::placed(
            display_lines(label_lines(text, Some(wrapping_width), &width)),
            rect.center() + shapes::label_offset(shape, rect),
            measure,
            Role::Text,
        );
        nodes.push(SceneNode {
            id: node.id.clone(),
            shape,
            rect,
            outline: shapes::outline(shape, rect),
            label,
        });
    }
    let node_silhouettes: HashMap<&str, (kurbo::BezPath, Rect)> = nodes
        .iter()
        .map(|n| (n.id.as_str(), (shapes::silhouette(n.shape, n.rect), n.rect)))
        .collect();
    let cluster_rect = |id: &str| {
        layout_clusters
            .get(id)
            .map(|c| Rect::from_center_size(shift(c.x, c.y), (c.width, c.height)))
    };

    // --- edges
    let layout_edges: HashMap<&str, _> = layout.edges.iter().map(|e| (e.id.as_str(), e)).collect();
    let mut scene_edges = Vec::new();
    for edge in &model.edges {
        if edge.visibility == FlowEdgeVisibility::Invisible {
            continue;
        }
        let laid = layout_edges
            .get(edge.id.as_str())
            .ok_or_else(|| internal(SCENE_CODE, format!("edge {:?} was not laid out", edge.id)))?;
        let mut points: Vec<Point> = laid.points.iter().map(|p| shift(p.x, p.y)).collect();
        // An end that touches a group is cut at the group's box, as Mermaid
        // cuts it; an end at a node is moved onto the node's shape.
        let clip = |points: &mut Vec<Point>, cluster: Option<&str>, node: &str, at_start: bool| {
            if let Some(rect) = cluster.and_then(cluster_rect) {
                if at_start {
                    edges::clip_start_to_rect(points, rect);
                } else {
                    edges::clip_end_to_rect(points, rect);
                }
            } else if let Some((silhouette, rect)) = node_silhouettes.get(node) {
                let outline = if silhouette.elements().is_empty() {
                    rect.to_path(0.0)
                } else {
                    silhouette.clone()
                };
                if at_start {
                    edges::clip_start_to_outline(points, &outline, rect.center());
                } else {
                    edges::clip_end_to_outline(points, &outline, rect.center());
                }
            } else if let Some(rect) = cluster_rect(node) {
                if at_start {
                    edges::clip_start_to_rect(points, rect);
                } else {
                    edges::clip_end_to_rect(points, rect);
                }
            }
        };
        clip(&mut points, laid.from_cluster.as_deref(), &laid.from, true);
        clip(&mut points, laid.to_cluster.as_deref(), &laid.to, false);
        let text = context
            .edge_label_for_render(edge)
            .or(edge.label.as_deref());
        let label = match (text, &laid.label) {
            (Some(text), Some(laid_label)) if !text.is_empty() => Some(Label::placed(
                display_lines(label_lines(text, Some(EDGE_LABEL_WRAP_WIDTH), &width)),
                shift(laid_label.x, laid_label.y),
                measure,
                Role::Text,
            )),
            _ => None,
        };
        scene_edges.push(SceneEdge {
            id: edge.id.clone(),
            from: edge.from.clone(),
            to: edge.to.clone(),
            path: edges::basis_spline(&points),
            start: marker(edge.start_marker),
            end: marker(edge.end_marker),
            stroke: match edge.stroke_kind {
                FlowEdgeStroke::Normal => StrokeKind::Normal,
                FlowEdgeStroke::Dotted => StrokeKind::Dotted,
                FlowEdgeStroke::Thick => StrokeKind::Thick,
            },
            label,
        });
    }

    // --- clusters, outermost first
    let parent: HashMap<&str, &str> = model
        .subgraphs
        .iter()
        .flat_map(|sg| {
            sg.nodes
                .iter()
                .filter(|child| model.subgraphs.iter().any(|s| &s.id == *child))
                .map(move |child| (child.as_str(), sg.id.as_str()))
        })
        .collect();
    let depth = |id: &str| {
        let mut depth = 0;
        let mut current = id;
        while let Some(&up) = parent.get(current) {
            depth += 1;
            current = up;
            if depth > model.subgraphs.len() {
                break; // a cycle merman would not have laid out; never loops
            }
        }
        depth
    };
    let mut clusters: Vec<SceneCluster> = layout
        .clusters
        .iter()
        .map(|cluster| {
            let text = model
                .subgraphs
                .iter()
                .enumerate()
                .find(|(_, sg)| sg.id == cluster.id)
                .map_or(cluster.title.as_str(), |(ordinal, sg)| {
                    context.subgraph_title_for_render(ordinal, sg)
                });
            let title = (!text.is_empty()).then(|| {
                Label::placed(
                    display_lines(label_lines(text, None, &width)),
                    shift(cluster.title_label.x, cluster.title_label.y),
                    measure,
                    Role::TextDim,
                )
            });
            SceneCluster {
                id: cluster.id.clone(),
                rect: Rect::from_center_size(
                    shift(cluster.x, cluster.y),
                    (cluster.width, cluster.height),
                ),
                title,
                depth: depth(&cluster.id),
            }
        })
        .collect();
    clusters.sort_by_key(|c| c.depth);

    Ok(DiagramScene {
        kind: Kind::Flowchart,
        bounds,
        counts,
        acc_title: model.acc_title.clone(),
        acc_descr: model.acc_descr.clone(),
        body: SceneBody::Flowchart(FlowchartScene {
            nodes,
            edges: scene_edges,
            clusters,
        }),
    })
}

fn marker(marker: FlowEdgeMarker) -> Marker {
    match marker {
        FlowEdgeMarker::None => Marker::None,
        FlowEdgeMarker::Point => Marker::Arrow,
        FlowEdgeMarker::Circle => Marker::Circle,
        FlowEdgeMarker::Cross => Marker::Cross,
    }
}

/// The lines merman sized, decoded for display: the entities Mermaid's SVG
/// would show literally (`&quot;`, `&#35;`, `&nbsp;`, …) become their
/// characters. The breaks are unchanged, so the lines still fit the box
/// merman sized for them.
pub(crate) fn display_lines(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| decode_html_entities_to_unicode(&line).into_owned())
        .collect()
}

/// The lines of a label as merman sized them for an SVG label: Mermaid's
/// `createText` for a non-HTML label, which merman mirrors when it measures.
///
/// The label is split at `\n` escapes and `<br>` tags; each line is cut into
/// words at ECMAScript whitespace, a `<tag>` being a word of its own and a
/// stray `<` or `>` nothing; when `max_width` is given, a line wider than it
/// is refilled word by word, and a single word wider than it is broken
/// where it stops fitting. Widths come from `width`, the same measurer merman
/// asked, on the visible text: `&lt;`, `&gt;` and `&amp;` decoded. merman's
/// own routine is private to it; this is the same algorithm, and the tests
/// hold its lines to the ones merman asked the measurer to size.
///
/// merman breaks an over-wide word between grapheme clusters; this breaks
/// between scalar values, which differs only for a word wider than the box
/// that holds a combining sequence.
pub(crate) fn label_lines(
    label: &str,
    max_width: Option<f64>,
    width: &dyn Fn(&str) -> f64,
) -> Vec<String> {
    if label.is_empty() {
        return Vec::new();
    }
    let text = normalize_line_breaks(&decode_label_escapes(&label.replace("\r\n", "\n")));
    let mut lines = Vec::new();
    for source_line in text.split('\n') {
        let words: Vec<&str> = svg_words(source_line).collect();
        let visible = |words: &[&str]| decode_entities(&words.join(" "));
        let Some(max_width) = max_width.filter(|w| w.is_finite() && *w > 0.0) else {
            lines.push(visible(&words));
            continue;
        };
        if words.is_empty() || width(&visible(&words)) <= max_width {
            lines.push(visible(&words));
            continue;
        }
        let mut current: Vec<&str> = Vec::new();
        let mut index = 0;
        while let Some(&word) = words.get(index) {
            let mut candidate = current.clone();
            candidate.push(word);
            if width(&visible(&candidate)) <= max_width {
                current = candidate;
                index += 1;
                continue;
            }
            if !current.is_empty() {
                lines.push(visible(&std::mem::take(&mut current)));
                continue;
            }
            // A lone word wider than the box: as much of it per line as fits,
            // at least one character.
            let decoded = decode_entities(word);
            let mut rest = decoded.as_str();
            while !rest.is_empty() {
                let mut end = rest.chars().next().map_or(0, char::len_utf8);
                for (offset, ch) in rest.char_indices().skip(1) {
                    if width(&rest[..offset + ch.len_utf8()]) <= max_width {
                        end = offset + ch.len_utf8();
                    } else {
                        break;
                    }
                }
                lines.push(rest[..end].to_owned());
                rest = &rest[end..];
            }
            index += 1;
        }
        if !current.is_empty() {
            lines.push(visible(&current));
        }
    }
    lines
}

/// Mermaid's `\\` and `\:` label escapes.
fn decode_label_escapes(label: &str) -> String {
    if !label.contains('\\') {
        return label.to_owned();
    }
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.peek() {
            Some('\\') => {
                out.push('\\');
                chars.next();
            }
            Some(':') => {
                out.push(':');
                chars.next();
            }
            _ => out.push('\\'),
        }
    }
    out
}

/// A `\n` escape or a `<br>`, `<br/>`, `<br />`, `</br>` tag (any case,
/// ASCII whitespace before the slash) becomes a line break.
fn normalize_line_breaks(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("\\n") {
            out.push('\n');
            rest = after;
            continue;
        }
        if let Some(after) = br_tag(rest) {
            out.push('\n');
            rest = after;
            continue;
        }
        let ch = rest
            .chars()
            .next()
            .expect("a non-empty rest starts with a character");
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

/// What follows a `<br>`-style tag at the start of `text`, if one is there.
fn br_tag(text: &str) -> Option<&str> {
    let mut rest = text.strip_prefix('<')?;
    rest = rest.strip_prefix('/').unwrap_or(rest);
    let mut chars = rest.chars();
    let (b, r) = (chars.next()?, chars.next()?);
    if !b.eq_ignore_ascii_case(&'b') || !r.eq_ignore_ascii_case(&'r') {
        return None;
    }
    let mut rest = chars.as_str().trim_start_matches(is_ecmascript_whitespace);
    rest = rest.strip_prefix('/').unwrap_or(rest);
    rest.strip_prefix('>')
}

/// Mermaid's non-Markdown SVG words: `line.trim().match(/<[^>]+>|[^\s<>]+/g)`.
fn svg_words(line: &str) -> impl Iterator<Item = &str> {
    let mut rest = line;
    std::iter::from_fn(move || {
        loop {
            let ch = rest.chars().next()?;
            if ch == '<' {
                if let Some(end) = rest[1..].find('>') {
                    if end > 0 {
                        let (tag, after) = rest.split_at(end + 2);
                        rest = after;
                        return Some(tag);
                    }
                    rest = &rest[end + 2..];
                    continue;
                }
                rest = &rest[1..];
                continue;
            }
            if ch == '>' || is_ecmascript_whitespace(ch) {
                rest = &rest[ch.len_utf8()..];
                continue;
            }
            let end = rest
                .char_indices()
                .find(|&(_, c)| c == '<' || c == '>' || is_ecmascript_whitespace(c))
                .map_or(rest.len(), |(offset, _)| offset);
            let (word, after) = rest.split_at(end);
            rest = after;
            return Some(word);
        }
    })
}

/// The whitespace ECMAScript's `\s` matches.
fn is_ecmascript_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

/// The entities Mermaid's sanitizer writes into an SVG label and its
/// `createText` decodes again — `&lt;`, `&gt;`, `&amp;` — and nothing else:
/// other spellings stay literal text, as they do in Mermaid's SVG.
fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        if let Some(after) = tail.strip_prefix("&lt;") {
            out.push('<');
            rest = after;
        } else if let Some(after) = tail.strip_prefix("&gt;") {
            out.push('>');
            rest = after;
        } else if let Some(after) = tail.strip_prefix("&amp;") {
            out.push('&');
            rest = after;
        } else {
            out.push('&');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}
