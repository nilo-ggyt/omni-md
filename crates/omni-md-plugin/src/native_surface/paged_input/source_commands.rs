//! 源码写作直接消费Core键盘命令；标记续写、缩进和历史不在Native层重写。

use super::*;
use omni_markdown_core::{MarkdownEditingCommand, MarkdownEditingOutcome};

impl NativeMarkdownPagedInput {
    /// 用原票提交一次源码命令；普通Backspace只消费调用方同Scene认证的真实GCB范围。
    ///
    /// 返回None仅表示Core没有适用标记规则、且本次没有可用GCB计划；调用方须保留原
    /// Pending/错误，不能把它当作已删除。整个动作只消费一张permit，不重新捕获或重放。
    ///
    /// # Errors
    /// marked、多选未知方向、Cell局部坐标、旧票、资源或Core预检失败时保留原文和历史。
    pub fn apply_source_editing_command(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        command: MarkdownEditingCommand,
        ordinary_backward: Option<NSRange>,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<Option<NativeMarkdownPagedInputOutcome>> {
        let _mutation = self.begin(permit, current)?;
        self.check_permit(permit, current)?;
        if self.is_cell() {
            return Err(NativeMarkdownPagedInputError::BodyTargetRequired);
        } // 表格Tab/Return归原客户控制器，不把局部草稿位置用于canonical块命令。
        if permit.before.marked.is_some() {
            return Err(NativeMarkdownPagedInputError::CompositionActive);
        } // 输入法仍拥有候选；块算法不能删标记或结束组合。
        if !permit.before.selection_is_directed {
            return Err(NativeMarkdownPagedInputError::MultipleSelectionInput);
        } // 旧命令只支持一个真实方向，不能只改主范围并悄悄丢弃其他选择。

        let original = self
            .document_identity()
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let observer = self.observer(permit.before)?;
        {
            // current已在借用外完成；Core和FoldObserver只运行纯Rust预检及一次原子发布。
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
            let (outcome, prepared) = document.apply_editing_command_observed(
                command,
                permit.before.selection,
                observer,
            )?;
            match outcome {
                MarkdownEditingOutcome::Changed(edit) => {
                    // Core已经成功；此后只交换原fold/选择metadata，不调用Native或后置可失败IO。
                    return Ok(Some(NativeMarkdownPagedInputOutcome::Committed(
                        self.publish(
                            permit,
                            document.generation(),
                            prepared,
                            None,
                            Some(edit),
                            &mut folds,
                        ),
                    )));
                }
                MarkdownEditingOutcome::Unchanged => {
                    // 已无缩进的BackTab仍消费动作，不移动焦点、不推进serial或破坏Redo。
                    return Ok(Some(NativeMarkdownPagedInputOutcome::Unchanged(
                        permit.before,
                    )));
                }
                MarkdownEditingOutcome::NotHandled => {
                    // 无标记规则不发布临时派生metadata；归还全部借用后才检查原GCB的current。
                }
            }
        }
        if let Some(range) = ordinary_backward {
            // 复用原replace及FoldObserver；begin已消费同票并保持busy，绝不另取permit重试。
            return self
                .replace(permit, "", range, None, None, current)
                .map(NativeMarkdownPagedInputOutcome::Committed)
                .map(Some);
        }
        Ok(None) // 没有真实GCB便没有普通删除；原View必须返回其实际Pending/失败。
    }
}
