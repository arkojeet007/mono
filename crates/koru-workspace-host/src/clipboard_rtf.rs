// SPDX-License-Identifier: MPL-2.0

//! Reading the **RTF** off the clipboard — the mixed-content case (Word).
//!
//! When a Word document with paragraphs and inline pictures is copied, the
//! clipboard holds the pictures inside the "Rich Text Format" entry, as
//! `{\pict\pngblip …hex…}` groups — not as a bitmap and not as a file list.
//! The shared bitmap path reads only the single `CF_DIB`, so without this reader
//! every picture but the first is dropped and the rest of the paste arrives as
//! text only.
//!
//! Lexing is delegated to [`rtf_parser`]; this module walks its token stream to
//! pull out body paragraphs and picture bytes **in document order**, which is
//! what an interleaved paste needs. The higher-level `rtf_parser::parse` is not
//! used: it drops pictures and leaks their hex into the body text, so it cannot
//! recover either the bytes or the order.
//!
//! Windows-only at the read edge (`Rich Text Format` is a registered Win32
//! clipboard format); the parsing here is pure and unit-tested cross-platform.

use rtf_parser::lexer::Lexer;
use rtf_parser::tokens::{ControlWord, Property, Token};

/// Temporary diagnostic for the RTF extractor. Off unless
/// `KORU_CLIPBOARD_DEBUG` is set; stderr only. Remove once Word image paste is
/// pinned down.
fn rtf_dbg(line: impl AsRef<str>) {
    let on = std::env::var("KORU_CLIPBOARD_DEBUG")
        .map(|v| {
            let v = v.trim();
            !v.is_empty()
                && v != "0"
                && !v.eq_ignore_ascii_case("false")
                && !v.eq_ignore_ascii_case("no")
                && !v.eq_ignore_ascii_case("off")
        })
        .unwrap_or(false);
    if on {
        eprintln!("[koru:clip] {}", line.as_ref());
    }
}

/// Control words that open a *destination* whose contents carry no body text:
/// font and color tables, document info, the `\nonshppict` duplicate of a
/// picture, embedded objects, field instructions. Skipping them keeps their raw
/// data out of the paragraphs. Word wraps every picture in `{\shppict{\pict…}}`
/// and a `{\nonshppict{\pict…}}` fallback; `nonshppict` is skipped here so the
/// image is not extracted twice.
const SKIP_DESTINATIONS: &[&str] = &[
    r"\fonttbl",
    r"\colortbl",
    r"\stylesheet",
    r"\filetbl",
    r"\info",
    r"\header",
    r"\footer",
    r"\footnote",
    r"\listtable",
    r"\listoverridetable",
    r"\rsidtbl",
    r"\userprops",
    r"\nonshppict",
    r"\fldinst",
    r"\object",
    r"\themedata",
    r"\colorschememapping",
    r"\datastore",
    r"\latentstyles",
];

/// A picture pulled from a `{\pict …}` group, bytes still raw. The shell decodes
/// them with `image::load_from_memory`; this type is the pure output of
/// [`extract`] and touches no disk and no clipboard.
#[derive(Debug)]
pub(crate) struct RawPicture {
    pub bytes: Vec<u8>,
    #[allow(dead_code)]
    pub mime: &'static str,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// A source name (`<img alt>`, filename, …) if the source carried one.
    /// RTF pictures carry none; CF_HTML and file-list may.
    #[allow(dead_code)]
    pub name: Option<String>,
}

/// A piece of the document in arrival order: a paragraph of text, a picture,
/// or an edge of a callout box (which only HTML carries).
#[derive(Debug)]
pub(crate) enum RawBlock {
    // The append-after paste reads pictures only (the text is already pasted by
    // the editor), so the text here is read only by tests / for future
    // interleaving — kept so `extract` still reports the document's structure.
    #[allow(dead_code)]
    Text(crate::RichTextBlock),
    Picture(RawPicture),
    /// A callout box opens; the blocks up to its end are inside it. `icon`
    /// is an emoji and `tint` an `AccentColor` token, `None` the default.
    CalloutStart {
        icon: Option<String>,
        tint: Option<String>,
    },
    /// The box closes.
    CalloutEnd,
}

/// Read raw platform RTF through the shared clipboard mechanism. Interpretation
/// remains here because the token stream has document semantics.
pub(crate) fn rtf_bytes() -> Option<Vec<u8>> {
    koru_clipboard::system()
        .raw(koru_clipboard::RawFormat::Rtf)
        .ok()
        .flatten()
        .map(|payload| payload.bytes)
}

