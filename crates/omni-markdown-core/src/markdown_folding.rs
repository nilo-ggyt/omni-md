//! Read-only Markdown block folding over the canonical piece tree.
//!
//! Foreground queries stream source context with fixed scratch and bounded line prefixes. Exact fence
//! metadata includes complete marker runs/info text; recognized HTML retains its actual termination.
//! Queries retain one result, never a full syntax tree or document-sized string. UTF-16 ranges always
//! address the complete unchanged source, including paragraphs hidden by the native presentation.

use std::ops::Range;

use crate::{MarkdownDocumentBuffer, MarkdownDocumentBufferError};

/// A presentation-only fold keeps its first source line visible and hides subsequent paragraphs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownFoldRegion {
    /// Zero-based logical source line used to toggle this fold from the native gutter.
    pub header_line: usize,
    /// Canonical UTF-16 interval, starting at the visible header's content end.
    pub hidden_utf16_range: Range<usize>,
}

/// Prefix storage remains finite even when a single logical line spans the whole document.
const PREFIX_UNITS: usize = 1024;

mod delimiters;
mod lines;

/// 真实旧坐标ChangeSet的有界折叠锚点映射；只准备候选，不修改文档或原生视图。
pub mod anchor_mapping;

use delimiters::{HtmlBlock, html_ends, scan_fence_delimiter};
use lines::{Line, LineCursor};

/// Return a cheap gutter hint using at most three finite line prefixes, without a document-prefix scan.
///
/// The hint grants no hiding authority: the explicit toggle must call `markdown_fold_region`.
/// Continuation/context ambiguities are resolved there before any paragraph is omitted.
///
/// Returns an error when `line` is outside the canonical document. The buffer is never modified.
pub fn markdown_line_may_fold(
    document: &MarkdownDocumentBuffer,
    line: usize,
) -> Result<bool, MarkdownDocumentBufferError> {
    let current = Line::read_with_blank_scan(document, line, false)?;
    if current.blank || line + 1 >= document.line_count() {
        // Empty and final lines have no following source interval to collapse.
        return Ok(false);
    }
    if current.thematic_break() || current.setext().is_some() {
        // A divider or underline is never the visible first line of a separate fold.
        return Ok(false);
    }
    let fence_hint = current.indent() <= 3
        && (current.marker_source().starts_with("```")
            || current.marker_source().starts_with("~~~"));
    if line > 0 && current.heading().is_none() && !current.list_item() && !fence_hint {
        // A bounded preceding prefix removes obvious continuation controls without granting hiding authority.
        let previous = Line::read_with_blank_scan(document, line - 1, false)?;
        if !previous.blank
            && ((current.quote() && previous.quote())
                || (current.indent() >= 4 && previous.indent() >= 4)
                || (!current.quote() && current.indent() < 4 && previous.plain_paragraph()))
        {
            // Paragraph, quote and code continuations share their earlier block's gutter control.
            return Ok(false);
        }
    }
    let next = Line::read_with_blank_scan(document, line + 1, false)?;
    Ok(current.heading().is_some() || !next.blank)
}

/// Finite container identity is sufficient to keep fenced children opaque without a nesting AST.
#[derive(Clone, Copy)]
struct Container {
    quote: bool,
    minimum_indent: usize,
}

impl Container {
    /// Locate child source only when this line explicitly remains inside the same container.
    fn child_start(self, line: &Line) -> Option<usize> {
        if self.quote {
            // Lazy unmarked continuation cannot extend a fenced/heading child of a block quote.
            line.quote()
                .then(|| {
                    // Only the actual explicit marker determines this quote child's source offset.
                    line.container_body(true)
                })
                .flatten()
        } else if line.indent() >= self.minimum_indent {
            // List continuation must reach its actual content indentation before child syntax starts.
            Some(line.prefix.len() - line.prefix.trim_start_matches([' ', '\t']).len())
        } else {
            // An unindented line may be a lazy paragraph, but cannot belong to a fenced child.
            None
        }
    }
}

/// One exact fence plus its optional container; nested source does not require a retained block stack.
#[derive(Clone, Copy)]
struct FenceContext {
    delimiter: (u8, usize),
    container: Option<Container>,
}

