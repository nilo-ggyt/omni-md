//! 查找替换只消费原坐标流；单次Core事务同时发布正文、历史和折叠锚点。

use super::*;
use omni_markdown_core::{MarkdownReplacement, MarkdownReplacementPart};

impl NativeMarkdownPagedInput {
    /// 消费有序、不重叠的旧正文替换流，保留原完整anchor/head作为唯一历史before。
    ///
    /// 搜索范围只属于每项replacement，不能拿匹配包络代替原选择。两层iterator只能返回
    /// 普通数据，不得调用Native、current或重入原文档；Core在一个短可变借用内消费它们。
    ///
    /// # Errors
    /// marked未结束、原票失效、原范围非法、输入流失败或资源不足时完整回滚。所有Err都在
    /// Core发布之前；成功提交之后的权限变化只能由返回Commit的check_current报告。
    pub fn apply_replacements_observed<'a, I, P>(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        replacements: I,
        after: MarkdownTextSelection,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome>
    where
        I: IntoIterator<
            Item = std::result::Result<MarkdownReplacement<P>, MarkdownDocumentBufferError>,
        >,
        P: IntoIterator<
            Item = std::result::Result<MarkdownReplacementPart<'a>, MarkdownDocumentBufferError>,
        >,
    {
        let _mutation = self.begin(permit, current)?;
        self.check_permit(permit, current)?;
        if self.is_cell() {
            return Err(NativeMarkdownPagedInputError::BodyTargetRequired);
        } // 原批量Find事务是canonical协议；未有局部原子批量实现前不能拿Cell坐标补签正文。
        if permit.before.marked.is_some() {
            // 查找栏不能借替换操作抢走输入法的真实候选及其相对选择。
            return Err(NativeMarkdownPagedInputError::CompositionActive);
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
            // 最后一轮可重入current已经结束；锁定原Doc后只核纯Rust事实，不补签新代次。
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        let (edit, prepared) = document.apply_replacements_observed(
            replacements,
            permit.before.selection,
            after,
            observer,
        )?;
        if edit.is_none() && *folds == prepared.folds {
            // 空流的Core合同保留原选择与redo；不能采用调用方after或制造一条假history。
            return Ok(NativeMarkdownPagedInputOutcome::Unchanged(permit.before));
        }
        // Core成功后只交换有限metadata。这里禁止Native/current及其他可失败操作，避免把
        // 已发生的正文修改报告成普通Err，导致调用方重放同一批替换。
        let commit = self.publish(
            permit,
            document.generation(),
            prepared,
            None,
            edit,
            &mut folds,
        );
        drop(folds);
        drop(document);
        drop(original);
        Ok(NativeMarkdownPagedInputOutcome::Committed(commit))
    }
}
