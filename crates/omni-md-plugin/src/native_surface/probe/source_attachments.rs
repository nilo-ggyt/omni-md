//! 正式Source窗口的受限附件、原Word输入顺序与关闭取消；只使用自有合成文件和私有剪贴板。

use super::*;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeFileURL};
use std::os::unix::fs::MetadataExt;

pub(super) const PREFIX: &str = "alpha beta ";
const TYPED: &str = "后续";
pub(super) const AFTER_FAILURE: &str = "失败后可编辑";
const FILE_NAME: &str = "source-attachment.txt";
const CANCELED_NAME: &str = "source-canceled-attachment.txt";
pub(super) const LINK: &str = "[source-attachment.txt](attachments/source-attachment.txt)";
const FILE_CHUNK: &[u8] = b"attachment-canonical-source\r\n";

/// 只保独占合成目录和外部源文件身份；原文稿、Scene、Body及Native对象均不由测试续体强持。
pub(super) struct SourceAttachments {
    directory: tempfile::TempDir,
    source: std::path::PathBuf,
    source_identity: (u64, u64, u64, i64, i64),
}

/// 原私有板名称由本次场景独占，任何成功或失败都不读取、覆盖用户的general clipboard。
struct PrivateBoard(Retained<NSPasteboard>);
impl Drop for PrivateBoard {
    fn drop(&mut self) {
        self.0.clearContents();
        // SAFETY: 只归还本场景实际创建的唯一板名；仍在AppKit主线程，不触及其他板的内容。
        unsafe {
            let _: () = msg_send![&*self.0, releaseGlobally];
        }
    }
}

/// 真实Foundation URL与私有pasteboard检查拒绝scheme/authority伪装，最后还原实际本地路径。
fn local_file_paths(source: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    let board = PrivateBoard(NSPasteboard::pasteboardWithUniqueName());
    // SAFETY: 框架公开file URL类型常量具有进程寿命；所有setter只作用于本次独占私有板。
    let file_type = unsafe { NSPasteboardTypeFileURL };
    for value in [
        "https://example.invalid/tmp/not-a-file",
        "file://remote.invalid/tmp/not-a-file",
    ] {
        board.0.clearContents();
        if !board
            .0
            .setString_forType(&NSString::from_str(value), file_type)
        {
            return Err("私有板未接受合成的文件类型反例".into());
        }
        if native_markdown_pasteboard_file_paths(&board.0).is_ok() {
            return Err("声明file类型的网络URL被误当成本地附件路径".into());
        }
    }
    board.0.clearContents();
    let url = objc2_foundation::NSURL::fileURLWithPath(&NSString::from_str(
        source.to_str().ok_or("合成附件路径不是UTF-8")?,
    ));
    let value = url
        .absoluteString()
        .ok_or("系统未能序列化实际本地附件URL")?;
    if !board.0.setString_forType(&value, file_type) {
        return Err("私有板未接受实际本地文件URL".into());
    }
    let paths = native_markdown_pasteboard_file_paths(&board.0)?;
    if paths != [source.to_path_buf()] {
        return Err("实际本地URL解码改变了源文件路径".into());
    }
    Ok(paths)
}

impl SourceAttachments {
    /// 按有限小块生成真实外部源，不在任何阶段持有完整附件或文稿String。
    fn new(workload: &NativeMarkdownMemoryWorkload) -> Result<Self, String> {
        let root = workload
            .path
            .parent()
            .and_then(Path::parent)
            .ok_or("合成工作负载没有独占根目录")?;
        let directory = tempfile::tempdir_in(root).map_err(|cause| cause.to_string())?;
        let source = directory.path().join(FILE_NAME);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&source)
            .map_err(|cause| cause.to_string())?;
        for _ in 0..4096 {
            file.write_all(FILE_CHUNK)
                .map_err(|cause| cause.to_string())?;
        }
        file.sync_all().map_err(|cause| cause.to_string())?;
        drop(file);
        let metadata = std::fs::metadata(&source).map_err(|cause| cause.to_string())?;
        let canceled = directory.path().join(CANCELED_NAME);
        std::fs::write(canceled, b"never import after accepted close")
            .map_err(|cause| cause.to_string())?;
        Ok(Self {
            directory,
            source,
            source_identity: (
                metadata.dev(),
                metadata.ino(),
                metadata.len(),
                metadata.mtime(),
                metadata.mtime_nsec(),
            ),
        })
    }

    /// 逐块核外部源未修改且受限目标字节完全相同；Undo不能删除仍由历史引用的已提交附件。
    fn verify_copied_file(&self, workload: &NativeMarkdownMemoryWorkload) -> Result<(), String> {
        let metadata = std::fs::metadata(&self.source).map_err(|cause| cause.to_string())?;
        if self.source_identity
            != (
                metadata.dev(),
                metadata.ino(),
                metadata.len(),
                metadata.mtime(),
                metadata.mtime_nsec(),
            )
        {
            return Err("附件导入修改或替换了外部源文件".into());
        }
        let destination = workload
            .path
            .parent()
            .ok_or("合成笔记缺少vault")?
            .join("attachments")
            .join(FILE_NAME);
        let mut file = std::fs::File::open(destination).map_err(|cause| cause.to_string())?;
        if file.metadata().map_err(|cause| cause.to_string())?.len() != self.source_identity.2 {
            return Err("受限附件目标长度不等于实际外部源".into());
        }
        let mut offset = 0;
        let mut scratch = [0; 4096];
        loop {
            let count = file.read(&mut scratch).map_err(|cause| cause.to_string())?;
            if count == 0 {
                break;
            }
            for (index, byte) in scratch[..count].iter().enumerate() {
                if *byte != FILE_CHUNK[(offset + index) % FILE_CHUNK.len()] {
                    return Err("受限附件目标字节不同于独立合成模式".into());
                }
            }
            offset += count;
        }
        Ok(())
    }
}

