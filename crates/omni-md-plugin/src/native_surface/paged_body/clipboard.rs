//! 显式复制只暂存真实选区；输出与Doc分离后才发布，剪切沿原输入事务删除。

use super::*;
use crate::native_surface::text_storage::{MarkdownPasteboardOutput, MarkdownPasteboardWriteError};
use objc2_app_kit::NSPasteboard;

impl NativeMarkdownPagedBody {
    /// 原系统复制动作使用标准剪贴板；原请求已由命令入口捕获，getter重入也不能换目标。
    pub(super) fn copy_source_selection(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> std::result::Result<(), ViewError> {
        let pasteboard = NSPasteboard::generalPasteboard();
        self.copy_source_selection_to(request, &pasteboard)
    }

    /// 发布到明确的Native目标；独立验收用私有剪贴板，产品始终由上方原动作传标准目标。
    pub(in crate::native_surface) fn copy_source_selection_to(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        pasteboard: &NSPasteboard,
    ) -> std::result::Result<(), ViewError> {
        let current = |epoch| {
            epoch == self.configuration.epoch
                && self.current_body()
                && self
                    .input
                    .target_domain()
                    .is_ok_and(|target| target.same(&request.target))
                && Weak::ptr_eq(&request.document, &self.input.document_identity())
                && self.input.state().is_ok_and(|state| state == request.input)
        };
        let permit = self.input.capture(self.configuration.epoch, &current)?;
        let ranges = self.input.selected_ranges(&permit, &current)?;
        if ranges.iter().all(std::ops::Range::is_empty) {
            // 单caret和多caret没有选中文字；复制/剪切不清空已有剪贴板，也不制造空行。
            return Ok(());
        }
        let source = self.input.capture_layout_source(&permit, &current)?;
        let output = MarkdownPasteboardOutput::prepare_source_ranges(&source, &ranges);
        // 原Source/Cell状态失败优先于暂存错误；只检查原票，不从当前状态重捕较新的输入。
        let checked = self.input.check_permit(&permit, &current);
        drop(source); // 先归还本次真实读取名额；Native输出只接收独立mmap，不保原Cell或Doc。
        checked?;
        let output = output.map_err(|cause| match cause {
            MarkdownPasteboardWriteError::DocumentReadUnavailable(cause) => {
                ViewError::Document(cause) // 原Source读取/回收失败继续保留真实因果，不改成空选区。
            }
            other => ViewError::ClipboardWrite(other.to_string()), // 未发布的文件/Native准备失败与源首错区分。
        })?;
        output
            .publish(pasteboard)
            .map_err(|cause| ViewError::ClipboardWrite(cause.to_string()))?;
        // 发布已是外部副作用；若其间切换或关闭，则拒绝后续Cut删除，不重捕或重放旧动作。
        self.input.check_permit(&permit, &current)?;
        Ok(())
    }
}
