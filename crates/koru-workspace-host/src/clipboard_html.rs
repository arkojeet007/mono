// SPDX-License-Identifier: MPL-2.0

//! Reading the **CF_HTML** off the clipboard — the browser / Google Docs case.
//!
//! Browsers put images in "HTML Format" as `<img src="data:image/…;base64,…">`
//! (embedded, recoverable) or `<img src="https://…">` (remote — the bytes are
//! not on the clipboard). This module walks the CF_HTML fragment in document
//! order, emitting text (tags stripped, entities decoded) and `data:` images as
//! interleaved `RawBlock`s — the same shape RTF extraction returns, so both
//! paths feed `paste_rich` identically.
//!
//! Windows-only at the read edge; parsing is pure and unit-tested.

use base64::Engine as _;

use crate::clipboard_rtf::{RawBlock, RawPicture};
use crate::{
    Heading, ListStyle, RichListSpec, RichTextBlock, RichTextKind, RichTextLink, RichTextRun,
};
use koru_core::{Mark, MarkSet};

/// `KORU_CLIPBOARD_DEBUG=1` on; `=0` / `false` / unset off.
fn clipboard_debug_on() -> bool {
    std::env::var("KORU_CLIPBOARD_DEBUG")
        .map(|v| {
            let v = v.trim();
            !v.is_empty()
                && v != "0"
                && !v.eq_ignore_ascii_case("false")
                && !v.eq_ignore_ascii_case("no")
                && !v.eq_ignore_ascii_case("off")
        })
        .unwrap_or(false)
}

/// Read the raw platform HTML bytes through the shared clipboard mechanism.
pub(crate) fn html_bytes() -> Option<Vec<u8>> {
    koru_clipboard::system()
        .raw(koru_clipboard::RawFormat::Html)
        .ok()
        .flatten()
        .map(|payload| payload.bytes)
}

/// Write plain text **and** CF_HTML in one clipboard session.
///
/// Windows requires `OpenClipboard` before `SetClipboardData`. Calling
/// `set_without_clear` with no open handle silently fails — which is why
/// Notion/OneNote only saw plain text (empty lines for images). One open →
/// empty → both formats → close is the correct multi-format pattern.
pub(crate) fn set_text_and_html(plain: &str, html: &str) {
    let result = koru_clipboard::system().set_text_and_html(plain, html);
    if clipboard_debug_on() {
        eprintln!(
            "[koru:clip] set_text_and_html: plain={} html_bytes={} ok={}",
            plain.len(),
            html.len(),
            result.is_ok(),
        );
    }
}

