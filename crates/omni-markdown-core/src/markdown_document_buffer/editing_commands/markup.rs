//! Portable Markdown Enter/Backspace planning over canonical source and shared atomic publication.
//!
//! Only active container descriptors and finite edit metadata are retained. Original body pieces stay
//! shared for sparse edits; dense ordered renumbering streams one canonical replacement in fixed scratch.

use super::super::formatting::replace_tree_range;
use super::*;

mod context;
mod foreign_blocks;
mod html_indentation;
mod markers;
mod source;
mod transaction;

use context::{Context, Kind, Scanner};
use source::{SourceCursor, SourceLine, SourceLines, range_is_whitespace};
use transaction::{Payload, Plan};

/// Resolve one primary native selection without relying on an AppKit or CodeMirror syntax owner.
pub(super) fn apply_markup_command(
    document: &mut MarkdownDocumentBuffer,
    command: MarkdownEditingCommand,
    before: MarkdownTextSelection,
) -> Result<MarkdownEditingOutcome, MarkdownDocumentBufferError> {
    let low = before.anchor_utf16.min(before.head_utf16);
    let high = before.anchor_utf16.max(before.head_utf16);
    let line_number = document.line_for_byte(document.byte_offset_for_utf16(low)?)?;
    let line = SourceLine::at(document, line_number)?;
    if low != high {
        // Markdown continuation only accepts carets; Enter still owns its ordinary selection replacement.
        return if command == MarkdownEditingCommand::InsertNewline {
            // The baseline default binding handles a nonempty selection after the Markdown command declines.
            fallback_newline(document, before, line)
        } else {
            // Ordinary native deletion retains grapheme/selection semantics outside this markup command.
            Ok(MarkdownEditingOutcome::NotHandled)
        };
    }
    let mut scanner = Scanner::new();
    let mut source_lines = SourceLines::new(document);
    for _ in 0..line_number {
        // Sequential prior boundaries prevent indexed rescans while the current body stays unmaterialized.
        let source = source_lines
            .next(document)
            .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
        scanner.consume(document, source)?;
    }
    let mut opaque = scanner.consume(document, line)?;
    if scanner
        .opening_body_from
        .is_some_and(|offset| low - line.units.0 <= offset)
    {
        // Left affinity keeps the surrounding Markdown marker active exactly before a new fence body.
        opaque = false;
    }
    let local = low - line.units.0;
    let selected = scanner
        .contexts
        .iter()
        .rfind(|context| context.eligible && context.from < local);
    let needs_future = selected
        .is_some_and(|context| context.is_list() && context.item_index == 0 && !context.loose)
        && line_number + 1 < document.line_count()
        && command == MarkdownEditingCommand::InsertNewline;
    let mut contexts = if needs_future {
        // Only an unresolved first-item loose-list query needs a second simultaneous active-path descriptor set.
        scanner.contexts.clone()
    } else {
        // Quote/backspace/already-known list commands transfer their path rather than doubling deep metadata.
        std::mem::take(&mut scanner.contexts)
    };
    contexts.retain(|context| {
        // A marker beginning to the right of the caret has no command authority at that position.
        context.eligible && context.from < low - line.units.0
    });
    if opaque || contexts.is_empty() {
        // Fenced/foreign syntax and ordinary prose delegate Enter to its default indentation behavior.
        return if command == MarkdownEditingCommand::InsertNewline {
            // Enter remains consumed even when no Markdown continuation applies.
            fallback_newline(document, before, line)
        } else {
            // Native Backspace performs ordinary grapheme deletion in this context.
            Ok(MarkdownEditingOutcome::NotHandled)
        };
    }
    if command == MarkdownEditingCommand::DeleteMarkupBackward {
        // Backspace needs only the current active path and has no future sibling-renumbering pass.
        return delete_markup(document, before, line, contexts);
    }
    // Loose-list status depends on the first two sibling items, which may follow this caret.
    let mut source_lines = if needs_future {
        // Restart after the current line without decoding its possibly enormous unmodified body.
        Some(SourceLines::from_line(document, line_number + 1)?)
    } else {
        // The final logical source line has no future sibling that could change looseness.
        None
    };
    while let Some(source) = source_lines.as_mut().and_then(|lines| lines.next(document)) {
        // Forward context scanning stores no suffix text or item list; it updates only current ancestors.
        scanner.consume(document, source)?;
        for context in &mut contexts {
            // Current list identities stay source-based even while later siblings replace active item metadata.
            if let Some(later) = scanner
                .contexts
                .iter()
                .find(|later| later.node_start == context.node_start && later.kind == context.kind)
            {
                // A detected first/second-item separator establishes the same loose-list rule as the baseline.
                context.loose |= later.loose;
            }
        }
        let inner = contexts.last().expect("selected context");
        let later = scanner
            .contexts
            .iter()
            .find(|later| later.node_start == inner.node_start && later.kind == inner.kind);
        if later.is_none_or(|later| later.item_index >= 1) {
            // First/second sibling geometry is now known, or this list ended; unrelated later source is irrelevant.
            break;
        }
    }
    // Future scanning no longer owns a second path when command output acquires canonical append storage.
    drop(scanner);
    insert_markup(document, before, line, contexts)
}

