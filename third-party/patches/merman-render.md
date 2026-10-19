# Koru patch: merman-render's sequence geometry in the layout JSON

`merman-render` **0.9.0** from crates.io (© the merman authors, MIT OR
Apache-2.0), with Koru's change in [`merman-render.patch`](merman-render.patch).
`scripts/setup/fetch-third-party.*` fetches the crate, checks it against its pin
in [`../fetch.manifest`](../fetch.manifest), applies the patch and places the
result in `third-party/crates/merman-render/`, which the workspace `Cargo.toml`
substitutes for the crates.io release through `[patch.crates-io]`, so `merman`
0.9.0 links it too. `koru-diagram` is the only crate that uses either
([guide/contributing/deps/MERMAN.md](../../guide/contributing/deps/MERMAN.md)).
The change is offered upstream as a pull request; the patch goes when a release
carries it.

## Why

Koru does not use merman's SVG. It asks merman for the **layout** of a diagram
(`layout_json()`, the `layout` member of the JSON a `LayoutJson` request
returns) and draws that layout itself, with its own font and theme. For a
sequence diagram, 0.9.0's `SequenceDiagramLayout` serialises the participants
(`actor-top-…`/`actor-bottom-…` nodes), the lifelines, the notes and the
message lines, and nothing else: the control frames (`loop`, `alt`, `opt`,
`par`, `critical`, `break`) and their `else`/`and`/`option` dividers are
computed during layout but kept in a field marked `#[serde(skip)]`
(`block_layouts_by_id`), while the activation bars, the `box` participant
groups and the title are worked out only inside the SVG renderer from the
layout and the model. A host drawing from the JSON would have to re-derive
Mermaid's placement rules for all four and drift from them silently.

## What changed

Everything is additive. No existing field, type or function is renamed or
removed, the SVG output is untouched, and a sequence diagram without frames,
activations, boxes or a title serialises **byte for byte as before**: each new
field is skipped when empty.

- `src/model.rs`: five new public structs, all `Serialize + Deserialize`, and
  four new fields on `SequenceDiagramLayout`, each `#[serde(default,
  skip_serializing_if = …)]`:
  `frames: Vec<SequenceFrameLayout>`, `activations: Vec<SequenceActivationLayout>`,
  `boxes: Vec<SequenceParticipantBoxLayout>`, `title: Option<SequenceTextLayout>`.
- `src/sequence/orchestration.rs`: while replaying Mermaid's layout cursor, each
  open control block also remembers its kind, its label, its dividers in order
  and the left edge of the leftmost participant its content touches; when the
  block closes, a `SequenceFrameLayout` is built from the same `start_x`,
  `stop_x`, `start_y`, `stop_y` and section y's the SVG renderer reads from
  `block_layouts_by_id`, with the one adjustment that renderer makes (a
  `critical` frame with `option` sections is widened to 9 px left of its
  leftmost participant). A block that encloses no message or note is not
  listed, because the SVG renderer draws nothing for it. Frames are sorted
  into source order.
- `src/sequence/block_steps.rs`: `block_start_kind`, the control kind of a
  frame-opening message, beside the existing `is_block_start`.
- `src/sequence/activation.rs`: `sequence_activation_layouts` replays the
  activation bars from the laid-out message lines and lifelines exactly as
  `svg/parity/sequence/activation.rs` does (the 2 px nudge after the first
  bar, the 18 px minimum height, an unterminated activation drawing nothing).
- `src/sequence.rs`: after the root bounds are known, the prepared artifact
  fills `activations`, `boxes` (the renderer's `drawBox` geometry: the
  actor-layout box widened by two box margins each side, a quarter of that
  padding above and three quarters below the footer, the title centred at
  `boxTextMargin + titleHeight / 2`) and `title` (Mermaid's
  `x = (box.stopx - box.startx) / 2 - 2 * diagramMarginX`, `y = -25`).

The smoke tests in `crates/koru-diagram/src/tests.rs` hold the new numbers to
the `<line>`, `<rect>` and `<text>` elements merman's own SVG writes for the
same source.

## The shape

Inside `layout.SequenceDiagram`, beside `nodes`, `edges`, `clusters` and
`bounds`. Coordinates are in the same space as the nodes and edges; `x`/`y` are
a rectangle's top-left corner. A field is **absent** when its list is empty or
the title is missing.

```json
"frames": [
  {
    "id": "12",
    "kind": "alt",
    "label": "Signature valid",
    "x": 260.0, "y": 434.0, "width": 426.0, "height": 369.0,
    "sections": [ { "id": "19", "label": "Signature rejected", "y": 668.0 } ]
  }
],
"activations": [
  { "actor": "Server", "x": 470.0, "y": 168.0, "width": 10.0, "height": 44.0, "depth": 0 }
],
"boxes": [
  {
    "x": -20.0, "y": -5.0, "width": 400.0, "height": 459.0,
    "fill": "transparent",
    "title": { "text": "Shop", "x": 180.0, "y": 14.0 }
  }
],
"title": { "text": "Checkout", "x": 185.0, "y": -25.0 }
```

| Field | Meaning |
| --- | --- |
| `frames[].id` | Id of the control message that opens the block, which is its key in `block_layouts_by_id` and in the semantic `messages`. |
| `frames[].kind` | `loop`, `opt`, `alt`, `par`, `par_over`, `critical` or `break` (merman-core's `SequenceControlKind::keyword()`). A `rect` is not a frame. |
| `frames[].label` | The text after the keyword, as written; Mermaid draws it in `[…]`. Empty when the source has none. |
| `frames[].sections[]` | The `else`/`and`/`option` dividers in source order: the opening message's `id`, its `label`, and the `y` of the dashed line. Absent for `loop`, `opt`, `break` and a sectioned frame with one section. |
| `activations[]` | One bar per `activate`/`deactivate` pair, in the order the activations start. `actor` is the participant id (the `actor_order` key), `depth` how many bars of that participant were already open (Mermaid shifts each level right by half the bar's width). |
| `boxes[]` | One per `box … end`, in source order. `fill` is the model's: the color written in the source, or `transparent`. `title` is the box's name centred at `x`, `y` the vertical centre of its first line; absent without a name. |
| `title` | `title …` from the source, or the front-matter title: the `x` and `y` attributes of the `<text>` Mermaid emits. |

## Updating

Edit `third-party/crates/merman-render/`, then run
`scripts\setup\fetch-third-party.ps1 -UpdatePatch merman-render` (or
`bash scripts/setup/fetch-third-party.sh --update-patch merman-render`) and put
the printed sha256 in `fetch.manifest`. To move to a new release, change its
version and crates.io sha256 there too, fetch, and resolve whatever no longer
applies; if the release carries the change, delete the manifest line, the
patch, this note and the `[patch.crates-io]` entry. `merman` and
`merman-render` are pinned to the same version in the workspace `Cargo.toml`
and must move together. [`../README.md`](../README.md) has the whole procedure.
