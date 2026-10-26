<!-- Thank you. Please keep one change per pull request. -->

## What and why

<!-- What this changes, and why. Link the issue or proposal it implements. -->

## How it was checked

<!-- The tests you added or ran, and anything you checked by hand. -->

## Checklist

- [ ] `cargo test --workspace` passes (`koru.bat test` / `./koru.sh test`).
- [ ] `cargo fmt --all --check` is clean.
- [ ] New files have their SPDX header (`python scripts/checks/spdx-headers.py`).
- [ ] No third-party file or binary is committed that can be fetched
      ([third-party/](../third-party/README.md)).
- [ ] Docs that describe the changed behavior are updated; a contested
      decision is settled on its issue or proposal, linked above.
- [ ] Nothing is copied from another project, or its license and origin are
      named here.