/// Continue, exit or loosen the innermost list/quote using one source-coordinate transaction.
fn insert_markup(
    document: &mut MarkdownDocumentBuffer,
    before: MarkdownTextSelection,
    line: SourceLine,
    contexts: Vec<Context>,
) -> Result<MarkdownEditingOutcome, MarkdownDocumentBufferError> {
    let inner = contexts.last().expect("nonempty command contexts");
    let pos = before.head_utf16;
    let local = pos - line.units.0;
    if inner.to - inner.after > local {
        // A caret inside marker punctuation precedes continuation authority and uses ordinary Enter.
        return fallback_newline(document, before, line);
    }
    let empty = range_is_whitespace(
        document,
        line.units.0 + inner.to.min(line.units.1 - line.units.0)..line.units.1,
        false,
    )?;
    let mut plan = Plan::new(contexts, line, newline_separator(document)?, document)?;
    let inner_index = plan.contexts.len() - 1;
    let inner = &plan.contexts[inner_index];
    if inner.is_list() && empty {
        // Empty item commands choose between exiting one level and establishing loose-list separation.
        if inner.item_line < line.number
            && !range_is_whitespace(
                document,
                line.units.0..line.units.0 + inner.to.min(line.units.1 - line.units.0),
                true,
            )?
        {
            // A continued nonblank marker-shaped line is not the list item's empty syntax line.
            return fallback_newline(document, before, line);
        }
        let previous_blank = if line.number > 0 {
            // The original command treats whitespace/quote-only source immediately before this item as blank.
            let previous = SourceLine::at(document, line.number - 1)?;
            range_is_whitespace(document, previous.units.0..previous.units.1, true)?
        } else {
            // The first document line has no preceding separator to authorize a loose-list transition.
            false
        };
        if inner.item_index != 1 || previous_blank {
            // Empty first/third-or-later items, and separated second items, remove exactly one markup level.
            let (start, payload) = if inner_index > 0 {
                // A parent list gains its next sibling marker; a parent quote retains its own marker prefix.
                let parent = &plan.contexts[inner_index - 1];
                if parent.is_list() {
                    // Nested list exit replaces this child's markup with the next outer sibling spelling.
                    (
                        line.units.0 + parent.from,
                        Payload::Marker {
                            index: inner_index - 1,
                            add: 1,
                        },
                    )
                } else {
                    // Quoted list exit deletes only the inner list, leaving the quote marker visible.
                    (line.units.0 + parent.to, Payload::Empty)
                }
            } else {
                // A top-level empty list exits directly to ordinary paragraph source.
                (line.units.0, Payload::Empty)
            };
            plan.add(start..pos, payload);
            plan.after = plan.edits[0].range.start + plan.payload_units(&plan.edits[0].payload)?;
            plan.add_renumber(inner_index, -2);
            if inner_index > 0 && plan.contexts[inner_index - 1].is_list() {
                // Advancing an outer ordered list also renumbers that parent's subsequent siblings.
                plan.add_renumber(inner_index - 1, 0);
            }
        } else {
            // The second empty item of a tight list first inserts a blank separator, preserving both items.
            plan.add(line.units.0..line.units.0, Payload::BlankOuterAndBreak);
            plan.after = pos + plan.payload_units(&plan.edits[0].payload)?;
        }
        return transaction::commit(document, before, plan);
    }
    if inner.kind == Kind::Quote && empty && line.number > 0 {
        // Two aligned quote-only lines exit one quote level by deleting each line's final marker suffix.
        let previous = SourceLine::at(document, line.number - 1)?;
        if last_nonwhitespace(document, previous)?
            .is_some_and(|value| value.0 == '>' && value.1 - previous.units.0 == inner.from)
        {
            // Both source ranges are retained as separate finite edits but publish one native history receipt.
            let inner_from = inner.from;
            let first_start = previous.units.0 + inner_from;
            plan.add(first_start..previous.units.1, Payload::Empty);
            plan.add(line.units.0 + inner_from..line.units.1, Payload::Empty);
            plan.after = line.units.0 + inner_from - (previous.units.1 - first_start);
            return transaction::commit(document, before, plan);
        }
    }
    let continued = inner.is_list() && inner.item_line < line.number;
    let preserve_prefix = !continued || allowed_markup_prefix(document, line)? >= inner.to;
    let trim = last_nonwhitespace_before(document, line, pos)?;
    plan.add(
        trim..pos,
        Payload::ContinuedNewline {
            continued,
            preserve_prefix,
            loose: inner.loose,
        },
    );
    plan.after = trim + plan.payload_units(&plan.edits[0].payload)?;
    plan.add_renumber(inner_index, 0);
    transaction::commit(document, before, plan)
}