impl SourceSession {
    /// 原模式已正式重入后，实际Word前进、文件与后继输入必须按同一队列落到alpha之后。
    pub(super) fn begin_attachments(
        &mut self,
        workload: &NativeMarkdownMemoryWorkload,
    ) -> Result<(), String> {
        self.verify_frame(4)?;
        let attachments = SourceAttachments::new(workload)?;
        let paths = local_file_paths(&attachments.source)?;
        let view = with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            let source = surface.editor.as_ref().ok_or("附件场景缺少正式Source")?;
            if !surface
                .panel
                .makeFirstResponder(Some(source.text_view.as_super()))
            {
                return Err("附件场景未取得原正文焦点".into());
            }
            Ok(source.text_view.clone())
        })?;
        if let Some(cause) = view.take_failure() {
            return Err(format!("附件连续输入开始前原视图已有失败：{cause}"));
        } // 原绘制/坐标的真实首错在输入前就拒绝；取出后立即返回，绝不清掉后继续场景。
        view.command(sel!(moveToBeginningOfDocument:))
            .map_err(|cause| format!("附件连续输入的文档首定位失败：{cause}"))?;
        let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**view);
        // SAFETY: 原实际客户处理有限新增文本；不直接操作Core、generation或队列内部状态。
        unsafe {
            client.insertText_replacementRange(
                &NSString::from_str(PREFIX),
                NSRange::new(NSNotFound as usize, 0),
            );
        }
        if let Some(cause) = view.take_failure() {
            return Err(format!("附件PREFIX原NSTextInputClient回调失败：{cause}"));
        } // 仍在同一同步栈；只归属实际void输入首因，不等待帧或给后继动作补写文字。
        view.command(sel!(moveToBeginningOfDocument:))
            .map_err(|cause| format!("附件前缀输入后的文档首定位失败：{cause}"))?;
        view.command(sel!(moveWordForward:))
            .map_err(|cause| format!("附件前缀后的真实Word前进未接纳：{cause}"))?;
        view.queue_attachment_paths(
            paths,
            None,
            view.input_state()
                .map_err(|cause| format!("附件入队前原输入状态失效：{cause}"))?,
        )
        .map_err(|cause| format!("Word之后的原附件批次未接纳：{cause}"))?;
        // SAFETY: 此输入发生在已接纳Word与文件之后，唯一原队列必须拥有并实际接续这一有限载荷。
        unsafe {
            client.insertText_replacementRange(
                &NSString::from_str(TYPED),
                NSRange::new(NSNotFound as usize, 0),
            );
        }
        if let Some(cause) = view.take_failure() {
            return Err(format!(
                "附件连续输入的原NSTextInputClient回调失败：{cause}"
            ));
        }
        self.attachments = Some(attachments);
        Ok(())
    }

    /// 只有静态有限前缀变化；完整canonical仍逐块比独立原FD，不能从待测文稿反推期待内容。
    pub(super) fn verify_attachment_prefix(
        &mut self,
        workload: &NativeMarkdownMemoryWorkload,
        edits: u64,
        prefix: &str,
    ) -> Result<(), String> {
        self.verify_frame(edits)?;
        self.attachments
            .as_ref()
            .ok_or("附件场景缺少外部源owner")?
            .verify_copied_file(workload)?;
        with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            let editor = surface.editor.as_ref().ok_or("附件场景缺少原文稿")?;
            let document = editor.document().borrow();
            if document.generation() != self.original.generation + edits
                || document.utf16_len() != self.original.length + prefix.encode_utf16().count()
            {
                return Err("附件与后继输入的实际generation或canonical长度不符".into());
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
                return Err("附件场景未完整核验canonical字节".into());
            }
            if let Some(cause) = editor.body.take_failure() {
                return Err(cause.to_string());
            }
            Ok(())
        })
    }

    /// 源文件不存在只拒绝本次导入；随后真实NSTextInputClient输入须完成，正文不能永久failed。
    pub(super) fn enqueue_missing_attachment(&self) -> Result<(), String> {
        let attachments = self.attachments.as_ref().ok_or("附件拒绝缺少原合成目录")?;
        let view = with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            Ok(surface
                .editor
                .as_ref()
                .ok_or("附件拒绝缺少正式Source")?
                .text_view
                .clone())
        })?;
        view.command(sel!(moveToBeginningOfDocument:))
            .map_err(|cause| cause.to_string())?;
        view.queue_attachment_paths(
            vec![attachments.directory.path().join("missing-source.txt")],
            None,
            view.input_state().map_err(|cause| cause.to_string())?,
        )
        .map_err(|cause| cause.to_string())?;
        let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**view);
        // SAFETY: 失败导入后沿原客户真实输入；必须实际入同队尾，不能在测试中直接补写canonical。
        unsafe {
            client.insertText_replacementRange(
                &NSString::from_str(AFTER_FAILURE),
                NSRange::new(NSNotFound as usize, 0),
            );
        }
        Ok(())
    }

    /// 消费预期原业务错误并核可见反馈；真实Body/Source首错仍由逐帧与严格关闭核验拒绝。
    pub(super) fn verify_attachment_rejection(&self) -> Result<(), String> {
        with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            let source = surface.editor.as_ref().ok_or("附件拒绝反馈缺少原Source")?;
            if !matches!(source.text_view.take_failure(), Some(crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::AttachmentImport(_))) {
                return Err("缺失附件没有交付原业务失败".into());
            }
            // 反馈通过正式controller状态栏发布，不能仅保存诊断错误而让用户看到无响应。
            let label = surface
                .controller
                .ivars()
                .status_label
                .get()
                .expect("正式窗口装配原状态栏");
            if label.isHidden() || label.stringValue().length() == 0 {
                return Err("缺失附件没有正式可见状态反馈".into());
            }
            Ok(())
        })
    }

    /// 返回前只排原文件动作，随后的原Return必须在同一同步栈接受关闭并取消它。
    pub(super) fn queue_canceled_attachment(&self) -> Result<(), String> {
        let attachments = self.attachments.as_ref().ok_or("附件取消缺少原目录owner")?;
        let view = with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            Ok(surface
                .editor
                .as_ref()
                .ok_or("附件取消缺少原Source")?
                .text_view
                .clone())
        })?;
        view.queue_attachment_paths(
            vec![attachments.directory.path().join(CANCELED_NAME)],
            None,
            view.input_state().map_err(|cause| cause.to_string())?,
        )
        .map_err(|cause| cause.to_string())
    }
}