#[derive(Default)]
struct PictureAccum {
    hex: String,
    kind: PictureKind,
    /// `\picwgoalN` in twips (1/1440 inch). ÷15 → display pixels.
    picwgoal: Option<i32>,
    /// `\pichgoalN` in twips.
    pichgoal: Option<i32>,
}

#[derive(Default, Clone, Copy)]
enum PictureKind {
    #[default]
    Unknown,
    Png,
    Jpeg,
    Bmp,
    /// Windows metafile / enhanced metafile — vector. The `image` crate cannot
    /// decode these, so they are dropped rather than shown as a broken block.
    Vector,
}

/// What a single open RTF group is being used for as `extract` walks the tokens.
enum Mode {
    /// Body text contributes; nested groups classify themselves.
    Body,
    /// A destination whose contents are ignored (fonttbl, `\*\generator`, …).
    Skip,
    /// A `\pict` group; collect hex + kind until the matching brace.
    Picture(PictureAccum),
}

/// One frame of the open-group stack. `uc` is the `\uc` unicode-ignore-count in
/// force when the group was entered (restored on close).
struct Frame {
    mode: Mode,
    uc: i32,
    /// True if this frame is Word's `\nonshppict` group (the legacy duplicate of
    /// an image). Its `\pict` children must be skipped to avoid double-pasting.
    is_nonshppict: bool,
}

/// Walk `rtf`'s token stream into body paragraphs and pictures in document
/// order.
///
/// Returns `Some` only when at least one decodable picture was found — that is
/// the signal a rich paste is wanted. A clipboard whose RTF carries no picture
/// (formatted text alone) yields `None`, and the caller pastes plain text.
pub(crate) fn extract(rtf: &str) -> Option<Vec<RawBlock>> {
    let tokens = match Lexer::scan(rtf) {
        Ok(t) => {
            rtf_dbg(format!("extract: scan ok, {} tokens", t.len()));
            t
        }
        Err(e) => {
            rtf_dbg(format!("extract: Lexer::scan failed: {e:?}"));
            return None;
        }
    };
    let mut blocks: Vec<RawBlock> = Vec::new();
    let mut para = String::new();
    let mut saw_picture = false;

    // One frame per open group. `uc` is the `\uc` unicode-ignore-count in force
    // *when the group was entered*; it is restored on close, the same scope rule
    // the RTF spec applies.
    let mut stack: Vec<Frame> = vec![Frame {
        mode: Mode::Body,
        uc: 1,
        is_nonshppict: false,
    }];
    let mut uc: i32 = 1;

    let len = tokens.len();
    let mut i = 0;
    while i < len {
        match &tokens[i] {
            Token::OpeningBracket => {
                // A `\pict` group is always a picture we want, wherever Word
                // nested it — inside `\*\shpinst`, `\*\picprop`, `\shppict`, or
                // bare. Override any inherited skip. The sole exception is
                // `\nonshppict` (Word's legacy duplicate of the same image):
                // its `\pict` children are skipped to avoid double-pasting.
                let first = first_meaningful_at(&tokens, i + 1).and_then(|j| tokens.get(j));
                let is_pict = matches!(
                    first,
                    Some(Token::ControlSymbol((cw, _)))
                        if matches!(*cw, ControlWord::Unknown(r"\pict"))
                );
                let is_nonshppict_group = matches!(
                    first,
                    Some(Token::ControlSymbol((cw, _)))
                        if matches!(*cw, ControlWord::Unknown(r"\nonshppict"))
                );
                let inside_nonshppict = stack.iter().any(|f| f.is_nonshppict);
                let inherits_skip = matches!(
                    stack.last(),
                    Some(Frame {
                        mode: Mode::Skip | Mode::Picture(_),
                        ..
                    })
                );
                let mode = if is_pict && !inside_nonshppict {
                    Mode::Picture(PictureAccum::default())
                } else if inherits_skip {
                    Mode::Skip
                } else {
                    child_mode(&tokens, i + 1)
                };
                stack.push(Frame {
                    mode,
                    uc,
                    is_nonshppict: is_nonshppict_group,
                });
            }
            Token::ClosingBracket => {
                if let Some(frame) = stack.pop() {
                    uc = frame.uc;
                    if let Mode::Picture(acc) = frame.mode {
                        let kind = match acc.kind {
                            PictureKind::Png => "pngblip",
                            PictureKind::Jpeg => "jpegblip",
                            PictureKind::Bmp => "dibitmap/wbitmap",
                            PictureKind::Vector => "emfblip/wmetafile",
                            PictureKind::Unknown => "unknown",
                        };
                        let hex_len = acc.hex.len();
                        match finalize_picture(acc) {
                            Some(pic) => {
                                rtf_dbg(format!(
                                    "extract: finalized {kind} ({} hex chars -> {} bytes)",
                                    hex_len,
                                    pic.bytes.len()
                                ));
                                flush_paragraph(&mut para, &mut blocks);
                                blocks.push(RawBlock::Picture(pic));
                                saw_picture = true;
                            }
                            None => rtf_dbg(format!(
                                "extract: dropped picture {kind} ({} hex chars) — vector or undecodable",
                                hex_len
                            )),
                        }
                    }
                }
            }
            Token::ControlSymbol((cw, prop)) => match stack.last_mut() {
                Some(Frame {
                    mode: Mode::Picture(acc),
                    ..
                }) => picture_control(acc, *cw, *prop),
                Some(Frame {
                    mode: Mode::Skip, ..
                }) => {}
                Some(Frame {
                    mode: Mode::Body, ..
                }) => match *cw {
                    ControlWord::Unicode => {
                        handle_unicode(&tokens, &mut i, *prop, uc, &mut para);
                    }
                    ControlWord::UnicodeIgnoreCount => uc = prop.get_value(),
                    // A paragraph or line break ends the current row.
                    ControlWord::Par | ControlWord::Line => {
                        flush_paragraph(&mut para, &mut blocks);
                    }
                    ControlWord::Tab => para.push('\t'),
                    ControlWord::Emdash => para.push('\u{2014}'),
                    ControlWord::Endash => para.push('\u{2013}'),
                    ControlWord::Bullet => para.push('\u{2022}'),
                    ControlWord::LeftSingleQuote => para.push('\u{2018}'),
                    ControlWord::RightSingleQuote => para.push('\u{2019}'),
                    ControlWord::LeftDoubleQuote => para.push('\u{201C}'),
                    ControlWord::RightDoubleQuote => para.push('\u{201D}'),
                    _ => {}
                },
                None => {}
            },
            Token::PlainText(text) => match stack.last_mut() {
                Some(Frame {
                    mode: Mode::Picture(acc),
                    ..
                }) => acc.hex.push_str(text),
                Some(Frame {
                    mode: Mode::Body, ..
                }) => para.push_str(text),
                _ => {}
            },
            // Raw line breaks in the RTF source are insignificant whitespace,
            // not content (a real paragraph break is the `\par` control word).
            Token::CRLF | Token::IgnorableDestination | Token::Empty => {}
        }
        i += 1;
    }

    flush_paragraph(&mut para, &mut blocks);
    if saw_picture { Some(blocks) } else { None }
}

