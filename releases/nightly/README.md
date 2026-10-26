# Nightly notes

The notes of every GitHub nightly — the pre-release on the moving `nightly`
tag of koruworks/mono — one file per build, named for its UTC date:
`2026-10-24.toml`, then `2026-10-24-2.toml` for a second build that day. Each
holds the descriptor's three fields (`title`, `summary`, `body`), text only.

`./koru.sh nightly-notes` drafts the next file from the commits since the
`nightly` tag. Edit it, commit it, then move and push the tag; the build takes
the one file the push added (`koru-release nightly-notes --select`).

**This folder is the archive.** The release page on GitHub shows only the
newest build, and koru.do lists this folder for the history, so a file is never
edited or removed once its nightly is published.
