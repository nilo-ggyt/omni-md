//! Atomic keyboard commands with bounded planning and shared canonical text/history publication.

use super::changes::{MarkdownObservedEditError, MarkdownTextChangeObserver};
use super::command_transaction::commit_command_observed;
use super::formatting::streaming::{CanonicalOutput, SourcePieceCursor};
use super::*;

mod markup;

/// Markdown keyboard behavior independent of a particular native text-system adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownEditingCommand {
    /// Continue Markdown block markup, otherwise insert an appropriately indented newline.
    InsertNewline,
    /// Remove the innermost applicable list/quote marker before native grapheme deletion.
    DeleteMarkupBackward,
    /// Add the default two-space indentation unit to selected lines.
    IndentMore,
    /// Remove one visual indentation unit, normalizing tab-derived remainder to spaces.
    IndentLess,
}

/// Distinguish native fallback, a consumed no-op, and one committed canonical text transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownEditingOutcome {
    /// The adapter should use its ordinary input behavior because no Markdown command applies.
    NotHandled,
    /// Consume the keyboard action without mutating text or creating a history entry.
    Unchanged,
    /// Notify the text system with this single committed edit, then apply its final selection.
    Changed(MarkdownHistoryTextEdit),
}

/// One original-coordinate indentation replacement; only scalar metadata survives each scan.
#[derive(Clone, Debug)]
struct IndentReplacement {
    bytes: Range<usize>,
    utf16: Range<usize>,
    spaces: usize,
}

impl MarkdownDocumentBuffer {
    /// Apply a primary-selection keyboard command as at most one canonical history transaction.
    ///
    /// Indentation uses the original editor's two-space unit and four-column tab stops. A nonempty
    /// selection ending exactly at the next line start excludes that line. Endpoint association is
    /// forward, including a collapsed caret at an inserted prefix. Read-only planning retains no
    /// line list or selected-text mirror; preparation streams new canonical bytes through fixed
    /// scratch and shares unaffected text. Failed capacity/boundary checks preserve all live state.
    ///
    /// # Errors
    ///
    /// Reject out-of-document/surrogate-interior selections, excessive live bytes or an exhausted
    /// session append budget. No rejected command changes canonical text, history or generation.
    pub fn apply_editing_command(
        &mut self,
        command: MarkdownEditingCommand,
        selection: MarkdownTextSelection,
    ) -> Result<MarkdownEditingOutcome, MarkdownDocumentBufferError> {
        self.apply_editing_command_observed(command, selection, changes::NoopObserver)
            .map(|(outcome, ())| outcome)
            .map_err(changes::legacy_error)
    }

    /// 键盘命令的真实变化流与正文共用一次可拒绝提交；NotHandled不产生伪变化。
    pub fn apply_editing_command_observed<O: MarkdownTextChangeObserver>(
        &mut self,
        command: MarkdownEditingCommand,
        selection: MarkdownTextSelection,
        observer: O,
    ) -> Result<(MarkdownEditingOutcome, O::Output), MarkdownObservedEditError<O::Error>> {
        self.ensure_text_input_idle()?;
        self.validate_selection(selection)?;
        match command {
            MarkdownEditingCommand::IndentMore | MarkdownEditingCommand::IndentLess => {
                self.apply_indentation(command, selection, observer)
            }
            MarkdownEditingCommand::InsertNewline
            | MarkdownEditingCommand::DeleteMarkupBackward => {
                markup::apply_markup_command_observed(self, command, selection, observer)
            }
        }
    }

