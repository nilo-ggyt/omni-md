//! Atomic Markdown commands over shared pieces, without materializing the selected body.

use super::changes::{MarkdownObservedEditError, MarkdownTextChangeObserver};
use super::*;

pub(super) mod streaming;

use streaming::{prepare_dense_line_prefix, scan_heading_whitespace};

/// Shared-marker insertion is finite; dense selections use a chunked canonical replacement instead.
const MAX_SPARSE_PREFIX_LINES: usize = 4096;

/// Product formatting commands, preserving the original editor's primary-selection behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownFormatAction {
    Bold,
    Italic,
    Underline,
    Strikethrough,
    InlineCode,
    InlineFormula,
    Superscript,
    Link,
    Heading1,
    Heading2,
    Heading3,
    BulletList,
    OrderedList,
    TaskList,
    Blockquote,
    Divider,
    CodeBlock,
    FormulaBlock,
    Table,
}

impl MarkdownFormatAction {
    #[must_use]
    /// 正文与已验证单格共用行内源标记和空选区占位文字；块命令不具有格内包裹语义。
    pub fn inline_markers(self) -> Option<(&'static str, &'static str, &'static str)> {
        match self {
            Self::Bold => Some(("**", "**", "加粗文字")), // 保留既有加粗分隔符与占位文字。
            Self::Italic => Some(("*", "*", "斜体文字")), // 不额外解释或切换已有标记。
            Self::Underline => Some(("<u>", "</u>", "下划线文字")), // 下划线继续使用 canonical HTML。
            Self::Strikethrough => Some(("~~", "~~", "删除线文字")), // GFM 成对波浪线。
            Self::InlineCode => Some(("`", "`", "code")),           // 空 caret 保留原 code 占位。
            Self::InlineFormula => Some(("$", "$", "x^2")),         // 行内公式只包裹表达式。
            Self::Superscript => Some(("<sup>", "</sup>", "上标")), // 上标仍为明确 HTML 源。
            Self::Link => Some(("[", "](https://)", "链接文字")),   // URL 模板不进入标签选区。
            _ => None, // 标题、列表与模板保持各自正文块计划，不伪造表格行内表达。
        }
    }
}

/// A constant-size plan keeps only markers and coordinates, never a selected-text copy.
enum FormatPlan {
    Replace {
        range: Range<usize>,
        text: &'static str,
        selection_after: MarkdownTextSelection,
    },
    Surround {
        range: Range<usize>,
        opening: &'static str,
        closing: &'static str,
        placeholder: Option<&'static str>,
    },
    PrefixLines {
        first: usize,
        last: usize,
        prefix: &'static str,
        selection_after: MarkdownTextSelection,
    },
}

impl MarkdownDocumentBuffer {
    /// Apply one formatting command as one structural undo/redo transaction.
    ///
    /// Coordinates are scalar-aligned UTF-16 positions. Heading commands replace the prefix on
    /// the head's line; list/quote commands prefix every intersected line, including an endpoint
    /// exactly at the next line start. Wrapping selects the inner text or placeholder; templates
    /// insert at the selection head. Heading/list selection mapping matches the original editor.
    ///
    /// The returned aggregate edit addresses the document before this command. Native adapters
    /// notify TextKit once using that range/delta, then apply `selection_after`. Rejections preserve
    /// text, selection, history, generation, and appended edit bytes. Inline wrappers and at most
    /// 4096 prefixed lines share original pieces and constant-size marker backing. Denser line
    /// selections stream one canonical replacement through fixed scratch, bounding new tree metadata
    /// by chunk count instead of line count. Its full output consumes the normal lifetime edit budget;
    /// no command creates a document-sized temporary string or attributed-text mirror. Dense source
    /// scanning is O(selected bytes), with O(output chunks * log(chunks)) tree assembly; its temporary
    /// scratch is 8 KiB plus an original-tree-height metadata stack. Heading whitespace is scanned once.
    pub fn apply_format(
        &mut self,
        action: MarkdownFormatAction,
        selection: MarkdownTextSelection,
    ) -> Result<MarkdownHistoryTextEdit, MarkdownDocumentBufferError> {
        self.apply_format_observed(action, selection, changes::NoopObserver)
            .map(|(edit, ())| edit)
            .map_err(changes::legacy_error)
    }

