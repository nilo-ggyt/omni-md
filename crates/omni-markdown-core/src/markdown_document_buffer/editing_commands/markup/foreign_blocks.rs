//! Finite HTML-block opacity and dispatch to source-backed default indentation.

use super::*;

/// Known foreign-block forms retain exact finite termination identities rather than body text.
#[derive(Clone, Copy)]
pub(super) enum HtmlKind {
    Raw(&'static str),
    Comment,
    Cdata,
    Processing,
    Declaration,
    UntilBlank,
}

/// A foreign block's opening line and source-container identity survive without body ownership.
#[derive(Clone, Copy)]
pub(super) struct HtmlContext {
    pub(super) kind: HtmlKind,
    pub(super) opening_line: usize,
    pub(super) scope: Option<(usize, usize)>,
}

/// Recognize finite HTML block starts without treating arbitrary angle-bracket prose as a list context.
pub(super) fn html_start(
    document: &MarkdownDocumentBuffer,
    line: SourceLine,
    offset: usize,
) -> Result<Option<HtmlKind>, MarkdownDocumentBufferError> {
    let mut cursor = line.ascii_cursor(document, offset);
    let mut spaces = 0;
    while cursor.peek(document)?.is_some_and(|value| value.0 == ' ') {
        // Four relative spaces already identify code indentation, so no giant prefix must be copied/scanned.
        cursor.next(document)?;
        spaces += 1;
        if spaces > 3 {
            // An indented code suffix cannot establish HTML block ownership.
            return Ok(None);
        }
    }
    if cursor.peek(document)?.is_none_or(|value| value.0 != '<') {
        // Ordinary source avoids even a finite lexical allocation or parser invocation.
        return Ok(None);
    }
    let mut prefix = String::with_capacity(64);
    for _ in 0..64 {
        // A finite lexical prefix suffices for raw names and explicit comment/declaration openers.
        let Some((scalar, _, _)) = cursor.next(document)? else {
            // A shorter complete source line supplies the whole finite lexical candidate.
            break;
        };
        prefix.push(scalar);
    }
    let text = prefix.trim_start_matches(' ');
    if prefix.len() - text.len() > 3 || !text.starts_with('<') {
        // A finite candidate still requires an actual angle-bracket start at a permitted block column.
        return Ok(None);
    }
    let lower = text.to_ascii_lowercase();
    for name in ["script", "pre", "style", "textarea"] {
        // Raw blocks use exact tag-name boundaries and case-insensitive matching through explicit close.
        if lower
            .strip_prefix(&format!("<{name}"))
            .is_some_and(|tail| tail.is_empty() || tail.starts_with([' ', '\t', '>']))
        {
            return Ok(Some(HtmlKind::Raw(name)));
        }
    }
    if text.starts_with("<!--") {
        // Comments terminate only at their literal three-character terminator.
        return Ok(Some(HtmlKind::Comment));
    }
    if text.starts_with("<![CDATA[") {
        // CDATA bodies cannot lend inner marker-like spelling to Markdown containers.
        return Ok(Some(HtmlKind::Cdata));
    }
    if text.starts_with("<?") {
        // A processing instruction retains its own question-mark terminator identity.
        return Ok(Some(HtmlKind::Processing));
    }
    if text.starts_with("<!") && text.as_bytes().get(2).is_some_and(u8::is_ascii_uppercase) {
        // Uppercase declaration grammar ends at the next literal angle bracket.
        return Ok(Some(HtmlKind::Declaration));
    }
    if pulldown_cmark::Parser::new(text).any(|event| {
        // This parser sees only the finite lexical candidate, never the original document/body.
        matches!(
            event,
            pulldown_cmark::Event::Start(pulldown_cmark::Tag::HtmlBlock)
        )
    }) {
        // Ordinary recognized block/generic tags are foreign until the next blank separator.
        return Ok(Some(HtmlKind::UntilBlank));
    }
    Ok(None)
}

/// Find one fixed ASCII terminator using constant sliding scratch across canonical source pieces.
fn find_ascii(
    document: &MarkdownDocumentBuffer,
    line: SourceLine,
    pattern: &str,
) -> Result<Option<Range<usize>>, MarkdownDocumentBufferError> {
    let mut cursor = line.cursor(document, 0)?;
    let mut scratch = [0_u8; 16];
    let mut used = 0;
    while let Some((scalar, _, unit)) = cursor.next(document)? {
        // Non-ASCII text cannot equal a delimiter but never changes canonical UTF-16 source coordinates.
        scratch.rotate_left(1);
        scratch[15] = if scalar.is_ascii() {
            // Known terminators compare case-insensitive ASCII only.
            scalar.to_ascii_lowercase() as u8
        } else {
            // Non-ASCII source invalidates this window without losing its physical native offset.
            0
        };
        used += 1;
        if used >= pattern.len() && &scratch[16 - pattern.len()..] == pattern.as_bytes() {
            // Fixed patterns contain only ASCII, so their byte width equals their native source-unit width.
            return Ok(Some(unit + 1 - pattern.len()..unit + 1));
        }
    }
    Ok(None)
}

/// HTML opacity ends only at an actual explicit terminator or its defined blank separator.
pub(super) fn html_close(
    document: &MarkdownDocumentBuffer,
    line: SourceLine,
    kind: HtmlKind,
) -> Result<Option<Range<usize>>, MarkdownDocumentBufferError> {
    match kind {
        // Explicit block kinds each select only their own finite terminator grammar.
        HtmlKind::Raw(name) => {
            // A fixed known raw name requires its own complete case-insensitive closing tag spelling.
            find_ascii(document, line, &format!("</{name}>"))
        }
        HtmlKind::Comment => {
            // Marker-looking comment payload remains opaque until the literal comment terminator.
            find_ascii(document, line, "-->")
        }
        HtmlKind::Cdata => {
            // CDATA has a distinct three-character terminator.
            find_ascii(document, line, "]]>")
        }
        HtmlKind::Processing => {
            // Processing instructions retain their complete two-character terminator.
            find_ascii(document, line, "?>")
        }
        HtmlKind::Declaration => {
            // Declaration opacity ends at the first closing angle bracket.
            find_ascii(document, line, ">")
        }
        HtmlKind::UntilBlank => {
            // Ordinary block tags release opacity at a blank separator rather than matching element nesting.
            Ok(
                range_is_whitespace(document, line.units.0..line.units.1, false)?
                    .then_some(line.units.0..line.units.1),
            )
        }
    }
}

/// Preserve the baseline raw-HTML indentation fallback while Markdown continuation remains disabled.
pub(super) fn html_indentation(
    document: &MarkdownDocumentBuffer,
    target: SourceLine,
    position: usize,
) -> Result<Option<usize>, MarkdownDocumentBufferError> {
    let mut pieces = SourcePieceCursor::new(document.root.clone(), 0..target.bytes.1);
    let mut has_angle = false;
    while let Some(piece) = pieces.next_piece() {
        if source::scan_piece(document, &piece, |bytes| Ok(bytes.contains(&b'<')))? {
            has_angle = true;
            break; // Enter HTML handling only after the actual lease returns; an I/O error cannot become ordinary prose.
        }
    }
    if !has_angle {
        // Most Enter actions need only the current line's finite whitespace metrics.
        return Ok(None);
    }
    let mut lines = SourceLines::new(document);
    let mut scanner = Scanner::new();
    for _ in 0..target.number {
        // The same block/container scanner excludes HTML-looking text inside fenced code.
        let line = lines
            .next(document)?
            .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
        scanner.consume(document, line)?;
    }
    let opaque = scanner.consume(document, target)?;
    if let Some(context) = scanner.current_html {
        // Comments/declarations stay foreign without applying element indentation to their payload text.
        if matches!(
            context.kind,
            HtmlKind::Comment | HtmlKind::Cdata | HtmlKind::Processing | HtmlKind::Declaration
        ) {
            return Ok(None);
        }
        return super::html_indentation::resolve(
            document,
            target,
            position,
            true,
            context.opening_line,
        );
    }
    if opaque {
        // HTML-looking source inside fenced code remains literal and receives ordinary code-line indentation.
        return Ok(None);
    }
    super::html_indentation::resolve(document, target, position, false, target.number)
}
