//! Body组合只合并同一当前marked中的实际替换；历史保首个root，通知仍描述本次变化。

use super::*;

/// 由同一Input的原marked状态翻译；新client的Start不继承旧client留在Doc里的历史标记。
pub enum MarkdownTextInputComposition {
    Start,
    Update { marked_before: Range<usize> },
    Commit { marked_before: Range<usize> },
}

pub(in crate::markdown_document_buffer) struct CompositionGroup {
    generation: u64,
    marked: Range<usize>,
    // 同次实际append决定文件独占或Heap前缀边界；子范围留下的旧两侧仍归canonical。
    allocation: Option<piece_tree::CompositionAllocation>,
}

struct MergedHistory {
    replacement: MarkdownReplacementMetrics,
    changes: change_journal::Entry,
}

/// 净日志始终inline；Heap重排候选只持至多两个64KiB段，取消不触碰旧正文或历史。
pub(super) struct CompositionPlan {
    merged: Option<MergedHistory>,
    marked: Option<Range<usize>>,
    allocation: Option<piece_tree::CompositionAllocation>,
    retire_previous: bool,
    heap_before: Option<piece_tree::HeapAllocation>,
    heap_cow: Option<piece_tree::CompositionHeapCow>,
    rebased_root: Option<PieceTreeLink>,
}

impl MarkdownDocumentBuffer {
    /// 同步组合开始、更新及确认复用原唯一候选；空候选也结束组合。
    ///
    /// # Errors
    /// 范围、Unicode、容量、观察者、原Source和候选清理失败均保留原root、组合组和两侧历史。
    pub fn prepare_composition_text_input_observed<O: MarkdownTextChangeObserver>(
        &mut self,
        range: Range<usize>,
        replacement: &str,
        selection_before: MarkdownTextSelection,
        selection_after: MarkdownTextSelection,
        composition: MarkdownTextInputComposition,
        observer: O,
    ) -> Result<(MarkdownPreparedTextInput, O::Output), MarkdownObservedEditError<O::Error>> {
        self.prepare_text_input_replacement_composition_observed(
            range,
            TextInputReplacement::Text(replacement),
            selection_before,
            selection_after,
            Some(composition),
            observer,
        )
    }

    /// unmark确认只结束当前组合历史归属；正文、generation、选择、Undo/Redo和日志均不改变。
    ///
    /// # Errors
    /// 原Source失败或尚有候选时保留组；不能绕过唯一候选来提前结束后继组合。
    pub fn finish_text_input_composition(&mut self) -> Result<(), MarkdownDocumentBufferError> {
        self.ensure_text_input_idle()?;
        self.text_input_composition = None;
        Ok(())
    }

    /// 原范围、标量端点和结果长度已由唯一prepare检验；这里只准备当前组的净历史。
    pub(super) fn plan_composition<E>(
        &self,
        range: &Range<usize>,
        inserted: usize,
        composition: MarkdownTextInputComposition,
    ) -> Result<CompositionPlan, MarkdownObservedEditError<E>> {
        let (marked_before, marked) = match composition {
            MarkdownTextInputComposition::Start => (None, true), // 原Input无marked，永远建立独立组。
            MarkdownTextInputComposition::Update { marked_before } => (Some(marked_before), true),
            MarkdownTextInputComposition::Commit { marked_before } => (Some(marked_before), false),
        };
        let end = range.start + inserted;
        let next_marked = (marked && inserted != 0).then_some(range.start..end);
        let previous = self.text_input_composition.as_ref().filter(|group| {
            group.generation == self.generation
                && marked_before.as_ref() == Some(&group.marked)
                && self.redo.is_empty()
                && range.start >= group.marked.start
                && range.end <= group.marked.end
        });
        let retire_previous = previous.is_some_and(|group| *range == group.marked);
        let heap_before = previous
            .filter(|_| retire_previous)
            .and_then(|group| group.allocation.as_ref())
            .and_then(piece_tree::CompositionAllocation::heap);
        let merged = if previous.is_some() {
            let previous = self.undo.back().expect("活动组合已经发布首个历史项");
            let net = previous
                .replacement
                .inserted_utf16_length
                .checked_sub(range.len())
                .and_then(|length| length.checked_add(inserted))
                .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
            let replacement = MarkdownReplacementMetrics::new(
                previous.replacement.location,
                previous.replacement.removed_utf16_length,
                net,
            )?;
            // 显式子范围只更新当前marked的一部分，净日志仍覆盖最初整组替换及保留下来的候选片。
            // baseline从原Undo root聚合读取；不能把本次局部日志冒充首个root到最终root的变化。
            let changes = change_journal::Entry::single_replacement(
                piece_tree_metrics(&previous.root).utf16_len,
                change_journal::Record {
                    start: replacement.location,
                    removed: replacement.removed_utf16_length,
                    inserted: replacement.inserted_utf16_length,
                },
            )?;
            Some(MergedHistory {
                replacement,
                changes,
            })
        } else {
            None // 明确跨出marked或原组已断开：本次独立成项，不吞掉外部正文的历史。
        };
        Ok(CompositionPlan {
            merged,
            marked: next_marked,
            allocation: None, // 真实append成功后才能登记，不根据待输入长度猜段编号。
            retire_previous,
            heap_before,
            heap_cow: None,
            rebased_root: None,
        })
    }
}