/// Count ASCII marker/padding columns using tab stops rather than treating a tab as one space.
fn content_columns(line: &Line, offset: usize) -> usize {
    let mut columns = 0;
    for byte in line.prefix.as_bytes().iter().take(offset) {
        // Markers and decimal digits are ASCII; padding tabs advance to the next four-column stop.
        columns += if *byte == b'\t' {
            // A tab advances from its current visual column to the next four-column stop.
            4 - columns % 4
        } else {
            // ASCII marker digits, punctuation and spaces occupy one source column each.
            1
        };
    }
    columns
}

/// Classify a complete top-level or explicit list/quote fence without forgetting long delimiters.
fn open_fence_context(
    document: &MarkdownDocumentBuffer,
    line: &Line,
) -> Result<Option<FenceContext>, MarkdownDocumentBufferError> {
    if let Some(delimiter) = line.fence() {
        // A top-level fence remains opaque across all source lines until its exact matching close.
        return Ok(Some(FenceContext {
            delimiter,
            container: None,
        }));
    }
    let quote = line.quote();
    if !quote && !line.list_item() {
        // Ordinary paragraphs and HTML starts have no fenced child at this explicit boundary.
        return Ok(None);
    }
    let Some(offset) = line.container_body(quote) else {
        // An incomplete container prefix has no child syntax authority.
        return Ok(None);
    };
    Ok(
        scan_fence_delimiter(document, line.start + offset, line.end)?
            .filter(|delimiter| delimiter.valid_opening)
            .map(|delimiter| {
                // Store only fixed delimiter/container metadata; all content stays in canonical pieces.
                FenceContext {
                    delimiter: (delimiter.marker, delimiter.length),
                    container: Some(Container {
                        quote,
                        minimum_indent: content_columns(line, offset),
                    }),
                }
            }),
    )
}

/// Check one line against the current fence; None means its surrounding container ended first.
fn fence_line_closes(
    document: &MarkdownDocumentBuffer,
    line: &Line,
    context: FenceContext,
) -> Result<Option<bool>, MarkdownDocumentBufferError> {
    let Some(container) = context.container else {
        // The line's complete stored delimiter metadata is enough for an ordinary top-level fence.
        return Ok(Some(line.closes_fence(context.delimiter)));
    };
    let Some(offset) = container.child_start(line) else {
        // Leaving a list/quote also ends its contained fence, without consuming this outside source line.
        return Ok(None);
    };
    if !container.quote && line.indent() > container.minimum_indent + 3 {
        // Four additional indentation columns are code content rather than a closing fence.
        return Ok(Some(false));
    }
    Ok(Some(
        scan_fence_delimiter(document, line.start + offset, line.end)?.is_some_and(|delimiter| {
            // A child close obeys the same complete-run, same-marker and whitespace-tail rules.
            delimiter.marker == context.delimiter.0
                && delimiter.length >= context.delimiter.1
                && delimiter.closing_tail
        }),
    ))
}

/// An interrupting block cannot be swallowed as a paragraph's or list item's lazy continuation.
fn interrupts_paragraph(line: &Line) -> bool {
    line.heading().is_some()
        || line.fence().is_some()
        || line.quote()
        || line.list_item()
        || line.thematic_break()
        || line.html().is_some()
}

/// Look ahead through one ordinary paragraph using fixed scratch to recognize multiline Setext text.
fn paragraph_heading(
    document: &MarkdownDocumentBuffer,
    first: &Line,
    next_line: usize,
) -> Result<Option<usize>, MarkdownDocumentBufferError> {
    if !first.plain_paragraph() || next_line >= document.line_count() {
        // Only the first line of a nonempty ordinary paragraph can precede an underline.
        return Ok(None);
    }
    let start = document.utf16_offset_for_byte(document.byte_offset_for_line(next_line)?)?;
    let mut lines = LineCursor::new(document, start, document.line_count() - next_line);
    while let Some(line) = lines.next_line()? {
        // Each paragraph source line is streamed once; no paragraph string or line list is accumulated.
        if let Some(level) = line.setext() {
            // The complete underline promotes all preceding paragraph lines into one heading.
            return Ok(Some(level));
        }
        if !line.plain_paragraph() {
            // A separator or a different block ends the paragraph before any possible underline.
            return Ok(None);
        }
    }
    Ok(None)
}