    /// Preflight affected prefixes, future selection and byte geometry without a per-line heap list.
    fn apply_indentation<O: MarkdownTextChangeObserver>(
        &mut self,
        command: MarkdownEditingCommand,
        before: MarkdownTextSelection,
        observer: O,
    ) -> Result<(MarkdownEditingOutcome, O::Output), MarkdownObservedEditError<O::Error>> {
        let low = before.anchor_utf16.min(before.head_utf16);
        let high = before.anchor_utf16.max(before.head_utf16);
        let first = self.line_for_byte(self.byte_offset_for_utf16(low)?)?;
        let mut last = self.line_for_byte(self.byte_offset_for_utf16(high)?)?;
        if low != high && high == self.utf16_offset_for_byte(self.byte_offset_for_line(last)?)? {
            // CodeMirror's selected-line command excludes a range's trailing line-start endpoint.
            last -= 1;
        }

        // Aggregate a contiguous native notification while mapping endpoints through disjoint edits.
        let mut result_bytes = self.byte_len();
        let mut anchor = before.anchor_utf16;
        let mut head = before.head_utf16;
        let mut removed_units = 0_usize;
        let mut inserted_units = 0_usize;
        let mut envelope: Option<Range<usize>> = None;
        let mut changed = false;
        for line in first..=last {
            // Each iteration consumes one selected logical line and holds only one prefix descriptor.
            let edit = indentation_replacement(self, line, command)?;
            if edit.utf16.is_empty() && edit.spaces == 0 {
                // An already unindented line contributes neither history nor a coordinate translation.
                continue;
            }
            changed = true;
            result_bytes = result_bytes
                .checked_sub(edit.bytes.len())
                .and_then(|length| length.checked_add(edit.spaces))
                .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
            // Mapping always uses original coordinates; previous deltas are applied only to the result.
            anchor = map_indent_endpoint(before.anchor_utf16, anchor, &edit);
            head = map_indent_endpoint(before.head_utf16, head, &edit);
            removed_units += edit.utf16.len();
            inserted_units += edit.spaces;
            if let Some(range) = &mut envelope {
                // Later disjoint edits extend the same before-document notification envelope.
                range.end = edit.utf16.end;
            } else {
                // The first real edit establishes the native notification's original start.
                envelope = Some(edit.utf16.clone());
            }
        }
        if !changed {
            // Shift-Tab on unindented text is handled without changing selection, dirty state or undo.
            return complete_unchanged(self, before, MarkdownEditingOutcome::Unchanged, observer);
        }
        let envelope = envelope.expect("a changed indentation command has an edit envelope");
        let inserted = envelope.len() - removed_units + inserted_units;
        let metrics = MarkdownReplacementMetrics::new(envelope.start, envelope.len(), inserted)?;
        let after = MarkdownTextSelection {
            anchor_utf16: anchor,
            head_utf16: head,
        };
        let (edit, output) = commit_command_observed(
            self,
            metrics,
            result_bytes,
            before,
            observer,
            |document, observer| {
                // Preparation recomputes finite descriptors from unchanged original text, then publishes once.
                let root = prepare_indentation(document, first, last, command, observer)?;
                Ok((root, after))
            },
        )?;
        Ok((MarkdownEditingOutcome::Changed(edit), output))
    }
}

/// 不改变正文时仍允许显式head纯校验；派生结果由caller按实际是否消费命令采用。
fn complete_unchanged<O: MarkdownTextChangeObserver>(
    document: &MarkdownDocumentBuffer,
    selection: MarkdownTextSelection,
    outcome: MarkdownEditingOutcome,
    observer: O,
) -> Result<(MarkdownEditingOutcome, O::Output), MarkdownObservedEditError<O::Error>> {
    let output = observer
        .finish(
            changes::MarkdownPreparedText {
                document,
                root: &document.root,
            },
            selection,
        )
        .map_err(MarkdownObservedEditError::Observer)?;
    Ok((outcome, output))
}

/// Map a forward-associated original selection endpoint through one disjoint prefix replacement.
fn map_indent_endpoint(original: usize, mapped: usize, edit: &IndentReplacement) -> usize {
    if original < edit.utf16.start || (original == edit.utf16.start && !edit.utf16.is_empty()) {
        // A replacement's start stays before its new content; an insertion uses forward association.
        mapped
    } else if original >= edit.utf16.end {
        // At an insertion or replacement end, forward association follows the complete new prefix.
        mapped - edit.utf16.len() + edit.spaces
    } else {
        // An endpoint removed with indentation attaches after its replacement, preserving prior deltas.
        mapped - (original - edit.utf16.start) + edit.spaces
    }
}

