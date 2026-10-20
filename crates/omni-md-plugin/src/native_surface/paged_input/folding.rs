//! Source 折叠只改变唯一输入 owner 的有限锚点；正文、保存与 Undo 历史始终留在原 Core。

use super::*;
use omni_markdown_core::markdown_folding::{
    anchor_mapping::MAX_MARKDOWN_FOLD_ANCHORS, markdown_fold_region,
};

impl NativeMarkdownPagedInput {
    /// Find 在同一次 canonical 选择提交中展开真实命中隐区；后续请求直接绑定新 fold revision。
    /// 与普通全选不同，这个业务意图必须让整个匹配可见，不能仅把端点夹到 header。
    pub(in crate::native_surface) fn select_find_result(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        selection: MarkdownTextSelection,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        if permit.cell.is_some() {
            // 单元格查找仍使用草稿局部选择；Body 折叠没有权限更改单元格，原事务保持独立。
            return self.set_selection(permit, selection, current);
        }
        let _mutation = self.begin(permit, current)?;
        self.require_fold_action(permit)?;
        let original = self
            .document_identity()
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let document = original
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        if !self.matches_document(&document, permit.before) {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        document.byte_offset_for_utf16(selection.anchor_utf16)?;
        document.byte_offset_for_utf16(selection.head_utf16)?;
        let range = selection.anchor_utf16.min(selection.head_utf16)
            ..selection.anchor_utf16.max(selection.head_utf16);
        let old = self
            .folds
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let mut folds = Vec::with_capacity(old.len());
        for fold in old.iter() {
            let hidden = folded_layout_range(&document, fold)?;
            let contains = if range.is_empty() {
                hidden.start < range.start && range.start < hidden.end
            } else {
                range.start < hidden.end && hidden.start < range.end
            };
            if !contains {
                folds.push(fold.clone());
            }
        }
        drop(old);
        drop(document);
        drop(original);
        self.publish_fold_intent(permit, folds, Some(selection), current)
    }

    /// 原菜单/行号动作消费同一输入票；查询成功后才发布折叠与必要的 header caret。
    /// 已折叠行只展开自身，嵌套的用户选择保留；不可折叠的上下文返回真实无变化。
    pub fn toggle_fold(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        line: usize,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        let _mutation = self.begin(permit, current)?;
        self.require_fold_action(permit)?;
        let mut folds = self
            .folds
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?
            .clone();
        let mut selection = None;
        if let Some(index) = folds.iter().position(|fold| fold.header_line == line) {
            folds.remove(index);
        } else {
            if permit.before.selection_range_count > 1 {
                return Err(NativeMarkdownPagedInputError::MultipleSelectionInput);
            }
            let original = self
                .document_identity()
                .upgrade()
                .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
            let document = original
                .try_borrow()
                .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
            if !self.matches_document(&document, permit.before) {
                return Err(NativeMarkdownPagedInputError::StateChanged);
            }
            let region = markdown_fold_region(&document, line)?;
            drop(document);
            drop(original);
            let Some(region) = region else {
                self.check_permit(permit, current)?;
                return Ok(NativeMarkdownPagedInputOutcome::Unchanged(permit.before));
            };
            if folds.len() == MAX_MARKDOWN_FOLD_ANCHORS {
                return Err(MarkdownFoldMappingError::TooManyAnchors.into());
            }
            let hidden = &region.hidden_utf16_range;
            if permit.before.selection_is_directed
                && hidden.start < permit.before.selection.head_utf16
                && permit.before.selection.head_utf16 < hidden.end
            {
                selection = Some(MarkdownTextSelection::caret(hidden.start));
            } // 只有已知真实 head 落入隐区才移到 header；未知方向的单范围保留原 metadata 和历史 memento。
            folds.push(region);
        }
        self.publish_fold_intent(permit, folds, selection, current)
    }

    /// “展开全部”只撤当前有限锚点，不创建一个文字事务或占用一次 Undo。
    pub fn expand_folds(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        let _mutation = self.begin(permit, current)?;
        self.require_fold_action(permit)?;
        self.publish_fold_intent(permit, Vec::new(), None, current)
    }

    /// Find/辅助功能定位先展开真实目标所穿过的隐区，再让调用方按提交后的 fold revision 签几何票。
    /// 不把匹配夹到 header，也不读取或复制匹配以外的正文。
    pub fn reveal_folded_range(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        range: Range<usize>,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        let _mutation = self.begin(permit, current)?;
        self.require_fold_action(permit)?;
        let original = self
            .document_identity()
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let document = original
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        if !self.matches_document(&document, permit.before) {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        if range.start > range.end || range.end > document.utf16_len() {
            return Err(NativeMarkdownPagedInputError::InvalidRange);
        }
        document.byte_offset_for_utf16(range.start)?;
        document.byte_offset_for_utf16(range.end)?;
        let original_folds = self
            .folds
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let mut retained = Vec::with_capacity(original_folds.len());
        for fold in original_folds.iter() {
            let hidden = folded_layout_range(&document, fold)?;
            let intersects = if range.is_empty() {
                hidden.start < range.start && range.start < hidden.end
            } else {
                range.start < hidden.end && hidden.start < range.end
            };
            if !intersects {
                retained.push(fold.clone());
            }
        }
        drop(original_folds);
        drop(document);
        drop(original);
        self.publish_fold_intent(permit, retained, None, current)
    }

    /// 构造原 Source composer 时只借同票的有限端点投影；不暴露可修改的第二 fold owner。
    pub fn folded_layout_ranges(
        &self,
        permit: &NativeMarkdownPagedInputPermit,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<Vec<Range<usize>>> {
        self.check_permit(permit, current)?;
        if permit.cell.is_some() {
            return Err(NativeMarkdownPagedInputError::BodyTargetRequired);
        }
        let original = self
            .document_identity()
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let document = original
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        if !self.matches_document(&document, permit.before) {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        let folds = self
            .folds
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let mut ranges = Vec::with_capacity(folds.len());
        for fold in folds.iter() {
            ranges.push(folded_layout_range(&document, fold)?);
        }
        drop(folds);
        drop(document);
        drop(original);
        self.check_permit(permit, current)?;
        Ok(ranges)
    }

    /// 行号发布只复制原有限锚点的行号；Native 几何转换前归还借用，不持另一份 fold owner。
    pub(in crate::native_surface) fn folded_header_lines(
        &self,
        permit: &NativeMarkdownPagedInputPermit,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<Vec<usize>> {
        self.check_permit(permit, current)?;
        if permit.cell.is_some() {
            return Err(NativeMarkdownPagedInputError::BodyTargetRequired);
        }
        let headers = self
            .folds
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?
            .iter()
            .map(|fold| fold.header_line)
            .collect();
        self.check_permit(permit, current)?;
        Ok(headers)
    }

    /// 单元格或仍有 marked 的输入没有修改 Body 折叠的权限；正式动作先走原组合提交流程。
    fn require_fold_action(&self, permit: &NativeMarkdownPagedInputPermit) -> Result<()> {
        if permit.cell.is_some() {
            return Err(NativeMarkdownPagedInputError::BodyTargetRequired);
        }
        if permit.before.marked.is_some() {
            return Err(NativeMarkdownPagedInputError::CompositionActive);
        }
        Ok(())
    }

    /// 最后一次可重入 current 在所有 Rust 借用外完成；随后只做原 Core 选择与有限 metadata 发布。
    fn publish_fold_intent(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        folds: Vec<MarkdownFoldRegion>,
        selection: Option<MarkdownTextSelection>,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        self.check_permit(permit, current)?;
        let original = self
            .document_identity()
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let mut document = original
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        if !self.matches_document(&document, permit.before) {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        let mut old_folds = self
            .folds
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let folds_changed = *old_folds != folds;
        if !folds_changed && selection.is_none() {
            return Ok(NativeMarkdownPagedInputOutcome::Unchanged(permit.before));
        }
        let revision = if folds_changed {
            permit
                .before
                .fold_revision
                .checked_add(1)
                .ok_or(NativeMarkdownPagedInputError::CounterExhausted)?
        } else {
            permit.before.fold_revision
        };
        let selection_changed = selection.is_some_and(|next| {
            next != permit.before.selection || !permit.before.selection_is_directed
        });
        let mut state = permit.before;
        if let Some(next) = selection {
            // 原 Core 先验证 scalar、Source 健康与 idle；失败时旧 folds/选择事实均未发布。
            document.set_selection(next)?;
            if !folds_changed && !selection_changed {
                return Ok(NativeMarkdownPagedInputOutcome::Unchanged(permit.before));
            }
            let serial = self
                .selection
                .publish(state.generation, next, true, None, None);
            drop(self.selection_ranges.replace(Vec::new()));
            state.selection = next;
            state.selection_serial = serial;
            state.selection_is_directed = true;
            state.selection_range_count = 1;
            state.primary_range = ordered_selection_range(next);
        }
        *old_folds = folds;
        self.fold_revision.set(revision);
        self.clear_navigation_geometry();
        state.fold_revision = revision;
        drop(old_folds);
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
                    selection_changed,
                    marked_changed: false,
                    folds_changed,
                },
            },
        ))
    }
}

/// 复用旧 Source delegate 的真实 CRLF-aware 省略合同，仅补被隐藏末行实际 terminator。
/// 闭包读取失败后仍让原 Doc 再核 Source 首错，不能把 EIO 压成普通无折叠。
pub(super) fn folded_layout_range(
    document: &MarkdownDocumentBuffer,
    fold: &MarkdownFoldRegion,
) -> Result<Range<usize>> {
    let hidden = &fold.hidden_utf16_range;
    document.byte_offset_for_utf16(hidden.start)?;
    document.byte_offset_for_utf16(hidden.end)?;
    let end = super::super::fold_state::native_fold_projection_end(
        hidden,
        document.utf16_len(),
        |offset| document.utf16_code_unit_at(offset).ok(),
        |offset| {
            let byte = document.byte_offset_for_utf16(offset).ok()?;
            let number = document.line_for_byte(byte).ok()?;
            let start = document
                .utf16_offset_for_byte(document.byte_offset_for_line(number).ok()?)
                .ok()?;
            let next = number.checked_add(1)?;
            let end = if next < document.line_count() {
                document
                    .utf16_offset_for_byte(document.byte_offset_for_line(next).ok()?)
                    .ok()?
            } else {
                document.utf16_len()
            };
            Some((start, end))
        },
    );
    document.byte_offset_for_utf16(hidden.start)?;
    let end = end.ok_or(NativeMarkdownPagedInputError::InvalidRange)?;
    Ok(hidden.start..end)
}
