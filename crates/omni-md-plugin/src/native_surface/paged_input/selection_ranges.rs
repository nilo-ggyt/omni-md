//! 保留查找全部选中的真实有限范围，明确区分有序主范围和未知主方向。

use super::*;
use omni_markdown_core::{MarkdownReplacement, MarkdownReplacementPart};

/// 与既有Find全部选择的范围预算一致；限制metadata，不限制原文或匹配总数。
pub const NATIVE_MARKDOWN_PAGED_SELECTION_RANGE_LIMIT: usize = 1000;

impl NativeMarkdownPagedInput {
    /// Cut及已有非空选择的删除消费全部真实范围，在一个Core事务内发布。
    ///
    /// 单选沿原insert_text路径处理，包括marked，不能借多选入口新增组合拒绝。空caret没有
    /// selected bytes，Cut/delete:只跳过它；Backspace/DeleteForward的GCB计划由命令层另行提供。
    ///
    /// # Errors
    /// 原票失效或Unicode/资源错误时保留原状态；提交后的失权由
    /// 返回Commit独立报告。此入口不承诺恢复AppKit本身也未恢复的多选Undo图。
    pub fn delete_selected_ranges(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        let mut ranges = self.selected_ranges(permit, current)?;
        if permit.before.selection_is_directed {
            if permit.before.primary_range.length == 0 {
                let _mutation = self.begin(permit, current)?;
                self.check_permit(permit, current)?;
                return Ok(NativeMarkdownPagedInputOutcome::Unchanged(permit.before));
            }
            // 原单选包括marked的完整明确范围，不经过会拒绝CompositionActive的批量Find入口。
            return self
                .insert_text(permit, "", permit.before.primary_range, current)
                .map(NativeMarkdownPagedInputOutcome::Committed);
        }
        ranges.retain(|range| !range.is_empty());
        if ranges.is_empty() {
            let _mutation = self.begin(permit, current)?;
            self.check_permit(permit, current)?;
            return Ok(NativeMarkdownPagedInputOutcome::Unchanged(permit.before));
        }
        let removed = ranges.iter().try_fold(0_usize, |sum, range| {
            sum.checked_add(range.len())
                .ok_or(NativeMarkdownPagedInputError::InvalidRange)
        })?;
        let last = ranges
            .last()
            .ok_or(NativeMarkdownPagedInputError::StateChanged)?;
        let caret = last
            .end
            .checked_sub(removed)
            .ok_or(NativeMarkdownPagedInputError::InvalidRange)?;
        let replacements = ranges.into_iter().map(|range_utf16| {
            Ok(MarkdownReplacement {
                range_utf16,
                parts: std::iter::empty::<
                    std::result::Result<
                        MarkdownReplacementPart<'static>,
                        MarkdownDocumentBufferError,
                    >,
                >(),
            })
        });
        // 真实NSTextView多选deleteBackward控制在最后一个删除范围的新末端落caret；不取第一项，
        // 也不把中间未选正文收入删除包络。Core原before仍保留permit中的完整canonical memento。
        self.apply_replacements_observed(
            permit,
            replacements,
            MarkdownTextSelection::caret(caret),
            current,
        )
    }

