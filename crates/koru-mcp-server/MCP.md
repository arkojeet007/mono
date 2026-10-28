# MCP semantics

| | |
| --- | --- |
| How document semantics cross the MCP boundary | this file |
| Connect Claude Desktop, ChatGPT/Codex, or a local Ollama model | [guide/user/](../../guide/user/README.md) |
| What the surface does not do yet | [docs/mcp/BACKLOG.md](../../docs/mcp/BACKLOG.md) |
| The bridge's own executable, and how it is found | [docs/contributing/operations/RUNBOOK.md](../../docs/contributing/operations/RUNBOOK.md) |

This is the canonical contract for exposing Koru through Model Context
Protocol. Read it before adding a tool, operation, result field, or internal
object to `koru-mcp`.

**This file ships in every release at `docs/mcp/README.md`, beside the
executables.** That installed path is the contract: the server looks for
exactly that relative path, so it never changes.
`koru_update::RELEASE_DOCUMENTS` (`crates/koru-update/src/exe.rs`) names it.
The source is this file, `crates/koru-mcp-server/MCP.md`, kept beside the
server so the repository builds without the `docs/` checkout; the release
packager copies it to the installed path (`RELEASE_DOCUMENT_SOURCES` in
`crates/koru-release/src/package.rs`), and
`every_runtime_document_is_explicit_and_present` fails if they disagree.

The running server publishes this exact file as the standard MCP resource
`koru://docs/model`. Clients without MCP resource support can retrieve the same
text with `get_documentation({"uri":"koru://docs/model"})`. Some desktop
bridges and remote-device proxies forward tools but omit `resources/list` and
`resources/read`; an empty resource list therefore does not mean this contract
is absent. `get_documentation` is the portable discovery path and must remain
available even while the standard resource exists.

## Keeping this contract current

This file is part of Koru's public MCP interface, not an implementation note.
Every change to an MCP tool, request or result schema, error/retry rule, or
document semantic must update this file in the same change. Development servers
read the repository copy directly. The release packager names this file
explicitly, refuses to package without it, and copies it only after building
`koru-mcp`, so every release serves the latest checked-out contract rather than
a string embedded in the executable.

The current tool surface is:

- `get_documentation`, `list_workspaces`, `list_documents`, `search_documents`,
  `list_tasks`, `read_document`, `list_files`, `search_files`, `get_file`, and
  `read_file` for discovery and reads;
- `create_document`, `edit_document`, `ensure_file_note`, and
  `extract_to_document` for durable semantic mutations; and
- `save_document_version`, `list_document_versions`,
  `compare_document_versions`, and `restore_document_version` for history; and
- `list_chats`, `search_chats`, and `read_chat` for bounded, read-only access to
  Koru AI chats.

