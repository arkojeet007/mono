# Upstream issues drafted, not filed

Koru's notices carry the license text of every crate it ships. For the crates
below, upstream has **no license file** at the revision published to crates.io,
so the text in `crates/koru-release/licences/crates/<crate>.txt` is the standard
text of the license the crate declares, under our recorded judgment that the
declared SPDX license and the Cargo authors (or the repository owner) are the
best evidence there is. That judgment is half of the release disposition; the
other half is asking upstream for a license file. These are the requests,
drafted for a maintainer to file under their own account. Once a crate
publishes a release whose package carries its license, the generator copies the
crate's own file and the record stops being used; delete it then.

Filed: none yet.

## gpu-descriptor

Crates `gpu-descriptor` 0.3.2 and `gpu-descriptor-types` 0.2.0. File at
<https://github.com/zakarumych/gpu-descriptor/issues/new>.

> **Title:** COPYING points at license/LICENSE-MIT and license/LICENSE-APACHE, which do not exist
>
> Hello, and thank you for gpu-descriptor. We ship it (through wgpu) in a
> desktop app and reproduce the license of every crate we distribute.
>
> `COPYING` says the project is licensed under Apache-2.0 or MIT, "<license/LICENSE-APACHE
> or …> or the MIT license <license/LICENSE-MIT or …>". Neither file is in the
> repository at the revisions published as gpu-descriptor 0.3.2 (bb28b4e) and
> gpu-descriptor-types 0.2.0 (9f2fb95), nor in the crates.io packages, so a
> redistributor has no license text to copy, only the SPDX expression in
> `Cargo.toml`.
>
> Would you add `LICENSE-MIT` and `LICENSE-APACHE` (at the paths `COPYING`
> names, or at the root with `COPYING` updated), and include them in the
> published packages (`include`/`exclude` in each `Cargo.toml`)? Until then we
> use the standard MIT text with "Copyright 2020 The gpu-descriptor Project
> Developers" from `COPYING`.

## hexf

Crate `hexf-parse` 0.2.1. File at <https://github.com/lifthrasiir/hexf/issues/new>.

> **Title:** Add a license file (CC0-1.0) to the repository and the crates.io packages
>
> Hello, and thank you for hexf. We ship `hexf-parse` (through naga) and
> reproduce the license of every crate we distribute.
>
> `Cargo.toml` declares CC0-1.0, but neither the repository at the revision
> published as hexf-parse 0.2.1 (4225763) nor the package contains a license
> file. Would you add the CC0 1.0 legal code as `LICENSE` and publish it with
> the crates? Until then we reproduce the standard CC0 1.0 text on the strength
> of the declaration.

## rustls-cert-utils

Crates `reloadable-core` 0.1.0, `reloadable-state` 0.1.0,
`rustls-cert-file-reader` 0.4.2, `rustls-cert-read` 0.3.0 and
`rustls-cert-reloadable-resolver` 0.7.1. File at
<https://github.com/MOZGIII/rustls-cert-utils/issues/new>.

> **Title:** Add a license file (MIT) and authors so the crates can be attributed
>
> Hello, and thank you for these crates. We ship them (through iroh) and
> reproduce the license and copyright notice of every crate we distribute.
>
> Each crate's `Cargo.toml` declares MIT, but the repository at the published
> revisions has no license file, the packages carry none, and `authors` is
> empty, so there is no copyright line to reproduce. The MIT license requires
> one. Would you add a `LICENSE` (MIT, with the copyright holder you intend),
> include it in each published crate, and ideally set `authors`? Until then we
> reproduce the standard MIT text with "MOZGIII and the rustls-cert-utils
> contributors" as the holder.

## rust-block

Crate `block` 0.1.6 (macOS, through the `objc` family). A request has been
open since 2021: issue <https://github.com/SSheldon/rust-block/issues/13> and
pull request <https://github.com/SSheldon/rust-block/pull/14>, which adds an MIT
file. So this is a comment on the pull request, not a new issue.

> Hello, and thank you for block. We ship it in a desktop app and reproduce the
> license of every crate we distribute.
>
> `Cargo.toml` declares MIT, but neither the repository (at the 0.1.6 tag,
> 4717879, or today) nor the crates.io package carries a license file, so a
> redistributor has no text to copy. #14 would fix that; would you merge it
> (with you as the holder, as your malloc_buf's LICENSE has it) and
> publish a release whose package includes it? Until then we reproduce the
> standard MIT text with "Copyright (c) Steven Sheldon", from the Cargo
> authors.

## rust-dispatch

Crate `dispatch` 0.2.0 (macOS). Issue
<https://github.com/SSheldon/rust-dispatch/issues/18> (2021) asked for the
license text and was closed without adding it, so this is a new issue. File at
<https://github.com/SSheldon/rust-dispatch/issues/new>.

> **Title:** Add the MIT license text to the repository and the crates.io package
>
> Hello, and thank you for dispatch. We ship it in a desktop app and reproduce
> the license of every crate we distribute.
>
> `Cargo.toml` declares MIT, but the repository at the revision published as
> 0.2.0 (82d6c7a), its head, and the package carry no license file. #18 raised
> this in 2021 and was closed, but the file was never added. Would you add a
> `LICENSE` (MIT, with you as the holder, as your malloc_buf's has it) and
> publish a release that includes it? Until then we reproduce the standard MIT
> text with "Copyright (c) Steven Sheldon", from the Cargo authors.

## stylo

Crate `selectors` 0.37.0 (reached through `lol_html`, from `merman-core`; the
0.38.0 package is the same). File at
<https://github.com/servo/stylo/issues/new>.

> **Title:** Add the MPL-2.0 license text to the repository and the `selectors` package
>
> Hello, and thank you for stylo. We ship `selectors` (through lol_html) in a
> desktop app and reproduce the license of every crate we distribute.
>
> `selectors/Cargo.toml` declares MPL-2.0, but neither the repository at the
> revisions published as selectors 0.37.0 (6cfce6f) and 0.38.0 (572ecba), nor
> its head, nor the crates.io packages contain a license file — not at the
> root and not in `selectors/` or `servo_arc/`. Would you add the MPL 2.0 text
> as `LICENSE` at the root (as servo/servo has it) and include it in the
> published packages (`include` in each crate's `Cargo.toml`)? Until then we
> reproduce the standard MPL 2.0 text on the strength of the declaration.

### Not requested: objc2

The `objc2-*`, `block2` and `dispatch2` crates Koru links were published from
revisions whose `LICENSE.md` names the license but holds no text. Upstream
fixed that on 2026-03-18 (<https://github.com/madsmtm/objc2/issues/826>,
commit ee9a7ad, `LICENSE-MIT.txt`, `LICENSE-APACHE.txt`, `LICENSE-ZLIB.txt`),
so their records use those texts as `Disposition: upstream` and nothing needs
filing. Releases after the fix (`objc2` 0.6.5 and 0.5.3, 2026-10-05) may carry
the files; once Cargo.lock moves to versions that do, delete the records the
generator no longer uses.