    /// 格式化保留真实插入/替换边界，不能把两侧Surround误当覆盖中间正文的一项替换。
    pub fn apply_format_observed<O: MarkdownTextChangeObserver>(
        &mut self,
        action: MarkdownFormatAction,
        selection: MarkdownTextSelection,
        observer: O,
    ) -> Result<(MarkdownHistoryTextEdit, O::Output), MarkdownObservedEditError<O::Error>> {
        self.ensure_text_input_idle()?;
        self.validate_selection(selection)?;
        let plan = self.plan_format(action, selection)?;
        let (metrics, result_bytes) = plan.geometry(self)?;
        super::command_transaction::commit_command_observed(
            self,
            metrics,
            result_bytes,
            selection,
            observer,
            |document, observer| plan.prepare(document, observer),
        )
    }

    /// Lower the product command to bounded markers and original-document coordinates.
    fn plan_format(
        &self,
        action: MarkdownFormatAction,
        selection: MarkdownTextSelection,
    ) -> Result<FormatPlan, MarkdownDocumentBufferError> {
        use MarkdownFormatAction::*;
        let start = selection.anchor_utf16.min(selection.head_utf16);
        let end = selection.anchor_utf16.max(selection.head_utf16);
        let wrap = |opening, closing, placeholder| {
            // Both populated and empty selections share the exact inline command contract.
            Ok(wrap_plan(start..end, opening, closing, placeholder))
        };
        match action {
            Bold | Italic | Underline | Strikethrough | InlineCode | InlineFormula
            | Superscript | Link => {
                // 此分支按相同八种 enum 准入；与单格格式消费同一领域投影，不维护第二套标记。
                let (opening, closing, placeholder) = action
                    .inline_markers()
                    .expect("行内格式分支必须具有领域标记");
                wrap(opening, closing, placeholder)
            }
            // Heading levels replace an existing H1-H6 prefix on the head's current line only.
            Heading1 => self.plan_heading("# ", selection),
            // H2 changes only the requested level; its line and selection mapping follow the same contract.
            Heading2 => self.plan_heading("## ", selection),
            // H3 replaces any existing H1-H6 marker instead of accumulating heading prefixes.
            Heading3 => self.plan_heading("### ", selection),
            // Each line receives the literal baseline prefix rather than a generated list number.
            BulletList => self.plan_line_prefix("- ", selection),
            // Every selected line receives the baseline literal ordinal rather than automatic renumbering.
            OrderedList => self.plan_line_prefix("1. ", selection),
            // Each selected line starts an unchecked task while retaining its existing content.
            TaskList => self.plan_line_prefix("- [ ] ", selection),
            // Quote every intersected line, including empty lines and a selected trailing line start.
            Blockquote => self.plan_line_prefix("> ", selection),
            // Templates insert at the head without deleting an existing selection.
            Divider => Ok(template_plan(selection.head_utf16, "\n---\n", 5, 5)),
            // Select only the code placeholder, keeping fence newlines outside the active range.
            CodeBlock => Ok(template_plan(
                selection.head_utf16,
                "\n```\ncode\n```\n",
                5,
                9,
            )),
            // Select the formula expression inside the two display-math fences.
            FormulaBlock => Ok(template_plan(selection.head_utf16, "\n$$\nx^2\n$$\n", 4, 7)),
            // The format-menu template retains the baseline's two-column table and header selection.
            Table => Ok(template_plan(
                selection.head_utf16,
                "\n| 列 1 | 列 2 |\n| --- | --- |\n| 内容 | 内容 |\n",
                3,
                6,
            )),
        }
    }

    /// Replace only a valid existing heading prefix, including its line-local Unicode whitespace.
    fn plan_heading(
        &self,
        prefix: &'static str,
        selection: MarkdownTextSelection,
    ) -> Result<FormatPlan, MarkdownDocumentBufferError> {
        let line = self.line_for_byte(self.byte_offset_for_utf16(selection.head_utf16)?)?;
        let start = self.utf16_offset_for_byte(self.byte_offset_for_line(line)?)?;
        let mut hashes = 0;
        while start + hashes < self.utf16_len() && self.utf16_code_unit_at(start + hashes)? == 35 {
            // A seventh hash cannot be an H1-H6 prefix, so scanning need not continue across the line.
            hashes += 1;
            if hashes == 7 {
                break;
            }
        }
        let mut end = start;
        if (1..=6).contains(&hashes) {
            // ECMAScript's original /^#{1,6}\s+/ is line-local; CR/LF remain untouched document bytes.
            // Visit each source scalar once instead of repeatedly decoding a 4-KiB leaf with nth().
            let whitespace_end = start + hashes + scan_heading_whitespace(self, start + hashes)?;
            if whitespace_end > start + hashes {
                // Only a heading followed by at least one whitespace unit qualifies for replacement.
                end = whitespace_end;
            }
        }
        let range = start..end;
        Ok(FormatPlan::Replace {
            selection_after: map_selection_replacement(selection, &range, prefix.len()),
            range,
            text: prefix,
        })
    }

