//! 局部输入只修改独立descriptor分支；全部可失败校验完成后才替换草稿root与serial。

use super::*;

/// 某次局部编辑的固定nonce与serial；只能由原会话捕获，不能Clone或从裸坐标构造。
pub struct MarkdownCellDraftEditCapsule {
    scope: Weak<Scope>,
    serial: u64,
}

pub(super) struct DraftSource<'a> {
    pub(super) document: &'a MarkdownDocumentBuffer,
    pub(super) edits: &'a MarkdownEditSegments,
    pub(super) scope: &'a Scope,
    pub(super) serial: u64,
}

impl PieceByteSource for DraftSource<'_> {
    fn source_health(&self) -> Result<(), MarkdownDocumentBufferError> {
        self.document
            .original_backing_health()
            .inspect_err(|&error| self.scope.record_source(error))?;
        self.edits
            .health()
            .inspect_err(|&error| self.scope.record_source(error))?;
        self.scope
            .require(self.serial)
            .map_err(|error| match error {
                MarkdownCellDraftError::Document(error) => error,
                // 树内部只返回Core错误；外层原token门仍以独立Stale区分草稿权限。
                _ => MarkdownDocumentBufferError::PreparedTextInputStale,
            })
    }

    fn piece_bytes(
        &self,
        piece: &MarkdownPiece,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.source_health()?;
        match piece.source {
            MarkdownPieceSource::Original => self.document.piece_bytes(piece),
            MarkdownPieceSource::EditSegment(index) => {
                let end = piece
                    .byte_start
                    .checked_add(piece.byte_len)
                    .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
                self.edits.read(index, piece.byte_start..end)
            }
        }
    }

    /// 原草稿仍用相同scope与段表，仅将本次原叶内字节范围交给真实存储。
    #[cfg(feature = "paged-shaping-frontier")]
    fn piece_range_bytes(
        &self,
        piece: &MarkdownPiece,
        local: Range<usize>,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.source_health()?;
        if local.start > local.end || local.end > piece.byte_len {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        match piece.source {
            MarkdownPieceSource::Original => self.document.piece_range_bytes(piece, local),
            MarkdownPieceSource::EditSegment(index) => self.edits.read(
                index,
                piece.byte_start + local.start..piece.byte_start + local.end,
            ),
        }
    }
}

pub(super) fn validate_positions(
    source: &DraftSource<'_>,
    root: &PieceTreeLink,
    selection: MarkdownTextSelection,
    marked: &Option<Range<usize>>,
) -> Result<(), MarkdownDocumentBufferError> {
    for at in [selection.anchor_utf16, selection.head_utf16] {
        // 选区不能落在surrogate内部；超尾也沿同一真实树查询拒绝。
        byte_offset_for_utf16_in_tree(source, root.as_deref(), at, 0)?;
    }
    if let Some(range) = marked {
        if range.start > range.end {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        for at in [range.start, range.end] {
            byte_offset_for_utf16_in_tree(source, root.as_deref(), at, 0)?;
        }
    }
    Ok(())
}

impl PendingCellDraft {
    fn replace(
        &mut self,
        document: &MarkdownDocumentBuffer,
        range: Range<usize>,
        text: &str,
        selection: MarkdownTextSelection,
        marked: Option<Range<usize>>,
        kind: Option<MarkdownCellDraftInputKind>,
    ) -> Result<MarkdownCellDraftFacts, MarkdownCellDraftError> {
        self.require(document)?;
        if range.start > range.end {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument.into());
        }
        let source = DraftSource {
            document,
            edits: &self.edits,
            scope: &self.scope,
            serial: self.serial,
        };
        let start = byte_offset_for_utf16_in_tree(&source, self.root.as_deref(), range.start, 0)?;
        let end = byte_offset_for_utf16_in_tree(&source, self.root.as_deref(), range.end, 0)?;
        if range.is_empty() && text.is_empty() {
            if let Some(kind) = kind {
                history::grouping::validate_input_kind(kind, range.start, 0, &marked)?;
            }
            // 没有字符变化只走输入状态门；不能因选区/unmark造历史root或撤销相同正文来源。
            return self.set_state(document, selection, marked);
        }
        let next_serial = self
            .serial
            .checked_add(1)
            .ok_or(MarkdownCellDraftError::Stale)?;
        let next_content_serial = self
            .content_serial
            .checked_add(1)
            .ok_or(MarkdownCellDraftError::Stale)?;
        let replacement =
            MarkdownReplacementMetrics::new(range.start, range.len(), text.encode_utf16().count())?;
        let group = self.history.plan_group(history::grouping::GroupChange {
            kind,
            serial: self.serial,
            next_serial,
            selection_before: self.selection,
            marked_before: &self.marked_range,
            selection_after: selection,
            marked_after: &marked,
            replacement,
        })?;
        self.history.reserve_group(&group)?;
        // 先切原候选再追加，失败不产生垃圾delta；原Source错误先于提交新输入。
        let (prefix, tail) = split_piece_tree(&source, self.root.clone(), start)?;
        let (_, suffix) = split_piece_tree(&source, tail, end - start)?;
        let checkpoint = command_transaction::AppendCheckpoint::capture_segments(&self.edits);
        let result = (|| {
            let added = if text.len() > EDIT_SEGMENT_BYTES {
                let backing = self
                    .edits
                    .freeze_input(text, MARKDOWN_CELL_DRAFT_MAX_DELTA_BYTES)?;
                // 只登记本次新建的raw文件；先取得实际票，覆盖后续叶读取失败及事务回退。
                // 激活时继承的canonical段不经过这里，仍由Doc承担最终释放义务。
                self.scope.observe_raw_backing(backing.observer());
                self.edits
                    .append_frozen(backing, MARKDOWN_CELL_DRAFT_MAX_DELTA_BYTES)?
            } else {
                // 原有限堆尾没有映射义务，不分配观察清单或制造虚构文件票。
                self.edits
                    .append(text, MARKDOWN_CELL_DRAFT_MAX_DELTA_BYTES)?
            };
            // 每段最多损失三个UTF-8尾字节；descriptor也有独立预算，不能靠大量标量留下无限小尾段。
            let maximum_descriptors = self
                .base_descriptors
                .saturating_add(
                    MARKDOWN_CELL_DRAFT_MAX_DELTA_BYTES.div_ceil(EDIT_SEGMENT_BYTES - 3),
                )
                .saturating_add(1);
            if self.edits.segments.len() > maximum_descriptors {
                return Err(MarkdownDocumentBufferError::EditBufferLimitExceeded);
            }
            let root = join_piece_trees(
                prefix,
                join_piece_trees(build_balanced_piece_tree(added), suffix),
            );
            let source = DraftSource {
                document,
                edits: &self.edits,
                scope: &self.scope,
                serial: self.serial,
            };
            validate_positions(&source, &root, selection, &marked)?;
            source.source_health()?;
            self.edits.pending_health()?; // 新文件只在此准备成功后移交raw Source，失败保原分支可编辑。
            Ok(root)
        })();
        let root = match result {
            Ok(root) => root,
            Err(error) => {
                // closure的失败候选root先实际Drop；随后只回退本分支，canonical与旧读取段均不写穿。
                checkpoint.rollback_segments(&mut self.edits);
                self.scope.record_source(error);
                return Err(error.into());
            }
        };
        self.history
            .record_group(group, self.root.take(), self.selection);
        self.edits.publish(); // 所有验证已成功；此刻起新匿名段才属于原raw Source。
        self.root = root;
        self.selection = selection;
        self.marked_range = marked;
        self.serial = next_serial;
        self.content_serial = next_content_serial;
        self.scope.publish_delta(self.edits.used_bytes);
        self.scope
            .content_serial
            .store(next_content_serial, Ordering::Release);
        self.scope.serial.store(next_serial, Ordering::Release);
        Ok(self.facts())
    }
}