/// Apply only the baseline marker/indentation deletion, leaving ordinary Backspace to the native view.
fn delete_markup(
    document: &mut MarkdownDocumentBuffer,
    before: MarkdownTextSelection,
    line: SourceLine,
    contexts: Vec<Context>,
) -> Result<MarkdownEditingOutcome, MarkdownDocumentBufferError> {
    let inner = contexts.last().expect("active context");
    let local = before.head_utf16 - line.units.0;
    let space_end = inner.to - inner.after + usize::from(inner.after != 0);
    let mut plan = Plan::new(contexts, line, newline_separator(document)?, document)?;
    let inner = plan.contexts.last().expect("active context");
    if local > space_end
        && range_is_whitespace(document, line.units.0 + space_end..before.head_utf16, false)?
    {
        // Extra marker padding is deleted before a later key can remove or blank the marker itself.
        plan.add(line.units.0 + space_end..before.head_utf16, Payload::Empty);
        plan.after = line.units.0 + space_end;
        return transaction::commit(document, before, plan);
    }
    if local == space_end
        && ((inner.is_list() && line.number <= inner.item_line)
            || range_is_whitespace(
                document,
                line.units.0..line.units.0 + inner.to.min(line.units.1 - line.units.0),
                true,
            )?)
    {
        let start = line.units.0 + inner.from;
        if inner.is_list()
            && inner.node_start < line.units.0 + inner.from
            && !range_is_whitespace(document, start..line.units.0 + inner.to, false)?
        {
            // A subsequent list item's first Backspace blanks its marker, retaining content indentation.
            let spaces = columns_until(document, line, inner.to)?
                - columns_until(document, line, inner.from)?;
            plan.add(start..line.units.0 + inner.to, Payload::Spaces(spaces));
            plan.after = start + spaces;
        } else if start < before.head_utf16 {
            // First-item/quote syntax is removed rather than preserving a sibling marker's indentation.
            plan.add(start..before.head_utf16, Payload::Empty);
            plan.after = start;
        } else {
            // A caret already before this marker has no Markdown-specific deletion to consume.
            return Ok(MarkdownEditingOutcome::NotHandled);
        }
        return transaction::commit(document, before, plan);
    }
    Ok(MarkdownEditingOutcome::NotHandled)
}

