//! 格内行内格式只追加原领域标记；私有候选完整后一次发布草稿root与局部历史。

use super::edit::{DraftSource, validate_positions};
use super::*;

impl PendingCellDraft {
    fn insert_format_marker(
        &mut self,
        document: &MarkdownDocumentBuffer,
        root: PieceTreeLink,
        byte: usize,
        marker: &'static str,
    ) -> Result<PieceTreeLink, MarkdownDocumentBufferError> {
        let added = self
            .edits
            .append(marker, MARKDOWN_CELL_DRAFT_MAX_DELTA_BYTES)?;
        let source = DraftSource {
            document,
            edits: &self.edits,
            scope: &self.scope,
            serial: self.serial,
        };
        // 只在原端点路径切开候选；所选正文仍共享原piece，不扫描或复制整个格。
        let (prefix, suffix) = split_piece_tree(&source, root, byte)?;
        Ok(join_piece_trees(
            prefix,
            join_piece_trees(build_balanced_piece_tree(added), suffix),
        ))
    }

    fn format(
        &mut self,
        document: &MarkdownDocumentBuffer,
        action: MarkdownFormatAction,
    ) -> Result<(MarkdownCellDraftFacts, MarkdownHistoryTextEdit), MarkdownCellDraftError> {
        self.require(document)?;
        if self.marked_range.is_some() {
            return Err(MarkdownCellDraftError::CompositionActive);
        } // marked仍归原IME；不能在Core猜确认或换一张输入票继续格式化。
        let (opening, closing, placeholder) = action
            .inline_markers()
            .ok_or(MarkdownCellDraftError::FormatRequiresDocument)?;
        let range = self.selection.anchor_utf16.min(self.selection.head_utf16)
            ..self.selection.anchor_utf16.max(self.selection.head_utf16);
        let placeholder = range.is_empty().then_some(placeholder);
        let opening_units = opening.encode_utf16().count();
        let growth = opening_units
            .checked_add(closing.encode_utf16().count())
            .and_then(|units| {
                units.checked_add(placeholder.map_or(0, |value| value.encode_utf16().count()))
            })
            .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
        let after = MarkdownTextSelection {
            anchor_utf16: range
                .start
                .checked_add(opening_units)
                .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?,
            head_utf16: range
                .end
                .checked_add(opening_units)
                .and_then(|units| {
                    units.checked_add(placeholder.map_or(0, |value| value.encode_utf16().count()))
                })
                .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?,
        }; // 沿原Surround选择正向inner；Undo仍保存调用前的真实有向选择。
        let replacement = MarkdownReplacementMetrics::new(
            range.start,
            range.len(),
            range
                .len()
                .checked_add(growth)
                .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?,
        )?;
        let next_serial = self
            .serial
            .checked_add(1)
            .ok_or(MarkdownCellDraftError::Stale)?;
        let next_content_serial = self
            .content_serial
            .checked_add(1)
            .ok_or(MarkdownCellDraftError::Stale)?;
        let source = DraftSource {
            document,
            edits: &self.edits,
            scope: &self.scope,
            serial: self.serial,
        };
        let start = byte_offset_for_utf16_in_tree(&source, self.root.as_deref(), range.start, 0)?;
        let end = byte_offset_for_utf16_in_tree(&source, self.root.as_deref(), range.end, 0)?;
        let group = self.history.plan_group(history::grouping::GroupChange {
            kind: Some(MarkdownCellDraftInputKind::Isolated),
            serial: self.serial,
            next_serial,
            selection_before: self.selection,
            marked_before: &self.marked_range,
            selection_after: after,
            marked_after: &None,
            replacement,
        })?;
        self.history.reserve_group(&group)?; // 预留原局部历史槽，格式不合并到此前typing/marked组。
        let checkpoint = command_transaction::AppendCheckpoint::capture_segments(&self.edits);
        let result = (|| {
            // 倒序插入保原byte坐标；任一步失败都只丢候选，不发布单侧标记或中间serial。
            let mut root = self.insert_format_marker(document, self.root.clone(), end, closing)?;
            if let Some(placeholder) = placeholder {
                root = self.insert_format_marker(document, root, start, placeholder)?;
            } // 只有空caret插静态占位；非空格内选择保持原Unicode字节和行尾。
            let root = self.insert_format_marker(document, root, start, opening)?;
            let maximum_descriptors = self
                .base_descriptors
                .saturating_add(
                    MARKDOWN_CELL_DRAFT_MAX_DELTA_BYTES.div_ceil(EDIT_SEGMENT_BYTES - 3),
                )
                .saturating_add(1);
            if self.edits.segments.len() > maximum_descriptors {
                return Err(MarkdownDocumentBufferError::EditBufferLimitExceeded);
            } // 同原replace的累计delta与descriptor上限，不借删除或格式动作补充预算。
            let source = DraftSource {
                document,
                edits: &self.edits,
                scope: &self.scope,
                serial: self.serial,
            };
            validate_positions(&source, &root, after, &None)?;
            source.source_health()?;
            Ok(root)
        })();
        let root = match result {
            Ok(root) => root,
            Err(error) => {
                // 失败候选先实际Drop，再退回所有本次marker段；原root/选区/marked/历史和读取票保持原样。
                checkpoint.rollback_segments(&mut self.edits);
                self.scope.record_source(error);
                return Err(error.into());
            }
        };
        self.history
            .record_group(group, self.root.take(), self.selection);
        self.edits.publish(); // 同一局部事务只在不可失败发布点移交新段的Source归属。
        self.root = root;
        self.selection = after;
        self.serial = next_serial;
        self.content_serial = next_content_serial;
        self.scope.publish_delta(self.edits.used_bytes);
        self.scope
            .content_serial
            .store(next_content_serial, Ordering::Release);
        self.scope.serial.store(next_serial, Ordering::Release);
        // 从record_group开始只有不可失败发布；回到Native时不再根据旧票回滚已完成的局部格式。
        Ok((self.facts(), replacement.redo_text_edit(after)))
    }
}

impl MarkdownDocumentBuffer {
    /// 消费原cell编辑胶囊执行一次行内格式；只新增静态标记，不物化所选内容或改变canonical。
    ///
    /// # Errors
    /// 源/原票失效、marked、正文块动作、端点或追加预算失败均保留原草稿和局部历史。
    pub fn format_cell_draft(
        &mut self,
        edit: MarkdownCellDraftEditCapsule,
        action: MarkdownFormatAction,
    ) -> Result<(MarkdownCellDraftFacts, MarkdownHistoryTextEdit), MarkdownCellDraftError> {
        self.require_cell_draft_edit(&edit)?;
        let Some(CellDraftSlot::Active(mut pending)) = self.cell_draft.take() else {
            unreachable!("已核原格式票的唯一草稿");
        };
        // 纯Rust候选只暂借原slot；同步期间没有Native回调、第二Source或canonical事务。
        let result = pending.format(self, action);
        self.cell_draft = Some(CellDraftSlot::Active(pending));
        result
    }
}
