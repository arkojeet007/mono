// SPDX-License-Identifier: MPL-2.0

//! Lines: how a byte offset in a diagram's text becomes the line Koru names.

use merman::{ParseDiagnosticSpanKind, SourceSpan};

/// The 1-based line of `byte_offset` in `text`.
///
/// Lines are split on `\n` (a `\r\n` ending counts once), so the result is the
/// row an editor shows. An offset at or past the end of the text names the
/// last line — for a text ending in a newline, the empty line after it.
pub fn line_of(text: &str, byte_offset: usize) -> u32 {
    let end = byte_offset.min(text.len());
    let newlines = text.as_bytes()[..end]
        .iter()
        .filter(|&&byte| byte == b'\n')
        .count();
    u32::try_from(newlines)
        .unwrap_or(u32::MAX)
        .saturating_add(1)
}

/// The 1-based line holding the last non-whitespace character, or `1` when
/// there is none.
pub(crate) fn last_non_empty_line(text: &str) -> u32 {
    match last_non_whitespace_byte(text) {
        Some(index) => line_of(text, index),
        None => 1,
    }
}

/// The index of the last byte of the last non-whitespace character.
fn last_non_whitespace_byte(text: &str) -> Option<usize> {
    text.trim_end().len().checked_sub(1)
}

/// The line Koru names for a parse error, from merman's span.
///
/// The line of the span's start — except that the **last non-empty line** is
/// named when there is no span, when merman marks the span `Fallback` and so
/// does not vouch for it, or when the span starts at or beyond the last
/// non-whitespace byte, which is where an `InsertionPoint` for a missing
/// `end` lands. That line is where the person has to type.
pub(crate) fn line_for_span(
    text: &str,
    span: Option<SourceSpan>,
    span_kind: Option<ParseDiagnosticSpanKind>,
) -> u32 {
    let last = last_non_empty_line(text);
    let (
        Some(span),
        Some(ParseDiagnosticSpanKind::Exact | ParseDiagnosticSpanKind::InsertionPoint),
    ) = (span, span_kind)
    else {
        return last;
    };
    match last_non_whitespace_byte(text) {
        Some(last_byte) if span.start < last_byte => line_of(text, span.start),
        _ => last,
    }
}
