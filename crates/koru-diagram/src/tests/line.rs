// SPDX-License-Identifier: MPL-2.0

//! The line arithmetic and the line rule, on spans built by hand.

use merman::{ParseDiagnosticSpanKind as SpanKind, SourceSpan};

use crate::line::{last_non_empty_line, line_for_span};
use crate::line_of;

#[test]
fn line_of_counts_the_newlines_before_the_offset() {
    let text = "flowchart LR\n    A --> B\n    B --> C\n";
    assert_eq!(line_of(text, 0), 1, "a line start");
    assert_eq!(line_of(text, 12), 1, "the newline belongs to its line");
    assert_eq!(line_of(text, 13), 2, "the next line start");
    assert_eq!(line_of(text, 17), 2, "mid-line");
    assert_eq!(line_of(text, text.len() - 1), 3, "the final newline");
    assert_eq!(line_of(text, text.len()), 4, "the end: the empty last line");
    assert_eq!(line_of(text, text.len() + 100), 4, "past the end: clamped");
    assert_eq!(line_of("", 0), 1);
    assert_eq!(line_of("", 5), 1);
    assert_eq!(line_of("a\r\nb", 3), 2, "a CRLF ending counts once");
}

#[test]
fn the_last_non_empty_line_ignores_trailing_whitespace() {
    assert_eq!(last_non_empty_line("a\nb\n"), 2);
    assert_eq!(last_non_empty_line("a\nb\n\n   \n"), 2);
    assert_eq!(last_non_empty_line("a\nb"), 2);
    assert_eq!(last_non_empty_line("a"), 1);
    assert_eq!(last_non_empty_line(""), 1);
    assert_eq!(last_non_empty_line("  \n\n"), 1);
}

#[test]
fn the_line_rule_names_the_span_or_the_last_non_empty_line() {
    let text = "flowchart LR\n    A -> B\n    B --> C\n\n";
    let last = 3;
    assert_eq!(last_non_empty_line(text), last);
    let arrow = SourceSpan::new(19, 21);
    assert_eq!(&text[arrow.start..arrow.end], "->");

    // An exact span names its line; so does an insertion point mid-text.
    assert_eq!(line_for_span(text, Some(arrow), Some(SpanKind::Exact)), 2);
    assert_eq!(
        line_for_span(
            text,
            Some(SourceSpan::new(19, 19)),
            Some(SpanKind::InsertionPoint)
        ),
        2
    );
    // A fallback span is not vouched for: the last non-empty line.
    assert_eq!(
        line_for_span(text, Some(arrow), Some(SpanKind::Fallback)),
        last
    );
    // No span at all: the last non-empty line.
    assert_eq!(line_for_span(text, None, None), last);
    assert_eq!(line_for_span(text, None, Some(SpanKind::Exact)), last);
    // An insertion point at the end of the input, as a missing `end` gives:
    // the last non-empty line, not the blank lines after it.
    let end = text.len();
    assert_eq!(
        line_for_span(
            text,
            Some(SourceSpan::new(end, end)),
            Some(SpanKind::InsertionPoint)
        ),
        last
    );
    // At the last non-whitespace byte as well.
    let last_byte = text.trim_end().len() - 1;
    assert_eq!(
        line_for_span(
            text,
            Some(SourceSpan::new(last_byte, last_byte + 1)),
            Some(SpanKind::Exact)
        ),
        last
    );
    // Past the end of the text: still the last non-empty line, never a panic.
    assert_eq!(
        line_for_span(
            text,
            Some(SourceSpan::new(end + 50, end + 60)),
            Some(SpanKind::Exact)
        ),
        last
    );
    // A blank text is all line 1.
    assert_eq!(
        line_for_span("  \n", Some(SourceSpan::new(0, 1)), Some(SpanKind::Exact)),
        1
    );
}
