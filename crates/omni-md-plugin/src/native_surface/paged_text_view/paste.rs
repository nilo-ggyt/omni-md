//! 原文件粘贴与拖放沿现存输入链排队，文件归属仍由受限导入的provisional事务确认。

use objc2::{DefinedClass, runtime::ProtocolObject};
use objc2_app_kit::{NSDragOperation, NSDraggingInfo, NSPasteboardTypeString};
use omni_markdown_core::MarkdownTextSelection;

use super::{NativeMarkdownPagedTextView, NativeMarkdownPagedTextViewError as Error, Result};
use crate::native_surface::workspace_actions::{
    insert_native_markdown_paged_attachments, native_markdown_pasteboard_file_paths,
};
use crate::payload_use::PayloadUseGuard;

impl NativeMarkdownPagedTextView {
    /// 路径到达时固定的原输入事实必须仍成立；原Body只接纳这一份有限载荷，不复制或打开外部文件。
    pub(in crate::native_surface) fn queue_attachment_paths(
        &self,
        paths: Vec<std::path::PathBuf>,
        drop: Option<(u64, usize)>,
        before: super::NativeMarkdownPagedInputState,
    ) -> Result<()> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let origin = self.origin()?;
        if origin.state != before {
            return Err(Error::Revoked); // Native路径读取期间已发生后继输入，旧文件请求不得领取较新的选区。
        }
        if origin.input.is_cell() {
            return Err(Error::UnsupportedCommand);
        }
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        if !origin.delegate.defer_attachments(paths, drop, &permit)? {
            return Err(Error::UnsupportedCommand);
        }
        Ok(())
    }

    /// 进入拖放只检查有限类型/路径，不复制文件、不改变选择，也不提前确认正在编辑的组合文字。
    pub(super) fn accepted_drag_operation(
        &self,
        sender: &ProtocolObject<dyn NSDraggingInfo>,
    ) -> NSDragOperation {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let accepted = (|| {
            let origin = self.origin()?;
            if origin.input.is_cell() {
                return Ok(false); // 单元格不能借正文附件语义产生未定义的表格转义或外部文件副作用。
            }
            if !origin
                .step(self, || sender.draggingSourceOperationMask())?
                .contains(NSDragOperation::Copy)
            {
                return Ok(false); // 本目标只复制外部源；源未允许Copy时不宣称Move/Link也能导入。
            }
            let board = origin.step(self, || sender.draggingPasteboard())?;
            let paths = origin
                .step(self, || native_markdown_pasteboard_file_paths(&board))?
                .map_err(Error::AttachmentImport)?;
            if !paths.is_empty() {
                return Ok(true);
            }
            // SAFETY: 系统公开字符串类型常量只用于实际拖板的类型查询，不读取或物化文本。
            let types = objc2_foundation::NSArray::from_slice(&[unsafe { NSPasteboardTypeString }]);
            Ok(origin
                .step(self, || board.availableTypeFromArray(&types))?
                .is_some())
        })();
        match accepted {
            Ok(true) => NSDragOperation::Copy,
            Ok(false) => NSDragOperation::None,
            Err(cause) => {
                self.report_attachment_rejection(&cause);
                self.failure(cause); // 拒绝完整超限批次，并沿原错误通知反馈；不静默接收前缀。
                NSDragOperation::None
            }
        }
    }

    /// 实际放下只使用现成Scene的真实命中；未Ready时拒绝，不为接收文件读取全篇或猜测行高。
    pub(super) fn accept_drop(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> Result<()> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        self.cancel_link_intent();
        let origin = self.origin()?;
        if origin.state.marked.is_some() {
            let context = self
                .ivars()
                .context
                .borrow()
                .clone()
                .ok_or(Error::ContextUnavailable)?;
            // 放下接受已显示组合，再结束原转换；不能让旧IME缓冲在附件动作后再次提交。
            // 同步回调仍核原客户，外部文件此时尚未复制，Scene命中仍在收尾后重新领取。
            self.finish_composition_in_context(
                &origin.input,
                &origin.delegate,
                &context,
                origin.epoch,
            )?;
        }
        let scene = self.scene()?;
        if scene.origin.input.is_cell() {
            return Err(Error::UnsupportedCommand);
        }
        if !scene
            .step(self, || sender.draggingSourceOperationMask())?
            .contains(NSDragOperation::Copy)
        {
            return Err(Error::UnsupportedCommand); // 放下时重核真实源操作许可，不能沿先前enter替后继mask补签。
        }
        let board = scene.step(self, || sender.draggingPasteboard())?;
        let point = scene.step(self, || sender.draggingLocation())?;
        let local = scene.step(self, || self.convertPoint_fromView(point, None))?;
        let position = scene.scene.hit(local)?;
        if !scene.current(self) {
            return Err(Error::Revoked);
        }
        let current = |epoch| epoch == scene.origin.epoch && scene.current(self);
        let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
        let paths = scene
            .step(self, || native_markdown_pasteboard_file_paths(&board))?
            .map_err(Error::AttachmentImport)?;
        if !paths.is_empty() {
            return self.queue_attachment_paths(
                paths,
                Some((scene.origin.state.generation, position)),
                scene.origin.state,
            ); // 只接纳原文件动作；文件归属、文本和Undo直到原队首才提交。
        }
        if scene.origin.delegate.has_native_input_barrier() {
            return Err(Error::Busy); // 真实导航尚未结束时不以鼠标旧选择覆盖它，也不把拖板改读成general clipboard。
        }
        let text = scene
            .step(self, || {
                // SAFETY: 只读取当前这次实际拖板的公开纯文本类型；有限转换由唯一Input执行。
                board.stringForType(unsafe { NSPasteboardTypeString })
            })?
            .ok_or(Error::ClipboardUnavailable)?;
        let commit = scene.origin.input.insert_native_isolated(
            &permit,
            &text,
            objc2_foundation::NSRange::new(position, 0),
            &current,
        )?;
        self.finish_edit_commit(&scene.origin, commit)
    }

    /// 唯一队首用前步真实收据接续；外部拖放只有canonical未变时能使用原物理命中。
    pub(in crate::native_surface) fn perform_queued_attachments(
        &self,
        paths: &[std::path::PathBuf],
        drop: Option<(u64, usize)>,
    ) -> Result<()> {
        self.with_interpreted_input(|| {
            let _callback = PayloadUseGuard::enter(&self.ivars().input_callback_depth);
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            self.cancel_link_intent();
            let origin = self.origin()?;
            if origin.state.marked.is_some() {
                let context = self
                    .ivars()
                    .context
                    .borrow()
                    .clone()
                    .ok_or(Error::ContextUnavailable)?;
                // 队首前面的真实输入可刚建立组合；仅接受该原客户显示文字并结束系统转换。
                // 不重领解释权限；下方拖放generation与原physical命中仍须在收尾后严格核对。
                self.finish_composition_in_context(
                    &origin.input,
                    &origin.delegate,
                    &context,
                    origin.epoch,
                )?;
            }
            if let Some((generation, position)) = drop {
                let mut origin = self.origin()?;
                origin.interpreted = true;
                if origin.state.generation != generation {
                    return Err(Error::AttachmentImport(
                        "拖入前原文稿已改变，请重新拖入".into(),
                    )); // 原命中已失效，拒绝文件动作但保留健康正文。
                }
                let current = |epoch| epoch == origin.epoch && origin.current(self);
                let permit = origin.input.capture(origin.epoch, &current)?;
                let outcome = origin.input.set_selection_intent(
                    &permit,
                    MarkdownTextSelection {
                        anchor_utf16: position,
                        head_utf16: position,
                    },
                    None,
                    None,
                    &current,
                )?;
                self.finish_outcome(&origin, outcome)?;
            }
            insert_native_markdown_paged_attachments(self, paths)
        })
    }
}
