//! 候选root、真实变化日志与派生状态先共同完成，随后一次发布；失败保留原文和两侧历史。

use super::changes::{MarkdownObservedEditError, MarkdownPreparedText, MarkdownTextChangeObserver};
use super::*;

/// 流式命令消费完输入后才知道完整geometry；该载荷仍是尚未发布的候选。
pub(super) struct PreparedCommand {
    pub(super) root: PieceTreeLink,
    pub(super) after: MarkdownTextSelection,
    pub(super) metrics: MarkdownReplacementMetrics,
}

/// 保存追加存储边界；候选失败时只删除未发布尾部，不复制旧正文。
pub(super) struct AppendCheckpoint {
    segment_count: usize,
    segment_capacity: usize,
    tail_bytes: usize,
    used_bytes: usize,
}

impl AppendCheckpoint {
    fn capture(document: &MarkdownDocumentBuffer) -> Self {
        Self::capture_segments(&document.edits)
    }

    /// 草稿分支复用相同的追加回退合同；只记录descriptor与有限尾段边界。
    pub(super) fn capture_segments(edits: &MarkdownEditSegments) -> Self {
        Self {
            segment_count: edits.segments.len(),
            segment_capacity: edits.segments.capacity(),
            tail_bytes: edits.segments.last().map_or(0, |tail| tail.len()),
            used_bytes: edits.used_bytes,
        }
    }

    pub(super) fn rollback(self, document: &mut MarkdownDocumentBuffer) {
        self.rollback_segments(&mut document.edits);
    }

    /// 调用方先丢弃失败候选root，再恢复同一个descriptor分支；旧快照仍保原Arc字节。
    pub(super) fn rollback_segments(self, edits: &mut MarkdownEditSegments) {
        // 候选tree已结束，才能删除它引用的新segment和旧tail的未发布字节。
        edits.segments.truncate(self.segment_count);
        if let Some(tail) = edits.segments.last_mut()
            && tail.len() != self.tail_bytes
        {
            // 只有实际追加才截尾；若准备期间曾冻结同一段，COW保留其原字节而不写穿。
            // 未追加的失败不触发复制，原固定段容量与所有已发布叶的编号继续有效。
            MarkdownEditSegments::writable_segment(tail).truncate(self.tail_bytes);
        }
        if edits.segments.capacity() != self.segment_capacity {
            // truncate不释放外层容量；只移动旧descriptor恢复失败前预算。
            let mut restored = Vec::with_capacity(self.segment_capacity);
            restored.append(&mut edits.segments);
            edits.segments = restored;
        }
        edits.used_bytes = self.used_bytes;
    }
}

/// 已知结果字节的命令先做预算admission，再进入相同可失败准备路径。
pub(super) fn commit_command_observed<O: MarkdownTextChangeObserver>(
    document: &mut MarkdownDocumentBuffer,
    metrics: MarkdownReplacementMetrics,
    result_bytes: usize,
    before: MarkdownTextSelection,
    observer: O,
    prepare: impl FnOnce(
        &mut MarkdownDocumentBuffer,
        &mut O,
    ) -> Result<
        (PieceTreeLink, MarkdownTextSelection),
        MarkdownObservedEditError<O::Error>,
    >,
) -> Result<(MarkdownHistoryTextEdit, O::Output), MarkdownObservedEditError<O::Error>> {
    document.ensure_text_input_idle()?;
    if result_bytes > document.maximum_document_bytes {
        // 预算失败必须先于正文append、变化日志创建或观察者回调。
        return Err(MarkdownDocumentBufferError::DocumentTooLarge.into());
    }
    commit_prepared_command_observed(document, before, observer, |document, observer| {
        let (root, after) = prepare(document, observer)?;
        Ok(PreparedCommand {
            root,
            after,
            metrics,
        })
    })
}

/// 观察者只准备拥有的纯结果；任何失败均发生在root/history/selection发布之前。
pub(super) fn commit_prepared_command_observed<O: MarkdownTextChangeObserver>(
    document: &mut MarkdownDocumentBuffer,
    before: MarkdownTextSelection,
    observer: O,
    prepare: impl FnOnce(
        &mut MarkdownDocumentBuffer,
        &mut O,
    ) -> Result<PreparedCommand, MarkdownObservedEditError<O::Error>>,
) -> Result<(MarkdownHistoryTextEdit, O::Output), MarkdownObservedEditError<O::Error>> {
    let (prepared, output) = prepare_command_observed(document, before, observer, prepare)?;
    Ok((publish_command(document, prepared), output))
}

