//! Transient offset-context classification for the deliberately bounded bracket-cue grammar.

use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, LinkType, Options, Parser, Tag};

/// Transient syntax identities, compared by kind across distinct nodes just like the baseline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Context {
    Document,
    Paragraph,
    Heading(HeadingLevel),
    SetextHeading(HeadingLevel),
    CodeText,
    CodeInfo,
    InlineCode,
    Escape,
    Emphasis,
    Strong,
    Strike,
    Subscript,
    Superscript,
    Link,
    Image,
    LinkMark,
    LinkLabel,
    Url,
    LinkTitle,
    TableCell,
    TaskMarker,
}

/// One bounded link range is enough to label source delimiters; it never owns destination text.
struct LinkRegion {
    range: Range<usize>,
    kind: LinkType,
    image: bool,
}

/// Establish admitted token kinds from bounded offset events and explicit delimiter/escape checks.
pub(super) fn classify_contexts(source: &str) -> Option<Vec<Context>> {
    if source.contains("\\`") {
        // Lezer rejects a backtick opener immediately following another backtick even when that
        // preceding backtick was escaped; pulldown may open a span there. Do not reuse its contexts.
        return None;
    }
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_SUBSCRIPT
        | Options::ENABLE_SUPERSCRIPT;
    // Lezer recognizes unresolved shortcut/reference links syntactically, without consulting a vault.
    let mut broken_link = |_link: pulldown_cmark::BrokenLink<'_>| {
        Some((
            pulldown_cmark::CowStr::Borrowed("omni-reference"),
            pulldown_cmark::CowStr::Borrowed(""),
        ))
    };
    let parser = Parser::new_with_broken_link_callback(source, options, Some(&mut broken_link));
    let mut contexts = vec![Context::Document; source.len()];
    let mut links = Vec::new();
    for (event, range) in parser.into_offset_iter() {
        // Parent ranges fill first and deeper offset events replace only their own source extent.
        match event {
            Event::Start(tag) => {
                let context = match tag {
                    // Equal block kinds stay comparable across distinct parsed nodes.
                    Tag::Paragraph | Tag::Item => Context::Paragraph,
                    Tag::Heading { level, .. } => {
                        if source[range.clone()]
                            .trim_start_matches([' ', '\t'])
                            .starts_with('#')
                        {
                            // ATX levels have distinct Lezer node types, separate from Setext headings.
                            Context::Heading(level)
                        } else {
                            // Setext levels also have distinct node identities despite similar typography.
                            Context::SetextHeading(level)
                        }
                    }
                    Tag::CodeBlock(kind) => {
                        contexts[range.clone()].fill(Context::CodeText);
                        if matches!(kind, CodeBlockKind::Fenced(_)) {
                            // Brackets in a fence info string have CodeInfo identity, not code-body identity.
                            let end = source[range.clone()]
                                .find(['\r', '\n'])
                                .map_or(range.end, |offset| range.start + offset);
                            contexts[range.start..end].fill(Context::CodeInfo);
                        }
                        continue;
                    }
                    // An Emphasis wrapper is distinct from Strong even when their body source is identical.
                    Tag::Emphasis => Context::Emphasis,
                    // Strong wrappers share a type across asterisk and underscore spellings.
                    Tag::Strong => Context::Strong,
                    // GFM strike wrappers stay separate from single-tilde subscript wrappers.
                    Tag::Strikethrough => Context::Strike,
                    // The preserved extended language gives subscript its own context identity.
                    Tag::Subscript => Context::Subscript,
                    // Superscript does not share ordinary Paragraph nesting despite similar source positions.
                    Tag::Superscript => Context::Superscript,
                    Tag::Link { link_type, .. } | Tag::Image { link_type, .. } => {
                        let image = source.as_bytes().get(range.start) == Some(&b'!');
                        links.push(LinkRegion {
                            range: range.clone(),
                            kind: link_type,
                            image,
                        });
                        if image {
                            // Image label text has a different Lezer type from ordinary link label text.
                            Context::Image
                        } else {
                            // Link marker/URL/label leaves replace this container kind after offset traversal.
                            Context::Link
                        }
                    }
                    // Cell text shares a kind across header/body rows, but not with paragraph text.
                    Tag::TableCell => Context::TableCell,
                    Tag::List(_)
                    | Tag::BlockQuote(_)
                    | Tag::Table(_)
                    | Tag::TableHead
                    | Tag::TableRow => {
                        // These containers do not override already established child paragraph/cell identities.
                        continue;
                    }
                    // No HTML or extension parser is silently substituted for a mixed Lezer syntax tree.
                    _ => return None,
                };
                contexts[range].fill(context);
            }
            Event::Code(_) => {
                // The entire code-span source, including escaped-looking text, stays in one InlineCode context.
                contexts[range].fill(Context::InlineCode);
            }
            Event::TaskListMarker(_) => {
                // Both square markers belong to the same task token rather than an unresolved LinkMark pair.
                contexts[range].fill(Context::TaskMarker);
            }
            Event::Html(_) | Event::InlineHtml(_) => {
                // HTML adds marked, sometimes multi-unit handles that this bounded classifier does not prove.
                return None;
            }
            Event::End(_) | Event::Text(_) | Event::SoftBreak | Event::HardBreak | Event::Rule => {
                // Text keeps its deepest enclosing context; line/processing markers contain no default brackets.
            }
            // Disabled math/footnote events must not accidentally enter the admitted grammar later.
            _ => return None,
        }
    }
    // Establish escape parity before finding link label boundaries; two backslashes leave a close unescaped.
    let mut offset = 0;
    while offset < source.len() {
        // Inspect source bytes only for ASCII grammar; non-ASCII bytes retain the enclosing parser context.
        let byte = source.as_bytes()[offset];
        if byte == b'\\'
            && inline_context(contexts[offset])
            && offset + 1 < source.len()
            && source.as_bytes()[offset + 1].is_ascii_punctuation()
        {
            // Lezer's Escape token includes the backslash and escaped punctuation as one shared node type.
            contexts[offset..offset + 2].fill(Context::Escape);
            offset += 2;
            continue;
        }
        offset += 1;
    }
    for link in links {
        // URL/title leaves deliberately overwrite escape-looking source inside their single lexical node.
        classify_link(source, &mut contexts, link)?;
    }
    for (offset, byte) in source.bytes().enumerate() {
        // Escaped/code/URL '<' stays in its already proven leaf; other HTML-like text is uncertain.
        if byte == b'<' && inline_context(contexts[offset]) {
            return None;
        }
    }
    let mut line_start = 0;
    for line in source.split_inclusive(['\r', '\n']) {
        // Definition nodes are not emitted by the offset iterator; never pretend their labels are plain text.
        let prefix = line.trim_start_matches([' ', '\t']);
        let marker = line_start + line.len() - prefix.len();
        if prefix.starts_with('[') && prefix.contains("]:") && contexts[marker] != Context::CodeText
        {
            return None;
        }
        line_start += line.len();
    }
    Some(contexts)
}

