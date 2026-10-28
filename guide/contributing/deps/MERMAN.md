# merman — Mermaid parsing and layout, sealed in one crate

`merman 0.9.0` and `merman-render 0.9.0`, pinned exactly, linked by
`crates/koru-diagram` and by nothing else. merman is a Rust port of Mermaid's
parsers and layout engines (and of its SVG renderer, which Koru does not use).
Koru hands it the text of a `mermaid` code block and gets back a **typed
layout** — where every participant, message, node, edge, frame and label goes
— and then measures the text and paints that layout itself, with its own font,
theme and file writers. This page is what merman does that its documentation
does not say, found by reading the 0.9.0 sources.

New to Rust syntax? [../RUST.md](../RUST.md) first.

---

## Why this and not something else

Nothing in the tree parsed or laid out Mermaid. Writing a parser for two
Mermaid grammars is a week; writing a layered graph layout that matches what
people expect from Mermaid (dagre's ranking, ordering and coordinate
assignment, with Mermaid's own spacing rules on top) and a sequence layout
that reproduces Mermaid's cursor replay is months, and would drift from
Mermaid for ever after. merman is a parity port — its SVG is tested against
Mermaid's own output — so a diagram that looks right in a Markdown preview
looks right in Koru. It is pure Rust, builds for Android, and exposes its
layout as data rather than only as a picture, which is the one thing Koru
needs and the alternatives (rendering merman's SVG, as Zed does; running
Mermaid itself in a WebView) do not give.

It is linked in **one place only**: the `koru-diagram` crate, behind an API in
Koru's vocabulary (the kind of diagram, whether it draws, a theme-free layout),
so the choice stays reversible. The same pattern seals `syntect` in
`koru-blocks` and `spellbook` in `koru-spell`.

## The shape

- **Two crates, one pin.** `merman` is the facade (the engine, the typed
  requests, cancellation, resource limits); `merman-render` holds the layout
  structs (`merman_render::model::{FlowchartLayout, SequenceDiagramLayout, …}`),
  which the facade does not re-export, so the sealing crate names both. Both
  are `=0.9.0`: the project is pre-1.0, the layout JSON has no schema version,
  and APIs are removed without aliases. A bump is a deliberate change with the
  fixtures re-checked. `merman-core`, the parsers, is not a direct dependency
  and floats within 0.9 under the pin of `Cargo.lock` (0.9.1 at the time of
  writing).
- **Features: `default-features = false`, `diagram-flowchart`,
  `diagram-sequence`, `diagram-pie`, `svg`** (and the three families on
  `merman-render`). The
  defaults are every diagram family plus `complete-svg-elk`, and two of those
  features carry licenses `deny.toml` does not allow: `layout-elk` is the ELK
  port under **EPL-2.0**, `math` bundles **OFL-1.1** fonts. `native-fonts`,
  `png`, `jpeg` and `pdf` are off because Koru draws the picture itself (below).
  `svg` is the leaf that compiles the layout engine at all; without it there is
  no layout, only parsing. cargo-deny runs with every feature on, so no
  workspace feature may forward to the excluded ones. `cargo tree --workspace
  -i merman-layout-elk` must stay empty (`--workspace`, because without it
  `cargo tree` looks only at the default member, `apps/koru`).
- **The patch.** `merman-render` is fetched from crates.io and patched through
  `third-party/fetch.manifest` so that its sequence layout JSON carries the
  control frames, activation bars, `box` groups and title it otherwise leaves
  out — what the patch adds and the JSON it produces are in
  [`third-party/patches/merman-render.md`](../../../third-party/patches/merman-render.md).
  Offered upstream; the patch goes when a release carries it.
- **MSRV.** merman declares `rust-version = "1.95"`, which is why the
  workspace's floor rose from 1.92 to 1.95. The pinned toolchain
  (`rust-toolchain.toml`) is above both.

## What the documentation does not say

- **merman bundles no font and does not measure one.** Its default
  `TextMeasurementPolicy::deterministic()` is a fixed table of per-character
  widths in em (`text/heuristic.rs`: a space is 0.33 em, a lowercase letter
  about 0.45, a capital 0.60, a digit 0.56). It is deterministic, not
  accurate — lowercase-heavy labels come out narrow against a real sans. A
  host that wants real widths installs a **`HostTextMeasurer`** through
  `TextMeasurementPolicy::host_display(identity, Arc<dyn HostTextMeasurer>,
  TextMeasurementPhase::ALL)` and `SvgEnvironment::deterministic()
  .with_text_measurement_policy(..)`. The host is asked per operation and may
  answer `Ok(None)` to fall back to the table for that one request. The
  `TextStyle` it receives carries the family, size and weight the diagram asks
  for; a host is free to ignore them, which is how `classDef`/`style` font
  directives are neutralised without parsing them.
- **Its SVG cannot embed a font**, and there is no text-to-path: the SVG
  pipeline's validators reject `@font-face` (`svg/pipeline/static_validation.rs`,
  `css_sanitize.rs`), so a merman SVG names a font family and relies on the
  viewer having it. A file that must look the same on a machine without the
  font has to be produced by the host from the layout.