/// 严格关闭之后再核真正无文件副作用；不能把入队成功或同步Return受理当成取消已完成。
pub(super) fn verify_canceled_attachment(
    workload: &NativeMarkdownMemoryWorkload,
) -> Result<(), String> {
    let destination = workload
        .path
        .parent()
        .ok_or("取消检查没有原vault")?
        .join("attachments")
        .join(CANCELED_NAME);
    match std::fs::symlink_metadata(destination) {
        Err(cause) if cause.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err("已接受关闭后旧附件队列仍创建了文件".into()),
        Err(cause) => Err(cause.to_string()),
    }
}

impl SourceSession {
    /// 恢复原字节后沿正式保存更新真实关闭事实；不把Undo的clean标志当成磁盘已经写入。
    pub(super) fn save_attachment_restore(
        &mut self,
        workload: &NativeMarkdownMemoryWorkload,
        edits: u64,
    ) -> Result<(), String> {
        with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            surface.save_current_document()?;
            let editor = surface.editor.as_ref().ok_or("附件恢复保存缺少原文稿")?;
            let document = editor.document().borrow();
            if document.is_dirty()
                || document.generation() != self.original.generation + edits
                || document
                    .current_revision()
                    .map_err(|cause| cause.to_string())?
                    != self.original.revision
            {
                return Err("附件恢复保存没有保原canonical revision".into());
            }
            drop(document);
            let mut saved =
                std::fs::File::open(&workload.path).map_err(|cause| cause.to_string())?;
            let metadata = saved.metadata().map_err(|cause| cause.to_string())?;
            let mut comparison = SourceComparison {
                original: &mut self.original_file,
                replaced: 0..0,
                inserted: &[],
                written: 0,
                scratch: [0; 64 * 1024],
            };
            std::io::copy(&mut saved, &mut comparison).map_err(|cause| cause.to_string())?;
            if comparison.written != self.original.disk.2 as usize
                || metadata.len() != self.original.disk.2
            {
                return Err("附件恢复保存磁盘字节不等于独立原文件".into());
            }
            let mut facts = workload.facts.borrow_mut();
            let facts = facts.as_mut().ok_or("附件保存缺少原关闭事实")?;
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
}

impl NativeMarkdownMemoryWorkload {
    /// 原严格关闭阶段消费本次已排队取消文件的真实无副作用结果，不在关闭前提前签成功。
    pub(crate) fn verify_source_attachment_canceled(&self) -> Result<(), String> {
        verify_canceled_attachment(self)
    }
}