impl CompositionPlan {
    /// 原append先提供真实候选读取；完整旧Heap替换另准备等价COW，失败或取消绝不写回旧段。
    pub(super) fn append(
        &mut self,
        edits: &mut MarkdownEditSegments,
        text: &str,
        maximum_session_edit_bytes: usize,
    ) -> Result<Vec<Arc<MarkdownPieceTreeNode>>, MarkdownDocumentBufferError> {
        let before_segments = edits.segments.len();
        let (pieces, allocation) = edits.append_composition(text, maximum_session_edit_bytes)?;
        self.allocation = allocation;
        if let Some(previous) = self.heap_before {
            let cow = edits.prepare_composition_heap_cow(previous, text, before_segments);
            if text.len() <= EDIT_SEGMENT_BYTES {
                self.allocation = cow.allocation(); // 新Heap叶的物理offset将随同一候选切到此真实COW。
            }
            self.heap_cow = Some(cow);
        }
        Ok(pieces)
    }

    /// 只重建本次插入和原split取得的前后缀连接路径；不扫描全树、历史或旧候选正文。
    pub(super) fn prepare_rebased_root(&mut self, prefix: &PieceTreeLink, suffix: &PieceTreeLink) {
        if let Some(cow) = &mut self.heap_cow
            && let Some(inserted) = cow.take_inserted()
        {
            self.rebased_root = Some(join_piece_trees(
                prefix.clone(),
                join_piece_trees(inserted, suffix.clone()),
            ));
        }
    }

    /// 所有真实端点与observer已在原append候选验证完毕；下一次读取只能在对应字节安装后发生。
    pub(super) fn finish_prepared(&mut self, prepared: &mut SealedCommand) {
        if let Some(root) = self.rebased_root.take() {
            prepared.command.root = root;
        }
    }
}

/// 所有净日志和候选都已准备；发布不调用外部代码，随后旧payload真实Drop的首错由原票保留。
pub(super) fn publish(
    document: &mut MarkdownDocumentBuffer,
    prepared: SealedCommand,
    mut plan: CompositionPlan,
) -> MarkdownHistoryTextEdit {
    let previous_allocation = document
        .text_input_composition
        .take()
        .and_then(|group| plan.retire_previous.then_some(group.allocation).flatten());
    if let Some(cow) = plan.heap_cow.take() {
        // 此处无Native、current、observer或可失败I/O；canonical与实际字节在同一发布栈内切换。
        cow.publish(&mut document.edits);
    }
    let edit = if let Some(merged) = plan.merged {
        let SealedCommand {
            command:
                PreparedCommand {
                    root,
                    after,
                    metrics,
                },
            changes,
            ..
        } = prepared;
        let previous = document.undo.back_mut().expect("原nonce仍独占活动组尾项");
        previous.replacement = merged.replacement;
        previous.counterpart_selection = Some(after);
        let old_changes = std::mem::replace(&mut previous.changes, merged.changes);
        document.change_journal.release(old_changes);
        document.change_journal.release(changes); // 本次局部日志已由真实净日志代替，不拖住中间页。
        document.edits.publish();
        document.root = root; // 首个历史root/selection仍在原尾项，中间root没有新历史owner。
        document.selection = after;
        document.generation = document.generation.wrapping_add(1);
        #[cfg(any(test, feature = "native-markdown-candidate"))]
        document
            .block_checkpoints
            .get_mut()
            .invalidate_after(metrics.location);
        metrics.redo_text_edit(after) // Native通知必须描述这次真实替换，不能使用组合的累计包络。
    } else {
        let edit = command_transaction::publish_command(document, prepared);
        document
            .undo
            .back_mut()
            .expect("首个真实组合字符事务已经入栈")
            .counterpart_selection = Some(document.selection);
        edit
    };
    document.text_input_composition = plan.marked.map(|marked| CompositionGroup {
        generation: document.generation,
        marked,
        allocation: plan.allocation,
    });
    if let Some(allocation) = previous_allocation {
        // canonical、净历史和新Input后态已经确定；此处才结束旧payload强边，不能回报未提交。
        // 最后Snapshot可合法延迟真实释放，原编号的observer仍保存其归还结果与首错。
        allocation.retire(&mut document.edits);
    }
    edit
}

/// 真正零字符变化仍可结束marked或移动组的最终选区；不创建空Undo，不改旧普通历史。
pub(super) fn publish_selection(
    document: &mut MarkdownDocumentBuffer,
    plan: Option<CompositionPlan>,
    after: MarkdownTextSelection,
) {
    if plan.is_some_and(|plan| plan.merged.is_some()) {
        document
            .undo
            .back_mut()
            .expect("同一活动组保留首个历史项")
            .counterpart_selection = Some(after);
    }
    // SelectionOnly只来自空range+空输入，绝无可新建的marked；普通选择和空候选都结束旧组。
    document.text_input_composition = None;
}

#[cfg(test)]
mod tests;
