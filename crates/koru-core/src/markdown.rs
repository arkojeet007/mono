// SPDX-License-Identifier: MPL-2.0

//! The one piece of Markdown the model spells itself: how a code block is
//! fenced.
//!
//! Koru stores no Markdown delimiters — a heading is `NodeMeta.heading`, a
//! code block is [`Kind::Code`](crate::node::Kind::Code) with its grammar in
//! `NodeMeta.language` — so Markdown exists only at the edges, where a block
//! leaves for another application: the Markdown exporter and the clipboard's
//! plain-text flavor. Both write a code block as a fence, and both have to
//! write the *same* fence or a block copied and the same block exported would
//! read differently in the next editor. This module is that agreement.
//!
//! The rule is CommonMark's: a fence is three or more backticks, and the
//! closing fence has to be at least as long as the opening one, so a body that
//! itself holds three backticks — Markdown about Markdown — needs a fence of
//! four or the body closes the block early.

/// The backtick fence for a code block holding `body`: the shortest run of at
/// least three backticks that is longer than any backtick run in the body.
///
/// Without the lengthening, a body holding ```` ``` ```` would end the block at
/// that line and leave the rest as prose. Pure: the same body always gets the
/// same fence, which is what lets the exporter and the clipboard agree.
pub fn code_fence(body: &str) -> String {
    let longest = body.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    "`".repeat((longest + 1).max(3))
}

/// A whole fenced code block: the fence and the language on one line, the body
/// as it is, and the fence again — with no trailing newline, so the caller
/// decides what follows (a blank line in a document, the next row on the
/// clipboard).
///
/// `language` is written verbatim, empty included: an empty info string is
/// how Markdown says "no language", and that is what a block set to
/// auto-detect means here too. A body that already ends in a newline does not
/// get a second one, so an empty last line in the block does not become two.
pub fn fenced_code(language: &str, body: &str) -> String {
    let fence = code_fence(body);
    let mut out = String::with_capacity(fence.len() * 2 + language.len() + body.len() + 2);
    out.push_str(&fence);
    out.push_str(language);
    out.push('\n');
    out.push_str(body);
    if !body.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&fence);
    out
}

#[cfg(test)]
mod tests;
