//! Finite source-referenced HTML tag context for the preserved default Enter indentation fallback.
//!
//! Active elements retain source-name ranges and indentation only. Tags/comments stream scalar by
//! scalar; complete names and bodies are never copied into an auxiliary string or document AST.

use super::*;

/// Name identity retains original source plus a fast reject hash; equality still compares exact bytes.
#[derive(Clone)]
struct Name {
    bytes: Range<usize>,
    unit: usize,
    hash: u64,
    length: usize,
}

/// Only still-open elements affect body indentation; closed elements leave no retained syntax nodes.
struct Element {
    name: Name,
    indent: usize,
    raw: Option<&'static str>,
}

/// One currently scanned lexical tag carries finite punctuation/quote/name metadata.
struct Tag {
    start: usize,
    column: usize,
    indent: usize,
    closing: bool,
    name: Option<Name>,
    name_done: bool,
    quote: Option<char>,
    slash: bool,
}

/// Resolve tag-interior/body indentation while preserving a Markdown inline fragment's limited scope.
pub(super) fn resolve(
    document: &MarkdownDocumentBuffer,
    target: SourceLine,
    position: usize,
    block: bool,
    opening_line: usize,
) -> Result<Option<usize>, MarkdownDocumentBufferError> {
    let first = SourceLine::at(document, opening_line)?;
    let mut cursor = SourceCursor::new(document, first.bytes.0..target.bytes.1, first.units.0);
    let mut elements: Vec<Element> = Vec::new();
    let mut tag: Option<Tag> = None;
    let mut column = 0;
    let mut indent = 0;
    let mut leading = true;
    let mut comment = false;
    let mut comment_tail = ['\0'; 2];
    let mut last_open_end = None;
    while let Some((scalar, byte, unit)) = cursor.peek(document)? {
        // Left affinity observes syntax ending at the caret, without consuming a source scalar after it.
        if unit >= position {
            // Syntax after the caret cannot affect its current indentation owner.
            break;
        }
        cursor.next(document)?;
        let scalar_column = column;
        if matches!(scalar, '\r' | '\n') {
            // Physical CRLF units stay canonical while either separator resets visual line indentation.
            column = 0;
            indent = 0;
            leading = true;
        } else {
            // HTML tag indentation uses the source tag's visual column and four-column tabs.
            column += if scalar == '\t' {
                // Keep tab-expanded source columns separate from physical native offsets.
                4 - column % 4
            } else {
                // A non-tab complete source scalar advances one visual column.
                1
            };
            if leading && indentation_whitespace(scalar) {
                // Leading JavaScript whitespace contributes to the element's source-line indentation.
                indent = column;
            } else {
                // Content fixes this source line's indentation before any subsequent inline tag starts.
                leading = false;
            }
        }
        if comment {
            // Comment text has no tag/element authority even when it contains angle-bracket spelling.
            if scalar == '>' && comment_tail == ['-', '-'] {
                // Only the complete literal terminator releases comment opacity.
                comment = false;
            }
            comment_tail = [comment_tail[1], scalar];
            continue;
        }
        if let Some(current) = &mut tag {
            // An unfinished lexical tag owns quote/name/punctuation state until its closing bracket.
            if let Some(quote) = current.quote {
                // Attribute quotes may contain > or < without completing/reopening a lexical tag.
                if scalar == quote {
                    // The matching literal delimiter releases this attribute quote only.
                    current.quote = None;
                }
                continue;
            }
            if !current.name_done {
                // Name identity grows as source coordinates/hash, never as copied arbitrary tag text.
                if name_scalar(scalar) {
                    // Tag names stay source ranges; a bounded rolling hash never replaces exact comparison.
                    if let Some(name) = &mut current.name {
                        // Extend the current exact source-name range and bounded quick-rejection counters.
                        name.bytes.end = byte + scalar.len_utf8();
                        name.length += 1;
                        name.hash = (name.hash ^ u64::from(scalar.to_ascii_lowercase() as u8))
                            .wrapping_mul(1_099_511_628_211);
                    } else {
                        // The first name scalar establishes a source-backed identity without allocating its text.
                        current.name = Some(Name {
                            bytes: byte..byte + scalar.len_utf8(),
                            unit,
                            hash: (14_695_981_039_346_656_037_u64
                                ^ u64::from(scalar.to_ascii_lowercase() as u8))
                            .wrapping_mul(1_099_511_628_211),
                            length: 1,
                        });
                    }
                    continue;
                }
                if current.name.is_none() && scalar == '/' && current.closing {
                    // The close-tag slash precedes rather than terminates its source-backed name.
                    continue;
                }
                current.name_done = true;
            }
            if matches!(scalar, '\'' | '"') {
                // Quoted attribute text is opaque until its matching literal quote.
                current.quote = Some(scalar);
            } else if scalar == '>' {
                // A complete tag updates only the active element path, never a retained closed-node tree.
                let completed = tag.take().expect("active tag");
                if let Some(name) = completed.name {
                    // A complete named tag either consumes an opener or becomes one active element descriptor.
                    if completed.closing {
                        // A close removes its exact matching opener and any malformed still-open descendants.
                        for index in (0..elements.len()).rev() {
                            // Search only the still-open path; closed sibling tags occupy no retained metadata.
                            if names_equal(document, &elements[index].name, &name)? {
                                // Exact comparison, rather than hash equality, authorizes removing this owner.
                                elements.truncate(index);
                                break;
                            }
                        }
                        last_open_end = None;
                    } else if !completed.slash && !void_name(document, &name)? {
                        // A nonvoid opener owns body indentation until an exact source-matching close arrives.
                        let raw = raw_name(document, &name)?;
                        elements.push(Element {
                            name,
                            indent: completed.indent,
                            raw,
                        });
                        last_open_end = Some((unit + 1, completed.indent));
                    } else {
                        // Void/self-closing tags own no following body indentation.
                        last_open_end = None;
                    }
                }
            } else if !indentation_whitespace(scalar) {
                // Only a final nonspacing slash before > identifies a self-closing tag.
                current.slash = scalar == '/';
            }
            continue;
        }
        if scalar != '<' {
            // Ordinary body scalars carry no tag-opening authority.
            continue;
        }
        if starts_ascii(document, byte, "<!--")? {
            // Explicit comments suppress inner fake tags using only a two-character terminator tail.
            comment = true;
            comment_tail = ['\0'; 2];
            continue;
        }
        if let Some(raw) = elements.last().and_then(|element| element.raw) {
            // Script/style/pre/textarea body is raw until its own exact closing tag spelling appears.
            if !starts_ascii(document, byte, &format!("</{raw}"))? {
                // An inner fake tag in raw text cannot create a new element owner.
                continue;
            }
        }
        let next = cursor.peek(document)?.map(|value| value.0);
        if next.is_some_and(|scalar| scalar.is_ascii_alphabetic() || scalar == '/') {
            // Ordinary < prose is ignored; an actual tag starts a finite lexical owner at this source column.
            tag = Some(Tag {
                start: unit,
                column: scalar_column,
                indent,
                closing: next == Some('/'),
                name: None,
                name_done: false,
                quote: None,
                slash: false,
            });
        }
    }
    if let Some(tag) = tag.filter(|tag| position > tag.start) {
        // Open/close tag interiors use their own visual start column, including inline tags after text.
        return Ok(Some(tag.column + 2));
    }
    if block {
        // Complete HTML blocks apply active element indentation to ordinary body source between tags.
        if let Some(element) = elements.last() {
            // The innermost still-open element defines body indentation or immediate closing-tag alignment.
            let closing = closing_after(document, target, position)?;
            if element.raw == Some("script") && !closing {
                // A broken unary statement continues from its own source line, rather than the outer HTML line.
                if let Some(indent) = unary_continuation_indent(document, target, position)? {
                    // This covered JavaScript continuation rule is lexical and retains no embedded-language AST.
                    return Ok(Some(indent));
                }
            }
            return Ok(Some(element.indent + if closing { 0 } else { 2 }));
        }
    } else if let Some((end, indent)) = last_open_end.filter(|(end, _)| *end == position) {
        // A Markdown inline fragment includes its opening tag's boundary, but not later prose body source.
        let _ = end;
        return Ok(Some(indent + 2));
    }
    Ok(None)
}