/// Replace a selection with the default Markdown newline/indent fallback, including adjacent brackets.
fn fallback_newline(
    document: &mut MarkdownDocumentBuffer,
    before: MarkdownTextSelection,
    line: SourceLine,
) -> Result<MarkdownEditingOutcome, MarkdownDocumentBufferError> {
    let mut from = before.anchor_utf16.min(before.head_utf16);
    let mut to = before.anchor_utf16.max(before.head_utf16);
    let indent = foreign_blocks::html_indentation(document, line, from)?
        .unwrap_or(leading_columns(document, line)?);
    let explode = from == to && adjacent_brackets(document, from)?;
    if to < line.units.1 {
        // Default Enter removes whitespace immediately to the right of a selection/caret on its first line.
        let mut cursor = line.cursor(document, to - line.units.0)?;
        while let Some((scalar, _, unit)) = cursor
            .peek(document)
            .filter(|value| indentation_whitespace(value.0))
        {
            // Whole scalar positions preserve supplementary-boundary validation of the original selection.
            cursor.next(document);
            to = unit + scalar.len_utf16();
        }
    }
    if !explode && from > line.units.0 && from < line.units.0 + 100 {
        // Match the preserved command's exact line-text slice association rather than rewriting its behavior.
        let logical_from =
            from.saturating_sub(line.number * (newline_separator(document)?.len() - 1));
        let slice_end = (line.units.0 + logical_from).min(line.units.1);
        if range_is_whitespace(document, line.units.0..slice_end, false)? {
            // Short whitespace-only prefixes are removed before the newline rather than left on the old line.
            from = line.units.0;
        }
    }
    let separator = newline_separator(document)?;
    let mut plan = Plan::new(Vec::new(), line, separator, document)?;
    plan.add(from..to, Payload::PlainNewline { indent, explode });
    plan.after = from + separator.len() + indent;
    transaction::commit(document, before, plan)
}

/// Determine the baseline configured physical separator without retaining any source string.
fn newline_separator(
    document: &MarkdownDocumentBuffer,
) -> Result<&'static str, MarkdownDocumentBufferError> {
    let mut pieces = SourcePieceCursor::new(document.root.clone(), 0..document.byte_len());
    let mut previous_cr = false;
    let mut carriage = false;
    while let Some(piece) = pieces.next_piece() {
        // Byte membership can skip LF-only giant bodies without scalar decoding or retained text.
        let bytes = &document.piece_bytes(&piece.piece)[piece.local];
        if previous_cr && bytes.first() == Some(&b'\n') {
            // A CRLF pair can straddle canonical leaf boundaries without changing separator identity.
            return Ok("\r\n");
        }
        if bytes.contains(&b'\r') {
            // Only source containing a CR needs the more specific adjacent-pair scan.
            carriage = true;
            if bytes.windows(2).any(|pair| pair == b"\r\n") {
                // The original configured separator prefers CRLF whenever it exists in the input.
                return Ok("\r\n");
            }
        }
        previous_cr = bytes.last() == Some(&b'\r');
    }
    Ok(if carriage {
        // A CR-only file preserves its physical standalone separators for the new line.
        "\r"
    } else {
        // LF remains the default for ordinary and initially empty documents.
        "\n"
    })
}

/// Count line-local indentation columns with the baseline four-column tab stops.
fn leading_columns(
    document: &MarkdownDocumentBuffer,
    line: SourceLine,
) -> Result<usize, MarkdownDocumentBufferError> {
    let mut cursor = line.cursor(document, 0)?;
    let mut columns = 0;
    while let Some((scalar, _, _)) = cursor.next(document) {
        // Leading whitespace alone determines default indentation; the body is never scanned for columns.
        if !indentation_whitespace(scalar) {
            // The first content scalar fixes the indentation regardless of a giant following body.
            break;
        }
        columns += if scalar == '\t' {
            // A tab advances to the next four-column editor stop.
            4 - columns % 4
        } else {
            // Every other complete scalar contributes one visual column.
            1
        };
    }
    Ok(columns)
}

/// Count visible prefix columns without treating a tab as one physical source unit.
fn columns_until(
    document: &MarkdownDocumentBuffer,
    line: SourceLine,
    end: usize,
) -> Result<usize, MarkdownDocumentBufferError> {
    let mut cursor = line.cursor(document, 0)?;
    let mut columns = 0;
    while let Some((scalar, _, unit)) = cursor.next(document) {
        // Count one ordered source prefix rather than repeatedly converting each tab-expanded boundary.
        if unit - line.units.0 >= end {
            // The requested native offset excludes this scalar and all later source.
            break;
        }
        columns += if scalar == '\t' {
            // A tab advances to the next four-column editor stop.
            4 - columns % 4
        } else {
            // Every other complete scalar contributes one visual column.
            1
        };
    }
    Ok(columns)
}

