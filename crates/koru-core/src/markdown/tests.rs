// SPDX-License-Identifier: MPL-2.0

use super::*;

/// Three is the floor: a body with no backticks, or with only short runs, gets
/// the fence everybody writes.
#[test]
fn a_plain_body_gets_three_backticks() {
    assert_eq!(code_fence(""), "```");
    assert_eq!(code_fence("fn main() {}"), "```");
    assert_eq!(code_fence("a `tick` and ``two``"), "```");
}

/// A body holding a three-backtick run would close a three-backtick fence
/// early, so the fence grows to four.
#[test]
fn a_three_backtick_run_in_the_body_lengthens_the_fence_to_four() {
    assert_eq!(code_fence("```\ncode\n```"), "````");
}

/// ...and a four-backtick run grows it to five. One longer than the longest
/// run, never more.
#[test]
fn a_four_backtick_run_in_the_body_lengthens_the_fence_to_five() {
    assert_eq!(code_fence("see ````raw```` here"), "`````");
    assert_eq!(code_fence("```\n````\n```"), "`````");
}

/// The whole block: fence and language on the first line, the body verbatim,
/// the fence again — and no newline after it.
#[test]
fn a_fenced_block_puts_the_language_on_the_opening_fence() {
    assert_eq!(
        fenced_code("rust", "fn main() {\n}"),
        "```rust\nfn main() {\n}\n```"
    );
}

/// No language is an empty info string, which is what Markdown means by it.
#[test]
fn a_block_without_a_language_has_a_bare_fence() {
    assert_eq!(fenced_code("", "x = 1"), "```\nx = 1\n```");
}

/// A body that already ends in a newline is not given a second one, so an
/// empty last line in the editor is not two in the file.
#[test]
fn a_trailing_newline_in_the_body_is_not_doubled() {
    assert_eq!(fenced_code("py", "x = 1\n"), "```py\nx = 1\n```");
}

/// The fence the block is wrapped in is the lengthened one, on both ends.
#[test]
fn a_fenced_block_uses_the_lengthened_fence_on_both_ends() {
    assert_eq!(fenced_code("md", "a\n```\nb"), "````md\na\n```\nb\n````");
}