/// HTML lexical names are ASCII; arbitrary name length remains a range/counter rather than a String.
fn name_scalar(scalar: char) -> bool {
    scalar.is_ascii_alphanumeric() || matches!(scalar, '-' | ':' | '_')
}

/// Reject unequal names cheaply, then compare exact case-insensitive source scalars to avoid hash authority.
fn names_equal(
    document: &MarkdownDocumentBuffer,
    left: &Name,
    right: &Name,
) -> Result<bool, MarkdownDocumentBufferError> {
    if left.length != right.length || left.hash != right.hash {
        // Different finite summaries prove inequality; equal summaries still require exact source comparison.
        return Ok(false);
    }
    let mut first = SourceCursor::new(document, left.bytes.clone(), left.unit);
    let mut second = SourceCursor::new(document, right.bytes.clone(), right.unit);
    while let Some((scalar, _, _)) = first.next(document)? {
        // Both validated name intervals contain ASCII scalars and terminate after the same known length.
        if second
            .next(document)?
            .is_none_or(|value| !scalar.eq_ignore_ascii_case(&value.0))
        {
            // A single unequal source scalar rejects the candidate even if its rolling hash collided.
            return Ok(false);
        }
    }
    Ok(true)
}

/// Compare a fixed grammar spelling directly against immutable source, without a name-sized copy.
fn named(
    document: &MarkdownDocumentBuffer,
    name: &Name,
    expected: &str,
) -> Result<bool, MarkdownDocumentBufferError> {
    if name.length != expected.len() {
        // Fixed grammar names can reject arbitrary custom names without scanning their complete spelling.
        return Ok(false);
    }
    let mut cursor = SourceCursor::new(document, name.bytes.clone(), name.unit);
    for expected in expected.chars() {
        if !cursor
            .next(document)?
            .is_some_and(|value| value.0.eq_ignore_ascii_case(&expected))
        {
            return Ok(false); // 已知不相等才能拒绝；不能把原读取错误当名称不匹配继续语法fallback。
        }
    }
    Ok(true)
}