/// Tell where Markdown escapes can create an Escape leaf rather than literal code/URL punctuation.
fn inline_context(context: Context) -> bool {
    matches!(
        context,
        Context::Paragraph
            | Context::Heading(_)
            | Context::SetextHeading(_)
            | Context::Emphasis
            | Context::Strong
            | Context::Strike
            | Context::Subscript
            | Context::Superscript
            | Context::Link
            | Context::Image
            | Context::TableCell
    )
}

/// Label admitted simple links without mistaking their outer parentheses for URL-internal nesting.
fn classify_link(source: &str, contexts: &mut [Context], link: LinkRegion) -> Option<()> {
    let bytes = source.as_bytes();
    let opening = link.range.start + usize::from(link.image);
    if bytes.get(opening) != Some(&b'[') {
        // Autolink/GFM URL grammar has different leaves and is deliberately outside this mapper.
        return None;
    }
    let mut depth = 1_usize;
    let mut close = opening + 1;
    while close < link.range.end {
        // Code spans and escaped brackets cannot close their surrounding link label.
        if matches!(contexts[close], Context::InlineCode | Context::Escape) {
            close += 1;
            continue;
        }
        match bytes[close] {
            b'[' => {
                // A nested label consumes its own closing marker before the outer label can end.
                depth += 1;
            }
            b']' => {
                // The first balanced label close separates inline text from destination/reference syntax.
                depth -= 1;
                if depth == 0 {
                    // A balanced outer label closes before any following destination/reference parser begins.
                    break;
                }
            }
            _ => {
                // Other source bytes do not change the finite bracket-label depth.
            }
        }
        close += 1;
    }
    if close >= link.range.end {
        // A parser-produced link without a reproducible label boundary is not safe to reinterpret.
        return None;
    }
    contexts[opening..opening + 1].fill(Context::LinkMark);
    contexts[close..close + 1].fill(Context::LinkMark);
    match link.kind {
        LinkType::Shortcut | LinkType::ShortcutUnknown => {
            // A shortcut has no separate trailing label or destination source.
            (close + 1 == link.range.end).then_some(())
        }
        LinkType::Reference
        | LinkType::ReferenceUnknown
        | LinkType::Collapsed
        | LinkType::CollapsedUnknown => {
            // The reference-label token includes both brackets, separate from the first label's LinkMark leaves.
            (bytes.get(close + 1) == Some(&b'[') && bytes.get(link.range.end - 1) == Some(&b']'))
                .then_some(())?;
            contexts[close + 1..link.range.end].fill(Context::LinkLabel);
            Some(())
        }
        LinkType::Inline => {
            // Only an exact destination/title parse licenses LinkMark/URL/LinkTitle identities.
            (bytes.get(close + 1) == Some(&b'(') && bytes.get(link.range.end - 1) == Some(&b')'))
                .then_some(())?;
            contexts[close + 1] = Context::LinkMark;
            contexts[link.range.end - 1] = Context::LinkMark;
            let mut cursor = skip_space(bytes, close + 2, link.range.end - 1);
            let destination = cursor;
            if bytes.get(cursor) == Some(&b'<') {
                // Angle destinations are one URL token, including their own angle delimiters.
                cursor += 1;
                while cursor < link.range.end - 1 && bytes[cursor] != b'>' {
                    // The parser already bounded this angle destination; retain every raw byte in its URL leaf.
                    cursor += 1;
                }
                (bytes.get(cursor) == Some(&b'>')).then_some(())?;
                cursor += 1;
            } else {
                // Raw destinations balance literal parentheses and honor escaped punctuation.
                let mut nesting = 0_usize;
                while cursor < link.range.end - 1 && !bytes[cursor].is_ascii_whitespace() {
                    match bytes[cursor] {
                        b'\\' if cursor + 1 < link.range.end - 1 => {
                            // Escaping consumes the following byte before it can affect destination balance.
                            cursor += 2;
                            continue;
                        }
                        b'(' => {
                            // Literal inner opening parentheses belong to the URL and require a matching inner close.
                            nesting += 1;
                        }
                        b')' if nesting > 0 => {
                            // This closes a URL-internal pair, not the surrounding LinkMark pair.
                            nesting -= 1;
                        }
                        b')' => {
                            // An outer close ends the raw destination before title/terminal delimiter validation.
                            break;
                        }
                        // Ordinary destination bytes stay inside the same URL leaf.
                        _ => {}
                    }
                    cursor += 1;
                }
                (nesting == 0).then_some(())?;
            }
            contexts[destination..cursor].fill(Context::Url);
            let title = skip_space(bytes, cursor, link.range.end - 1);
            if title < link.range.end - 1 {
                // A title requires whitespace after a destination and one complete quoted/parenthesized token.
                (title > cursor).then_some(())?;
                let closing = match bytes[title] {
                    // A single-quoted title closes only at the same quote outside escaped source.
                    b'\'' => b'\'',
                    // Double-quoted titles retain the entire quoted source under LinkTitle identity.
                    b'"' => b'"',
                    // Parenthesized titles use ')' as their one matching terminator.
                    b'(' => b')',
                    // An unquoted tail cannot be assumed to share Lezer's LinkTitle node.
                    _ => return None,
                };
                cursor = title + 1;
                while cursor < link.range.end - 1 && bytes[cursor] != closing {
                    // Escaped quote/parenthesis bytes remain within the title's single syntax leaf.
                    cursor += if bytes[cursor] == b'\\' {
                        // Escaping consumes the next byte before it can act as the title terminator.
                        2
                    } else {
                        // Unescaped source advances one byte within this already validated UTF-8 range.
                        1
                    };
                }
                // A trailing escape cannot borrow a terminator from source outside this parser-owned region.
                (cursor < link.range.end - 1 && bytes.get(cursor) == Some(&closing))
                    .then_some(())?;
                cursor += 1;
                contexts[title..cursor].fill(Context::LinkTitle);
            } else {
                // An absent title ends at the destination, allowing trailing whitespace before the close.
                cursor = title;
            }
            (skip_space(bytes, cursor, link.range.end - 1) == link.range.end - 1).then_some(())
        }
        // No implicit HTML/GFM email or URL parser is admitted through a destination-shaped shortcut.
        _ => None,
    }
}

/// Skip only source whitespace inside one already bounded link region.
fn skip_space(bytes: &[u8], mut cursor: usize, end: usize) -> usize {
    while cursor < end && bytes[cursor].is_ascii_whitespace() {
        // Source offsets, including CRLF, stay canonical rather than using decoded parser strings.
        cursor += 1;
    }
    cursor
}