/// Rewrite every `<img data-koru-hash="…">` placeholder the editor emitted into
/// a real `data:` URI, then re-wrap the CF_HTML header so byte offsets stay
/// honest. `resolve(hash) -> (mime, bytes)` is the shell's blob lookup.
///
/// Placeholders whose hash cannot be resolved are dropped (empty string) so a
/// missing blob does not leave a broken image tag for OneNote to choke on.
pub(crate) fn embed_koru_images(
    html: &str,
    mut resolve: impl FnMut(&str) -> Option<(String, Vec<u8>)>,
) -> String {
    let fragment = cf_html_fragment(html).unwrap_or(html);
    let mut out = String::with_capacity(fragment.len());
    let mut rest = fragment;
    while let Some(start) = rest.find("<img ") {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let end = after.find('>').map(|i| i + 1).unwrap_or(after.len());
        let tag = &after[..end];
        rest = &after[end..];
        if let Some(hash) = attr_value(tag, "data-koru-hash") {
            if let Some((mime, bytes)) = resolve(&hash) {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                let mime = attr_value(tag, "data-mime").unwrap_or(mime);
                let mut img = format!(r#"<img src="data:{mime};base64,{b64}""#);
                if let Some(w) = attr_value(tag, "width") {
                    img.push_str(&format!(r#" width="{w}""#));
                }
                if let Some(h) = attr_value(tag, "height") {
                    img.push_str(&format!(r#" height="{h}""#));
                }
                if let Some(alt) = attr_value(tag, "alt") {
                    img.push_str(&format!(r#" alt="{alt}""#));
                }
                img.push('>');
                out.push_str(&img);
            }
            // else: drop unresolved placeholder
        } else {
            out.push_str(tag);
        }
    }
    out.push_str(rest);
    wrap_cf_html(&out)
}

/// Wrap an HTML fragment body in the CF_HTML header with correct offsets.
fn wrap_cf_html(body: &str) -> String {
    let header = "Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n";
    let pre = "<html>\r\n<body>\r\n<!--StartFragment-->";
    let post = "<!--EndFragment-->\r\n</body>\r\n</html>";
    let start_html = header.len();
    let start_fragment = start_html + pre.len();
    let end_fragment = start_fragment + body.len();
    let end_html = end_fragment + post.len();
    let mut out = String::with_capacity(end_html);
    out.push_str(&format!(
        "Version:0.9\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\nStartFragment:{start_fragment:010}\r\nEndFragment:{end_fragment:010}\r\n"
    ));
    out.push_str(pre);
    out.push_str(body);
    out.push_str(post);
    out
}

/// One frame of the open-element stack, carrying the list context a paragraph
/// inherits while it is inside a `<ul>`/`<ol>`.
#[derive(Clone, Copy)]
struct ListFrame {
    style: ListStyle,
}

/// Nesting counts for the inline tags Koru can represent as marks. Counts,
/// rather than booleans, make malformed-but-readable nested HTML close safely.
#[derive(Default)]
struct InlineState {
    bold: u16,
    italic: u16,
    underline: u16,
    strike: u16,
    code: u16,
}

impl InlineState {
    fn marks(&self) -> MarkSet {
        MarkSet {
            bold: self.bold > 0,
            italic: self.italic > 0,
            underline: self.underline > 0,
            strike: self.strike > 0,
            code: self.code > 0,
        }
    }

    fn change(&mut self, mark: Mark, opening: bool) {
        let depth = match mark {
            Mark::Bold => &mut self.bold,
            Mark::Italic => &mut self.italic,
            Mark::Underline => &mut self.underline,
            Mark::Strike => &mut self.strike,
            Mark::Code => &mut self.code,
        };
        *depth = if opening {
            depth.saturating_add(1)
        } else {
            depth.saturating_sub(1)
        };
    }
}

/// An `<aside>` that is open: a callout box. Its first paragraph settles
/// the icon — a leading emoji, which is not prose — and everything inside
/// it, until `</aside>`, follows the box's start marker.
#[derive(Default)]
struct OpenBox {
    icon: Option<String>,
    /// Whether the icon has been settled — only the first line can carry it.
    settled: bool,
    /// Whether the start marker is in the stream; what follows is inside.
    started: bool,
}

/// Put the open box's start marker in the stream, once, before its first
/// block.
fn start_box(open: &mut Option<OpenBox>, blocks: &mut Vec<RawBlock>) {
    if let Some(open) = open
        && !open.started
    {
        open.started = true;
        blocks.push(RawBlock::CalloutStart {
            icon: open.icon.clone(),
            tint: None,
        });
    }
}

/// Close the open box, if any: its end marker follows what was inside it. A
/// box that never started — nothing was inside it — leaves nothing behind.
fn close_box(open: &mut Option<OpenBox>, blocks: &mut Vec<RawBlock>) {
    if let Some(open) = open.take()
        && open.started
    {
        blocks.push(RawBlock::CalloutEnd);
    }
}

/// Drop the first `cut` bytes of a paragraph — the icon its text led with is
/// not prose — moving its runs and links with it.
fn strip_front(
    text: &mut String,
    runs: &mut Vec<RichTextRun>,
    links: &mut Vec<RichTextLink>,
    cut: usize,
) {
    use koru_import_markdown::shift_span;
    text.drain(..cut);
    runs.retain_mut(|run| shift_span(&mut run.start, &mut run.end, cut));
    links.retain_mut(|link| shift_span(&mut link.start, &mut link.end, cut));
}

/// Move one raw HTML text segment into the decoded paragraph and record its
/// formatted byte range. Adjacent segments with identical marks are coalesced.
fn append_inline(
    raw: &mut String,
    text: &mut String,
    runs: &mut Vec<RichTextRun>,
    inline: &InlineState,
) {
    if raw.is_empty() {
        return;
    }
    let decoded = decode_entities(raw);
    raw.clear();
    let start = text.len();
    text.push_str(&decoded);
    let end = text.len();
    let marks = inline.marks();
    if start == end || marks.is_empty() {
        return;
    }
    if let Some(last) = runs.last_mut()
        && last.end == start
        && last.marks == marks
    {
        last.end = end;
    } else {
        runs.push(RichTextRun { start, end, marks });
    }
}

/// Walk a CF_HTML string in document order, emitting text and `data:` images as
/// interleaved `RawBlock`s.
///
/// Returns `Some` when the fragment carried at least one *structured* block — a
/// `data:` image, a list item, or a heading. Pure unformatted text yields
/// `None`, so the caller falls back to the carried plain text (correctly: there
/// is no structure to preserve). Remote-URL images are skipped — the bytes are
/// not on the clipboard.
pub(crate) fn extract(html: &str) -> Option<Vec<RawBlock>> {
    let fragment = cf_html_fragment(html).unwrap_or(html);
    let mut blocks = Vec::new();
    let mut text = String::new();
    let mut raw_text = String::new();
    let mut runs = Vec::new();
    let mut links = Vec::new();
    let mut saw_picture = false;
    let mut saw_structured = false;

    // The open list stack: each `<ul>`/`<ol>` pushes, its closing tag pops. A
    // paragraph inherits the innermost style and the stack's depth as its
    // nesting. A heading, similarly, tags the next block.
    let mut list_stack: Vec<ListFrame> = Vec::new();
    let mut pending_heading: Option<Heading> = None;
    let mut quote_depth = 0_u16;
    let mut callout: Option<OpenBox> = None;
    let mut inline = InlineState::default();
    let mut open_links: Vec<(usize, String)> = Vec::new();

    let flush = |text: &mut String,
                 runs: &mut Vec<RichTextRun>,
                 links: &mut Vec<RichTextLink>,
                 blocks: &mut Vec<RawBlock>,
                 list_stack: &[ListFrame],
                 quote_depth: u16,
                 pending_heading: &mut Option<Heading>,
                 callout: &mut Option<OpenBox>,
                 saw_structured: &mut bool| {
        if text.trim().is_empty() {
            text.clear();
            runs.clear();
            links.clear();
            *pending_heading = None;
            return;
        }
        let structured = !list_stack.is_empty()
            || pending_heading.is_some()
            || quote_depth > 0
            || callout.is_some()
            || !runs.is_empty()
            || !links.is_empty();
        let kind = if quote_depth > 0 {
            RichTextKind::Quote
        } else {
            RichTextKind::Paragraph
        };
        // The first paragraph inside an open box settles its icon: a plain
        // one's leading emoji is the icon, not prose. Then the box starts,
        // and this block and everything after it are inside.
        if let Some(open) = callout
            && !open.started
            && !open.settled
        {
            open.settled = true;
            let plain = list_stack.is_empty() && pending_heading.is_none() && quote_depth == 0;
            if plain && let Some((icon, cut)) = koru_import_markdown::leading_icon(text) {
                open.icon = Some(icon);
                strip_front(text, runs, links, cut);
                if text.trim().is_empty() {
                    // The icon alone in its paragraph: the box's first line
                    // is the next one.
                    text.clear();
                    runs.clear();
                    links.clear();
                    return;
                }
            }
        }
        start_box(callout, blocks);
        let block = RichTextBlock {
            text: std::mem::take(text),
            list: list_stack.last().map(|f| RichListSpec {
                style: f.style,
                depth: (list_stack.len() - 1) as u16,
            }),
            heading: pending_heading.take(),
            kind,
            runs: std::mem::take(runs),
            links: std::mem::take(links),
        };
        blocks.push(RawBlock::Text(block));
        if structured {
            *saw_structured = true;
        }
    };

    let mut in_tag = false;
    let mut tag = String::new();
    for ch in fragment.chars() {
        if in_tag {
            if ch == '>' {
                in_tag = false;
                let lower = tag.to_ascii_lowercase();
                if lower.starts_with("img") {
                    if let Some(pic) = parse_img_tag(&tag) {
                        flush(
                            &mut text,
                            &mut runs,
                            &mut links,
                            &mut blocks,
                            &list_stack,
                            quote_depth,
                            &mut pending_heading,
                            &mut callout,
                            &mut saw_structured,
                        );
                        start_box(&mut callout, &mut blocks);
                        blocks.push(RawBlock::Picture(pic));
                        saw_picture = true;
                    }
                } else if let Some((url, opening)) = link_action(&tag) {
                    if opening {
                        open_links.push((text.len(), url));
                    } else if let Some((start, url)) = open_links.pop() {
                        let end = text.len();
                        if start < end && !url.is_empty() {
                            links.push(RichTextLink { start, end, url });
                        }
                    }
                } else if let Some((mark, opening)) = inline_action(&lower) {
                    inline.change(mark, opening);
                } else if let Some(action) = tag_action(&lower) {
                    match action {
                        TagAction::BlockEnd => flush(
                            &mut text,
                            &mut runs,
                            &mut links,
                            &mut blocks,
                            &list_stack,
                            quote_depth,
                            &mut pending_heading,
                            &mut callout,
                            &mut saw_structured,
                        ),
                        // Seal the pending text at the *current* depth before
                        // going deeper, so an outer item's text does not get
                        // swept into the inner list. `<li>` opening is a break
                        // too: it closes the previous item's text run.
                        TagAction::ListOpen(style) => {
                            flush(
                                &mut text,
                                &mut runs,
                                &mut links,
                                &mut blocks,
                                &list_stack,
                                quote_depth,
                                &mut pending_heading,
                                &mut callout,
                                &mut saw_structured,
                            );
                            list_stack.push(ListFrame { style });
                        }
                        TagAction::ListClose => {
                            list_stack.pop();
                        }
                        TagAction::Heading(h) => {
                            pending_heading = Some(h);
                        }
                        TagAction::QuoteOpen => {
                            flush(
                                &mut text,
                                &mut runs,
                                &mut links,
                                &mut blocks,
                                &list_stack,
                                quote_depth,
                                &mut pending_heading,
                                &mut callout,
                                &mut saw_structured,
                            );
                            quote_depth = quote_depth.saturating_add(1);
                        }
                        TagAction::QuoteClose => {
                            flush(
                                &mut text,
                                &mut runs,
                                &mut links,
                                &mut blocks,
                                &list_stack,
                                quote_depth,
                                &mut pending_heading,
                                &mut callout,
                                &mut saw_structured,
                            );
                            quote_depth = quote_depth.saturating_sub(1);
                        }
                        // A box does not nest in a box: an aside inside one
                        // closes it first.
                        TagAction::CalloutOpen => {
                            flush(
                                &mut text,
                                &mut runs,
                                &mut links,
                                &mut blocks,
                                &list_stack,
                                quote_depth,
                                &mut pending_heading,
                                &mut callout,
                                &mut saw_structured,
                            );
                            close_box(&mut callout, &mut blocks);
                            callout = Some(OpenBox::default());
                        }
                        TagAction::CalloutClose => {
                            flush(
                                &mut text,
                                &mut runs,
                                &mut links,
                                &mut blocks,
                                &list_stack,
                                quote_depth,
                                &mut pending_heading,
                                &mut callout,
                                &mut saw_structured,
                            );
                            close_box(&mut callout, &mut blocks);
                        }
                    }
                }
                tag.clear();
            } else {
                tag.push(ch);
            }
        } else if ch == '<' {
            append_inline(&mut raw_text, &mut text, &mut runs, &inline);
            in_tag = true;
            tag.clear();
        } else {
            raw_text.push(ch);
        }
    }
    append_inline(&mut raw_text, &mut text, &mut runs, &inline);
    // Flush any remaining text.
    if !text.trim().is_empty() {
        flush(
            &mut text,
            &mut runs,
            &mut links,
            &mut blocks,
            &list_stack,
            quote_depth,
            &mut pending_heading,
            &mut callout,
            &mut saw_structured,
        );
    }
    // An `<aside>` nobody closed still ends with the fragment.
    close_box(&mut callout, &mut blocks);

    if saw_picture || saw_structured {
        Some(blocks)
    } else {
        None
    }
}

/// Opening anchors carry the destination; closing anchors merely seal the byte
/// range accumulated since that opening tag. Unknown/missing hrefs stay text.
/// The name is matched whole: `</aside>` and `</abbr>` are not anchors.
fn link_action(tag: &str) -> Option<(String, bool)> {
    let trimmed = tag.trim();
    let (closing, rest) = match trimmed.strip_prefix('/') {
        Some(rest) => (true, rest),
        None => (false, trimmed),
    };
    let name = rest
        .split_ascii_whitespace()
        .next()?
        .trim_end_matches('/')
        .to_ascii_lowercase();
    if name != "a" {
        return None;
    }
    Some(if closing {
        (String::new(), false)
    } else {
        (attr_value(tag, "href").unwrap_or_default(), true)
    })
}

/// What a tag means to the block structure: a paragraph break, a list
/// open/close, or a heading marker. `None` for inline/unknown tags.
enum TagAction {
    /// Ends the current text block (paragraph break).
    BlockEnd,
    /// Opens a list; carried until the matching close.
    ListOpen(ListStyle),
    /// Closes the innermost list.
    ListClose,
    /// A heading; the next text block inherits this level.
    Heading(Heading),
    /// Opens or closes a semantic block quotation.
    QuoteOpen,
    QuoteClose,
    /// Opens or closes a callout box — an `<aside>`.
    CalloutOpen,
    CalloutClose,
}

/// Map semantic inline HTML onto the mark vocabulary the editor already uses.
/// CSS classes are intentionally ignored: browser copy has stable semantic
/// tags, while generated class names are not a clipboard contract.
fn inline_action(lower: &str) -> Option<(Mark, bool)> {
    let lower = lower.trim();
    let (opening, tag) = match lower.strip_prefix('/') {
        Some(tag) => (false, tag),
        None => (true, lower),
    };
    let tag = tag.split_ascii_whitespace().next()?.trim_end_matches('/');
    let mark = match tag {
        "b" | "strong" => Mark::Bold,
        "i" | "em" => Mark::Italic,
        "u" => Mark::Underline,
        "s" | "strike" | "del" => Mark::Strike,
        "code" => Mark::Code,
        _ => return None,
    };
    Some((mark, opening))
}

fn tag_action(lower: &str) -> Option<TagAction> {
    // Closing tags.
    if lower.starts_with("/ul") || lower.starts_with("/ol") {
        return Some(TagAction::ListClose);
    }
    if lower.starts_with("/blockquote") {
        return Some(TagAction::QuoteClose);
    }
    if lower.starts_with("/aside") {
        return Some(TagAction::CalloutClose);
    }
    if lower.starts_with("/p")
        || lower.starts_with("/div")
        || lower.starts_with("br")
        || lower.starts_with("/h")
        || lower.starts_with("/li")
        || lower == "li"
        || lower.starts_with("li ")
    {
        return Some(TagAction::BlockEnd);
    }
    // Opening list tags — `<ol type="a">` etc. Word and Docs emit the type
    // attribute; its default is `1` (numbered).
    if lower.starts_with("ul") {
        return Some(TagAction::ListOpen(ListStyle::Bullet));
    }
    if lower.starts_with("ol") {
        return Some(TagAction::ListOpen(list_style_for_ol(lower)));
    }
    if lower == "blockquote" || lower.starts_with("blockquote ") {
        return Some(TagAction::QuoteOpen);
    }
    if lower == "aside" || lower.starts_with("aside ") {
        return Some(TagAction::CalloutOpen);
    }
    // Opening heading tags.
    let h = if lower.starts_with("h1") {
        Some(Heading::H1)
    } else if lower.starts_with("h2") {
        Some(Heading::H2)
    } else if lower.starts_with("h3") {
        Some(Heading::H3)
    } else if lower.starts_with("h4") {
        Some(Heading::H4)
    } else if lower.starts_with("h5") {
        Some(Heading::H5)
    } else if lower.starts_with("h6") {
        Some(Heading::H6)
    } else {
        None
    };
    h.map(TagAction::Heading)
}

/// Map an `<ol>`'s `type` attribute to a list style. Default `1` is numbered.
fn list_style_for_ol(lower: &str) -> ListStyle {
    // `type` may be unquoted (`type=a`) or quoted (`type="a"`).
    let after = lower.split("type").nth(1);
    let t = after
        .and_then(|s| s.trim_start_matches(['=', ' ', '"', '\'']).chars().next())
        .unwrap_or('1');
    match t {
        'a' => ListStyle::Letter,
        'i' => ListStyle::Roman,
        _ => ListStyle::Numbered,
    }
}

/// Extract `src`, `width`, `height` from an `<img …>` tag. Returns a
/// `RawPicture` if `src` is a `data:` URI; `None` for remote/blob/file URLs.
fn parse_img_tag(tag: &str) -> Option<RawPicture> {
    let src = attr_value(tag, "src")?;
    let data = src.strip_prefix("data:image/")?;
    let semi = data.find(';')?;
    let mime_str = &data[..semi];
    let payload = data[semi + 1..].strip_prefix("base64,")?;
    let end = payload.find('"').unwrap_or(payload.len());
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&payload[..end])
        .ok()?;
    if bytes.is_empty() {
        return None;
    }
    let width = attr_value(tag, "width").and_then(|w| w.parse::<u32>().ok());
    let height = attr_value(tag, "height").and_then(|h| h.parse::<u32>().ok());
    let name = attr_value(tag, "alt")
        .filter(|s| !s.is_empty())
        .or_else(|| attr_value(tag, "title").filter(|s| !s.is_empty()));
    Some(RawPicture {
        bytes,
        mime: static_mime(mime_str),
        width,
        height,
        name,
    })
}

/// Extract the value of a named attribute from an HTML tag string.
fn attr_value(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let needle = format!("{name}=\"");
    let start = lower.find(&needle)? + needle.len();
    let rest = &tag[start..]; // original case (for data: payloads)
    let end = rest.find('"').unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

/// Parse the CF_HTML header to find the fragment offsets and slice it out.
fn cf_html_fragment(html: &str) -> Option<&str> {
    let start = parse_header_int(html, "StartFragment")?;
    let end = parse_header_int(html, "EndFragment")?;
    if end <= start || end > html.len() {
        return None;
    }
    html.get(start..end)
}

fn parse_header_int(html: &str, key: &str) -> Option<usize> {
    let line = html.lines().find(|l| l.starts_with(key))?;
    let val = line.split(':').nth(1)?;
    val.trim().parse::<usize>().ok()
}

fn static_mime(s: &str) -> &'static str {
    match s {
        "png" => "image/png",
        "jpeg" | "jpg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk\
                           YPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

    #[test]
    fn text_and_data_image_interleaved() {
        let html =
            format!(r#"<p>hello</p><img src="data:image/png;base64,{PNG_B64}"><p>world</p>"#);
        let blocks = extract(&html).expect("has a picture");
        assert_eq!(blocks.len(), 3, "{blocks:?}");
        assert!(matches!(&blocks[0], RawBlock::Text(t) if t.text.contains("hello")));
        assert!(matches!(&blocks[1], RawBlock::Picture(_)));
        assert!(matches!(&blocks[2], RawBlock::Text(t) if t.text.contains("world")));
    }

    #[test]
    fn skips_remote_url_images() {
        let html = r#"<p>hello</p><img src="https://example.com/photo.jpg"><p>world</p>"#;
        // No data: image → None (falls back to carried text).
        assert!(extract(html).is_none());
    }

    #[test]
    fn extracts_width_and_height() {
        let html =
            format!(r#"<img src="data:image/png;base64,{PNG_B64}" width="800" height="600">"#);
        let blocks = extract(&html).expect("has a picture");
        match &blocks[0] {
            RawBlock::Picture(p) => {
                assert_eq!(p.width, Some(800));
                assert_eq!(p.height, Some(600));
            }
            _ => panic!("expected Picture"),
        }
    }

    #[test]
    fn entity_decoding() {
        let html = format!(
            r#"<p>hello &amp; goodbye &lt;tag&gt;</p><img src="data:image/png;base64,{PNG_B64}">"#
        );
        let blocks = extract(&html).expect("has a picture");
        match &blocks[0] {
            RawBlock::Text(t) => {
                assert!(t.text.contains("hello & goodbye <tag>"), "got: {}", t.text);
            }
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn multiple_data_images_in_order() {
        let html = format!(
            r#"<img src="data:image/png;base64,{PNG_B64}">text<img src="data:image/jpeg;base64,{PNG_B64}">"#
        );
        let blocks = extract(&html).expect("has pictures");
        assert_eq!(blocks.len(), 3);
        assert!(matches!(&blocks[0], RawBlock::Picture(p) if p.mime == "image/png"));
        assert!(matches!(&blocks[1], RawBlock::Text(_)));
        assert!(matches!(&blocks[2], RawBlock::Picture(p) if p.mime == "image/jpeg"));
    }

    #[test]
    fn no_data_image_returns_none() {
        let html = r#"<p>just text</p>"#;
        assert!(extract(html).is_none());
    }

    #[test]
    fn extracts_alt_as_name() {
        let html = format!(r#"<img src="data:image/png;base64,{PNG_B64}" alt="Diagram of flow">"#);
        let blocks = extract(&html).expect("has a picture");
        match &blocks[0] {
            RawBlock::Picture(p) => {
                assert_eq!(p.name.as_deref(), Some("Diagram of flow"));
            }
            _ => panic!("expected Picture"),
        }
    }

    #[test]
    fn falls_back_to_title_when_no_alt() {
        let html = format!(r#"<img src="data:image/png;base64,{PNG_B64}" title="Chart">"#);
        let blocks = extract(&html).expect("has a picture");
        match &blocks[0] {
            RawBlock::Picture(p) => assert_eq!(p.name.as_deref(), Some("Chart")),
            _ => panic!("expected Picture"),
        }
    }

    /// A `<ul>` pastes its items as bullet list blocks, even with no picture on
    /// the clipboard — the gate now opens for any structured block.
    #[test]
    fn an_html_ul_pastes_as_bullet_items() {
        let html = r"<ul><li>one</li><li>two</li><li>three</li></ul>";
        let blocks = extract(html).expect("a list is structured");
        assert_eq!(blocks.len(), 3, "{blocks:?}");
        for b in &blocks {
            match b {
                RawBlock::Text(t) => assert_eq!(
                    t.list,
                    Some(RichListSpec {
                        style: ListStyle::Bullet,
                        depth: 0
                    })
                ),
                _ => panic!("expected Text, got {b:?}"),
            }
        }
    }

    /// `<ol type="a">` carries the letter style; the default `<ol>` is numbered.
    #[test]
    fn an_html_ol_type_a_pastes_as_letter_items() {
        let html = r#"<ol type="a"><li>x</li></ol><ol><li>y</li></ol>"#;
        let blocks = extract(html).expect("a list is structured");
        assert!(matches!(
            &blocks[0],
            RawBlock::Text(t) if t.list == Some(RichListSpec { style: ListStyle::Letter, depth: 0 })
        ));
        assert!(matches!(
            &blocks[1],
            RawBlock::Text(t) if t.list == Some(RichListSpec { style: ListStyle::Numbered, depth: 0 })
        ));
    }

    #[test]
    fn a_chatgpt_list_item_preserves_bold_and_inline_code() {
        let html = concat!(
            "<ol><li><strong>Identify canonical documents.</strong> ",
            "For each important subject, decide which document represents the current truth. ",
            "For example: <code>architecture/README.md</code>, <code>SEARCH.md</code>.",
            "</li></ol>",
        );
        let blocks = extract(html).expect("the numbered item is structured");
        let RawBlock::Text(text) = &blocks[0] else {
            panic!("expected text, got {:?}", blocks[0]);
        };
        let marked: Vec<_> = text
            .runs
            .iter()
            .map(|run| (&text.text[run.start..run.end], run.marks))
            .collect();
        assert!(
            marked
                .iter()
                .any(|(value, marks)| { *value == "Identify canonical documents." && marks.bold })
        );
        assert!(
            marked
                .iter()
                .any(|(value, marks)| *value == "architecture/README.md" && marks.code)
        );
        assert!(
            marked
                .iter()
                .any(|(value, marks)| *value == "SEARCH.md" && marks.code)
        );
    }

    #[test]
    fn inline_mark_ranges_follow_decoded_utf8_bytes() {
        let html = "<p><strong>日本語 &amp; café</strong> then <code>文件.md</code></p>";
        let blocks = extract(html).expect("inline marks are structured");
        let RawBlock::Text(text) = &blocks[0] else {
            panic!("expected text, got {:?}", blocks[0]);
        };
        assert!(text.runs.iter().any(|run| {
            &text.text[run.start..run.end] == "日本語 & café" && run.marks.bold
        }));
        assert!(
            text.runs
                .iter()
                .any(|run| { &text.text[run.start..run.end] == "文件.md" && run.marks.code })
        );
    }

    #[test]
    fn semantic_html_anchor_preserves_its_destination() {
        let html = r#"<p>Read <a href="https://example.com/review">the review</a>.</p>"#;
        let blocks = extract(html).expect("a link is structured");
        let RawBlock::Text(text) = &blocks[0] else {
            panic!("expected text, got {:?}", blocks[0]);
        };
        assert_eq!(text.text, "Read the review.");
        assert_eq!(text.links.len(), 1);
        let link = &text.links[0];
        assert_eq!(&text.text[link.start..link.end], "the review");
        assert_eq!(link.url, "https://example.com/review");
    }

    /// Nested `<ul>` raises the depth on the inner items.
    #[test]
    fn nested_html_lists_preserve_depth() {
        let html = r"<ul><li>top<ul><li>inner</li></ul></li></ul>";
        let blocks = extract(html).expect("a list is structured");
        assert!(matches!(
            &blocks[0],
            RawBlock::Text(t) if t.list == Some(RichListSpec { style: ListStyle::Bullet, depth: 0 })
                && t.text == "top"
        ));
        assert!(matches!(
            &blocks[1],
            RawBlock::Text(t) if t.list == Some(RichListSpec { style: ListStyle::Bullet, depth: 1 })
                && t.text == "inner"
        ));
    }

    /// A heading tags its block, and is enough on its own to open the gate.
    #[test]
    fn an_html_heading_tags_its_block() {
        let html = r"<h2>A title</h2><p>body</p>";
        let blocks = extract(html).expect("a heading is structured");
        assert!(matches!(&blocks[0], RawBlock::Text(t) if t.heading == Some(Heading::H2)));
        assert!(matches!(&blocks[1], RawBlock::Text(t) if t.heading.is_none()));
    }

    #[test]
    fn a_chatgpt_style_html_blockquote_tags_each_quoted_paragraph() {
        // ChatGPT's browser copy carries ordinary semantic `<blockquote>` HTML
        // with changing CSS classes around it. Semantics, not those classes,
        // are the stable clipboard contract.
        let html = r#"<p>before</p><blockquote class="border-l-2 pl-4"><p>one</p><p>two</p></blockquote><p>after</p>"#;
        let blocks = extract(html).expect("a blockquote is structured");
        let kinds: Vec<_> = blocks
            .iter()
            .filter_map(|block| match block {
                RawBlock::Text(text) => Some(text.kind.clone()),
                RawBlock::Picture(_) | RawBlock::CalloutStart { .. } | RawBlock::CalloutEnd => None,
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                RichTextKind::Paragraph,
                RichTextKind::Quote,
                RichTextKind::Quote,
                RichTextKind::Paragraph,
            ]
        );
    }

    /// An `<aside>` pastes as a callout box: its start marker carries the
    /// icon the first paragraph led with; that paragraph and the paragraphs
    /// and lists after it are inside the box, which ends where the aside
    /// does. The marks around the icon survive the icon being cut away.
    #[test]
    fn an_html_aside_pastes_as_a_callout_box() {
        let html = concat!(
            "<p>before</p>",
            "<aside><p>💡 <b>Keep</b> it</p><p>inside</p><ul><li>one</li></ul></aside>",
            "<p>after</p>",
        );
        let blocks = extract(html).expect("a callout is structured");
        assert_eq!(blocks.len(), 7, "{blocks:?}");
        assert!(
            matches!(&blocks[0], RawBlock::Text(t) if t.kind == RichTextKind::Paragraph && t.text == "before")
        );
        assert!(matches!(
            &blocks[1],
            RawBlock::CalloutStart { icon: Some(icon), tint: None } if icon == "💡"
        ));
        let RawBlock::Text(text) = &blocks[2] else {
            panic!("expected the first line, got {:?}", blocks[2]);
        };
        assert_eq!(text.kind, RichTextKind::Paragraph);
        assert_eq!(text.text, "Keep it");
        assert_eq!(&text.text[text.runs[0].start..text.runs[0].end], "Keep");
        assert!(
            matches!(&blocks[3], RawBlock::Text(t) if t.kind == RichTextKind::Paragraph && t.text == "inside")
        );
        assert!(matches!(&blocks[4], RawBlock::Text(t) if t.list.is_some() && t.text == "one"));
        assert!(matches!(&blocks[5], RawBlock::CalloutEnd));
        assert!(
            matches!(&blocks[6], RawBlock::Text(t) if t.kind == RichTextKind::Paragraph && t.text == "after")
        );
    }

    /// A later heading must not be consumed by the first one's close or by the
    /// ordinary paragraphs between them. This is the shape a copied rendered
    /// Markdown response uses when its section labels are semantic headings.
    #[test]
    fn repeated_html_headings_each_tag_their_own_block() {
        let html = concat!(
            "<h2>Where these concepts belong</h2>",
            "<p>I'd now think of the documents like this:</p>",
            "<h3>VISION/WHY</h3>",
            "<p>Why Koru should exist.</p>",
            "<h3>PRINCIPLES</h3>",
            "<p>How Koru should behave.</p>",
            "<h3>GOALS</h3>",
            "<p>What we need to make true.</p>",
        );
        let blocks = extract(html).expect("headings are structured");
        let headings: Vec<_> = blocks
            .iter()
            .filter_map(|block| match block {
                RawBlock::Text(text) => text.heading,
                RawBlock::Picture(_) | RawBlock::CalloutStart { .. } | RawBlock::CalloutEnd => None,
            })
            .collect();

        assert_eq!(
            headings,
            vec![Heading::H2, Heading::H3, Heading::H3, Heading::H3]
        );
    }

    /// Pure unstructured text still returns `None`, so the editor's plain-text
    /// path handles it — that is the right path for text with no structure.
    #[test]
    fn unstructured_text_still_returns_none() {
        assert!(extract(r"<p>just text</p>").is_none());
        assert!(extract(r"<div><span>x</span></div>").is_none());
    }

    #[test]
    fn embed_koru_images_rewrites_placeholders_to_data_uris() {
        let body = r#"<p>hi</p><img data-koru-hash="h1" data-mime="image/png" width="1" height="1" alt=""><p>bye</p>"#;
        let html = wrap_cf_html(body);
        let png = base64::engine::general_purpose::STANDARD
            .decode(PNG_B64.replace(char::is_whitespace, ""))
            .expect("fixture");
        let out = embed_koru_images(&html, |hash| {
            assert_eq!(hash, "h1");
            Some(("image/png".into(), png.clone()))
        });
        assert!(out.contains("data:image/png;base64,"), "{out}");
        assert!(!out.contains("data-koru-hash"), "{out}");
        assert!(out.contains("<p>hi</p>"), "{out}");
        assert!(out.contains("<p>bye</p>"), "{out}");
        // Round-trip through the reader.
        let blocks = extract(&out).expect("embedded picture");
        assert!(matches!(&blocks[1], RawBlock::Picture(_)), "{blocks:?}");
    }
}