/// Known void elements have no body and therefore contribute no active-element indentation owner.
fn void_name(
    document: &MarkdownDocumentBuffer,
    name: &Name,
) -> Result<bool, MarkdownDocumentBufferError> {
    for expected in [
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
        "source", "track", "wbr",
    ] {
        if named(document, name, expected)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Raw element names disable tag parsing inside their bodies until their own literal closing name.
fn raw_name(
    document: &MarkdownDocumentBuffer,
    name: &Name,
) -> Result<Option<&'static str>, MarkdownDocumentBufferError> {
    for expected in ["script", "style", "pre", "textarea"] {
        if named(document, name, expected)? {
            return Ok(Some(expected));
        }
    }
    Ok(None)
}

/// A finite source-prefix comparison may inspect lookahead beyond the caret without mutating its position.
fn starts_ascii(
    document: &MarkdownDocumentBuffer,
    byte: usize,
    expected: &str,
) -> Result<bool, MarkdownDocumentBufferError> {
    let end = byte
        .checked_add(expected.len())
        .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?
        .min(document.byte_len());
    if end - byte != expected.len() {
        // Source exhaustion cannot satisfy a complete fixed grammar lookahead.
        return Ok(false);
    }
    let mut pieces = SourcePieceCursor::new(document.root.clone(), byte..end);
    let mut expected = expected.bytes();
    while let Some(piece) = pieces.next_piece() {
        // Byte comparison rejects non-ASCII candidates without decoding a possibly partial UTF-8 overlap.
        let rejected = source::scan_piece(document, &piece, |bytes| {
            for &byte in bytes {
                // Partial UTF-8 is harmless here because only exact ASCII byte equality can succeed.
                if expected
                    .next()
                    .is_none_or(|expected| !byte.eq_ignore_ascii_case(&expected))
                {
                    // Any different byte ends this fixed grammar lookahead immediately.
                    return Ok(true);
                }
            }
            Ok(false)
        })?;
        if rejected {
            return Ok(false);
        }
    }
    Ok(expected.next().is_none())
}

/// A closing tag immediately after optional line-local whitespace aligns to its element's source indent.
fn closing_after(
    document: &MarkdownDocumentBuffer,
    line: SourceLine,
    position: usize,
) -> Result<bool, MarkdownDocumentBufferError> {
    let mut cursor = line.cursor(document, position - line.units.0)?;
    while cursor
        .peek(document)?
        .is_some_and(|value| indentation_whitespace(value.0))
    {
        // Only whitespace before an immediate closing-tag prefix affects alignment.
        cursor.next(document)?;
    }
    Ok(cursor.next(document)?.is_some_and(|value| value.0 == '<')
        && cursor.next(document)?.is_some_and(|value| value.0 == '/'))
}

/// Preserve the embedded script service's indentation for a caret inside a simple unary statement.
fn unary_continuation_indent(
    document: &MarkdownDocumentBuffer,
    line: SourceLine,
    position: usize,
) -> Result<Option<usize>, MarkdownDocumentBufferError> {
    let mut cursor = line.cursor(document, 0)?;
    let first = loop {
        // Prefix whitespace is counted in place, including source tabs and Unicode editor whitespace.
        let Some(value) = cursor.next(document)? else {
            // A blank line contains no unary statement needing embedded continuation indentation.
            return Ok(None);
        };
        if !indentation_whitespace(value.0) {
            // The first content scalar alone decides whether this covered lexical rule applies.
            break value;
        }
    };
    if !matches!(first.0, '-' | '+' | '!' | '~') {
        // Other embedded-language syntax does not acquire this specific unary-statement rule.
        return Ok(None);
    }
    let last = super::last_nonwhitespace(document, line)?;
    if position > first.2 && last.is_some_and(|(scalar, unit)| position < unit + scalar.len_utf16())
    {
        // Interior breaks add one unit to the statement's line; its completed end returns to HTML body indent.
        return Ok(Some(leading_columns(document, line)? + 2));
    }
    Ok(None)
}