    /// Derive inclusive endpoint lines without retaining a list or copying their contents.
    fn plan_line_prefix(
        &self,
        prefix: &'static str,
        selection: MarkdownTextSelection,
    ) -> Result<FormatPlan, MarkdownDocumentBufferError> {
        let low = selection.anchor_utf16.min(selection.head_utf16);
        let high = selection.anchor_utf16.max(selection.head_utf16);
        let first = self.line_for_byte(self.byte_offset_for_utf16(low)?)?;
        let last = self.line_for_byte(self.byte_offset_for_utf16(high)?)?;
        let map = |position, associate_after| -> Result<usize, MarkdownDocumentBufferError> {
            // Earlier affected lines shift every endpoint; a marker exactly at it follows its association.
            let line = self.line_for_byte(self.byte_offset_for_utf16(position)?)?;
            let line_start = self.utf16_offset_for_byte(self.byte_offset_for_line(line)?)?;
            let affected = line - first + usize::from(position > line_start || associate_after);
            position
                .checked_add(
                    affected
                        .checked_mul(prefix.len())
                        .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?,
                )
                .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)
        };
        let mapped_low = map(low, low != high)?;
        let mapped_high = map(high, false)?;
        Ok(FormatPlan::PrefixLines {
            first,
            last,
            prefix,
            selection_after: orient_selection(selection, mapped_low, mapped_high),
        })
    }
}