/// Close the current paragraph as a `Text` block, unless it is empty. Empty
/// paragraphs are dropped: this path always carries pictures, and a blank row
/// beside an image is not worth the noise it adds.
fn flush_paragraph(para: &mut String, blocks: &mut Vec<RawBlock>) {
    if !para.is_empty() {
        blocks.push(RawBlock::Text(crate::RichTextBlock {
            text: std::mem::take(para),
            list: None,
            heading: None,
            kind: crate::RichTextKind::Paragraph,
            runs: Vec::new(),
            links: Vec::new(),
        }));
    }
}

/// Index of the first meaningful token at or after `start`, skipping raw line
/// breaks and the parser's padding — used to classify a freshly opened group.
fn first_meaningful_at(tokens: &[Token], start: usize) -> Option<usize> {
    let mut j = start;
    while j < tokens.len() {
        match &tokens[j] {
            Token::CRLF | Token::Empty => j += 1,
            _ => return Some(j),
        }
    }
    None
}

/// Classify a group from the tokens after its `{` into Body / Skip / Picture.
fn child_mode<'a>(tokens: &'a [Token<'a>], after_brace: usize) -> Mode {
    let Some(a) = first_meaningful_at(tokens, after_brace) else {
        return Mode::Body;
    };
    match &tokens[a] {
        // `\*\foo` is an ignorable destination (metadata: `\*\generator`,
        // `\*\shpinst`, `\*\picprop`, …). Skip it; if it nests a `\pict`, the
        // `is_pict` override in the caller enters that.
        Token::IgnorableDestination => Mode::Skip,
        Token::ControlSymbol((cw, _)) => match *cw {
            ControlWord::Unknown(r"\pict") => Mode::Picture(PictureAccum::default()),
            ControlWord::Unknown(name) if is_skip_destination(name) => Mode::Skip,
            // `fonttbl`, `colortbl`, `filetbl` and `stylesheet` are *known*
            // control-word variants, so they never reach the `Unknown` arm above.
            ControlWord::FontTable
            | ControlWord::ColorTable
            | ControlWord::FileTable
            | ControlWord::StyleSheet => Mode::Skip,
            _ => Mode::Body,
        },
        _ => Mode::Body,
    }
}