/// 已封日志的候选由文档独占；取消先丢root再回退append，发布只移动现有载荷。
pub(super) struct SealedCommand {
    pub(super) command: PreparedCommand,
    pub(super) before: MarkdownTextSelection,
    pub(super) checkpoint: AppendCheckpoint,
    pub(super) changes: change_journal::Entry,
}

/// 完整执行正文、观察者与日志的可失败阶段，canonical仍为旧root和旧history。
pub(super) fn prepare_command_observed<O: MarkdownTextChangeObserver>(
    document: &mut MarkdownDocumentBuffer,
    before: MarkdownTextSelection,
    mut observer: O,
    prepare: impl FnOnce(
        &mut MarkdownDocumentBuffer,
        &mut O,
    ) -> Result<PreparedCommand, MarkdownObservedEditError<O::Error>>,
) -> Result<(SealedCommand, O::Output), MarkdownObservedEditError<O::Error>> {
    document.ensure_text_input_idle()?;
    let checkpoint = AppendCheckpoint::capture(document);
    document.change_journal.begin(document.utf16_len())?;
    let prepared = (|| {
        let prepared = prepare(document, &mut observer)?;
        if piece_tree_metrics(&prepared.root).byte_len > document.maximum_document_bytes {
            // 流式caller到此才知道结果；共享pieces也不豁免live文档上限。
            return Err(MarkdownDocumentBufferError::DocumentTooLarge.into());
        }
        // 在真实候选树上校验两端，不能交换document.root或复制正文来验证。
        byte_offset_for_utf16_in_tree(
            document,
            prepared.root.as_deref(),
            prepared.after.anchor_utf16,
            0,
        )?;
        byte_offset_for_utf16_in_tree(
            document,
            prepared.root.as_deref(),
            prepared.after.head_utf16,
            0,
        )?;
        let output = observer
            .finish(
                MarkdownPreparedText {
                    document,
                    root: &prepared.root,
                },
                prepared.after,
            )
            .map_err(MarkdownObservedEditError::Observer)?;
        document.original_backing_health()?; // 原Source首错仍先于本次候选文件失败。
        document.edits.pending_health()?; // observer之后真正核未发布映射，失败仍由checkpoint完整回退。
        // 派生结果也已通过后才封日志；短写、预算或校验失败仍不能发布候选root。
        let changes = document
            .change_journal
            .finish(piece_tree_metrics(&prepared.root).utf16_len)?;
        Ok((prepared, output, changes))
    })();
    let (
        PreparedCommand {
            root,
            after,
            metrics,
        },
        output,
        changes,
    ) = match prepared {
        Ok(prepared) => prepared,
        Err(cause) => {
            // 闭包内候选tree及派生结果已drop；先恢复append，再回收本事务日志页。
            checkpoint.rollback(document);
            return match document.change_journal.abort() {
                Ok(()) => Err(cause),
                Err(cleanup) => Err(MarkdownObservedEditError::RollbackUnconfirmed {
                    cause: Box::new(cause),
                    cleanup,
                }),
            };
        }
    };
    Ok((
        SealedCommand {
            command: PreparedCommand {
                root,
                after,
                metrics,
            },
            before,
            checkpoint,
            changes,
        },
        output,
    ))
}

/// 调用方已验证独占权限；此阶段没有观察者、追加、日志写入或可失败校验。
pub(super) fn publish_command(
    document: &mut MarkdownDocumentBuffer,
    prepared: SealedCommand,
) -> MarkdownHistoryTextEdit {
    let SealedCommand {
        command:
            PreparedCommand {
                root,
                after,
                metrics,
            },
        before,
        changes,
        ..
    } = prepared;
    // 从此没有可失败步骤：旧root与唯一日志票据同归一条history，再撤销redo分支。
    let previous = MarkdownEditorMemento {
        root: document.root.clone(),
        selection: before,
        counterpart_selection: None,
        replacement: metrics,
        changes,
    };
    document.edits.publish(); // Source归属只在此不可失败发布边界推进，不提前污染旧正文健康门。
    document.root = root;
    document.selection = after;
    document.text_input_composition = None; // 普通命令独立成项，不继承更早marked组的合并权限。
    if let Some(evicted) = push_bounded_snapshot(&mut document.undo, previous) {
        document.change_journal.release(evicted.changes);
    }
    for discarded in document.redo.drain(..) {
        document.change_journal.release(discarded.changes);
    }
    document.generation = document.generation.wrapping_add(1);
    #[cfg(any(test, feature = "native-markdown-candidate"))]
    document
        .block_checkpoints
        .get_mut()
        .invalidate_after(metrics.location);
    metrics.redo_text_edit(after)
}
