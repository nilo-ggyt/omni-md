//! 格式动作只消费原领域枚举和输入票；原共享piece计划负责文字、历史与真实选择。

use super::*;
use omni_markdown_core::MarkdownFormatAction;

impl NativeMarkdownPagedInput {
    /// 用原Body/Cell有向选择执行一次格式事务；块位置与行内inner均沿Core原语义。
    ///
    /// 格式计划、真实变化observer与历史先共同完成，成功后只发布有限输入metadata。
    /// 返回收据后即使窗口/输入失权，调用方也只能停止显示后果，不能重放本次格式动作。
    ///
    /// # Errors
    /// marked、未知方向多选、旧票、Core预算或observer失败均保留原文和历史。
    /// Cell只接原八项行内格式，正文块动作明确返回位置错误，不提交草稿或改到canonical正文。
    pub fn apply_format(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        action: MarkdownFormatAction,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputCommit> {
        let _mutation = self.begin(permit, current)?;
        self.check_permit(permit, current)?;
        if permit.before.marked.is_some() {
            return Err(NativeMarkdownPagedInputError::CompositionActive);
        } // 组合文字仍归IME；格式按钮不强制确认或取消marked来重新捕获一份较新选择。
        if !permit.before.selection_is_directed {
            return Err(NativeMarkdownPagedInputError::MultipleSelectionInput);
        } // Core格式只有一个有向选择；不拿多选memento冒充主范围而静默丢掉其他选择。
        if let Some(cell) = &permit.cell {
            // 原permit的Cell胶囊来自同一真实target；只取原会话，不从canonical范围重捕局部输入。
            let session = cell.session()?;
            let original = session.document()?;
            let mut document = original
                .try_borrow_mut()
                .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
            let (facts, edit) = document.format_cell_draft(cell.take()?, action)?;
            let commit = self.publish_cell(
                permit,
                facts,
                Some((edit.replaced_utf16_range, edit.change_in_utf16_length)),
                None,
            ); // Core已一次发布局部root/历史；只复用同一metadata发布，不运行正文observer或第二次格式。
            drop(document);
            drop(original);
            drop(session);
            return Ok(commit); // 不在成功后调用current或读Source，把失权的显示后果留给原收据核验。
        }
        let original = self
            .document_identity()
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let observer = self.observer(permit.before)?;
        let mut document = original
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let mut folds = self
            .folds
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        if !self.matches_document(&document, permit.before) {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        // 仅Core生成标记、共享原选中文字并准备真实变化流；Native不维护第二份格式或tag表。
        let (edit, prepared) =
            document.apply_format_observed(action, permit.before.selection, observer)?;
        let commit = self.publish(permit, &document, prepared, None, Some(edit), &mut folds); // Core已发布；从这里到收据返回无Native/current/后置可失败IO，不把已提交结果伪装成拒绝。
        drop(folds);
        drop(document);
        drop(original);
        Ok(commit)
    }
}