impl MarkdownDocumentBuffer {
    pub(super) fn require_cell_draft_edit(
        &self,
        edit: &MarkdownCellDraftEditCapsule,
    ) -> Result<&PendingCellDraft, MarkdownCellDraftError> {
        let pending = self.active_cell_draft_scope(&edit.scope)?;
        if pending.serial != edit.serial {
            // 会话仍活也不能把旧UI范围补签为当前serial；失败后调用方须重新捕获一次权限。
            return Err(MarkdownCellDraftError::Stale);
        }
        Ok(pending)
    }

    /// 在调用Native/读取UI之前捕获本次输入权限；后来任一成功输入都会撤销这张旧票。
    pub fn capture_cell_draft_edit(
        &self,
        token: &MarkdownCellDraft,
    ) -> Result<MarkdownCellDraftEditCapsule, MarkdownCellDraftError> {
        let pending = self.active_cell_draft(token)?;
        Ok(MarkdownCellDraftEditCapsule {
            scope: Arc::downgrade(&pending.scope),
            serial: pending.serial,
        })
    }

    /// 按cell局部坐标替换原始输入；大草稿准入不提前使用64Ki提交限制。
    /// 所有失败保留原草稿、serial、局部选区/marked与canonical的一步Undo基线。
    pub fn replace_cell_draft(
        &mut self,
        edit: MarkdownCellDraftEditCapsule,
        range: Range<usize>,
        text: &str,
        selection_after: MarkdownTextSelection,
        marked_after: Option<Range<usize>>,
    ) -> Result<MarkdownCellDraftFacts, MarkdownCellDraftError> {
        self.require_cell_draft_edit(&edit)?;
        let Some(CellDraftSlot::Active(mut pending)) = self.cell_draft.take() else {
            unreachable!("已核唯一活动草稿");
        };
        // 同步纯Rust编辑期间暂借出slot，以便候选source只借Doc；不运行任何外部或Native回调。
        let result = pending.replace(self, range, text, selection_after, marked_after, None);
        self.cell_draft = Some(CellDraftSlot::Active(pending));
        result
    }

    /// 消费同一原输入的两张封闭票；只合并已证明相邻的typing或同一完整marked替换。
    /// 权限、语义载荷或真实追加失败不改变原root、分组、delta、选区与canonical。
    pub fn replace_cell_draft_grouped(
        &mut self,
        edit: MarkdownCellDraftEditCapsule,
        intent: MarkdownCellDraftInputIntent,
        range: Range<usize>,
        text: &str,
        selection_after: MarkdownTextSelection,
        marked_after: Option<Range<usize>>,
    ) -> Result<MarkdownCellDraftFacts, MarkdownCellDraftError> {
        let pending = self.require_cell_draft_edit(&edit)?;
        let kind = intent.require(pending)?;
        let Some(CellDraftSlot::Active(mut pending)) = self.cell_draft.take() else {
            unreachable!("已核原输入意图的唯一草稿");
        };
        let result = pending.replace(self, range, text, selection_after, marked_after, Some(kind));
        self.cell_draft = Some(CellDraftSlot::Active(pending));
        result
    }
}