/// Find the final non-whitespace scalar with constant metadata, including its canonical native offset.
fn last_nonwhitespace(
    document: &MarkdownDocumentBuffer,
    line: SourceLine,
) -> Result<Option<(char, usize)>, MarkdownDocumentBufferError> {
    let mut cursor = line.cursor(document, 0)?;
    let mut last = None;
    while let Some((scalar, _, unit)) = cursor.next(document) {
        // Quote-exit and embedded continuation checks require the final content scalar, not a copied line.
        if !indentation_whitespace(scalar) {
            // Save only its identity and canonical native start; following whitespace cannot replace it.
            last = Some((scalar, unit));
        }
    }
    Ok(last)
}

/// Trim only contiguous command whitespace immediately preceding the caret, never source before content.
fn last_nonwhitespace_before(
    document: &MarkdownDocumentBuffer,
    line: SourceLine,
    end: usize,
) -> Result<usize, MarkdownDocumentBufferError> {
    let mut byte_end = document.byte_offset_for_utf16(end)?;
    let mut scratch = [0_u8; ORIGINAL_PIECE_BYTES];
    while byte_end > line.bytes.0 {
        // Walk backward in fixed byte windows: ordinary giant body text stops at its final scalar.
        let start = byte_end.saturating_sub(scratch.len()).max(line.bytes.0);
        let mut used = 0;
        super::super::piece_tree::visit_piece_tree_range(
            document,
            document.root.as_deref(),
            start..byte_end,
            0,
            &mut |bytes| {
                // A window may begin inside UTF-8; byte copying retains no backing borrow or body mirror.
                scratch[used..used + bytes.len()].copy_from_slice(bytes);
                used += bytes.len();
                Ok(())
            },
        )?;
        let skip = scratch[..used]
            .iter()
            .take_while(|byte| **byte & 0xc0 == 0x80)
            .count();
        let text = std::str::from_utf8(&scratch[skip..used])
            .expect("window tail ends at a canonical scalar boundary");
        for (offset, scalar) in text.char_indices().rev() {
            // Only trailing ECMAScript whitespace is discarded; a complete content scalar ends the search.
            if !indentation_whitespace(scalar) {
                // Convert the single final content boundary once after the reverse scan finishes.
                return document.utf16_offset_for_byte(start + skip + offset + scalar.len_utf8());
            }
        }
        // Revisit an initial partial scalar in the preceding window rather than silently dropping it.
        byte_end = start + skip;
    }
    Ok(line.units.0)
}

/// Measure the original continuation command's finite character-class prefix without copying it.
fn allowed_markup_prefix(
    document: &MarkdownDocumentBuffer,
    line: SourceLine,
) -> Result<usize, MarkdownDocumentBufferError> {
    let mut cursor = line.cursor(document, 0)?;
    let mut end = 0;
    while let Some((scalar, _, unit)) = cursor.next(document) {
        // Preserve the original command character class exactly, including digits and mixed marker punctuation.
        if !indentation_whitespace(scalar)
            && !scalar.is_ascii_digit()
            && !matches!(scalar, '.' | ')' | '-' | '+' | '*' | '>')
        {
            // Ordinary body text ends prefix authority immediately without inspecting the remaining line.
            break;
        }
        end = unit - line.units.0 + scalar.len_utf16();
    }
    Ok(end)
}

/// The default command explodes immediately adjacent conventional bracket pairs into two lines.
fn adjacent_brackets(
    document: &MarkdownDocumentBuffer,
    position: usize,
) -> Result<bool, MarkdownDocumentBufferError> {
    if position == 0 || position == document.utf16_len() {
        // A document edge cannot have both members of an immediately adjacent bracket pair.
        return Ok(false);
    }
    let mut units = [0; 2];
    document.copy_utf16_code_units(position - 1, &mut units)?;
    Ok(matches!(units, [40, 41] | [91, 93] | [123, 125]))
}

#[cfg(test)]
mod markup_tests;