/// Recognize the baseline ECMAScript whitespace set without consuming preserved line separators.
fn indentation_whitespace(scalar: char) -> bool {
    // NEL is deliberately absent, while BOM and Unicode line/paragraph separators match JS /\s/.
    matches!(
        scalar,
        '\t' | '\u{b}' | '\u{c}' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// Derive the minimal prefix edit, preserving an equal existing ASCII-space prefix on outdent.
fn indentation_replacement(
    document: &MarkdownDocumentBuffer,
    line: usize,
    command: MarkdownEditingCommand,
) -> Result<IndentReplacement, MarkdownDocumentBufferError> {
    let start = document.byte_offset_for_line(line)?;
    let start_utf16 = document.utf16_offset_for_byte(start)?;
    if command == MarkdownEditingCommand::IndentMore {
        // Tab always adds one two-space unit at the line start, even for a collapsed selection.
        return Ok(IndentReplacement {
            bytes: start..start,
            utf16: start_utf16..start_utf16,
            spaces: 2,
        });
    }
    let mut cursor = SourcePieceCursor::new(document.root.clone(), start..document.byte_len());
    let mut columns = 0_usize;
    let mut bytes = 0_usize;
    let mut units = 0_usize;
    let mut leading_ascii_spaces = 0_usize;
    let mut only_ascii_spaces = true;
    while let Some(source) = cursor.next_piece() {
        // Source pieces remain immutable; only scalar prefix metrics are accumulated across leaves.
        let stopped = document.with_piece_bytes(&source.piece, |source_bytes| {
            let text = std::str::from_utf8(&source_bytes[source.local])
                .expect("document pieces remain scalar aligned");
            for scalar in text.chars() {
                if !indentation_whitespace(scalar) {
                    // The first content scalar or CR/LF ends indentation without scanning the line body.
                    return Ok(true);
                }
                bytes += scalar.len_utf8();
                units += scalar.len_utf16();
                if scalar == '\t' {
                    // A tab advances to the next four-column stop used by the original editor.
                    columns += 4 - columns % 4;
                } else {
                    // Other JS whitespace contributes one visible indentation column.
                    columns += 1;
                }
                if only_ascii_spaces && scalar == ' ' {
                    // Reuse only the exact leading bytes that equal the normalized all-space target.
                    leading_ascii_spaces += 1;
                } else {
                    // Any tab or Unicode space ends the common literal prefix permanently.
                    only_ascii_spaces = false;
                }
            }
            Ok(false)
        })?;
        if stopped {
            break;
        }
    }
    let normalized = columns.saturating_sub(2);
    let keep = leading_ascii_spaces.min(normalized);
    Ok(IndentReplacement {
        bytes: start + keep..start + bytes,
        utf16: start_utf16 + keep..start_utf16 + units,
        spaces: normalized - keep,
    })
}

/// Stream one immutable source interval into canonical output without borrowing across append growth.
fn copy_source(
    document: &mut MarkdownDocumentBuffer,
    output: &mut CanonicalOutput,
    range: Range<usize>,
) -> Result<(), MarkdownDocumentBufferError> {
    let mut cursor = SourcePieceCursor::new(document.root.clone(), range);
    let mut scratch = [0_u8; ORIGINAL_PIECE_BYTES];
    while let Some(source) = cursor.next_piece() {
        // Copy one bounded scalar-aligned leaf before output may grow the backing segment vector.
        let length = document.with_piece_bytes(&source.piece, |bytes| {
            let bytes = &bytes[source.local];
            scratch[..bytes.len()].copy_from_slice(bytes);
            Ok(bytes.len())
        })?;
        output.push(document, &scratch[..length])?;
    }
    Ok(())
}

/// Prepare bounded output for dense commands, sharing a giant final-line body and outside pieces.
fn prepare_indentation<O: MarkdownTextChangeObserver>(
    document: &mut MarkdownDocumentBuffer,
    first: usize,
    last: usize,
    command: MarkdownEditingCommand,
    observer: &mut O,
) -> Result<PieceTreeLink, MarkdownObservedEditError<O::Error>> {
    if last - first < 4096 {
        // A finite sparse command shares all selected bodies and appends only normalized markers.
        let mut root = document.root.clone();
        let spaces = [b' '; ORIGINAL_PIECE_BYTES];
        let mut removed_bytes = 0_usize;
        let mut inserted_bytes = 0_usize;
        for line in first..=last {
            // 原坐标升序发变化；仅split位置应用此前字节delta，源查询仍读旧root。
            let edit = indentation_replacement(document, line, command)?;
            if edit.bytes.is_empty() && edit.spaces == 0 {
                // Unchanged prefixes need neither canonical bytes nor additional tree structure.
                continue;
            }
            changes::emit(document, observer, edit.utf16.clone(), edit.spaces)?;
            let mut output = CanonicalOutput::new();
            let mut remaining = edit.spaces;
            while remaining > 0 {
                // Giant tab-derived indentation still uses fixed scratch rather than a temporary string.
                let length = remaining.min(spaces.len());
                output.push(document, &spaces[..length])?;
                remaining -= length;
            }
            let inserted = output.finish(document)?;
            let at = edit.bytes.start - removed_bytes + inserted_bytes;
            let (prefix, rest) = split_piece_tree(document, root, at)?;
            let (_, suffix) = split_piece_tree(document, rest, edit.bytes.len())?;
            root = join_piece_trees(prefix, join_piece_trees(inserted, suffix));
            removed_bytes += edit.bytes.len();
            inserted_bytes += edit.spaces;
        }
        return Ok(root);
    }
    let start = document.byte_offset_for_line(first)?;
    let mut consumed = start;
    let mut output = CanonicalOutput::new();
    let spaces = [b' '; ORIGINAL_PIECE_BYTES];
    for line in first..=last {
        // Only original prefix descriptors are rescanned; unpublished output never changes source indexes.
        let edit = indentation_replacement(document, line, command)?;
        if !edit.utf16.is_empty() || edit.spaces != 0 {
            // 无变化行不虚构delete段；真实prefix替换仍按旧坐标流式进入journal。
            changes::emit(document, observer, edit.utf16.clone(), edit.spaces)?;
        }
        copy_source(document, &mut output, consumed..edit.bytes.start)?;
        let mut remaining = edit.spaces;
        while remaining > 0 {
            // Chunk normalized whitespace directly into canonical storage rather than a giant String.
            let length = remaining.min(spaces.len());
            output.push(document, &spaces[..length])?;
            remaining -= length;
        }
        consumed = edit.bytes.end;
    }
    let inserted = output.finish(document)?;
    // One split/rejoin retains the final line body and every unselected subtree by shared ownership.
    let (prefix, rest) = split_piece_tree(document, document.root.clone(), start)?;
    let (_, suffix) = split_piece_tree(document, rest, consumed - start)?;
    Ok(join_piece_trees(prefix, join_piece_trees(inserted, suffix)))
}

#[cfg(test)]
mod tests;