/// Resolve one covered Markdown block or heading section without retaining a full syntax tree.
///
/// UTF-16 coordinates always address unchanged canonical text. Exact fences, paragraphs, common
/// containers and bounded HTML starts are recognized with fixed scratch. Ambiguous long HTML starts
/// grant no hiding authority but remain opaque during context scans. This conservative block scanner
/// is not an exhaustive Markdown parser; the reference corpus states the independently covered cases.
///
/// Returns an error when `line` is outside the canonical document. An unresolved/one-line block returns
/// `None`; a result never changes source text, selection, generation, history or edit-storage ownership.
pub fn markdown_fold_region(
    document: &MarkdownDocumentBuffer,
    line: usize,
) -> Result<Option<MarkdownFoldRegion>, MarkdownDocumentBufferError> {
    let current = Line::read(document, line)?;
    if current.blank || line + 1 >= document.line_count() {
        // A fold needs a nonempty visible header and at least one following source line.
        return Ok(None);
    }
    let mut fence: Option<FenceContext> = None;
    let mut html_context = None;
    let mut previous = None;
    let mut paragraph_active = false;
    let mut previous_setext_heading = false;
    let mut prefix_lines = LineCursor::new(document, 0, line);
    for _ in 0..line {
        // Sequential prefix traversal avoids indexed rescans while retaining one preceding record only.
        let record = prefix_lines
            .next_line()?
            .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
        if let Some(context) = fence {
            match fence_line_closes(document, &record, context)? {
                Some(closes) => {
                    // Every contained line remains opaque, including the actual closing delimiter.
                    if closes {
                        // The complete matching delimiter releases this opaque context after its own line.
                        fence = None;
                    }
                    // Fenced source cannot leave an ordinary paragraph active after its closing line.
                    paragraph_active = false;
                    previous_setext_heading = false;
                    previous = Some(record);
                    continue;
                }
                None => {
                    // A container ended before this line; classify the outside line normally below.
                    fence = None;
                }
            }
        }
        if let Some(context) = html_context {
            // HTML-looking headings/fences cannot gain independent authority inside an opaque block.
            if html_ends(document, &record, context)? {
                // A real explicit/blank terminator ends this previously acquired opaque context.
                html_context = None;
            }
            // Raw HTML source likewise cannot lend paragraph continuation to the following outside line.
            paragraph_active = false;
            previous_setext_heading = false;
            previous = Some(record);
            continue;
        }
        previous_setext_heading = paragraph_active && record.setext().is_some();
        paragraph_active = !previous_setext_heading && record.plain_paragraph();
        fence = open_fence_context(document, &record)?;
        if fence.is_none() {
            // A non-fenced HTML start keeps its precise explicit/blank termination state.
            if let Some(context) = record.html()
                && !html_ends(document, &record, context)?
            {
                // A same-line close needs no persistent context; every other start remains opaque.
                html_context = Some(context);
            }
        }
        previous = Some(record);
    }
    if fence.is_some() || html_context.is_some() {
        // Inner opaque content and its closing line cannot start an independent fold.
        return Ok(None);
    }
    let next_start = document.utf16_offset_for_byte(document.byte_offset_for_line(line + 1)?)?;
    let opening_fence = current.fence();
    let quote = current.quote();
    let list = current.list_item();
    let indented = current.indent() >= 4;
    let html = current.html();
    if current.thematic_break() || matches!(html, Some(HtmlBlock::Uncertain)) {
        // Dividers and undecidable long tag lines cannot hide subsequent independent source blocks.
        return Ok(None);
    }
    if current.heading().is_none()
        && opening_fence.is_none()
        && !list
        && previous.as_ref().is_some_and(|previous: &Line| {
            // A Setext candidate cannot restart a paragraph/container that already began on a prior line.
            !previous.blank
                && ((quote && previous.quote())
                    || (indented && previous.indent() >= 4)
                    || (!quote
                        && !indented
                        && current.plain_paragraph()
                        && !previous_setext_heading
                        && previous.plain_paragraph()))
        })
    {
        // A continuation line shares the earlier source block rather than gaining an independent fold.
        return Ok(None);
    }
    let heading = if let Some(level) = current.heading() {
        // An explicit ATX marker identifies its own heading without paragraph lookahead.
        Some(level)
    } else {
        // A complete paragraph may span multiple source lines before its Setext underline.
        paragraph_heading(document, &current, line + 1)?
    };
    let container_offset = if quote || list {
        // An explicit marker identifies where this container's first child starts.
        current.container_body(quote)
    } else {
        // Ordinary top-level blocks need no container-relative interpretation.
        None
    };
    let container = container_offset.map(|offset| {
        // Tab stops determine required list continuation indentation; quote identity uses explicit markers.
        Container {
            quote,
            minimum_indent: content_columns(&current, offset),
        }
    });
    let mut allows_lazy = container_offset.is_some_and(|offset| {
        // Heading/fence/HTML children cannot extend into unmarked outside paragraphs.
        current.lazy_paragraph_body(offset)
    });
    let mut child_fence = if container.is_some() {
        // Exact contained fences are handled before lazy paragraph logic.
        open_fence_context(document, &current)?
    } else {
        // An ordinary top-level block has no separately owned contained fence state.
        None
    };
    let mut in_fence = opening_fence.map(|delimiter| {
        // The requested top-level fence begins the section with exact delimiter ownership.
        FenceContext {
            delimiter,
            container: None,
        }
    });
    let mut html_context = html;
    if let Some(context) = html_context {
        // A same-line explicit HTML close has no following body to fold.
        if html_ends(document, &current, context)? {
            // A block fully contained on its visible line has no following source interval to hide.
            return Ok(None);
        }
    }
    let mut end = current.end;
    let mut blank_seen = false;
    let mut section_container: Option<(Container, bool)> = None;
    let mut paragraph_continuation = current.plain_paragraph();
    let mut section_lines = LineCursor::new(document, next_start, document.line_count() - line - 1);
    for number in line + 1..document.line_count() {
        // Consume only the next needed record; do not scan a giant outside line after the fold has ended.
        let record = section_lines
            .next_line()?
            .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
        if let Some(context) = in_fence {
            // Delimiter ownership is resolved before any nested-looking Markdown can gain authority.
            match fence_line_closes(document, &record, context)? {
                Some(closes) => {
                    // Fenced text is opaque to section boundaries, even if it contains apparent headings.
                    end = record.end;
                    if closes {
                        // A complete close ends only the code block; surrounding heading sections may continue.
                        in_fence = None;
                        if heading.is_none() {
                            // A standalone fence owns no following block after its closing delimiter.
                            break;
                        }
                    }
                    continue;
                }
                None => {
                    // A fenced child's container ended; the outside line needs normal section classification.
                    in_fence = None;
                }
            }
        }
        if let Some(context) = html_context {
            // Existing opaque HTML owns its termination before any apparent new block is classified.
            if html_ends(document, &record, context)? {
                // Explicit closes belong to the block; a terminating blank is excluded from its range.
                if !record.blank {
                    // The explicit closing line belongs to the block, unlike a separating blank line.
                    end = record.end;
                }
                html_context = None;
                if heading.is_none() {
                    // Standalone HTML folding ends with its own terminator rather than the next block.
                    break;
                }
            } else if !record.blank {
                // Interior HTML content can include Markdown-looking markers without ending opacity.
                end = record.end;
            }
            continue;
        }
        if let Some(level) = heading {
            // A heading section may contain independent blocks without ending until a peer section begins.
            if let Some(context) = record.html() {
                // HTML starts a separate block, so following source cannot continue the prior paragraph.
                paragraph_continuation = false;
                if matches!(context, HtmlBlock::Uncertain) {
                    // Uncertain inner HTML cannot safely authorize hiding an entire surrounding section.
                    return Ok(None);
                }
                // A bounded recognized HTML start suppresses fake section headings until its true end.
                end = record.end;
                if !html_ends(document, &record, context)? {
                    // A same-line close needs no persistent context; every other start remains opaque.
                    html_context = Some(context);
                }
                continue;
            }
            if let Some((container, allows_lazy)) = section_container {
                if record.blank || container.child_start(&record).is_some() {
                    // A child heading belongs to its list/quote, rather than ending the surrounding section.
                    if !record.blank {
                        // Interior blank separators stay outside the final range unless content resumes.
                        end = record.end;
                    }
                    in_fence = open_fence_context(document, &record)?;
                    continue;
                }
                if allows_lazy && !interrupts_paragraph(&record) {
                    // Lazy paragraph prose still belongs to this container until an interrupting outside block.
                    end = record.end;
                    continue;
                }
                // This outside block ends the prior container and regains ordinary section boundary authority.
                section_container = None;
            }
            if record.quote() || record.list_item() {
                // Keep only the current explicit container's finite content indentation and laziness state.
                let quote = record.quote();
                if let Some(offset) = record.container_body(quote) {
                    // A valid explicit marker supplies finite container state for subsequent child lines.
                    section_container = Some((
                        Container {
                            quote,
                            minimum_indent: content_columns(&record, offset),
                        },
                        record.lazy_paragraph_body(offset),
                    ));
                }
            }
            let subsequent_setext = if !paragraph_continuation && record.plain_paragraph() {
                // Only a paragraph's first line can be a multiline Setext section boundary.
                paragraph_heading(document, &record, number + 1)?
            } else {
                // Continuation text and block openers cannot start a separate Setext heading.
                None
            };
            paragraph_continuation = record.plain_paragraph();
            if record
                .heading()
                .or(subsequent_setext)
                .is_some_and(|candidate| candidate <= level)
            {
                // Leave the next equal/higher section's heading visible at its original source position.
                break;
            }
            in_fence = open_fence_context(document, &record)?;
            if !record.blank {
                // Trailing section separators are excluded until a later nonblank body line resumes.
                end = record.end;
            }
            continue;
        }
        if let Some(container) = container {
            // Explicit child ownership and paragraph laziness decide whether the container continues.
            if let Some(context) = child_fence {
                // A contained fence cannot extend past the source boundary of its own list/quote.
                match fence_line_closes(document, &record, context)? {
                    Some(closes) => {
                        // Contained code is never lazy paragraph text; preserve its exact closing line.
                        end = record.end;
                        if closes {
                            // Finished code cannot lend paragraph laziness to an unmarked outside line.
                            child_fence = None;
                            allows_lazy = false;
                        }
                        continue;
                    }
                    None => {
                        // The list/quote ended before its unclosed fence could consume outside source.
                        break;
                    }
                }
            }
            if record.blank {
                // Lists may bridge an interior blank; an unmarked quote blank terminates the quote.
                if list {
                    // An item can continue after an interior blank only with explicit content indentation.
                    blank_seen = true;
                    continue;
                }
                // An unmarked blank terminates this block quote.
                break;
            }
            if list && record.list_item() && record.indent() <= current.indent() {
                // A sibling item owns its own fold and cannot be swallowed by the previous item.
                break;
            }
            if let Some(offset) = container.child_start(&record) {
                // Explicit quoted/indented content can begin a new bounded child block.
                let delimiter = scan_fence_delimiter(document, record.start + offset, record.end)?;
                child_fence =
                    delimiter
                        .filter(|delimiter| delimiter.valid_opening)
                        .map(|delimiter| {
                            // Track only exact delimiter/container metadata, never a nested syntax tree.
                            FenceContext {
                                delimiter: (delimiter.marker, delimiter.length),
                                container: Some(container),
                            }
                        });
                allows_lazy = record.lazy_paragraph_body(offset);
                end = record.end;
                blank_seen = false;
                continue;
            }
            if blank_seen || !allows_lazy || interrupts_paragraph(&record) {
                // Only an actual paragraph child can consume an unmarked, noninterrupting lazy line.
                break;
            }
            end = record.end;
            continue;
        }
        if record.blank {
            // Empty lines distinguish paragraph termination from a resumable indented code block.
            if indented {
                // Indented code may resume after interior blanks without including trailing separators.
                continue;
            }
            // Ordinary paragraphs stop at their first blank separator.
            break;
        }
        if indented {
            // Indented code continues only while nonblank source retains its required four-column padding.
            if record.indent() < 4 {
                // An unindented nonblank line starts a different block after the code block.
                break;
            }
        } else if interrupts_paragraph(&record) {
            // A thematic break, heading, fence or container starts an independent block.
            break;
        }
        end = record.end;
    }
    Ok((end > current.end).then_some(MarkdownFoldRegion {
        header_line: line,
        hidden_utf16_range: current.end..end,
    }))
}

#[cfg(test)]
mod tests;