A **delegated run** reaches a different server — six artifact tools at
`/v1/runs/<RunId>/mcp`, never the bridge above — described under
[The run endpoint](#the-run-endpoint).

## What `tools/list` publishes

The `tools/list` reply is one JSON line that every connected agent reads into
its context, so it carries what an agent needs to choose and call a tool and
nothing else ([ADR-433-compacttools](../../docs/decisions/ADR-433-compacttools.md)).
This file is the long form; the reply points here.

- **Each tool:** its name, a title, one description, an input schema and
  safety hints. A read-only tool publishes `readOnlyHint: true` and
  `openWorldHint: false` only — MCP defines `destructiveHint` and
  `idempotentHint` as meaningful only when `readOnlyHint` is false.
- **No `outputSchema`.** Every result is still a JSON object in
  `structuredContent` beside a text copy; its shape is the table under
  [Results](#results) below. A field marked `?` may be `null` or absent.
- **Prose lives in the description, not in the schema.** The input schema is
  generated from the Rust request types and then compacted by
  `crates/koru-mcp-server/src/published_schema.rs`: every `description`,
  `title` and `$schema` is dropped, a local `$ref` is replaced by its
  definition and `$defs` removed, and an integer width (`int64`, `uint`) is not
  published as a `format`. A doc comment on a request field is therefore a note
  for developers; what an agent needs from it is written in the tool's
  description.
- **A tagged union is one closed object.** `edit_document`'s `operations`,
  `create_document`'s `initial_operations`, `inline` and an attribute `value`
  each publish one object whose tag (`op` or `type`) is an `enum`, whose
  properties are the union of the variants', with `additionalProperties:
  false`, and whose description is generated from the variants — for example
  `insert(node_id, parent_id, kind, text, after_node_id?)`. Which fields a
  given op takes is enforced by the server, which rejects an unknown or
  misplaced field with an error rather than ignoring it.
- **Kept:** every property name, type, enum value and `required`, `minimum` and
  `maximum`, a string `format` (`uuid`), and a default that names a value
  (`"open"`, `"relevance"`). `compaction_keeps_every_parameter_name_and_value`
  compares the published schemas with the generated ones.

The reply was 57,071 bytes before this and is 17,137 bytes now. It cannot get
under 8 KiB with 21 typed tools: the names, titles, safety hints and typed
parameters alone come to about 9.6 KB before a word of description. That
matters for Grok CLI 1.0.34, which can lose an 8,192-byte chunk of any longer
line it reads from a stdio server — see the ADR for what was measured.

### Pages, off by default

The bridge can serve `tools/list` in pages, each response line under 7,000
bytes, as MCP pagination defines it: the reply carries `nextCursor` until the
last page, and the client sends it back as `cursor`. **It is off**, and a
bridge answers the whole list in one reply, because two agents a chat can
start read only the first page — Kimi 2.0.0 and Codex 0.156.1 register four
tools and never send the cursor back (measured 2026-09-26; see the ADR).
`koru-mcp serve --stdio --tool-pages on` turns it on for one bridge, `off`
forces one reply; without the flag the build's default applies
(`PAGED_TOOLS_BY_DEFAULT` in `crates/koru-mcp-server`). A chat's bridge is
paged for Claude, Grok and OpenCode, the agents measured to follow the cursor:
the session host passes the flag for those three (maintainers, 2026-09-26).

When on (`crates/koru-mcp-server/src/tool_pages.rs`):

- Pages are packed greedily from the router's sorted list, so the same build
  always pages the same way. Every tool is on exactly one page; a tool larger
  than a page on its own gets a page of its own.
- A cursor is opaque to the client and stable across calls and across bridges
  of the same build. A cursor the bridge did not hand out — and any cursor when
  paging is off — is a JSON-RPC `invalid_params` error (-32602), never a
  silent page one.
- Koru AI's own providers read `workspace_tool_catalog`, not `tools/list`, and
  always see every tool.
- The bridge logs every `tools/list` it answers — the cursor it was given, the
  tools it returned, the cursor it handed back — in `koru-mcp.log`, which is
  how an agent's paging was verified.

### Results

What each tool answers, as the output schemas published until ADR-433 said
and as a test held them against real values. A failure is
`{ "code", "message", "details"? }` under `isError`.

| Tool | Result |
| --- | --- |
| `get_documentation` | `{ uri, mime_type, text }` |
| `list_workspaces` | `{ workspaces: [{ id, owner, name, created_at, last_opened_at?, icon?, color?, pinned, archived, deleted_at?, position }] }` |
| `list_documents` | `{ documents: [{ id, parent?, ordinal, title, kind, trashed_at?, trashed_by? }] }` |
| `search_documents` | `{ hits: [{ workspace, workspace_name, document, node?, document_kind, mime?, kind, field, title, breadcrumb, breadcrumb_spans, snippet, spans, lifecycle, trashed_at?, trashed_by?, score, observed_modified_at_ms?, workspace_last_used_at_ms? }], next_offset?, updating_workspaces, corrected_query? }`; a span is `{ start, end }` |
| `list_tasks` | `{ tasks: [{ task, workspace_id, document_id, node_id, node_kind, title, breadcrumb, lifecycle, due?, priority?, list?, document_trashed, in_trash }], updating_workspaces, next_after?, restarted }` |
| `list_files`, `search_files` | `{ files: [file] }`, a file being `{ file_id, name, mime, size?, hash, root_node_id, note_id? }` |
| `get_file` | a file |
| `read_file` | `{ file, offset, length, total_bytes, eof, base64 }` |
| `read_document`, `create_document` | `{ document_id, root_node_id, revision, schema_version, nodes }` — `nodes` in preorder, each with identity, parent, ordinal, depth, kind, typed metadata and typed content; a `code` node whose language is `mermaid` also carries `diagram` ([Diagrams](#diagrams)) |
| `ensure_file_note` | `{ note_id, created }` |
| `edit_document`, `restore_document_version` | `{ revision, history: entry }`, an entry being `{ id, document, version, reason, recorded_at, label?, message?, boundary?, pinned }` |
| `extract_to_document` | `{ document_id, document_revision, source_revision, source_link_node_id, history: { document: entry, source: entry } }` |
| `save_document_version` | an entry |
| `list_document_versions` | `{ entries: [entry], next_before_at?, next_before_id? }` |
| `compare_document_versions` | `{ unchanged_nodes, changes }` — `unchanged_nodes` is a count |
| `list_chats` | `{ chats: [{ chat_id, title, message_count, created_at, updated_at }], next_cursor? }` |
| `search_chats` | `{ hits: [{ chat_id, message_id, chat_title, snippet, score }], next_cursor? }` |
| `read_chat` | `{ chat_id, title, total_messages, messages: [{ message_id, ordinal, role, state, markdown, truncated, created_at }], next_cursor? }` |

## The boundary

**A chat's local agent reaches this bridge without any hand configuration**
(AGENTUX, [ADR-430](../../docs/decisions/ADR-430-chatagentbridge.md)): the session
host attaches `koru-mcp serve --stdio` to every session a chat creates, so
Codex, Claude, OpenCode, Kimi, Grok and Antigravity in a chat call the same tools a
hand-configured ChatGPT does, under the same local principal and policies.

MCP exposes **Koru semantics**, never CRDT mechanics. A client asks to insert a
paragraph, create a document reference, extract a block, compare versions, or
restore one. The server translates that intent through `WorkspaceDoc` and
publishes the resulting Loro operations through the normal durable Store path.

Raw Loro updates, op ids, `TreeID`s, container keys, anchor characters, mark
keys, snapshot bytes, and version-vector arithmetic are not MCP input. Standard
Loro Protocol remains the peer-sync protocol; duplicating it in MCP would create
a second, incompatible CRDT protocol.

Every tool uses the official MCP SDK. `structuredContent` is always a JSON
object, request structs reject unknown fields, and stdio stdout contains MCP
framing only.

## Authorization and workspace scope

The semantic executor authorizes every call before it asks Store for data. A
request context carries the authenticated tenant and principal, Koru scopes,
and one of two explicit workspace authorities:

- the trusted local desktop MCP bridge uses all workspaces available to its
  local principal; and
- product callers such as Koru AI provide an explicit selected-workspace set.
  An empty selected set means no workspace access, never all workspaces.

Discovery and read tools require `read_documents`; document and history
mutations require `edit_documents`. `get_documentation` remains available
without a content scope so a caller can learn the contract before requesting
authority. `list_workspaces` filters its result to the authorized set. Every
tool carrying one `workspace_id` rejects an unselected workspace with
`permission_denied` before opening it. A cross-workspace search intersects its
requested IDs with the authorized set; an empty intersection is a successful
empty result and never falls through to Store search's ordinary empty-scope
meaning of every workspace.

Authorization is part of the shared executor rather than the MCP transport.
Koru AI calls that executor directly through the gateway's bounded internal
Store boundary, so it receives the same schemas, checks, semantic operations,
and results without an HTTP or MCP loopback.

Koru AI's durable tool activity is not a replay log. It retains the tool name,
status, timing, byte count, stable target workspace/document ids, and at most a
bounded public-safe failure summary. Raw arguments, results, queries, titles,
paths, and workspace content remain absent. The trusted desktop resolves a
target document's current title from Store for display, so a rename cannot make
AI audit data stale.

The semantic backend has two Store adapters, not two implementations. The MCP
desktop bridge uses its reconnecting local Store client. The product gateway
injects a typed embedded adapter over the already-running bounded Store worker.
That embedded port names Store operations such as workspace/document listing,
indexed search, snapshots, history, blobs, seeded creation, and durable update
publication; it carries no HTTP request, bearer, MCP envelope, discovery
address, or provider type. Both adapters end in the same `WorkspaceDoc`
operation code and the same structured tool result.

**The desktop bridge is never handed to a delegated run.** A run gets a
grant set resolved for it at its own endpoint ([below](#the-run-endpoint)),
which is a different thing from inheriting a bridge that already holds
every local workspace.

Chat tools are a separate authorization domain. The trusted local desktop MCP
bridge receives chat authority and reaches the AI service through the AI facet
of its existing Koru API client; it does not open `ai.db` itself. Product
contexts created with an explicit workspace selection receive no MCP chat
authority. The three public gateway operations require the existing `ai.chat`
scope, which is already included in the default desktop session. If the gateway
or AI worker is unavailable, the chat tool returns a structured error while the
MCP bridge remains available for its other tools.

## Identities and document shape

- `ProfileId`, `WorkspaceId`, Store `DocumentId`, `NodeId`, and `AttrId` are
  distinct client-minted UUID types. Their strings may look alike; their
  meanings are not interchangeable.
- `TreeID` is Loro-local and must never cross the MCP boundary.
- One Store document owns one Loro document. Its single root
  `Kind::Document` node is row 0 and its text is the document title.
- A file is a separate manifest entry of kind `file`; its opaque document has
  one root `Kind::File` blob node. It never opens as an editor document.
- The Store manifest carries the `DocumentId`, title copy, nesting, ordering,
  kind, and trash state without opening content.
- A body edit cannot insert, move, or remove the document root. Creating a real
  document goes through `create_document`, never `insert kind=document`.

Node IDs are stable across text edits, moves, imports, and ordinary concurrent
editing. A different ID means a structural operation deleted and recreated the
node; it is not normal CRDT behavior.

Text content is not itself the identity. `Enter` splits one node into two: the
existing ID remains on the left-hand node and the new right-hand node receives
a fresh ID. At byte offset zero that means the original text moves into the new
right-hand node while the old ID names the empty row above it. Merging or
removing blocks can likewise retire an ID. An MCP client must therefore reread
after a concurrent structural edit; `stale_node` returns the complete UUID and
`retry: read_document` rather than abbreviating the identifier.

Every operation that targets existing text carries its exact `expected` value,
including `move`, `remove`, and `set_done`. This matters when the old UUID still
exists after a concurrent split but now names the other half: Koru returns
`precondition_failed` rather than applying the operation to the wrong content.

## Typed blocks, text, and ordering

Reads return an exhaustive preorder tree. Each row carries its node id, parent,
derived ordinal, depth, kind, metadata, typed content, attributes, and text
runs; a `mermaid` code block also carries `diagram`, whether its text draws
([Diagrams](#diagrams)). The opaque `revision` is the precondition for a later
mutation.

`schema_version` is the document's Koru schema version as a reader should treat
it: an integer, always, and `1` for a document that records none. Recording none
is ordinary rather than damaged — a document the Store created starts as an
empty Loro document that the application seeds, and any version above 1 is
written only when content needing it is added. It is a fact about the document,
not about this server, and it is never `null`.

Structural writes name an existing parent and, when ordering matters, an
existing sibling through `after_node_id`. `null` means first. Numeric ordinals
are read-only projections and are never accepted for remote writes: concurrent
insertion makes a remembered number unstable while sibling identity remains
meaningful.

The live-edit operation vocabulary is `replace_text`, `insert`, `insert_inline`,
`move`, `remove`, `set_done`, `set_language`, `set_callout_look`,
`set_attribute`, `clear_attribute`, `add_tag`, and `remove_tag`. The last four
are described
under [Attributes and tags](#attributes-and-tags), and what `set_done` and a
`koru.due` write mean on a **task** is under [Tasks](#tasks).

The `insert` schema enumerates every accepted body kind: `task`, `paragraph`,
`quote`, `listblock`, `table`, `code`, `contact`, `divider`, and `callout`.
Operation variants are inlined in the published tool schema so an MCP host
need not resolve a private `$ref` to discover their fields. All request
objects deny unknown fields; a misspelling is an error, never a successful
no-op.

**A callout is a box, and the box is a subtree.** The `callout` node is a
container with no text of its own: a read reports it as `"kind": "callout"`
with an empty `content.text`, and its children — the paragraphs and list
blocks whose `parent_id` is the box — are its lines. Its look lives on the
box: `metadata.icon` (one emoji, or `null` for the default 💡) and
`metadata.colour` (an accent token — `red`, `orange`, `yellow`, `green`,
`teal`, `blue`, `purple`, `pink`, `gray` — or `null` for the default tint),
the same two metadata fields every other node reports. An `insert` of kind
`callout` creates the box under `node_id` and, when its `text` is non-empty,
a first paragraph inside it with that text, so one call still makes a
one-line callout; an empty `text` makes an empty box. To add more lines,
insert paragraphs or list blocks with the box as `parent_id`. A new callout
has the default icon and tint until `set_callout_look` changes them.

`set_callout_look` sets the look:
`{"op": "set_callout_look", "node_id": "<the box>", "expected": "", "icon": "⚠️", "color": "red"}`.
`node_id` is the box, never one of its lines, and `expected` is the box's own
text, which is empty. Each field stands alone: leave it out to keep it, send
`null` to restore the default (💡, the default tint), or send a value to set
it. At least one is required. `icon` is one emoji, stored in its fully
qualified spelling. `color` is one of `red`, `orange`, `yellow`, `green`,
`teal`, `blue`, `purple`, `pink` or `gray`, which a read reports as
`metadata.colour`; `colour` names the same field. A target that is not a
callout, an `icon` that is not one emoji and a `color` outside the nine are
`invalid_request` and write nothing, and a look the box already has writes
nothing either. `set_attribute` of `koru.color` is a different thing: a content
attribute on the node, reported under `attributes`, not the box's tint.

Text positions exposed by MCP are UTF-8 byte boundaries because that is what
the editor and returned runs use. Loro indexes Unicode scalars; conversion stays
inside `WorkspaceDoc` and its `_utf8` APIs. A tool must reject an offset outside
the current expected string or in the middle of a scalar.

## Creating a complete document

`create_document` has one semantic model and two useful forms. Omitting
`initial_operations` creates the ordinary Koru blank document: title root plus
one empty paragraph. Supplying `initial_operations` creates a complete body in
the same call. This is the efficient path for an agent generating twenty
paragraphs at once; it is not a second document format and it never accepts
Markdown or raw CRDT operations.

**Where it goes is `parent_document_id`.** Absent means the workspace root;
naming another document nests this one under it, which is the same relationship
Explore draws and the same one `list_documents` reports as `parent`. It is a
*manifest* relationship: nothing is inserted into the parent document's tree,
and a document is never a node inside another document. An unknown parent is
refused rather than quietly appended to the root, and the parent is part of the
request's retry identity — two creates differing only in where they go are two
requests, so one idempotency key across both is a conflict rather than a replay
that hands back a document nested somewhere else.

Both forms return exactly the same projection as `read_document`, including
`document_id`, `root_node_id`, `revision`, `schema_version`, and the exhaustive
`nodes` array. A caller can therefore treat successful creation as the first
read, immediately target returned node ids, and avoid a mandatory follow-up
call.

`kind` is absent or `page`; MCP does not create opaque file entries through
`create_document`. Files arrive through the blob/import seam so their bytes,
hash, metadata, root node, and manifest entry are committed consistently.

When `initial_operations` is present:

- it must be non-empty and `root_node_id` must be a fresh client-minted UUID;
- Koru does not add the default paragraph;
- the caller supplies every body `node_id`;
- only `insert`, `insert_inline`, `set_done`, `set_language`, and
  `set_callout_look` are legal;
- `parent_id` names the supplied root or an earlier inserted node;
- `after_node_id` is `null` for first or names an earlier inserted sibling; and
- inline, completion, language, and callout-look operations can target only a
  node inserted earlier in the same array.

Forward references, duplicate ids, unknown fields, and edit-only operations
such as `replace_text`, `move`, or `remove` fail loudly. Operations execute in
array order as one creation. The result contains precisely the supplied body,
including nesting and inline objects.

`set_language` sets a code block's grammar (`"rust"`, `"py"`, …) and takes
`null` to restore auto-detect; on any kind other than `code` it is refused
rather than silently ignored, because an agent that believed it had labelled a
block would otherwise have no way to learn otherwise. `"mermaid"` is the one
language that changes what a block *is*: the block becomes a diagram, drawn
from its text ([Diagrams](#diagrams)).

**Seeding takes no attribute operation.** `set_attribute`, `clear_attribute`,
`add_tag` and `remove_tag` are live-edit operations only. This is a decision,
not an omission: a tag name has to be resolved against the workspace tag table
*before* the batch is applied, and `create_document` is the one call in this
surface that replays — so a seed carrying a tag would write to the tag table
for a document that may already exist. Create the document, then annotate it
with `edit_document`. `creation_does_not_offer_the_attribute_operations` pins
the absence.

Creation is content-first. The Store durably records the complete initial Loro
state and its retry record before adding the document to the workspace
manifest. A crash between those commits can leave hidden recoverable bytes, but
never a visible empty or half-created document. Retrying completes publication.

For retry safety, supply a UUID `idempotency_key`. Its scope is workspace plus
authenticated tenant/principal (the local desktop is one principal), and it is
retained for seven days. Replaying the same semantic request returns the
original document id and original success projection, even if the live document
has since changed. Reusing the key for different input returns
`idempotency_conflict`; after expiry the key may name a new request. Omitting
the key is convenient for interactive use but a caller retry after an unknown
transport outcome can create a duplicate.

## Diagrams

A diagram is a `code` block whose language is `mermaid`. To write one, `insert`
a `code` block whose text is the Mermaid source, then `set_language` it with
`"mermaid"` — in one `edit_document` batch, or in `create_document`'s
`initial_operations`. Nothing else is needed and there is no diagram tool: the
text is the diagram, it draws in every open view as it arrives, and a read
tells you whether it drew.

**What draws today.** Flowcharts — a `flowchart` or `graph` header, in any
direction, `subgraph` groups included — sequence diagrams, with their
notes, activations and `loop`, `alt`/`else` and `opt` frames, and pie charts
(`pie`, with an optional `title` and `showData`). Every other
Mermaid kind (`gantt`, `classDiagram`, …) keeps its text and is shown as
text, with a note that it is not drawn. A flowchart draws up to **800 nodes**,
a sequence diagram up to **500 messages**, and a pie's slices count against
the node limit; over that the block shows its text. The node limit was measured (every generated flowchart of that size
lays out well within a page's open time; denser graphs can still be refused
by the layout engine's own work budget, which the view reports as too large);
the status below reports the count, so a client never has to know the numbers.

**Koru's theme and font decide how a diagram looks.** Styling and interaction
directives — `%%{init: …}%%`, `classDef`, `class`, `style`, `linkStyle` and
`click` — are ignored for drawing but preserved verbatim in the text, so a
source brought from elsewhere round-trips unchanged. Do not write them to
change the look; they have no effect on it.

### Reading whether a diagram draws

Every node projection this surface returns — `read_document`, the result of
`create_document`, and the `before`/`after` nodes in a
`compare_document_versions` change — carries one more field on a `code` node
whose `language` is `mermaid`: **`diagram`**, computed from the node's text as
read. No other node has it. It is a parse and a count, never a layout: it says
whether the block draws, not how large it is drawn. `edit_document` returns no
nodes, so read the document after an edit to see whether the change drew.

`diagram.status` is one of five values; the other fields depend on it:

| `status` | beside it | meaning, and what to do |
| --- | --- | --- |
| `draws` | `kind` — `flowchart`, `sequence` or `pie` — and its counts: `nodes`, `edges`, `groups` for a flowchart; `participants`, `messages`, `notes` for a sequence diagram; `slices` for a pie | The block draws. Nothing to do. |
| `not_parsed` | `line`, `message`, `code` | The parser could not read the text. `line` is the 1-based line of the error in the text as read, front matter included — for a missing `end`, the last non-empty line; `message` is the parser's own English; `code` is its stable diagnostic code. Fix that line with `replace_text` and read again. |
| `kind_not_drawn` | `kind` — the Mermaid type id, or `null` when the text opens with no diagram keyword | Not a kind Koru draws yet. The text stays and is shown as text; leave it, or rewrite it as a flowchart, a sequence diagram or a pie chart. |
| `too_large` | `kind`, `count` | Over the size limit. `count` is the nodes of a flowchart, the messages of a sequence diagram or the slices of a pie, and `0` when the source was too large to parse at all. Draw fewer boxes or messages, or split the diagram across blocks. |
| `empty` | — | The block has no text yet. Write the Mermaid source with `replace_text`. |

```jsonc
{"status":"draws","kind":"flowchart","nodes":10,"edges":12,"groups":2}
{"status":"draws","kind":"sequence","participants":4,"messages":12,"notes":2}
{"status":"not_parsed","line":7,"message":"Diagram parse error …","code":"merman.parse.diagram_parse"}
{"status":"draws","kind":"pie","slices":2}
{"status":"kind_not_drawn","kind":"gantt"}
{"status":"kind_not_drawn","kind":null}
{"status":"too_large","kind":"flowchart","count":1000}
{"status":"empty"}
```

The values are codes, not sentences — `status`, `kind` and `code` are stable
tokens to branch on — and only `message` is prose, the parser's own, because
it names the token the parser tripped on and nothing else could. A block with
`language` other than `mermaid` is never inspected, whatever its text holds:
to draw a flowchart that sits in a `code` block today, `set_language` it.

`draws` is a statement about the text: it parsed, it is a kind Koru draws, and
it is within the limits. One case lies outside it — a diagram within the limits
whose layout still takes longer than Koru allows shows as too large in the view
while this field says `draws`. The status is computed without laying out, which
is what keeps a read cheap, so that case is reported by the view alone.

## Inline objects

An inline object is an attribute rendered over one invisible U+2060 anchor in a
text node. Internally it comprises:

1. an `Attr` stored in the node metadata;
2. a semantic role such as `document`, `due`, or `link`;
3. a typed `AttrValue`;
4. the anchor character in `LoroText`; and
5. the non-expanding pill mark that connects the anchor to the attribute id.

Those five pieces are one semantic object. MCP must never expose separate
`set_attr`, `insert_anchor`, or `mark_text` calls. The public operation is
`insert_inline`; the bridge implements it through `insert_pill` or a higher
semantic model helper so rollback, formatting inheritance, attribute cleanup,
and Unicode conversion remain correct.

A `/document` object is **not** a nested `Kind::Document`. It is a labelled
`AttrValue::Document` whose `DocumentReference` stores the stable Store
`document_id` and an optional `node_id`. It never stores the document's current
profile or workspace; activation resolves the current location through the
Store, so a later move does not break or retarget the object.

**`node_id` is caller-asserted.** It names one block inside the target — the
same deep link the `@` picker's `#` writes — and the bridge checks that it is a
UUID and nothing more. Proving the block lives in that document would mean
loading the document's CRDT state, which the inline context has not got; and an
unresolvable node degrades to opening the document, which is what a stale
reference already promises. A `node_id` that is not a UUID is refused, because
it names nothing at all.

An `attachment` inline object uses the same routed-reference mechanism but
accepts `file_id` from `list_files`/`search_files`. Its role is `attachment`,
its fallback label defaults to the filename, and it points at the file entry's
root. Do not insert `kind=file` into a document body.

## Attributes and tags

An **attribute** is a typed value stored under a key on one node — a due date,
a priority, an accent color, a tag, or a key the workspace defined for itself.
`read_document` reports every one of them in each text node's `attributes`
array as `{attribute_id, role, label, value}`, where `role` **is** the key.
Four operations write them, and all four are node-addressed and carry the
node's exact `expected` text like every other edit:

```jsonc
{"op":"set_attribute",   "node_id":"…", "expected":"Ship it",
 "key":"koru.due",       "value":{"type":"date","value":"2026-08-30"}}

{"op":"clear_attribute", "node_id":"…", "expected":"Ship it",
 "key":"koru.priority"}

{"op":"add_tag",         "node_id":"…", "expected":"Ship it", "tag":"fun/home"}

{"op":"remove_tag",      "node_id":"…", "expected":"Ship it", "tag":"fun/home"}
```

**The value shapes are exactly the ones a read reports**, so a value taken off
one node can be written onto another without being understood:

| `type` | fields | notes |
| --- | --- | --- |
| `date` | `value` | strictly `YYYY-MM-DD`, never an instant |
| `datetime_floating` | `wall` | `YYYY-MM-DDTHH:MM:SS`, no zone — a *personal* time |
| `datetime_zoned` | `instant_ms`, `timezone` | an instant plus an IANA zone name — a *shared* time |
| `text` | `value` | also how a `select` token and a `koru.tag` id are spelled |
| `number` | `value` | |
| `bool` | `value` | |
| `url` | `value` | |

The three temporal shapes are three types and never one with an optional zone;
[`TIME.md`](../../docs/architecture/TIME.md) and ADR-183 say why, and this contract does not restate
it. The two shapes a read emits that are **not** writable here are `document`
and `ref`: those are inline objects, written with `insert_inline`, which builds
the anchor and mark that make them one semantic thing.

**Validation belongs to the model, and its refusals reach the wire intact.**
Whether a value fits a key is decided by that key's definition, not by this
bridge: `koru.due` is shared and takes a `date` or a `datetime_zoned` — a
`datetime_floating` is refused; `koru.priority` and `koru.color` take one of
their own tokens; `koru.plan` is personal and never lands on a shared node at
all. A value that does not fit is `attribute_rejected` with `{key, why}`; a key
nothing defines is `unknown_attribute` with `{key}` — on `clear_attribute` too,
because a misspelled clear that answered *nothing was there* would be
indistinguishable from a real one. Clearing a key the node genuinely does not
carry is a no-op that writes no operation, so it costs the person at the
keyboard no undo step.

**A tag is named, not spelled as a UUID.** `add_tag` and `remove_tag` take
`tag` — a name, `parent/child` for a path, folded case-insensitively for
identity and preserved as typed for display — or `tag_id`, the id form a
`koru.tag` value already carries. Exactly one of the two. `add_tag` resolves
with create-or-reuse: a name that already answers reuses that record and writes
nothing, and a new `fun/home` creates `fun` as well, so the parent is a real
tag in its own right (ADR-185). `remove_tag` **never writes to the tag table**:
a name nothing answers to removes nothing and creates nothing.

Two consequences worth knowing before relying on either. The tag table and the
custom attribute definitions live in the workspace's hidden `koru-system`
document, so a batch naming a tag or a custom key opens a second document —
which is why a batch naming neither does not, and an ordinary text edit stays
one round trip. And **a batch that fails validation creates no tag**: names are
resolved twice, first against a copy of the tag table so the batch can be
checked with real ids, and again into the table itself only once the whole batch
has been proved. A `/`-path's parents wait with it, and so does the
registration of a `koru-system` document for a workspace that has never held a
tag. Nothing is left behind by a rejected batch, and a successful one creates
each tag exactly once (ADR-190, amended).

## Tasks

A **task** is an ordinary node that carries the task marker, or one that
carries a deadline: a due date *is* a task, so there is no separate object to
create and no `create_task` here. This surface therefore names no new
operation. Four of the ones it already has mean something more when the node
they point at is a task, and each reaches exactly the same outcome the desktop
reaches from the same gesture — the same lifecycle, the same cascade, the same
reopening. Only the attribution differs. The third is `move`: moving
unfinished structural work under a completed task reopens that task (reason
`unfinished_child_added`), exactly as dragging it there in the app would, and
moving a structural child out of a task records its departure.

**And the fourth is `insert`**, for the same rule read the other way round:
work does not have to be moved under a completed task to reopen it, it only has
to appear there. Inserting an unchecked item into a completed task's checklist
reopens that task, reason `unfinished_child_added`, exactly as pressing Enter in
it would. A node inserted anywhere else costs nothing and says nothing: a
paragraph under a task is a note rather than work, and a row with no completed
task above it has no chain to reopen.

**`set_done` on a task is the lifecycle.** It completes the task or reopens
it, which is more than moving a checkbox: completing checks every checkable the
task contains and completes every task nested below it, as one atomic change;
reopening never cascades downward, and it reopens any completed task above the
one it touched. The checkbox follows in the same change, so a read afterwards
reports both. **On a plain checklist item `set_done` is unchanged**: the box
moves, and the node gains no task, no lifecycle and no marker. There is no way
to ask for one meaning and get the other — the target decides, because the two
are not the same thing.

**`set_attribute` with `koru.due` creates the task it dates.** The request is
unchanged and the value shapes are the ordinary ones; what happens underneath
is that a node that was not a task becomes one, in the same atomic change as
the deadline, and a read afterwards reports the marker beside the due.
`clear_attribute` of `koru.due` takes the deadline away and **keeps the task** —
a task that existed only because of its deadline does not evaporate when the
deadline goes. Setting `koru.priority` creates nothing: priority is an ordinary
content attribute, and a priority on a plain row is a plain row with a
priority.

**Not every kind can carry task state.** A task lives on a paragraph — which is
what a checklist item is — a `task` node, a document, an image or a file.
`quote`, `callout`, `listblock`, `table`, `code`, `canvas`, `contact` and
`divider` cannot, and a deadline aimed at one of them is refused rather than
quietly dropped:

| Attempt | Error | `details` |
| --- | --- | --- |
| `set_attribute` or `clear_attribute` of `koru.due` on an excluded kind | `task_rejected` | `{node_id, why:"kind_not_capable"}` |
| `set_attribute` or `clear_attribute` of `koru.task` or `koru.task_state` | `attribute_rejected` | `{key, why}` |

`why` is a token rather than a sentence, so a client can branch on it. A
refusal writes nothing: the batch is proved against a fork before the live
document is touched, so a rejected task operation leaves the document exactly
as it was, like every other rejected operation here.

The second row is the one worth reading twice. The marker and the canonical
lifecycle are **readable** — `read_document` reports them like any other
attribute — but they are not writable through a generic attribute door, by
anyone, including this bridge. Their invariants are the cascade, the checkbox
mirror and the reopen rule, and a raw write would be a second authority on all
three. Write them by meaning instead: `set_done` for the lifecycle, `koru.due`
for the deadline.

One limit that follows from the surface rather than from tasks: every operation
that targets an existing node carries its exact `expected` text, so operations
address text-shaped nodes only. An image or a file can be a task in Koru; it
cannot be made one from here yet.

**A task change made through MCP is attributed to the MCP client.** Task
mutations append activity — created, completed, reopened, due changed, and the
child events a structural change produces — and every entry this bridge
produces carries the agent identity `mcp`, where the same change made in the
app carries the person's. Every entry of one operation shares one correlation
id, so a cascade reads as one thing that happened rather than five. The append
is **best effort and happens after the durability acknowledgement**: a task
change that reached the disk is never rolled back because its activity entry
did not, and an entry is never retried or written later to make a history look
complete. Attribution is a record, not a permission: it does not widen or
narrow what a client may do, and nothing in this surface branches on it.

### Listing what is outstanding

`list_tasks` answers *what should I do next* across workspaces — or across every
workspace this session can reach, when `workspace_ids` is absent or empty. It
reads the store's read model: **no document is opened to build a page**, which
is why it can span a whole corpus, and why the answer trails a live edit rather
than blocking on one. It requires `read_documents`, like every other read here.

```jsonc
{"workspace_ids": [],              // empty lists everywhere this session reaches
 "lifecycle": "open",              // open (default) | completed | all
 "due": {"dates":    {"before": "2026-09-08"},
         "instants": {"before_ms": 1788998400000}},
 "min_priority": "high",           // low < medium < high < urgent
 "list": "0193…000a",              // a koru.list id, never a name
 "include_trashed": false,
 "sort": "due",                    // due (default) | priority | modified
 "limit": 100,                     // clamped to 1..=500
 "cursor": null}                   // next_after from the previous page
```

Every field is optional and absence means no constraint; filters compose with
**AND**. Unknown fields are errors, as everywhere else here — a misspelled
`min_priority` that were quietly dropped would answer a different question and
still look like an answer.

**The due window is stated per temporal type**, for the reason a
`search_documents` attribute filter is: Koru has three temporal types, not one
with a nullable timezone. `dates` compares calendar days and floating wall
clocks; `instants` compares zoned instants in epoch milliseconds. Naming only
`dates` leaves every timed deadline out of the answer.

`min_priority` reads in the semantic order `low < medium < high < urgent`, never
the alphabetic one. `list` is a `koru.list` **id**, because the workspace's list
table owns names and renaming a list must not change what a saved query matches;
an id nothing names matches nothing.

A row is a **node**, and carries what a client needs to act on it without a
second request:

```jsonc
{"tasks": [
   {"task": "task:<workspace>:<document>:<node>",
    "workspace_id": "…", "document_id": "…", "node_id": "…",
    "node_kind": "paragraph",           // or task, document, image, file
    "title": "Ship the report",
    "breadcrumb": "Specifications / Meeting notes",
    "lifecycle": "open",                // or completed
    "due": {"type": "date", "value": "2026-09-04"},
    "priority": "high",
    "list": "0193…000a",
    "document_trashed": false,
    "in_trash": false}],
 "updating_workspaces": [],
 "next_after": null,
 "restarted": false}
```

The `task:` reference is the row's identity, its pagination cursor, and the
subject its activity is filed under; the three ids beside it are what
`read_document` and `edit_document` take, spelled out so a follow-up call never
begins by parsing a string. **`due` is the same shape `read_document` reports
and `set_attribute` accepts** — one of the three temporal shapes — so a deadline
read here goes straight back through the edit door with no translation.
`due`, `priority` and `list` are **absent** rather than empty when the task
carries none: a marked task needs no date, and `""` would read as something
somebody wrote.

**Two trash flags, answering different questions.** `document_trashed` is the
document-level answer, inherited down the document tree, and those rows are
*flagged, never hidden*, because a restore has to be able to find them.
`in_trash` is the node-level answer — the task sits in its own document's bin —
and rows like that come back only when `include_trashed` asks for them. Both
true means both things happened.

**Paging is a keyset, never an offset.** Pass the previous page's `next_after`
as `cursor`; an absent `next_after` is the end of the list. When the row that
cursor names no longer matches the filter — completed, removed or binned between
pages — the answer is page one again with `restarted: true`, so a client can
tell a fresh list from a continuation instead of silently drawing the first page
twice. An offset would repeat or skip a row every time anything was inserted
above it.

**`updating_workspaces` is named rather than waited for**, exactly as
`search_documents` does it: the projector is debounced, so a task created a
moment ago may not be a row yet. An empty list means every workspace in scope
has reached the read model. There is no separate `indexing` flag here because
it would be that same list said twice.

To change what a list reports, use `edit_document`: `set_done` for the
lifecycle, `set_attribute` of `koru.due` for the deadline, `koru.priority` for
the priority.

### Two limits on tasks through this surface

Both are limits of the surface rather than of tasks, and both are deliberate
this release:

- **There is no remove-from-tasks operation.** A task can be created from here
  (a `koru.due`), completed and reopened (`set_done`), re-dated and un-dated
  (`clear_attribute` of `koru.due`, which keeps the task). Taking the *task*
  itself away — clearing the marker and the canonical lifecycle, with the
  cascade that implies — is a Koru gesture with no MCP door, for the reason the
  attribute table above gives: the marker and `koru.task_state` are readable
  and are not writable through a generic attribute door by anyone, this bridge
  included.
- **An image or a file cannot be made a task from here**, per the `expected`
  precondition described above: every operation targeting an existing node
  carries its exact text, so this surface addresses text-shaped nodes only.
  Image and file tasks are created in the editor, and are then listed, read,
  completed and re-dated through here like any other.

## Searching

`search_documents` answers a question across a workspace — or across every
workspace this session can reach, when `workspace_id` is absent — and is the
only way to find content without reading documents one at a time.

```jsonc
{"workspace_id": "…",              // absent searches everywhere
 "text": "quarterly",              // words to match; may contain #tag
 "tags": ["fun/home"],             // names, not ids; matches descendants
 "attributes": [
   {"key": "koru.due", "op": "before", "values": ["2026-09-10"],
    "numeric_values": ["1788998400000"],        // the same edge, as an instant
    "kinds": ["date", "datetime_zoned"]},
   {"key": "koru.priority", "op": "is", "values": ["high"],
    "kinds": ["text"]}],
 "limit": 50}
```

A hit is a **node**, not a document: the answer names the workspace, document
and node, with a snippet and the spans that matched inside it. Every filter is
node-scoped, so `text: "Hello"` with a `#fun` tag filter finds a line that
both reads *Hello* and carries `#fun` — not a line reading *Hello* in a
document that mentions `#fun` somewhere else.

Filters compose with **AND**: a node must satisfy all of them.

### Read the definitions rather than guessing a key

**The `search_documents` response does not carry them yet.** This section
used to say every response carries `definitions` and `tags`; the handler has
never attached either ([BACKLOG.md](../../docs/mcp/BACKLOG.md)). Until it does, take a key
from what `read_document` reports on a node — its `role` — rather than
inventing one. A workspace
defines its own attributes, so `user.effort` may exist here and nowhere else,
and a key you invented matches nothing while looking exactly like an empty
result.

### Every attribute has a type, and a filter names the kinds it applies to

`kinds` is required. It says which stored shapes the filter compares against,
and a value of any other kind does not match:

| kind | what it holds | compared as |
| --- | --- | --- |
| `date` | `2026-09-10` — a calendar day | ISO text |
| `datetime_floating` | `2026-09-10T17:00:00` — a wall clock, no zone | ISO text |
| `datetime_zoned` | an instant, with the zone it was meant in | number (epoch ms) |
| `number` | any number | number |
| `bool` | `true` / `false` | text |
| `text`, `select`, `ref`, `url` | a token, an id, an address | text |

**The three temporal types are three different questions.** A deadline set as
*August 30* is a `date`; the same deadline set as *August 30, 5 pm* is a
`datetime_zoned`. They are never converted into one another — a calendar day
means the same day to everybody, while an instant is a moment — so a window
naming only `date` silently omits every timed deadline.

> **The mistake to avoid.** `{"key":"koru.due","op":"before",
> "values":["2026-09-10"],"kinds":["date"]}` looks like *everything due before
> the 10th* and is really *everything due before the 10th that was set without
> a time*. Name both kinds, and give the instant bound in epoch milliseconds as
> `numeric_values`: two filters cannot express it, because filters are ANDed
> and no value is both a date and an instant — one filter naming both kinds is
> the OR.

**`numeric_values` is the bound for the kinds that compare as numbers**, and it
is positional: same count, same order, same meaning as `values`. Give it
whenever `kinds` names `datetime_zoned` beside a text kind, because the two are
compared through different columns and an ISO date is not a number — leave it
out and the instant half of the window is dropped, silently. Omit it when every
named kind compares the same way (`user.effort` over `["number"]` needs only
`values`). **You compute it, because you know which zone the question is being
asked in**; the store has no answer to that.

A filter that names no kinds is **refused**, with the kinds it could have
named. An empty answer would have been indistinguishable from a workspace with
nothing in it.

### Operators

| `op` | `values` | meaning |
| --- | --- | --- |
| `is` | one | equal to it |
| `one_of` | any number | equal to any of them |
| `before` | one | strictly less than it |
| `after` | one | greater than **or equal to** it |
| `between` | two | `[first, second)` |
| `exists` | none | carries this key at all |
| `missing` | none | does not carry it |

Ranges are half-open, which is what makes adjacent windows partition a range
without overlapping: *before the 10th* excludes the 10th, *after the 10th*
includes it.

Text comparison folds case and accents the way the word search does, so
`"HIGH"` and `"high"` are one question. The ISO forms are unaffected by
folding, so date ordering is exact.

### `#tag` inside the text

A `#word` in `text` is lifted out and applied as a tag filter — it is not
searched for as a word, does not affect ranking, and matches its descendants
(`#fun` finds `#fun/home`). Passing `tags` explicitly does the same thing and
is clearer from a program; both work, and they combine.

### Two honest signals

- `indexing: true` means a workspace in scope has not finished indexing, so
  the answer may be incomplete. **This is not "no results"** — ask again
  shortly rather than concluding the content is absent.
- `total_is_partial: true` means more pages exist beyond `limit`.

## Files

`list_documents` returns page entries only. `list_files` returns first-class
workspace file metadata: `file_id`, `name`, `mime`, `size`, `hash`,
`root_node_id`, and `note_id`. `search_files` applies a case-insensitive
filename fragment and caps results at 200; `get_file` reads one metadata
record. None returns file bytes.

### A file's note

A file may carry **one note** — an ordinary document attached to it
([ADR-277](../../docs/decisions/ADR-276-300.md#adr-277-filenote--a-files-note-is-its-own-document-registered-as-the-files-manifest-child)).
`note_id` on a file payload is the note's document id, or `null` when the file
has none. Notes never appear in `list_documents`; they are discovered through
their file and through `search_documents`.

`ensure_file_note` takes `workspace_id` and `file_id` and answers `note_id`
plus `created`. It is idempotent: the same call serves *Add note* and *Open
note*, and `created` reports which just happened. A non-file `file_id` is the
store's refusal surfacing as the tool's error. The returned note is a document,
so `read_document` and `edit_document` apply to `note_id` directly. A note no
app has opened yet is an empty document — the store registers it without a
content root, which the first opener seeds (ADR-80), and `read_document`
refuses it until then exactly as it refuses any unseeded document.

`read_file` accepts `file_id`, optional byte `offset`, and optional `length`.
One call returns at most 1 MiB as standard base64 plus `total_bytes` and `eof`;
continue at `offset + length` until `eof`. The hash is metadata, never a public
filesystem path.

## Indexed document search

`search_documents` searches indexed page content without opening every Loro
document. `query` is matched against document titles, blocks, attributes, and
breadcrumbs. Optional `kinds` constrain matching Koru block kinds;
`lifecycle` is `live` by default and may be `trash` or `all`; `sort` is
`relevance` by default and also accepts `observed_modified` or
`last_used_workspace`. `page_size` is clamped to 1–200 and `next_offset`
continues the indexed candidate stream.

`workspace_ids` may narrow the call. Empty means all workspaces authorized for
this caller, not all workspaces in Store. The result contains page and note
hits — a note is the one document attached to a file, found here like any page;
file and workspace hits are omitted. A page can consequently contain fewer
hits than `page_size`, while `next_offset` still advances over the underlying
candidate stream. Each hit carries stable workspace, document, and optional
node IDs plus the semantic field, kind, title, breadcrumb, snippet, match byte
spans, lifecycle, score, and observed local timestamps. Search may be
temporarily behind; `updating_workspaces` reports that state rather than making
the call wait without bound.

The query is sent in a request body and is not written to logs, tool audit,
chat metadata, or the recent-query list. MCP search does not record navigation.

## Chat tools

`list_chats`, `search_chats`, and `read_chat` expose the same read-only catalog
to a connected MCP assistant and to Koru AI's own agent. All three are
read-only, non-destructive, idempotent, and closed-world operations, so they do
not require mutation approval.

A chat is public to tools by default. Marking it **Private chat** removes it
from tool listing and search and makes direct reads answer exactly as a missing
chat does. The user's transcript, history list, and ordinary Koru search still
show private chats. An agent may always see its own calling chat, even when
that chat is private; an external MCP call has no calling-chat exemption.
Branching snapshots the source chat's privacy setting. Message-block context
uses the same rule, so a request cannot smuggle a block from another private
chat into a provider prompt.

`list_chats` is ordered by most recent update and accepts an optional decimal
offset `cursor` plus `page_size` (default 50, maximum 200). It returns chat id,
title, terminal-message count, timestamps, and `next_cursor`.

`search_chats` searches finalized response blocks with the same ranking and
snippet behavior as Koru's chat search. It accepts `query`, optional offset
`cursor`, and `page_size` (default 20, maximum 200), and returns composable chat
and message ids. Privacy is applied in the database query so paging cannot skip
or leak hidden results. Search queries, chat titles, snippets, and results are
not written to logs or tool audit.

`read_chat` accepts `chat_id`, optional `cursor`, and `page_size` (default 20,
maximum 50). It includes terminal messages only. Pages walk backward from the
most recent message while each returned page stays in chronological order. Its
decimal cursor means "before this ordinal"; ordinals are immutable, so walking
the cursor cannot skip or duplicate messages. Interrupted and failed messages
remain visible with their state. Each message body is capped at 16 KiB on a
UTF-8 boundary and reports `truncated: true` when shortened; the result also
reports the complete terminal-message count.

## Preconditions, history, and durability

- Read immediately before writing and send its opaque base revision.
- Operations whose placement or replacement depends on text also send the
  exact expected text.
- Validate a complete edit batch against a fork before changing the live
  document.
- Success is returned only after Store durability acknowledgement.
- MCP mutations capture paired `mcp_before` and `mcp_after` history markers
  sharing a boundary id.
- Restore is forward-moving: it creates new operations matching retained state
  rather than rewinding or replacing the live CRDT.
- **A client changes only what it can read.** `edit_document`,
  `extract_to_document` (its source and an existing `target_document_id`),
  `save_document_version`, `list_document_versions` and
  `restore_document_version` admit exactly the manifest kinds `read_document`
  opens — a page or a file's note. Any other kind is refused before anything is
  written, with `read_document`'s own `invalid_request` naming the field that
  carried it. `compare_document_versions` names history entries rather than a
  document, and those ids reach a client only through the tools above
  ([BACKLOG](../../docs/mcp/BACKLOG.md#consistency)).

`compare_document_versions` answers `changes` — added, removed, moved,
reordered, and modified nodes in the destination's preorder, removals last — and
`unchanged_nodes`, **a count** of the nodes the two versions share untouched.
The versions are identical when `changes` is empty; the count is there because
four changes across two hundred untouched blocks is a different document from
four across five.

Conflicts fail explicitly. The client reads again and replans; the bridge never
guesses which concurrent content to overwrite.

The read-before-write rule applies to changing an existing document. Bulk
creation has no prior revision to read: its strict, ordered seed batch and
idempotency key provide the corresponding safety boundary.

## Crossing documents

Loro tree moves cannot cross Loro documents. A cross-document "move" is:

1. read a `Cutting` from the source;
2. create and durably plant a copy in the destination with fresh node ids; then
3. delete or replace the source in its own document.

Destination-first ordering prevents data loss. If the destination becomes
durable and the source step fails, return a structured error containing the new
document id and state that the source is unchanged. Duplication is recoverable;
lost content is not. High-level tools such as `extract_to_document` own this
coordination rather than asking an LLM to assemble several low-level calls.

`extract_to_document` covers every form of the move: several subtrees at once
(`extra_source_node_ids`, replaced together by one trail pill at the first
node's slot), a destination that already exists (`target_document_id`, content
landing at its top), and a block to land first beneath (`under_node_id`, which
the trail pill then deep-links to). Exactly one of `new_document_title` and
`target_document_id` names the destination; the destination write is durable
before the source is touched, in every form. This is the same machinery the
app's own *Move to document…* uses.

## Extending the model through MCP

MCP is a projection and command adapter over the Koru model, not where a new
model concept begins. Add and test the semantic operation in `WorkspaceDoc`
first; then expose that operation in MCP. If the bridge has to manipulate a
Loro container, mark, anchor, storage row, or snapshot directly to implement a
feature, the model API is missing the operation that every other peer will also
need.

For a new block `Kind`, follow [`BLOCKS.md`](../../guide/contributing/BLOCKS.md) first, then make an
explicit MCP decision even if the answer is "read-only for now". Reads are
exhaustive: `document_json`/`node_json` must continue to describe every node a
workspace can contain. Writable body kinds are the deliberately smaller
`InsertKind` set. Never silently map an unknown or unsupported kind to
`paragraph`; return its real stable token on reads and reject an unsupported
write explicitly.

For a new mutation or object, use this sequence:

1. **Name the public meaning.** Identify stable public IDs, typed values, and
   the user intent. Keep Store ids distinct from `NodeId`, and keep `TreeID`,
   storage keys, Loro containers, anchors, marks, op ids, updates, and snapshots
   out of public inputs.
2. **Put semantics in the model.** Add a rollback-safe `WorkspaceDoc` method
   which validates the complete operation. MCP should call it rather than
   recreate its invariants.
3. **Choose the smallest public vocabulary.** Prefer another strict variant on
   `EditOperation` or `InlineValue` when it composes with the existing tools.
   Add a high-level tool only when it coordinates a semantic transaction, such
   as cross-document extraction, that an LLM should not assemble from several
   failure-prone calls.
4. **Separate existing-document and creation semantics.** A live edit needs a
   base revision and exact expected values. If the operation is also meaningful
   during creation, add the identical wire shape to `InitialOperation`, allow
   references only to the supplied root or earlier inserts, and delegate to the
   ordinary edit implementation. Do not add `move`, `remove`, or another
   operation to seeding merely because it exists for live documents.
5. **Keep the published list small and portable** (see
   [What `tools/list` publishes](#what-toolslist-publishes)). Every request and
   tagged variant uses `deny_unknown_fields`; operation/value enums use
   `#[schemars(inline)]`. What an agent needs from a field's doc comment —
   ordering, UTF-8 coordinates, preconditions, defaults, limits, retry
   behavior — goes, condensed, into the tool's `#[tool]` description, because
   the published schema carries no prose. Every tool publishes a human title
   and explicit MCP safety hints (`readOnlyHint` and `openWorldHint`, plus
   `destructiveHint` and `idempotentHint` on a tool that writes), and no
   `outputSchema`. A new tool adds a sample of each new operation to
   `every_operation_fits_the_published_input_schema` when it adds a tagged
   union, and keeps `the_published_tool_list_is_compact_and_within_its_budget`
   green — raising that budget is a decision to record, not a number to bump.
6. **Authorize, then resolve routes in the adapter.** The shared executor maps
   each tool to its required Koru scope and workspace target before Store is
   touched. `InlineContext` turns public references
   into routed Koru values. The Store client may carry opaque CRDT bytes only on
   a private adapter route; raw bytes never become an MCP input. Every backend
   call retains `RequestContext` tenant, principal, and scopes so Koru Cloud can
   implement the same contract without trusting a local-only assumption.
7. **Return the shared projection.** Mutation results are JSON objects. When a
   mutation creates or materially changes a document, prefer the same shape as
   `read_document` over a smaller one-off response; returned ids and revision
   should remove a read, not force one.
8. **Make failure actionable.** Reject unknown fields, stale ids, failed text
   preconditions, invalid UTF-8 boundaries, unsupported kinds, and reused
   idempotency keys with stable error codes and a concrete reread/retry remedy.
   Never turn a bad field into a successful no-op.
9. **Test every boundary.** Pin the generated JSON Schema (including no
   unresolved `$ref`), strict deserialization, model behavior, Unicode,
   ordering/concurrency cases, Store durability, idempotent replay where
   relevant, semantic read-back, and a real official-SDK stdio exchange.
10. **Move the contract with the code.** Update this document, `docs/developers/API.md`,
    the relevant model guide/ADR, and host-facing user guides in the same
    change. Name every public tool here: a test enforces that. Add a Store
    capability when an adapter depends on new Store support.

This file is itself the runtime `koru://docs/model` resource (installed as
`docs/mcp/README.md`). Development servers read this source from disk on
every request, so this file is the
quickest way to teach a restarted model a new contract without embedding prose
in the binary.
Release packaging copies the same file beside the executable and fails if it is
missing. Do not maintain a second hard-coded version.

## The run endpoint

A delegated run (see the [delegation index](../../docs/features/delegate/README.md)) does
not get the desktop bridge. It gets its own MCP server at
`<gateway>/v1/runs/<RunId>/mcp`, served by the Koru API gateway, with
**six artifact tools and no document tool**: nothing at this endpoint reads,
creates or edits a document, lists a workspace, or searches documents. The
bridge above carries every local workspace under the person's own
authority; a run carries a grant set resolved for it, and the two are
different servers with different credentials (ADR-348).

**Transport.** Streamable HTTP, stateless, JSON responses — the shape
Codex's `mcp_servers.<name>.url` expects. One JSON-RPC 2.0 message per
`POST`, answered as `application/json`; a notification is `202` with no
body; there is no `Mcp-Session-Id`; `GET` and `DELETE` answer `405` with
`{ "code": "method_not_allowed" }`. `initialize`, `ping`, `tools/list` and
`tools/call` are served; any other method is JSON-RPC `-32601`. A message
over 64 KiB is refused: bytes are registered from files, never sent inline.

**Authority.** The bearer is the run's endpoint token, issued once per
attempt by the orchestrator through `POST /v1/artifacts/authorities`
([API.md](../../docs/developers/API.md#artifacts)) and resolved on **every**
message, `initialize` included, against the durable authority record in
`artifacts.db`. **How it reaches the provider is part of the contract**
(ADR-359): the delegated launch carries it as `DelegationLaunch.credential`,
and an adapter presents it to the provider through the provider's own
*environment*, never on its command line — argv is readable by every process
on the machine and an environment block is not. For Codex that is `-c
mcp_servers.koru_run.bearer_token_env_var='KORU_RUN_MCP_TOKEN'` with the
variable set on the child alone
([codex.md](../../docs/features/terminal/providers/codex.md)). A launch that carries no
bearer renders no bearer override, and every call at its endpoint is
`401 authority_unknown`. Nothing in a call says who is calling: the tools take no
run id, no actor and no workspace, every input rejects unknown fields, and
`produced_by`, the output link and the family's creator all come from the
record. A refusal is `401` with one of three codes — `authority_unknown`
(never issued, a gateway session bearer, or a token for another run at
this URL), `stale_generation` (an earlier attempt's token; the orchestrator
advanced the generation) or `authority_revoked` (the run's authority was
revoked). A revocation or a fresh attempt is a row the next call reads, so
a provider that outlives a crashed delegate worker keeps publishing, and a
restarted gateway answers the same token.

**Grants (D28).** The record carries a set of workspace ids, frozen for the
attempt: the delegating workspace by default, removable in the modal; an
empty set denies all workspace access. A family is reachable when its
workspace is granted **or the run itself created it**, so with no grant a
run can still `create` into its own workspace and read and extend what it
made, while `get`, `read`, `search`, `attach_to_run` and `create_version`
on anything else are `permission_denied`.

**The register path rule.** `create` and `create_version` take a `path`
under `<run-workspace>/.artifacts/<RunId>/` — relative to it, or absolute
and inside it. It is refused with a distinct code when it contains `..`
(`path_traversal`), passes through or is a symbolic link (`path_symlink`)
or a junction (`path_junction`), resolves outside the folder after
canonicalization (`path_outside_run_folder`), when the folder itself does
not exist (`run_folder_missing`), when nothing is there (`file_not_found`)
or it is not a regular file (`not_a_file`), when it cannot be read
(`file_unreadable`), and when its size or modification time changed
between the listing and the read (`file_changed`). The bytes are hashed,
filed in the target workspace's blob store, and registered with an id
derived from `(run, attempt, canonical relative path, hash)` — so the same
call again returns the same version with `retried: true`, and the same
path with new bytes is the next version of the same family. No refusal
names the path.

**The tools.** Inputs and outputs are `structuredContent` objects; a
failure is `{ "code", "message" }` (optionally `details`) under `isError`,
never a JSON-RPC error, so the model can read the code. Ids are the
canonical v4 UUID strings of [API.md](../../docs/developers/API.md#artifacts).

| Tool | Input | Output |
| --- | --- | --- |
| `create` | `{ path, title, mime? }` — a new family in the run's workspace, titled `title`; `mime` is `type/subtype`, `application/octet-stream` when absent | `{ version, family, retried }` |
| `create_version` | `{ family, path, mime? }` — the next version of a reachable family, derived from its latest; `mime` defaults to the latest version's | `{ version, family, retried }` |
| `get` | `{ version }` | `{ version, family }` |
| `read` | `{ version }` | `{ version, mime, size_bytes, encoding: "utf8" \| "base64", content }` up to 1 MiB; above it `{ version, mime, size_bytes, reason: "content_too_large", bound_bytes }` and no bytes |
| `search` | `{ query }` — families in the granted workspaces whose title contains `query`, case-insensitive; empty matches all | `{ hits: [{ family, versions }], truncated }`, at most 50 families |
| `attach_to_run` | `{ version }` — record the version as this run's input | `{ link }` |

Failure codes beyond the path rule are the artifact store's
(`artifact_family_not_found`, `artifact_not_found`,
`artifact_family_retired`, `registration_conflict`, `permission_denied`,
`invalid_request`, `artifact_storage_failed`, `artifacts_unavailable`),
plus `unknown_tool` for a name outside the six.

**Not here, by design.** No document-editing tool; no `list_workspaces`;
no provider-native child sessions (§7.3 flags and terminates them); no
callback from the gateway to the run. The instructions the model reads
first are P1.12's preamble, which names `.artifacts/<RunId>/` and these six
tools exactly. `KORU_RUN_MCP_DEBUG` prints one line per call — the tool,
the code, the elapsed time; never an argument, a path or a title.

## Future MCP improvements

What this surface does not do yet, and what being without it costs, is
[BACKLOG.md](../../docs/mcp/BACKLOG.md) — one place, so a follow-up is not recorded twice and
half-fixed once.

## Local and cloud hosting

Local desktop hosts launch `koru-mcp serve --stdio` as their child process. Koru
Store is the persistent service; the stdio bridge is not a separately managed
daemon. A future Koru Cloud host will supply remote transport, tenant,
authenticated principal, and scopes through the existing backend context. Tool
schemas and document semantics remain the same.