/// Collect a run of `\uN` / `\'hh` escapes, dropping the `\uc`-counted fallback
/// bytes, and append the resulting text to the paragraph. Mirrors the rule in
/// `rtf_parser`'s parser: the value after a unicode escape may be a fallback
/// (a byte `<= 255`) that readers who understand unicode must skip.
///
/// `i` is the index of the first escape in the run; it is advanced to the last
/// one consumed, and the caller's loop then steps past it.
fn handle_unicode(tokens: &[Token], i: &mut usize, first: Property, uc: i32, para: &mut String) {
    let mut codes: Vec<u16> = Vec::new();
    if let Ok(v) = first.get_unicode_value() {
        codes.push(v);
    }
    while *i + 1 < tokens.len() {
        if let Token::ControlSymbol((ControlWord::Unicode, p)) = &tokens[*i + 1] {
            if let Ok(v) = p.get_unicode_value() {
                codes.push(v);
            }
            *i += 1;
        } else {
            break;
        }
    }

    let mut keep = vec![true; codes.len()];
    let mut ignore_counter = 0i32;
    for idx in 1..codes.len() {
        if codes[idx] <= 255 && ignore_counter < uc {
            ignore_counter += 1;
            keep[idx] = false;
        } else {
            ignore_counter = 0;
        }
    }
    let codes: Vec<u16> = codes
        .into_iter()
        .enumerate()
        .filter(|(idx, _)| keep[*idx])
        .map(|(_, v)| v)
        .collect();
    if let Ok(s) = String::from_utf16(&codes) {
        para.push_str(&s);
    }
}

fn is_skip_destination(name: &str) -> bool {
    SKIP_DESTINATIONS.contains(&name)
}

/// Record a picture's format kind and display dimensions from its control words.
fn picture_control(acc: &mut PictureAccum, cw: ControlWord, prop: Property) {
    if let ControlWord::Unknown(name) = cw {
        match name {
            r"\pngblip" => acc.kind = PictureKind::Png,
            r"\jpegblip" => acc.kind = PictureKind::Jpeg,
            r"\dibitmap" | r"\wbitmap" => acc.kind = PictureKind::Bmp,
            r"\wmetafile" | r"\wmetafile8" | r"\emfblip" => acc.kind = PictureKind::Vector,
            r"\picwgoal" => acc.picwgoal = Some(prop.get_value()),
            r"\pichgoal" => acc.pichgoal = Some(prop.get_value()),
            _ => {}
        }
    }
}

/// Decode a finished picture's hex and tag it with a MIME type + display dims,
/// or drop it.
fn finalize_picture(acc: PictureAccum) -> Option<RawPicture> {
    let bytes = hex_decode(&acc.hex)?;
    let mime = match acc.kind {
        PictureKind::Png => "image/png",
        PictureKind::Jpeg => "image/jpeg",
        PictureKind::Bmp => "image/bmp",
        // Vector formats (EMF/WMF) and anything untagged: drop.
        _ => return None,
    };
    // `\picwgoal`/`\pichgoal` are in twips (1/1440 inch). At 96 DPI, 1 inch =
    // 96 px = 1440 twips → px = twips / 15.
    let twips_to_px = |t: Option<i32>| t.map(|v| (v as f32 / 15.0).round() as u32);
    Some(RawPicture {
        bytes,
        mime,
        width: twips_to_px(acc.picwgoal),
        height: twips_to_px(acc.pichgoal),
        name: None,
    })
}

