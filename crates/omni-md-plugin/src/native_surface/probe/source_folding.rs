//! 正式 Source 折叠场景只走原菜单、行号和输入队列；期待正文由原独立 FD 流式核验。

use super::*;
use crate::native_surface::paged_body::NativeMarkdownPagedBodyResourceReceipts;
use crate::native_surface::paged_text_view::{
    NativeMarkdownPagedTextView, NativeMarkdownPagedTextViewError,
};
use std::os::unix::fs::MetadataExt;

const PREFIX: &str = "# F\r\nalpha beta\r\n# N\r\n";
const TYPED: &str = "后续";
const WORD_INSERTED: &str = "# F\r\nalpha后续 beta\r\n# N\r\n";
const DELETE_INSERTED: &str = "# F后续alpha beta\r\n# N\r\n";
const DELETED: &str = "# Falpha beta\r\n# N\r\n";
const LONG_HEADER: &str = "# 中文长文、分级标题与二百四十行混合表格交互验收";

/// 只存原 Producer 的弱资源收据及显示代次，不持正文、图、字体或另一份 fold 锚点。
pub(super) struct SourceFolding {
    pub(super) expected_revision: u64,
    retired: Option<NativeMarkdownPagedBodyResourceReceipts>,
}

impl SourceSession {
    /// 原菜单项拥有固定 tag；归还 Active 租约后再进入其原 controller selector。
    pub(super) fn source_fold_menu(&self, tag: isize) -> Result<(), String> {
        let (controller, item) = with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            let controls = surface
                .controller
                .ivars()
                .workspace_controls
                .get()
                .expect("正式窗口已装配原文档菜单");
            let popup = controls[1]
                .downcast_ref::<NSPopUpButton>()
                .expect("原工作区第二项是文档菜单");
            let menu = popup.menu().ok_or("折叠验收没有原文档菜单")?;
            for group in menu.itemArray().iter() {
                if let Some(submenu) = group.submenu() {
                    for item in submenu.itemArray().iter() {
                        if item.tag() == tag && item.isEnabled() {
                            return Ok((surface.controller.clone(), item));
                        }
                    }
                }
            }
            Err("正式菜单缺少可用的原折叠叶项".into())
        })?;
        // SAFETY: 原 controller 与其实际拥有的 NSMenuItem 均存活，ABI 与真实菜单点击一致。
        unsafe {
            let _: () = msg_send![&*controller, performMarkdownWorkspaceAction: &*item];
        }
        Ok(())
    }

    /// 只在原窗口取得真正 Paged Source 焦点，不新建客户、上下文或第二正文。
    pub(super) fn source_fold_view(
        &self,
    ) -> Result<Rc<Retained<NativeMarkdownPagedTextView>>, String> {
        with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            let source = surface
                .editor
                .as_ref()
                .ok_or("折叠验收没有正式 Source 后端")?;
            if !surface
                .panel
                .makeFirstResponder(Some(source.text_view.as_super()))
            {
                return Err("折叠验收未取得原正文焦点".into());
            }
            Ok(source.text_view.clone())
        })
    }

    /// 动作前捕原图实际收据；下一完整帧必须证明旧 Source、spool 与 worker 均已退休。
    pub(super) fn expect_source_fold_transition(&mut self, delta: u64) -> Result<(), String> {
        let (revision, retired) = with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            let source = surface.editor.as_ref().ok_or("折叠变更缺少原 Source")?;
            let revision = source
                .text_view
                .input_state()
                .map_err(|cause| cause.to_string())?
                .fold_revision;
            Ok((
                revision.checked_add(delta).ok_or("折叠验收代次耗尽")?,
                source
                    .body
                    .resource_receipts()
                    .map_err(|cause| cause.to_string())?,
            ))
        })?;
        let state = self.folding.as_mut().ok_or("折叠场景没有原观察 owner")?;
        state.expected_revision = revision;
        state.retired = Some(retired);
        Ok(())
    }

    /// 首次原菜单折叠长文 H1；后续真实帧不能用同 generation 的旧展开图冒充完成。
    pub(super) fn begin_source_folding(&mut self) -> Result<(), String> {
        self.verify_retired()?;
        self.verify_frame(0)?;
        self.folding = Some(SourceFolding {
            expected_revision: 0,
            retired: None,
        });
        let view = self.source_fold_view()?;
        view.command(sel!(moveToBeginningOfDocument:))
            .map_err(|cause| cause.to_string())?;
        self.expect_source_fold_transition(1)?;
        self.source_fold_menu(20)
    }

    /// 原行号鼠标入口实际消费 header 的屏幕字形及同窗口坐标，不直接调用折叠内部方法。
    pub(super) fn click_source_fold_gutter(&mut self) -> Result<(), String> {
        self.expect_source_fold_transition(1)?;
        let (ruler, event) = with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            let editor = surface.editor.as_ref().ok_or("行号验收缺少原文稿")?;
            let source = editor;
            let header = source
                .text_view
                .screen_rect(NSRange::new(0, 1))
                .map_err(|cause| format!("原行号点击前 header 0..1 无实际矩形：{cause:?}"))?;
            let ruler = editor
                .scroll_view()
                .verticalRulerView()
                .ok_or("正式 Source 没有原行号视图")?;
            let bounds = ruler.bounds();
            let x = ruler
                .convertPoint_toView(
                    NSPoint::new(bounds.origin.x + bounds.size.width * 0.5, bounds.origin.y),
                    None,
                )
                .x;
            let header = surface.panel.convertRectFromScreen(header.rect);
            let point = NSPoint::new(x, header.origin.y + header.size.height * 0.5);
            let timestamp = objc2_foundation::NSProcessInfo::processInfo().systemUptime();
            // 有限合成鼠标事件只交原自有 ruler；不发全局事件、不改用户鼠标位置或热键。
            let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                    NSEventType::LeftMouseDown,
                    point,
                    NSEventModifierFlags::empty(),
                    timestamp,
                    surface.panel.windowNumber(),
                    None,
                    1,
                    1,
                    1.0,
                ).ok_or("系统未创建本窗口行号鼠标事件")?;
            Ok((ruler, event))
        })?;
        // 原 NSRulerView 的公开 mouseDown 消费同窗口坐标；先归还 Active，再交付这个实际对象。
        ruler.mouseDown(&event);
        Ok(())
    }

    /// 逐次显示变更先核同 fold revision 和旧图真实释放，再流式核 canonical 与有限新增前缀。
    pub(super) fn verify_source_fold_prefix(
        &mut self,
        edits: u64,
        prefix: &str,
    ) -> Result<(), String> {
        self.verify_frame(edits)?;
        let state = self.folding.as_mut().ok_or("折叠帧缺少原观察 owner")?;
        if let Some(receipts) = state.retired.take() {
            let writer = receipts.writer.snapshot();
            let reader = receipts.reader.as_ref().map(|reader| reader.snapshot());
            if !receipts.source.payload_released()
                || !receipts.worker.is_released()
                || writer.file_open
                || writer.mapped_bytes != 0
                || writer.buffer_bytes != 0
                || writer.cleanup_failed
                || writer.reader_failed
                || reader.is_some_and(|reader| {
                    reader.file_open
                        || reader.mapped_bytes != 0
                        || reader.buffer_bytes != 0
                        || reader.cleanup_failed
                        || reader.reader_failed
                })
            {
                return Err("折叠新帧仍保留上一原图的 Source、索引或 worker 资源".into());
            }
        }
        let expected_revision = state.expected_revision;
        with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            let editor = surface.editor.as_ref().ok_or("折叠字节验收缺少原文稿")?;
            let source = editor;
            let state = source
                .text_view
                .input_state()
                .map_err(|cause| cause.to_string())?;
            if state.fold_revision != expected_revision {
                return Err("折叠终点没有提交本次原显示代次".into());
            }
            let document = editor.document().borrow();
            if document.generation() != self.original.generation + edits
                || document.utf16_len() != self.original.length + prefix.encode_utf16().count()
            {
                return Err("折叠或后继输入改变了未声明的 canonical 代次/长度".into());
            }
            let mut comparison = SourceComparison {
                original: &mut self.original_file,
                replaced: 0..0,
                inserted: prefix.as_bytes(),
                written: 0,
                scratch: [0; 64 * 1024],
            };
            document
                .write_to(&mut comparison)
                .map_err(|cause| cause.to_string())?;
            if comparison.written != self.original.disk.2 as usize + prefix.len() {
                return Err("折叠验收未完整比较原 canonical 字节".into());
            }
            if edits == 0 && (document.can_undo() || document.can_redo()) {
                return Err("仅显示折叠制造了文字 Undo/Redo 历史".into());
            }
            Ok(())
        })
    }

    /// 不可见的真实 scalar 必须没有几何；header 仍须给出原可命中矩形，而不是清空整个正文。
    pub(super) fn verify_source_fold_hidden(&self, finite: bool) -> Result<(), String> {
        with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            let source = surface.editor.as_ref().ok_or("隐区几何验收缺少 Source")?;
            source
                .text_view
                .screen_rect(NSRange::new(0, 1))
                .map_err(|cause| format!("折叠 header 0..1 无实际矩形：{cause:?}"))?;
            let hidden = if finite {
                5 // CRLF 后第一个实际正文 scalar；不把 LF 半边或 header 终止符冒充隐藏文字。
            } else {
                LONG_HEADER.encode_utf16().count() + 2
            };
            match source.text_view.screen_rect(NSRange::new(hidden, 1)) {
                Err(NativeMarkdownPagedTextViewError::Pending) => Ok(()),
                Ok(_) => Err("折叠正文仍返回可命中的隐藏 scalar 字形".into()),
                Err(cause) => Err(format!(
                    "合法隐藏 scalar {hidden}..{} 没有保留无几何结果：{cause:?}",
                    hidden + 1
                )),
            }
        })
    }

    /// 使用原有序范围的全选菜单契约；全文只选坐标，不读取或复制正文以构造测试输入。
    pub(super) fn select_all_and_source_fold(&mut self) -> Result<(), String> {
        let view = self.source_fold_view()?;
        view.command(sel!(selectAll:))
            .map_err(|cause| cause.to_string())?;
        self.expect_source_fold_transition(1)?;
        self.source_fold_menu(20)
    }

    /// 全选两端都在隐区外，原单范围必须完整保留；原菜单仍折第一标题，而非有向 head 所在尾行。
    pub(super) fn verify_source_fold_select_all(&self) -> Result<(), String> {
        with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            let source = surface.editor.as_ref().ok_or("全选折叠验收缺少 Source")?;
            let state = source
                .text_view
                .input_state()
                .map_err(|cause| cause.to_string())?;
            if state.primary_range != NSRange::new(0, self.original.length)
                || state.selection_range_count != 1
                || state.directed_selection()
                    != Some(MarkdownTextSelection {
                        anchor_utf16: 0,
                        head_utf16: self.original.length,
                    })
            {
                return Err("原菜单全选折叠改变了真实范围或方向".into());
            }
            Ok(())
        })?;
        self.verify_source_fold_hidden(false)
    }

    /// Find 仍选择独立原夹具中的整段命中，并用实际封存字形确认隐区已展开到屏幕内。
    pub(super) fn verify_source_fold_find(&self) -> Result<(), String> {
        with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            let source = surface.editor.as_ref().ok_or("折叠 Find 验收缺少 Source")?;
            let state = source
                .text_view
                .input_state()
                .map_err(|cause| cause.to_string())?;
            let range = &self.original.table_match;
            if state.primary_range != NSRange::new(range.start, range.len()) {
                return Err("Find 没有选择隐区中的原完整查询命中".into());
            }
            let key = source
                .text_view
                .capture_ready_scene_key()
                .map_err(|cause| cause.to_string())?;
            source
                .text_view
                .verify_find_target_visible(&key, state.primary_range)
                .map_err(|cause| cause.to_string())?;
            Ok(())
        })
    }

    /// 有限前缀提供独立词边界；只用真实 NSTextInputClient 输入，后继定位仍由原命令队列排序。
    pub(super) fn insert_source_fold_prefix(&mut self) -> Result<(), String> {
        let view = self.source_fold_view()?;
        view.command(sel!(moveToBeginningOfDocument:))
            .map_err(|cause| cause.to_string())?;
        self.expect_source_fold_transition(1)?;
        let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**view);
        // SAFETY: 正式原客户接收固定有限前缀，NSNotFound 使用前一真实命令提交的当前选择。
        unsafe {
            client.insertText_replacementRange(
                &NSString::from_str(PREFIX),
                NSRange::new(NSNotFound as usize, 0),
            );
        }
        view.command(sel!(moveToBeginningOfDocument:))
            .map_err(|cause| cause.to_string())
    }

    /// 从真实 header 行尾穿过 opaque 区：同队首先展开，再重算原字素/词组，后继文字不能抢先。
    pub(super) fn source_fold_word_then_type(&mut self, delete: bool) -> Result<(), String> {
        let view = self.source_fold_view()?;
        view.command(sel!(moveToEndOfLine:))
            .map_err(|cause| cause.to_string())?;
        // 原 Input 的每次文字发布也递增 fold revision；包括真实展开、可选删除与后继文字各一次。
        self.expect_source_fold_transition(if delete { 3 } else { 2 })?;
        view.command(if delete {
            sel!(deleteWordForward:)
        } else {
            sel!(moveWordForward:)
        })
        .map_err(|cause| cause.to_string())?;
        let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**view);
        // SAFETY: 原客户的后继有限输入实际排在词组意图后；测试不能展开或补写 Core 来伪造先后顺序。
        unsafe {
            client.insertText_replacementRange(
                &NSString::from_str(TYPED),
                NSRange::new(NSNotFound as usize, 0),
            );
        }
        Ok(())
    }

    /// Undo 恢复原 canonical 后按正式保存落盘；独立原 FD 再比较一次，并更新真实 strict-close 基线。
    pub(super) fn save_source_fold_restore(
        &mut self,
        workload: &NativeMarkdownMemoryWorkload,
        edits: u64,
    ) -> Result<(), String> {
        with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            surface.save_current_document()?;
            let document = surface
                .editor
                .as_ref()
                .ok_or("折叠恢复保存缺少文稿")?
                .document()
                .borrow();
            if document.is_dirty()
                || document.generation() != self.original.generation + edits
                || document
                    .current_revision()
                    .map_err(|cause| cause.to_string())?
                    != self.original.revision
            {
                return Err("折叠恢复保存没有保原 canonical revision".into());
            }
            drop(document);
            let mut file =
                std::fs::File::open(&workload.path).map_err(|cause| cause.to_string())?;
            let metadata = file.metadata().map_err(|cause| cause.to_string())?;
            let mut comparison = SourceComparison {
                original: &mut self.original_file,
                replaced: 0..0,
                inserted: &[],
                written: 0,
                scratch: [0; 64 * 1024],
            };
            std::io::copy(&mut file, &mut comparison).map_err(|cause| cause.to_string())?;
            if comparison.written != self.original.disk.2 as usize
                || metadata.len() != self.original.disk.2
            {
                return Err("折叠恢复保存的磁盘字节与独立原文件不同".into());
            }
            let mut facts = workload.facts.borrow_mut();
            let facts = facts.as_mut().ok_or("折叠保存没有原关闭事实")?;
            facts.generation = self.original.generation + edits;
            facts.disk = (
                metadata.dev(),
                metadata.ino(),
                metadata.len(),
                metadata.mtime(),
                metadata.mtime_nsec(),
            );
            Ok(())
        })
    }

    /// 同步返回紧接真实已接纳导航/折叠；后继原回调只能取消，不得复活正文或写回新显示锚点。
    pub(super) fn queue_source_fold_before_return(&self) -> Result<(), String> {
        let view = self.source_fold_view()?;
        view.command(sel!(moveToBeginningOfDocument:))
            .map_err(|cause| cause.to_string())?;
        view.command(sel!(moveWordForward:))
            .map_err(|cause| cause.to_string())?;
        self.source_fold_menu(20)
    }
}

