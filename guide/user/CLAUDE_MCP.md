# Connect Claude Desktop to Koru

Claude Desktop can launch Koru's local MCP bridge directly over stdio.

## Prepare Koru

From the repository root, build Koru and verify the bridge against an isolated
Store root:

```bat
koru.bat run-all --mcp
```

Leave Koru Store running. `run-all --mcp` builds and checks the bridge; it does
not keep an MCP stdio process running. Claude must own that child process.

## Claude Code

The command-line Claude registers the bridge in one step, from the repository
root:

```bat
koru.bat mcp-add
```

(`./koru.sh mcp-add` on macOS and Linux.) It builds `koru-mcp`, then runs
`claude mcp add --scope user koru -- <repo>\target\debug\koru-mcp.exe serve --stdio`,
with `--root` added when `KORU_ROOT` is set, so the bridge serves the same
store the window shows. User scope is deliberate: Claude's default, local
scope, ties a server to the directory the command ran in, and a Claude
started anywhere else — a terminal inside Koru, say — does not list it. Give a name to register it as something else —
`koru.bat mcp-add mcp_test` — and take it away again with
`koru.bat mcp-remove [name]`. Koru Store has to be running for the tools to
answer; `claude mcp list` shows what is registered.

Run `mcp-add` again after changing Koru: the build is part of the verb, so the
registered bridge is always this checkout's latest. A Claude Code session that
is already open keeps the bridge it started; open a new session, or ask it to
reconnect with `/mcp`, to get the rebuilt one.

## Configure Claude

Open `%APPDATA%\Claude\claude_desktop_config.json` and merge this entry into
the existing JSON. Replace `<repo>` with the absolute repository path.

```json
{
  "mcpServers": {
    "koru": {
      "command": "<repo>\\target\\debug\\koru-mcp.exe",
      "args": [
        "serve",
        "--stdio",
        "--root",
        "<repo>\\.dev-data\\history-smoke"
      ]
    }
  }
}
```

JSON does not allow trailing commas or comments. Validate the file before
restarting Claude:

```powershell
Get-Content "$env:APPDATA\Claude\claude_desktop_config.json" -Raw | ConvertFrom-Json
```

Fully quit Claude, including its notification-area process, then reopen it.
Koru should appear under the tools/connectors menu. Claude starts and stops the
bridge; do not separately run `koru.bat mcp --console` for this connection.

## Discover the Koru model

Before asking Claude to edit, ask:

> Use Koru's `get_documentation` tool with
> `{"uri":"koru://docs/model"}`. Summarise the node model and the supported
> `edit_document` operations before making any changes.

`koru://docs/model` is also exposed as a standard MCP resource. Some bridges,
including versions of remote-device proxies, do not forward `resources/list`
or `resources/read`; `get_documentation` returns the same canonical document as
a normal tool call and is the portable fallback. An empty resource list from a
proxy does not mean Koru failed to publish the resource. The `edit_document` input
schema itself is fully inlined, so Claude should not receive unresolved
`$ref`/`$defs` pointers.

## Smoke test

Ask Claude:

> Use Koru's `get_documentation` tool for `koru://docs/model`. Then create a
> document called "Claude bulk-create smoke" in one `create_document` call. Use
> a UUID idempotency key and `initial_operations` containing three ordered
> paragraphs and one task. Read it back, verify the create result equals the
> read projection, and tell me whether the `create_document` and
> `edit_document` schemas contain unresolved `$ref` values.

This checks documentation discovery, the host's refreshed tool schema,
content-first creation, ordering, returned node ids, and semantic read-back. A
second call with the same key and identical input must return the original
success without adding another document; changing the input while retaining the
key must return `idempotency_conflict`.

For the cross-document path, ask Claude:

> Use Koru. List my workspaces and documents. Read the History smoke document,
> then extract its first paragraph into a new document called "Claude extracted
> paragraph" and leave a /document reference where the paragraph was. Read both
> documents again and report their new revisions.

Claude sends semantic MCP calls. It does not create Loro operations, nested
document nodes, anchors, marks, or attributes itself.

## Troubleshooting

- `store_unreachable` means no Koru Store is listening for that exact `--root`.
  Start it; the bridge dials again on the next call and needs no restart of its
  own. `storage_failed` is the other answer — Store is running and its storage
  failed.
- A JSON parse position from Claude means the desktop config is invalid; run
  the PowerShell validation command.
- If tools use old schemas or accept `ordinal`, fully quit Claude so its old
  child process exits, rebuild `koru-mcp`, and reopen Claude.
- If `koru://docs/model` does not appear in Claude's resource browser, call
  `get_documentation` directly. If that tool is also absent, Claude or its
  proxy is still connected to an older `koru-mcp.exe`.
- A revision conflict is safe: ask Claude to read the document again and retry.
- Use an isolated root while testing because MCP write tools modify documents.
- MCP connection, resource, and tool-call diagnostics are written to
  `<store-root>\logs\koru-mcp.log.YYYY-MM-DD`. Each startup line includes the
  executable path, process id, and version, which makes an old proxy-launched
  binary visible. Logs roll daily and retain seven files.

Anthropic's current local-server guidance recommends packaged MCPB extensions
for distribution. The raw config above is appropriate for this development
binary; a future Koru release can package the same stdio command as MCPB.

References: [Anthropic local MCP servers](https://support.claude.com/en/articles/10949351-getting-started-with-local-mcp-servers-on-claude-desktop), [Anthropic custom connectors](https://support.claude.com/en/articles/11175166-get-started-with-custom-connectors-using-remote-mcp)