    /// 在同一原票下取得完整有限范围快照，不借正文、不返回跨Native调用的RefCell租约。
    ///
    /// # Errors
    /// 原票失效或状态正在修改时拒绝；不会从后继代次重捕多选数据。
    pub fn selected_ranges(
        &self,
        permit: &NativeMarkdownPagedInputPermit,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<Vec<Range<usize>>> {
        self.check_permit(permit, current)?;
        let ranges = {
            let ranges = self
                .selection_ranges
                .try_borrow()
                .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
            if ranges.is_empty() {
                // 有方向的单选也经同一范围API读取；仅复制两个整数，不读取被选正文。
                let range = permit.before.primary_range;
                std::iter::once(range.location..range.location + range.length).collect()
            } else {
                ranges.clone()
            }
        };
        self.check_permit(permit, current)?;
        Ok(ranges)
    }

    /// 接收Find提供的有序、互不重叠范围；多选的真实主范围遵循NSTextView首项规则，方向未知。
    ///
    /// 单目标是本次显式选择，可以发布anchor/head；多目标保留Core原历史memento，不能用第一
    /// 目标、整篇范围或包络伪装完整选择。完整集合最多1000项，不创建正文副本或历史事务。
    ///
    /// # Errors
    /// marked未结束、范围越界/切入scalar、次序错误、预算不足或原票失效时原状态保持不变。
    pub fn set_selected_ranges(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        ranges: &[Range<usize>],
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        if let [range] = ranges {
            if range.start > range.end {
                // 单项也必须是范围而非反向anchor/head；失败消费原票，不能换有效载荷重试。
                let _mutation = self.begin(permit, current)?;
                self.check_permit(permit, current)?;
                return Err(NativeMarkdownPagedInputError::InvalidRange);
            }
            // 唯一显式目标可认证方向；仍由原单选入口消费同一permit和核对scalar边界。
            return self.set_selection_intent(
                permit,
                MarkdownTextSelection {
                    anchor_utf16: range.start,
                    head_utf16: range.end,
                },
                None,
                None,
                current,
            );
        }
        let _mutation = self.begin(permit, current)?;
        self.check_permit(permit, current)?;
        if self.is_cell() {
            return Err(NativeMarkdownPagedInputError::BodyTargetRequired);
        } // 单目标已在上方走局部set_selection；多目标不借Body历史memento伪造局部方向。
        if permit.before.marked.is_some() {
            return Err(NativeMarkdownPagedInputError::CompositionActive);
        }
        if ranges.is_empty() {
            return Ok(NativeMarkdownPagedInputOutcome::Unchanged(permit.before));
        }
        if ranges.len() > NATIVE_MARKDOWN_PAGED_SELECTION_RANGE_LIMIT {
            return Err(NativeMarkdownPagedInputError::InvalidSelectionRanges);
        }
        let original = self
            .document_identity()
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let document = original
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        if !self.matches_document(&document, permit.before) || document.has_prepared_text_input() {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        for (index, range) in ranges.iter().enumerate() {
            if range.start > range.end || range.end > permit.before.utf16_len {
                return Err(NativeMarkdownPagedInputError::InvalidRange);
            }
            if index > 0 {
                let previous = &ranges[index - 1];
                if range.start < previous.end
                    || (range.is_empty() && previous.is_empty() && range.start == previous.start)
                {
                    // 允许相邻的不同匹配，拒绝重叠、倒序及同位置重复caret，不能排序掩盖坏输入。
                    return Err(NativeMarkdownPagedInputError::InvalidSelectionRanges);
                }
            }
            document.byte_offset_for_utf16(range.start)?;
            document.byte_offset_for_utf16(range.end)?;
        }
        let mut stored = self
            .selection_ranges
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let previous = std::mem::replace(&mut *stored, ranges.to_vec());
        drop(previous); // 只有有限范围metadata替换，不能保留旧集合容量或任何正文载荷。
        let serial = self.selection.publish(
            permit.before.generation,
            permit.before.selection,
            true,
            None,
            None,
        );
        let primary = &ranges[0];
        let state = NativeMarkdownPagedInputState {
            selection_is_directed: false,
            selection_range_count: ranges.len(),
            primary_range: NSRange::new(primary.start, primary.len()),
            selection_serial: serial,
            ..permit.before
        };
        drop(stored);
        drop(document);
        drop(original);
        Ok(NativeMarkdownPagedInputOutcome::Committed(
            NativeMarkdownPagedInputCommit {
                owner: Rc::downgrade(self),
                epoch: permit.epoch,
                state,
                invalidation: NativeMarkdownPagedInputInvalidation {
                    replaced_utf16_range: None,
                    change_in_utf16_length: 0,
                    characters_changed: false,
                    selection_changed: true,
                    marked_changed: false,
                    folds_changed: false,
                },
            },
        ))
    }
}
