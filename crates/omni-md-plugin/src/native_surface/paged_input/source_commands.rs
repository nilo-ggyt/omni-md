//! 源码写作直接消费Core键盘命令；标记续写、缩进和历史不在Native层重写。

use super::*;
use omni_markdown_core::{MarkdownEditingCommand, MarkdownEditingOutcome};

impl NativeMarkdownPagedInput {
    /// 源码插表复用原中文模板，一次正文事务同时发布首格caret；不复制被替换的正文。
    ///
    /// # Errors
    /// 组合、多选或Cell客户不具备块插入权限；原票、源IO、准备或发布失败保留旧正文和历史。
    pub fn insert_default_table(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputCommit> {
        let _mutation = self.begin(permit, current)?;
        if self.is_cell() {
            return Err(NativeMarkdownPagedInputError::BodyTargetRequired);
        } // 单元格草稿不能通过插表按钮把局部偏移当作canonical块位置。
        if permit.before.marked.is_some() {
            return Err(NativeMarkdownPagedInputError::CompositionActive);
        } // 候选仍归系统输入法；按钮不得强制确认或取消后再领取另一份选择。
        if !permit.before.selection_is_directed {
            return Err(NativeMarkdownPagedInputError::MultipleSelectionInput);
        }
        let range = permit.before.primary_range;
        let end = range
            .location
            .checked_add(range.length)
            .ok_or(NativeMarkdownPagedInputError::InvalidRange)?;
        let original = self
            .document_identity()
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let observer = self.observer(permit.before)?;
        let (token, prepared) = {
            let mut document = original
                .try_borrow_mut()
                .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
            // 只读取原选择两端相邻的UTF16单位；真实IO错误必须传播，不能当成普通非换行。
            let needs_prefix = range.location > 0
                && !matches!(document.utf16_code_unit_at(range.location - 1)?, 10 | 13);
            let needs_suffix =
                end < document.utf16_len() && !matches!(document.utf16_code_unit_at(end)?, 10 | 13);
            let markdown =
                super::super::native_markdown_default_table_source(needs_prefix, needs_suffix);
            let first = range
                .location
                .checked_add(usize::from(needs_prefix) * 2 + 2)
                .ok_or(NativeMarkdownPagedInputError::InvalidRange)?;
            document.prepare_text_input_observed(
                range.location..end,
                &markdown,
                permit.before.selection,
                MarkdownTextSelection::caret(first),
                observer,
            )?
        };
        // 准备不发布正文；归还Doc借用后才调用可重入的前台门，失权只取消本nonce。
        if let Err(cause) = self.check_permit(permit, current) {
            return Err(self.cancel(&original, &token, cause));
        }
        let mut document = match original.try_borrow_mut() {
            Ok(document) => document,
            Err(_) => {
                return Err(self.cancel(
                    &original,
                    &token,
                    NativeMarkdownPagedInputError::Borrowed,
                ));
            }
        };
        let mut folds = match self.folds.try_borrow_mut() {
            Ok(folds) => folds,
            Err(_) => {
                drop(document);
                return Err(self.cancel(
                    &original,
                    &token,
                    NativeMarkdownPagedInputError::Borrowed,
                ));
            }
        };
        let edit = match document.publish_text_input(&token) {
            Ok(edit) => edit,
            Err(cause) => {
                drop(folds);
                drop(document);
                return Err(self.cancel(&original, &token, cause.into()));
            }
        };
        // Core发布后只交换已准备好的选择/折叠metadata，无Native调用或第二次caret事务。
        let commit = self.publish(permit, &document, prepared, None, Some(edit), &mut folds);
        drop(folds);
        drop(document);
        drop(original);
        drop(token);
        Ok(commit)
    }

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
                        self.publish(permit, &document, prepared, None, Some(edit), &mut folds),
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