impl NativeMarkdownMemoryWorkload {
    /// 独立折叠前缀沿原 SourceSession owner 运行；原附件场景及其立即 Return 不插入任何新动作。
    pub(crate) fn exercise_source_folding(&self, stage: i16) -> Result<(), String> {
        let mut case = self
            .source_session
            .borrow_mut()
            .take()
            .ok_or("正式折叠场景缺少原 SourceSession")?;
        let result = (|| match stage {
            540 => case.begin_source_folding(),
            541 => {
                case.verify_source_fold_prefix(0, "")
                    .map_err(|cause| format!("原折叠帧/canonical/退休收据：{cause}"))?;
                case.verify_source_fold_hidden(false)
                    .map_err(|cause| format!("原折叠可见/隐藏几何：{cause}"))?;
                case.click_source_fold_gutter()
                    .map_err(|cause| format!("原行号实际 mouseDown：{cause}"))
            }
            542 => {
                case.verify_source_fold_prefix(0, "")?;
                case.expect_source_fold_transition(1)?;
                case.source_fold_menu(20)
            }
            543 => {
                case.verify_source_fold_prefix(0, "")?;
                case.verify_source_fold_hidden(false)?;
                case.expect_source_fold_transition(1)?;
                perform_native_markdown_find_action(MarkdownFindAction::Next).map(|_| ())
            }
            544 => {
                case.verify_source_fold_prefix(0, "")?;
                case.verify_source_fold_find()?;
                case.select_all_and_source_fold()
            }
            545 => {
                case.verify_source_fold_prefix(0, "")?;
                case.verify_source_fold_select_all()?;
                case.expect_source_fold_transition(1)?;
                case.source_fold_menu(21)
            }
            546 => {
                case.verify_source_fold_prefix(0, "")?;
                case.insert_source_fold_prefix()
            }
            547 => {
                case.verify_source_fold_prefix(1, PREFIX)?;
                case.expect_source_fold_transition(1)?;
                case.source_fold_menu(20)
            }
            548 => {
                case.verify_source_fold_prefix(1, PREFIX)?;
                case.verify_source_fold_hidden(true)?;
                case.source_fold_word_then_type(false)
            }
            549 => {
                case.verify_source_fold_prefix(2, WORD_INSERTED)?;
                case.expect_source_fold_transition(1)?;
                case.history(false)
            }
            550 => {
                case.verify_source_fold_prefix(3, PREFIX)?;
                case.source_fold_view()?
                    .command(sel!(moveToBeginningOfDocument:))
                    .map_err(|cause| cause.to_string())?;
                case.expect_source_fold_transition(1)?;
                case.source_fold_menu(20)
            }
            551 => {
                case.verify_source_fold_prefix(3, PREFIX)?;
                case.verify_source_fold_hidden(true)?;
                case.source_fold_word_then_type(true)
            }
            552 => {
                case.verify_source_fold_prefix(5, DELETE_INSERTED)?;
                case.expect_source_fold_transition(1)?;
                case.history(false)
            }
            553 => {
                case.verify_source_fold_prefix(6, DELETED)?;
                case.expect_source_fold_transition(1)?;
                case.history(false)
            }
            554 => {
                case.verify_source_fold_prefix(7, PREFIX)?;
                case.expect_source_fold_transition(1)?;
                case.history(false)
            }
            555 => {
                case.verify_source_fold_prefix(8, "")?;
                case.save_source_fold_restore(self, 8)
                    .map_err(|cause| format!("原完整字节恢复后的正式保存：{cause}"))?;
                case.queue_source_fold_before_return()
                    .map_err(|cause| format!("原纯导航/折叠到达队列：{cause}"))
            }
            _ => Err("未声明的正式 Source 折叠场景事件".into()),
        })();
        *self.source_session.borrow_mut() = Some(case);
        result?;
        if stage == 555 {
            // 原导航和菜单已受理后，在同一栈接受 Return；实际 strict close 再核所有弱图及资源退场。
            self.request_return()
                .map_err(|cause| format!("原 Return 接纳与迟到动作取消：{cause}"))
        } else {
            Ok(())
        }
    }

    /// 540 等原 Preview 真实退休，其余只等同一次动作的 canonical 与 fold revision 完整帧。
    pub(crate) fn observe_source_folding_terminal(
        &self,
        stage: i16,
        observer: impl FnOnce() + 'static,
    ) -> Result<(), String> {
        let case = self
            .source_session
            .borrow_mut()
            .take()
            .ok_or("折叠终点观察缺少原 SourceSession")?;
        let result = (|| {
            if stage == 540 {
                return case.observe_retired(observer);
            }
            let edits = match stage {
                541..=546 => 0,
                547..=548 => 1,
                549 => 2,
                550..=551 => 3,
                552 => 5,
                553 => 6,
                554 => 7,
                555 => 8,
                _ => return Err("未声明的正式折叠帧终点".into()),
            };
            let revision = case
                .folding
                .as_ref()
                .ok_or("折叠终点没有原显示观察 owner")?
                .expected_revision;
            case.frame.fold_revision.set(Some(revision));
            case.observe_frame(edits, observer)
        })();
        *self.source_session.borrow_mut() = Some(case);
        result
    }
}
