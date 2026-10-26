# Koru release descriptors

Every cloud release has one descriptor named after its full display version:

    releases/0.1.0.toml
    releases/0.1.1-beta.0.toml
    releases/0.1.1-nightly.20261101+abc1234.toml

The publisher infers and prints the version first. If the descriptor does not
exist, it offers to generate one: enter `codex` for Codex CLI, `kimi` for Kimi
Code, or anything else to stop and write it yourself. There is no default
generator. Set `KORU_NOTES_COMMAND` to a command of your own (`{channel}` in it
is replaced by the channel, and `{path}` by the exact file to write) and the
publisher also offers it, as `y`.
Generated notes are printed in full for approval before publishing; `--yes`
still requires an existing descriptor. Publishing itself needs Koru Works'
release credentials, which are not in this repository
([release runbook](../docs/contributing/operations/RUNBOOK.md#releasing)).
A descriptor is:

    title = "A faster, more reliable Koru"
    summary = [
      "Documents now open faster.",
      "Keyboard navigation is more consistent.",
    ]
    body = """
    ## What changed

    Long-form release notes use Markdown.
    """

[WRITING.md](WRITING.md) holds the shared release-writing instructions. Codex
and Kimi receive its full contents in their prompts, and a `KORU_NOTES_COMMAND`
generator can read the same file. The release tool embeds it, so updating the
guide means rebuilding the tool.

The path is never passed as a command-line option; the full release version
always determines it. The Markdown becomes an immutable HTML release page on
the update origin.

`title` and `summary` are now read by strangers scanning a list, not only by
somebody already on the notes page: they are the row for this release on
https://updates.koru.do/releases/. Write them so they still make sense beside
twenty other releases.

**The GitHub nightly is the exception.** Its notes are dated files in
[nightly/](nightly/README.md), drafted by `./koru.sh nightly-notes` with the
same generators, and that folder *is* the record of what each nightly said.

**This directory is not the record of what shipped.** The origin is. A
descriptor can exist for a release that was never published, and a published
release can have no descriptor here -- every release before 0.1.0 has none -- so
the index page is rebuilt from `history/` on the update origin and never from
these files. See [ADR-147](../docs/decisions/ADR-126-150.md#adr-147-releaseindex--the-release-index-is-derived-from-the-origin-keyed-by-channel-build-and-sorted-by-build-number).