/// Hex-decode the picture payload, ignoring the whitespace RTF allows inside it.
fn hex_decode(hex: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut hi: Option<u8> = None;
    for b in hex.bytes() {
        let n = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => continue,
        };
        match hi {
            None => hi = Some(n),
            Some(h) => {
                bytes.push((h << 4) | n);
                hi = None;
            }
        }
    }
    // An odd trailing nibble is malformed; ignore it.
    if bytes.is_empty() { None } else { Some(bytes) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-pixel PNG as hex, the smallest real image the decoder will accept.
    const PNG_HEX: &str = "89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4\
                          890000000d49444154789c6300010000000500010090fc7f9a0000000049454e44ae426082";
    const JPEG_HEX: &str = "ffd8ffe000104a46494600010100000100010000ffdb004300030222030222\
                            0303030303030303030303030303030303030303030303030303030303030303\
                            030303030303030303030ffc9000b080001000101011100ffcc00060010100501\
                            ffda000801010000000000fffe000b48656c6c6f0affd9";

    fn pict_hex(kind: &str, hex: &str) -> String {
        format!(r"{{\rtf1\ansi before\par{{\pict{kind} {hex}}}\par after}}",)
    }

    #[test]
    fn text_and_one_png_in_order() {
        let rtf = pict_hex(r"\pngblip", PNG_HEX);
        let blocks = extract(&rtf).expect("has a picture");
        assert_eq!(blocks.len(), 3, "{blocks:?}");
        match &blocks[0] {
            RawBlock::Text(t) => assert_eq!(t.text, "before"),
            other => panic!("expected Text, got {other:?}"),
        }
        assert!(matches!(blocks[1], RawBlock::Picture(_)));
        match &blocks[2] {
            RawBlock::Text(t) => assert_eq!(t.text, "after"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn two_pictures_keep_their_order_across_paragraphs() {
        let rtf = format!(
            r"{{\rtf1\ansi one\par{{\pict\pngblip {png}}}\par two\par{{\pict\jpegblip {jpg}}}}}",
            png = PNG_HEX,
            jpg = JPEG_HEX,
        );
        let blocks = extract(&rtf).expect("has pictures");
        let kinds: Vec<&str> = blocks
            .iter()
            .map(|b| match b {
                RawBlock::Text(t) => t.text.as_str(),
                RawBlock::Picture(p) => p.mime,
                RawBlock::CalloutStart { .. } | RawBlock::CalloutEnd => "",
            })
            .collect();
        assert_eq!(kinds, vec!["one", "image/png", "two", "image/jpeg"]);
    }

    #[test]
    fn rtf_without_a_picture_is_none() {
        let rtf = r"{\rtf1\ansi just some text\par and more}";
        assert!(extract(rtf).is_none());
    }

    #[test]
    fn fonttbl_does_not_leak_into_body_text() {
        // With a picture so `extract` returns Some, the font table's text must
        // not appear among the blocks.
        let rtf = format!(
            r"{{\rtf1\ansi{{\fonttbl\f0 ShouldNotLeak;}}visible\par{{\pict\pngblip {png}}}}}",
            png = PNG_HEX,
        );
        let blocks = extract(&rtf).expect("has a picture");
        let body: String = blocks
            .iter()
            .filter_map(|b| match b {
                RawBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
        assert!(body.contains("visible"));
        assert!(!body.contains("ShouldNotLeak"), "font table leaked: {body}");
    }

    #[test]
    fn nonshppict_duplicate_is_not_extracted_twice() {
        // Word's real shape: a `{\*\shppict{\pict…}}` (ignorable destination)
        // followed by a `{\nonshppict{\pict…}}` legacy copy of the same image.
        // The shppict is entered and read; the nonshppict duplicate is skipped.
        let rtf = format!(
            r"{{\rtf1\ansi{{\*\shppict{{\pict\pngblip {png}}}}}{{\nonshppict{{\pict\pngblip {png}}}}}}}",
            png = PNG_HEX,
        );
        let blocks = extract(&rtf).expect("has a picture");
        let count = blocks
            .iter()
            .filter(|b| matches!(b, RawBlock::Picture(_)))
            .count();
        assert_eq!(count, 1, "the nonshppict duplicate must be skipped");
    }

    #[test]
    fn shppict_picture_interleaves_with_text_in_order() {
        // The real Word case the whole feature is for: text, then a picture
        // wrapped in `\*\shppict`, then more text — landing in that order.
        let rtf = format!(
            r"{{\rtf1\ansi before\par{{\*\shppict{{\pict\pngblip {png}}}}}\par after}}",
            png = PNG_HEX,
        );
        let blocks = extract(&rtf).expect("has a picture");
        assert_eq!(blocks.len(), 3, "{blocks:?}");
        assert!(matches!(&blocks[0], RawBlock::Text(t) if t.text == "before"));
        assert!(matches!(blocks[1], RawBlock::Picture(_)));
        assert!(matches!(&blocks[2], RawBlock::Text(t) if t.text == "after"));
    }

    #[test]
    fn other_ignorable_destinations_are_still_skipped() {
        // `\*\shppict` must be entered, but `\*\generator` and friends stay
        // skipped so their text never leaks into the body.
        let rtf = format!(
            r"{{\rtf1\ansi{{\*\generator Should Not Leak}}body\par{{\pict\pngblip {png}}}}}",
            png = PNG_HEX,
        );
        let blocks = extract(&rtf).expect("has a picture");
        let body: String = blocks
            .iter()
            .filter_map(|b| match b {
                RawBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
        assert!(body.contains("body"));
        assert!(
            !body.contains("Should Not Leak"),
            "generator leaked: {body}"
        );
    }

    #[test]
    fn picture_inside_shpinst_wrapper_is_extracted() {
        // The real Word shape that failed: `{\shppict{\*\shpinst{\pict…}}}`.
        // `\*\shpinst` is an ignorable destination (Skip), but the `is_pict`
        // override enters the inner `\pict` regardless.
        let rtf = format!(
            r"{{\rtf1\ansi text\par{{\shppict{{\*\shpinst{{\pict\pngblip {png}}}}}}}\par after}}",
            png = PNG_HEX,
        );
        let blocks = extract(&rtf).expect("has a picture");
        assert!(blocks.iter().any(|b| matches!(b, RawBlock::Picture(_))));
    }

    #[test]
    fn picprop_inside_pict_does_not_interfere() {
        // Word puts `{\*\picprop…}` as a child of `\pict`. The picprop is
        // skipped (inherited skip), the `\pngblip` after it is captured.
        let rtf = format!(
            r"{{\rtf1\ansi{{\pict{{\*\picprop\shplid1028{{\sp{{\sn shapeType}}{{\sv 75}}}}}}\pngblip {png}}}}}",
            png = PNG_HEX,
        );
        let blocks = extract(&rtf).expect("has a picture");
        let count = blocks
            .iter()
            .filter(|b| matches!(b, RawBlock::Picture(_)))
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn shppict_raster_with_nonshppict_vector_yields_one_raster() {
        // The user's exact doc shape: each image as `{\*\shppict{\pict\pngblip…}}`
        // (raster) followed by `{\nonshppict{\pict\wmetafile8…}}` (vector legacy).
        let rtf = format!(
            r"{{\rtf1\ansi text\par{{\*\shppict{{\pict\pngblip {png}}}}}{{\nonshppict{{\pict\wmetafile8 010203}}}}\par after}}",
            png = PNG_HEX,
        );
        let blocks = extract(&rtf).expect("has a picture");
        let pics: Vec<_> = blocks
            .iter()
            .filter_map(|b| match b {
                RawBlock::Picture(p) => Some(p),
                _ => None,
            })
            .collect();
        assert_eq!(
            pics.len(),
            1,
            "one raster; the nonshppict vector copy is skipped"
        );
        assert_eq!(pics[0].mime, "image/png");
    }

    #[test]
    fn vector_metafile_is_dropped() {
        let rtf = pict_hex(r"\wmetafile8", "0102030405");
        let blocks = extract(&rtf);
        // No decodable picture -> falls back to plain text.
        assert!(blocks.is_none(), "vector pictures are dropped, not emitted");
    }

    #[test]
    fn jpeg_decodes_and_is_tagged() {
        let rtf = pict_hex(r"\jpegblip", JPEG_HEX);
        let blocks = extract(&rtf).expect("has a picture");
        match &blocks[1] {
            RawBlock::Picture(p) => {
                assert_eq!(p.mime, "image/jpeg");
                assert!(!p.bytes.is_empty());
            }
            other => panic!("expected Picture, got {other:?}"),
        }
    }

    #[test]
    fn hex_decode_ignores_whitespace() {
        let bytes = hex_decode("48 65-6c\n6c 6f").unwrap();
        assert_eq!(bytes, b"Hello");
        assert!(hex_decode("   \n\t  ").is_none());
        // Odd trailing nibble is dropped, not fatal.
        assert_eq!(hex_decode("4a4").unwrap(), &[0x4a]);
    }
}