impl FormatPlan {
    /// Preflight aggregate edit geometry and the actual live-byte cap before acquiring marker storage.
    fn geometry(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<(MarkdownReplacementMetrics, usize), MarkdownDocumentBufferError> {
        let (range, inserted_utf16, result_bytes) = match self {
            // One replacement may remove only a heading prefix or insert a fixed-size template.
            Self::Replace { range, text, .. } => {
                let removed_bytes = document.byte_offset_for_utf16(range.end)?
                    - document.byte_offset_for_utf16(range.start)?;
                let result_bytes = document
                    .byte_len()
                    .checked_sub(removed_bytes)
                    .and_then(|length| length.checked_add(text.len()))
                    .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
                (range.clone(), text.encode_utf16().count(), result_bytes)
            }
            // Selected body bytes stay present; only wrapper marker bytes contribute to growth.
            Self::Surround {
                range,
                opening,
                closing,
                placeholder,
            } => {
                let growth = opening.len() + closing.len() + placeholder.map_or(0, str::len);
                let utf16_growth = opening.encode_utf16().count()
                    + closing.encode_utf16().count()
                    + placeholder.map_or(0, |text| {
                        // The historical Chinese placeholder uses UTF-16 units independently of UTF-8 bytes.
                        text.encode_utf16().count()
                    });
                let result_bytes = document
                    .byte_len()
                    .checked_add(growth)
                    .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
                (
                    range.clone(),
                    range
                        .len()
                        .checked_add(utf16_growth)
                        .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?,
                    result_bytes,
                )
            }
            // Logical marker growth counts every affected line in either sparse or dense preparation.
            Self::PrefixLines {
                first,
                last,
                prefix,
                ..
            } => {
                let start =
                    document.utf16_offset_for_byte(document.byte_offset_for_line(*first)?)?;
                let end = document.utf16_offset_for_byte(document.byte_offset_for_line(*last)?)?;
                let growth = (last - first + 1)
                    .checked_mul(prefix.len())
                    .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
                let result_bytes = document
                    .byte_len()
                    .checked_add(growth)
                    .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
                (
                    start..end,
                    (end - start)
                        .checked_add(growth)
                        .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?,
                    result_bytes,
                )
            }
        };
        let metrics = MarkdownReplacementMetrics::new(range.start, range.len(), inserted_utf16)?;
        Ok((metrics, result_bytes))
    }

    /// Build an unpublished persistent root, sharing every unaffected and selected original piece.
    fn prepare<O: MarkdownTextChangeObserver>(
        self,
        document: &mut MarkdownDocumentBuffer,
        observer: &mut O,
    ) -> Result<(PieceTreeLink, MarkdownTextSelection), MarkdownObservedEditError<O::Error>> {
        // Each plan prepares a private root; the caller alone publishes it and its history memento.
        match self {
            // Static replacement text is appended only after the caller's complete size preflight.
            Self::Replace {
                range,
                text,
                selection_after,
            } => {
                let byte_range = document.byte_offset_for_utf16(range.start)?
                    ..document.byte_offset_for_utf16(range.end)?;
                changes::emit(
                    document,
                    observer,
                    range.clone(),
                    text.encode_utf16().count(),
                )?;
                let inserted = append_marker(document, text)?;
                let root =
                    replace_tree_range(document, document.root.clone(), byte_range, inserted)?;
                Ok((root, selection_after))
            }
            // Descending original positions keep the opening coordinate stable after closing insertion.
            Self::Surround {
                range,
                opening,
                closing,
                placeholder,
            } => {
                if let Some(placeholder) = placeholder {
                    // 空选区模板是一项真实插入；不构造三项重叠的伪替换。
                    let inserted = opening.encode_utf16().count()
                        + placeholder.encode_utf16().count()
                        + closing.encode_utf16().count();
                    changes::emit(document, observer, range.clone(), inserted)?;
                } else {
                    // 非空选区仅两端插入，中间原文与其中fold保持独立。
                    changes::emit(
                        document,
                        observer,
                        range.start..range.start,
                        opening.encode_utf16().count(),
                    )?;
                    changes::emit(
                        document,
                        observer,
                        range.end..range.end,
                        closing.encode_utf16().count(),
                    )?;
                }
                let start = document.byte_offset_for_utf16(range.start)?;
                let end = document.byte_offset_for_utf16(range.end)?;
                let closing_root = append_marker(document, closing)?;
                let mut root =
                    replace_tree_range(document, document.root.clone(), end..end, closing_root)?;
                if let Some(placeholder) = placeholder {
                    // Only a collapsed selection receives a static placeholder; selected bytes remain untouched.
                    let inner = append_marker(document, placeholder)?;
                    root = replace_tree_range(document, root, start..start, inner)?;
                }
                let opening_root = append_marker(document, opening)?;
                let root = replace_tree_range(document, root, start..start, opening_root)?;
                let offset = opening.encode_utf16().count();
                Ok((
                    root,
                    MarkdownTextSelection {
                        anchor_utf16: range.start + offset,
                        head_utf16: range.end
                            + offset
                            + placeholder.map_or(0, |text| {
                                // Select the complete placeholder in native UTF-16 coordinates, including CJK content.
                                text.encode_utf16().count()
                            }),
                    },
                ))
            }
            // Sparse selections share one marker; dense selections bound metadata by streamed chunks.
            Self::PrefixLines {
                first,
                last,
                prefix,
                selection_after,
            } => {
                if last - first + 1 > MAX_SPARSE_PREFIX_LINES {
                    // Copy directly into new canonical edit segments, with no selected-body mirror.
                    let root = prepare_dense_line_prefix(document, first, last, prefix, observer)?;
                    return Ok((root, selection_after));
                }
                // 稀疏行数已有4096上限；先升序发真实插入，随后仍沿旧算法倒序组树。
                for line in first..=last {
                    let at =
                        document.utf16_offset_for_byte(document.byte_offset_for_line(line)?)?;
                    changes::emit(document, observer, at..at, prefix.encode_utf16().count())?;
                }
                // Only a finite number of split/rejoin operations may duplicate original leaf metadata.
                let inserted = append_marker(document, prefix)?;
                let mut root = document.root.clone();
                for line in (first..=last).rev() {
                    // Descending line starts remain original coordinates, including a trailing empty line.
                    let at = document.byte_offset_for_line(line)?;
                    root = replace_tree_range(document, root, at..at, inserted.clone())?;
                }
                Ok((root, selection_after))
            }
        }
    }
}

/// Describe inline formatting using constant-size markers and a caret-only placeholder.
fn wrap_plan(
    range: Range<usize>,
    opening: &'static str,
    closing: &'static str,
    placeholder: &'static str,
) -> FormatPlan {
    // A nonempty selection stays in its original shared pieces instead of becoming an edit-buffer copy.
    let placeholder = range.is_empty().then_some(placeholder);
    FormatPlan::Surround {
        range,
        opening,
        closing,
        placeholder,
    }
}

/// Append only a fixed marker/template, with the document's normal lifetime edit budget.
fn append_marker(
    document: &mut MarkdownDocumentBuffer,
    text: &str,
) -> Result<PieceTreeLink, MarkdownDocumentBufferError> {
    let pieces = document
        .edits
        .append(text, document.maximum_document_bytes)?;
    Ok(build_balanced_piece_tree(pieces))
}

/// Path-copy one validated split/rejoin against an unpublished root.
pub(super) fn replace_tree_range(
    document: &MarkdownDocumentBuffer,
    root: PieceTreeLink,
    range: Range<usize>,
    inserted: PieceTreeLink,
) -> Result<PieceTreeLink, MarkdownDocumentBufferError> {
    // Share the unaffected prefix/suffix and discard only the replaced subtree from this candidate.
    let (prefix, remainder) = split_piece_tree(document, root, range.start)?;
    let (_, suffix) = split_piece_tree(document, remainder, range.len())?;
    Ok(join_piece_trees(prefix, join_piece_trees(inserted, suffix)))
}

/// Construct a fixed insertion and explicitly select its editable inner placeholder.
fn template_plan(at: usize, text: &'static str, from: usize, to: usize) -> FormatPlan {
    // Static offsets address the template's editable placeholder, independent of the selected body.
    FormatPlan::Replace {
        range: at..at,
        text,
        selection_after: MarkdownTextSelection {
            anchor_utf16: at + from,
            head_utf16: at + to,
        },
    }
}

/// Match the original JavaScript whitespace set while preserving actual CR/LF line separators.
fn is_heading_whitespace(unit: u16) -> bool {
    // These ECMAScript whitespace code points are all single-unit scalars; CR/LF delimit the line.
    matches!(
        unit,
        0x0009 | 0x000b | 0x000c | 0x0020 | 0x00a0 | 0x1680 | 0x2000
            ..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff
    )
}

/// Map a heading replacement using the original editor's lower/upper endpoint association.
fn map_selection_replacement(
    selection: MarkdownTextSelection,
    range: &Range<usize>,
    inserted: usize,
) -> MarkdownTextSelection {
    let map = |position, after| {
        if position < range.start {
            // Prefix positions precede the heading command and retain their exact coordinate.
            position
        } else if position > range.end || (!range.is_empty() && position == range.end) {
            // Positions after the replacement translate by its complete net length change.
            position - range.len() + inserted
        } else if range.is_empty() {
            // At a pure insertion the lower endpoint follows it; the upper/caret stays before it.
            if after {
                // A nonempty range's lower endpoint excludes the newly inserted heading marker.
                range.start + inserted
            } else {
                // A caret or upper endpoint at the insertion remains on the preceding side.
                range.start
            }
        } else if position == range.start || !after {
            // Start positions and backward-associated deleted positions map to the prefix boundary.
            range.start
        } else {
            // A forward-associated endpoint inside the removed prefix follows the replacement.
            range.start + inserted
        }
    };
    let low = selection.anchor_utf16.min(selection.head_utf16);
    let high = selection.anchor_utf16.max(selection.head_utf16);
    orient_selection(selection, map(low, low != high), map(high, false))
}

/// Preserve backward orientation for commands whose selection is automatically mapped.
fn orient_selection(
    before: MarkdownTextSelection,
    low: usize,
    high: usize,
) -> MarkdownTextSelection {
    if before.anchor_utf16 <= before.head_utf16 {
        // Forward ranges and carets retain their original orientation.
        MarkdownTextSelection {
            anchor_utf16: low,
            head_utf16: high,
        }
    } else {
        // Backward primary selections still use their original head to choose the heading line.
        MarkdownTextSelection {
            anchor_utf16: high,
            head_utf16: low,
        }
    }
}

#[cfg(test)]
mod tests;