- **Its PNG draws with system fonts**, and the `native-fonts` path that would
  supply font files **cannot coexist with a host measurer**:
  `SvgEnvironment::with_text_measurement_policy` clears any native-font
  context, and `with_native_font_context` installs itself as the host
  measurement policy. A layout measured with the host's font and a PNG
  painted by merman would therefore disagree about every width. Koru rasters
  its own scene for both files.
- **The sequence layout JSON is incomplete without the patch.** 0.9.0
  serialises the actors (`actor-top-<id>`, `actor-bottom-<id>` nodes), the
  lifelines (`lifeline-<id>` edges), the notes (`note-<msgId>` nodes) and the
  message lines (`msg-<msgId>` edges, two points and a label box, no marker or
  dash — those are in the semantic model's `signal_semantics()`). The
  `loop`/`alt`/`opt`/`par`/`critical`/`break` frames are computed and then
  skipped (`#[serde(skip)]` on `block_layouts_by_id`), and the activation
  bars, `box` groups and title exist only inside the SVG renderer. The patch
  adds them as new optional fields and leaves every existing byte alone.
- **Labels are raw source strings.** `<br>` and entities are not resolved and
  there are no per-line positions; the host wraps and places the lines.
  `merman_render::text` has the wrapping helpers merman itself uses.
- **The default config names ELK and falls back to dagre silently.** merman's
  pinned Mermaid default has `"layout": "elk"`; with `layout-elk` not compiled
  (our case), an `elk` request lays out with dagre and nothing says so (the
  crate's own test is `graph_layout_unregistered_loader_falls_back_to_dagre`).
  Set `layout: "dagre"` and `look: "classic"` in the site config so the result
  does not depend on which features are on.
- **Front-matter and `%%{init}` pass through a `secure` list, and an explicit
  list replaces the default.** Every key named in the site config's `secure`
  list is dropped from the diagram's own config, at any depth, without a
  diagnostic (`MermaidConfig::source_filtered_overrides`). The default list is
  `secure, securityLevel, startOnLoad, maxTextSize, suppressErrorRendering,
  maxEdges, themeCSS`; setting `secure` yourself **replaces** it rather than
  extending it, so repeat those seven before adding `theme`, `themeVariables`,
  `fontFamily`, `fontSize`, `fontWeight`, `look`, `layout`, `htmlLabels` and
  whatever else the host owns. There is no "ignore directives" switch.
- **Every syntax error is one code.** A flowchart or sequence parse failure is
  `merman.parse.diagram_parse` with the parser's English text and a byte span
  (`TerminalDiagnostic`); the span is reliable, the message is not stable
  across releases. Map the span to a line; keep the message for logs.
- **Cancellation is fine-grained.** merman checks its `OperationControl`
  before and after every text measurement and every so many layout steps, so
  a host measurer gives a typing user cancellation for free; a running host
  callback itself cannot be interrupted.
- **`cargo tree -i <crate>` is per default member.** In this workspace that is
  `apps/koru`, which does not link `koru-diagram` yet, so without
  `--workspace` the command reports nothing for every merman crate — present
  or not.
- **The typed layout is reachable only as JSON.** `family::prepare(parsed,
  &LayoutOptions, session)` gives a `FamilyRenderArtifact` whose typed
  layout is private; `layout_json()` is the one way out, and
  `serde_json::from_value::<FlowchartLayout>(json["layout"]["FlowchartV2"])`
  (`SequenceDiagramLayout` under `"SequenceDiagram"`) the way back in. The
  session comes from `merman_render::environment::RenderEnvironment::deterministic()
  .with_text_measurement_policy(..).begin_session_with_control(control)`;
  `prepare` consumes the parse, so anything wanted from the semantic model
  (shapes, markers, the parser's render context with the labels as measured)
  is cloned out first.
- **A host measurer must be `Arc<dyn HostTextMeasurer>` and `'static`.**
  `TextMeasurementPolicy::host_display` stores the `Arc`, so a measurer
  borrowed for one call cannot be installed; the host shares its measurer
  as an `Arc`. The flowchart layout asks it two operations only —
  `computed-length` (`getComputedTextLength`, a growing prefix of a label as
  it wraps) and `wrapped` (a block of lines at its `\n`s) — and the sequence
  layout five: `mermaid-calculate-text-dimensions`, `raw-bbox-width`,
  `raw-bbox-height`, `tspan-bbox-width`, `tspan-bbox-height`. Every request
  arrives with the diagram's own style (16 px, a sans stack, a weight);
  `koru-diagram` answers at its fixed size and ignores it.
- **`flowchart.wrappingWidth` is 120 in merman's pinned Mermaid config**,
  not the 200 of merman's own fallback constant, so node labels wrap at 120 px
  of the measurer's advance; edge labels wrap at a fixed 200 px and plain
  group titles only at `<br>`. The wrapping of an SVG label (Mermaid's
  `createText`) is private to merman — `koru-diagram` replays the same
  algorithm and its tests hold the result to the lines merman asked the
  measurer to size.
- **An SVG label is measured as Mermaid's sanitizer wrote it.** The parser's
  render context (`ParsedDiagramRender::flowchart_render_context()`,
  `node_label_for_render` and friends) carries `&amp;`, `&quot;`, `&#35;`
  for `&`, `"`, a `#35;` code; tags stay as text; an authored `&#9829;`
  gains a second ampersand. Mermaid's SVG decodes `&lt;`, `&gt;` and
  `&amp;` and shows every other entity literally — `#quot;` reads as
  `&quot;` on screen — and merman measures exactly that text. The semantic
  `FlowNode::label` is the decoded, sanitized text (unknown tags removed)
  and is not what was measured.
- **The `interactive` resource policy caps layout work, not only time.**
  `max_layout_work_units` is 14,100,000; a 25 × 40 grid of 1,000 boxes with
  1,935 arrows charges 14,129,825 and is refused after 38 ms as
  `ResourceLimitExceeded` — which `koru-diagram` shows as *too large*, the
  same as a deadline. The budget is a function of the graph's edges and
  ranks as much as its nodes, which is why the node limit below leaves
  headroom under it.

## How large a diagram draws

`Limits::default().max_nodes` is **800**: the largest round hundred of
generated flowchart boxes, in a chain and in a grid alike, whose layout
completes in under 200 ms through `koru_diagram::layout` with the
deterministic test measurer. Measured on an Apple M4 (10 cores, 16 GiB)
with `cargo test`'s profile, which in this workspace is `dev` at
`opt-level = 1` for workspace crates and `opt-level = 3` for dependencies
(merman included) — the table reports `optimized + debuginfo`, and a
`--release` run gave the same numbers within a few milliseconds. Each row is
one layout, timed around the call; the engine was warmed first. The ignored
test `measure_how_large_a_diagram_draws` in `crates/koru-diagram/src/tests/size.rs`
prints it: `cargo test -p koru-diagram -- --ignored measure --nocapture`.

| shape | nodes | layout |
| --- | ---: | ---: |
| chain `N0 --> N1 --> …` | 100 | 3 ms |
| grid 10 × 10, arrows right and down | 100 | 4 ms |
| chain | 200 | 5 ms |
| grid 10 × 20 | 200 | 10 ms |
| chain | 300 | 8 ms |
| grid 15 × 20 | 300 | 15 ms |
| chain | 500 | 13 ms |
| grid 20 × 25 | 500 | 29 ms |
| chain | 800 | 21 ms |
| grid 20 × 40 | 800 | 56 ms |
| chain | 900 | 24 ms |
| grid 30 × 30 | 900 | 58 ms |
| chain | 1,000 | 25 ms |
| grid 25 × 40 | 1,000 | refused: `max_layout_work_units` 14,129,825 of 14,100,000, after 38 ms |

Time is not what bounds the size: at 1,000 boxes the grid is refused by
merman's `interactive` work budget while the chain still lays out in 25 ms.
Of the sizes measured (100, 200, 300, 500, 800, 1,000), 800 is the largest at
which both shapes complete; 900 was measured as well and also completes, and
800 is kept because a 1,000-box grid already exceeds the budget by 0.2 %, so
a graph denser than a grid can be refused by merman below 1,000 boxes
whatever the clock says. A diagram under the limit that merman still refuses
shows as *too large* after a layout attempt rather than before it.

`max_messages` stays at the working default of **500**. A two-participant
sequence diagram lays out in 1 ms at 100 messages and 5 ms at 600, so the
measurement found no reason to move it; the number is a product choice about
what a page can usefully hold, not a cost.

| shape | messages | layout |
| --- | ---: | ---: |
| sequence, 2 participants, alternating `->>` and `-->>` | 100 | 1 ms |
| sequence | 200 | 1 ms |
| sequence | 300 | 2 ms |
| sequence | 400 | 3 ms |
| sequence | 500 | 4 ms |
| sequence | 600 | 5 ms |

## New transitive dependencies

Thirty-seven crates reach the lock file through `koru-diagram` and nothing
else (`cargo tree -p koru-diagram -e normal`), all from crates.io and all
within `deny.toml`'s list: merman's own `merman`, `merman-core`,
`merman-render`, `dugong` and `dugong-graphlib` (the dagre port), `roughr-merman`
and `merman-unicode-linebreak` (MIT OR Apache-2.0, Apache-2.0); the CSS and
HTML parsing behind Mermaid's `style`/`classDef` and HTML labels — `cssparser`
0.36 and 0.38, `cssparser-macros`, `dtoa-short` and `selectors` (**MPL-2.0**),
`servo_arc`, `lol_html` (**BSD-3-Clause**), `htmlize`, `precomputed-hash`,
`phf` 0.14 with its generator and macros; the YAML front-matter and JSON5
directive parsers `granit-parser` and `json5`; `lalrpop-util`; `palette`
(with `palette_math` and `palette_derive`); `quick-xml` 0.42, `roxmltree`,
`svgtypes`, `data-url`; `ryu-js` (Apache-2.0 OR BSL-1.0), `dtoa`,
`arraydeque`, `by_address`. ICU4X, `html5ever`, `kurbo` and `euclid` were
already in the tree. The pin also moved `thiserror` 2.0.20 → 2.0.21.

Of these, only `selectors` ships no license file in its package; its record
is `crates/koru-release/licences/crates/selectors.txt`
([licensing/](../../../licensing/README.md#a-crate-without-a-license-file)).
None of them is linked by a shipped binary until something links
`koru-diagram`; `koru-mobile-native` does not.

## The one integration point

`koru-diagram` (the only crate that may name `merman` or `merman-render`):

- detection: `Engine::new().registry().detect_type(text, &mut MermaidConfig)`
  gives a diagram type (`flowchart-v2`, `sequence`, `pie`, …) without parsing
  the body; `merman::diagram_type_family_id` maps it to a family
  (`flowchart`, `sequence`).
- layout: the parse of `inspect` continued — `engine().parse_diagram_for_render_model_controlled_sync`
  under a child of the host's `OperationControl` carrying the deadline, then
  `RenderEnvironment::deterministic().with_text_measurement_policy(host)
  .begin_session_with_control(control)`, `merman_render::family::prepare`,
  `layout_json()` → `{ meta, semantic, layout: { "FlowchartV2" |
  "SequenceDiagram": … } }`, deserialised into
  `merman_render::model::FlowchartLayout` / `SequenceDiagramLayout` and built
  into a `DiagramScene` with the semantic model cloned out before the
  layout. `merman::Renderer` is used only by the tests, as the independent
  path the scene's bounds are checked against.

Nothing above `koru-diagram` sees either type.

## Where to look

- `crates/koru-diagram/src/lib.rs` — the crate's charter; `src/layout.rs`
  the merman session and the failure mapping; `src/measure.rs` the host
  measurer's adapter; `src/flowchart.rs` the scene builder and the label
  wrapping; `src/tests/smoke.rs` — the smoke tests that hold the patched
  layout to merman's own SVG; `src/tests/size.rs` — the measurement above
- `third-party/patches/merman-render.md` — the patch and the JSON it adds
- [merman on docs.rs](https://docs.rs/merman) and
  [merman-render](https://docs.rs/merman-render) — upstream
