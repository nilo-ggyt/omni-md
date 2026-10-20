//! 同一Host长文会话的真实Unicode输入、结构历史与恢复保存；只保有限选中字和弱正文票据。

use super::*;
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;

const INSERTED_TEXT: &str = "中文😀验收e\u{301}";
const MAX_SELECTED_UNITS: usize = 64;
type DiskStamp = (u64, u64, u64, i64, i64);

#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    AwaitInserted,
    Inserted,
    AwaitUndo,
    Undone,
    AwaitRedo,
    Redone,
    AwaitRestored,
    Restored,
    AwaitSaved,
    Saved,
}

/// 不强持document、piece root或Native图；原片段最多64 UTF16，revision只保存固定摘要。
pub(super) struct HostedEditingProbe {
    original: MemoryWorkloadFacts,
    selection: MarkdownTextSelection,
    range: std::ops::Range<usize>,
    original_fragment: String,
    original_bytes: usize,
    inserted_units: usize,
    epoch: MemoryActionEpoch,
    input: crate::native_surface::paged_input::NativeMarkdownPagedInputState,
    // 每次真实输入后只观察原 Body 的完整 CPU 结果；弱帧票不保活正文、字体或像素。
    frame: RefCell<Option<Rc<MemoryFrameObservation>>>,
    inserted_revision: Option<omni_markdown_core::MarkdownDocumentRevision>,
    saved_disk: Option<DiskStamp>,
    phase: Phase,
}

impl HostedEditingProbe {
    /// 原Find全部验证后才捕获完整选择；不能改成固定caret或在输入后才决定预期范围。
    pub(super) fn begin(workload: &NativeMarkdownMemoryWorkload) -> Result<Self, String> {
        workload.verify_unchanged()?;
        let original = workload.facts()?;
        let probe = with_active_native_markdown_surface(|surface| {
            if !workload.current(surface, &original) {
                return Err("长文编辑开始时原Host正文已失效".into());
            }
            let editor = surface.editor.as_ref().ok_or("长文编辑缺少原编辑器")?;
            let paged = editor;
            let input = paged
                .text_view
                .input_state()
                .map_err(|cause| cause.to_string())?;
            let document = editor.document().borrow();
            let selection = document.selection();
            let range = selection.anchor_utf16.min(selection.head_utf16)
                ..selection.anchor_utf16.max(selection.head_utf16);
            if range.is_empty()
                || range.len() > MAX_SELECTED_UNITS
                || document
                    .current_revision()
                    .map_err(|error| format!("无法读取原文档revision：{error}"))?
                    != original.revision
                || document.can_undo()
                || document.can_redo()
            {
                // 本样本必须从只读Find的真实有限选择和空历史开始，不能改用更小的替代笔记。
                return Err("长文编辑原选择、保存根或历史不符合预声明前态".into());
            }
            let mut units = [0_u16; MAX_SELECTED_UNITS];
            document
                .copy_utf16_code_units(range.start, &mut units[..range.len()])
                .map_err(|_| "无法读取长文编辑的有限原选中字")?;
            let original_fragment = String::from_utf16(&units[..range.len()])
                .map_err(|_| "长文编辑原选区切断Unicode标量")?;
            Ok(Self {
                original: original.clone(),
                selection,
                range,
                original_fragment,
                original_bytes: document.byte_len(),
                inserted_units: INSERTED_TEXT.encode_utf16().count(),
                epoch: MemoryActionEpoch::capture(surface),
                input,
                frame: RefCell::new(None),
                inserted_revision: None,
                saved_disk: None,
                phase: Phase::AwaitInserted,
            })
        })?;
        // 准备对象只有弱票据；真正输入仍发生在同一产品surface上的公开NSTextInputClient入口。
        with_active_native_markdown_surface(|surface| probe.insert(surface))?;
        Ok(probe)
    }

    pub(super) fn completed(&self) -> bool {
        self.phase == Phase::Saved
    }

    /// 原 Host 继续使用唯一终点通知；编辑已有声明后态，不借只读 g0 收据给新输入补票。
    pub(super) fn observe_terminal(&self, observer: impl FnOnce() + 'static) -> Result<(), String> {
        let frame = self
            .frame
            .borrow()
            .clone()
            .ok_or("长文编辑缺少原完整 CPU 帧观察")?;
        frame.observe(observer)
    }

    /// 只有已核对有限文字、确切世代及历史方向的后态才能登记；不把任意新合法 Input 当预期。
    fn arm_frame(&self, surface: &NativeMarkdownWindowSurface, edits: u64) -> Result<(), String> {
        self.check_state(surface, edits)?;
        let paged = surface
            .editor
            .as_ref()
            .ok_or("长文编辑完整帧缺少原分页正文")?;
        let input = paged
            .text_view
            .input_state()
            .map_err(|cause| cause.to_string())?;
        let frame = MemoryFrameObservation::capture(surface, &self.original, self.epoch, input)?;
        let old = self.frame.replace(Some(frame));
        drop(old); // 先结束槽借用再释放旧观察，不能跨 Native weak 析构保 RefMut。
        Ok(())
    }

    /// 每个动作与检查独立接收；错误交给外层sticky失败，不能重新执行已变化的历史步骤。
    pub(super) fn step(
        &mut self,
        workload: &NativeMarkdownMemoryWorkload,
        step: NativeMarkdownHostedStep,
    ) -> Result<(), String> {
        match step {
            NativeMarkdownHostedStep::VerifyInserted if self.phase == Phase::AwaitInserted => {
                let revision = with_active_native_markdown_surface(|surface| {
                    self.verify_state(surface, 1, None)
                })?;
                self.inserted_revision = Some(revision); // 只接纳一次已知输入的真实成功后果。
                self.phase = Phase::Inserted;
            }
            NativeMarkdownHostedStep::UndoEditing if self.phase == Phase::Inserted => {
                with_active_native_markdown_surface(|surface| self.history(surface, 1, false))?;
                self.phase = Phase::AwaitUndo;
            }
            NativeMarkdownHostedStep::VerifyUndo if self.phase == Phase::AwaitUndo => {
                with_active_native_markdown_surface(|surface| {
                    self.verify_state(surface, 2, Some(self.original.revision))
                })?;
                self.phase = Phase::Undone;
            }
            NativeMarkdownHostedStep::RedoEditing if self.phase == Phase::Undone => {
                with_active_native_markdown_surface(|surface| self.history(surface, 2, true))?;
                self.phase = Phase::AwaitRedo;
            }
            NativeMarkdownHostedStep::VerifyRedo if self.phase == Phase::AwaitRedo => {
                let inserted = self.inserted_revision.ok_or("重做缺少原已验证输入摘要")?;
                with_active_native_markdown_surface(|surface| {
                    self.verify_state(surface, 3, Some(inserted))
                })?;
                self.phase = Phase::Redone;
            }
            NativeMarkdownHostedStep::RestoreEditing if self.phase == Phase::Redone => {
                with_active_native_markdown_surface(|surface| self.history(surface, 3, false))?;
                self.phase = Phase::AwaitRestored;
            }
            NativeMarkdownHostedStep::VerifyRestored if self.phase == Phase::AwaitRestored => {
                with_active_native_markdown_surface(|surface| {
                    self.verify_state(surface, 4, Some(self.original.revision))
                })?;
                self.phase = Phase::Restored;
            }
            NativeMarkdownHostedStep::SaveRestored if self.phase == Phase::Restored => {
                self.saved_disk = Some(with_active_native_markdown_surface(|surface| {
                    self.check_state(surface, 4)?;
                    surface.save_current_document()?; // 正式保存自行消费debounce；没有暂停此前autosave。
                    self.check_saved_memory(surface)?;
                    self.arm_frame(surface, 4)?;
                    disk_stamp(&workload.path)
                })?);
                self.phase = Phase::AwaitSaved;
            }
            NativeMarkdownHostedStep::VerifySaved if self.phase == Phase::AwaitSaved => {
                let expected_disk = self.saved_disk.ok_or("恢复保存缺少原成功保存元数据")?;
                with_active_native_markdown_surface(|surface| {
                    self.verify_saved_disk(surface, &workload.path, expected_disk)
                })?;
                // 只有精确四次历史代次、原字节恢复、真实保存和磁盘流式等同都完成才更新旧只读收据。
                let expected_generation = self
                    .original
                    .generation
                    .checked_add(4)
                    .ok_or("长文编辑预期代次溢出")?;
                workload.settle_known_hosted_save(
                    &self.original,
                    expected_generation,
                    self.selection,
                    expected_disk,
                )?;
                self.phase = Phase::Saved;
                println!(
                    "native_hosted_edit verified=true unicode_insert=true undo=true redo=true restored=true saved=true commits=1 history_transitions=3 source_mirror=false"
                );
            }
            _ => return Err("长文编辑步骤重复、乱序或缺少原阶段收据".into()),
        }
        Ok(())
    }

    fn selection_after(&self, edits: u64) -> Option<MarkdownTextSelection> {
        if edits.is_multiple_of(2) {
            Some(self.selection) // Undo恢复方向完整的原anchor/head；不能只比较有序范围。
        } else {
            self.range
                .start
                .checked_add(self.inserted_units)
                .map(MarkdownTextSelection::caret)
        }
    }

    /// 保留原 Input 的完整有向选择、marked 与世代权限；不以 TextKit 私有通知计数代替真实提交。
    fn permits(&self, surface: &NativeMarkdownWindowSurface, edits: u64) -> bool {
        if !native_markdown_controller_has_foreground_authority(&surface.controller)
            || std::ptr::from_ref(&*surface.controller).addr() != self.original.owner
            || !self.epoch.permits(surface)
        {
            return false;
        }
        let Some(editor) = surface.editor.as_ref() else {
            return false;
        };
        let paged = editor;
        if !WeakMarkdownDocument::ptr_eq(&self.original.document, &Rc::downgrade(editor.document()))
            || !std::rc::Weak::ptr_eq(&self.original.body, &Rc::downgrade(&paged.body))
            || !std::rc::Weak::ptr_eq(&self.original.input, &paged.body.probe_find_input_weak())
        {
            return false;
        }
        let Ok(input) = paged.text_view.input_state() else {
            return false;
        };
        let Some(selection) = self.selection_after(edits) else {
            return false;
        };
        let expected_units = if edits.is_multiple_of(2) {
            Some(self.original.length)
        } else {
            self.original
                .length
                .checked_sub(self.range.len())
                .and_then(|units| units.checked_add(self.inserted_units))
        };
        let expected_bytes = if edits.is_multiple_of(2) {
            Some(self.original_bytes)
        } else {
            self.original_bytes
                .checked_sub(self.original_fragment.len())
                .and_then(|bytes| bytes.checked_add(INSERTED_TEXT.len()))
        };
        let document = editor.document().borrow();
        self.original.generation.checked_add(edits) == Some(document.generation())
            && expected_units == Some(document.utf16_len())
            && expected_bytes == Some(document.byte_len())
            && document.selection() == selection
            && input.generation == document.generation()
            && input.utf16_len == document.utf16_len()
            && input.directed_selection() == Some(selection)
            && input.selection_range_count == 1 && input.marked.is_none()
            && input.primary_range == NSRange::new(selection.anchor_utf16.min(selection.head_utf16), selection.anchor_utf16.abs_diff(selection.head_utf16))
            // 每次原字符/历史事务只发布自己的后态；这两项限定同一 Input 意图，不能用后继合法选区补票。
            && self.input.selection_serial.checked_add(edits) == Some(input.selection_serial)
            && self.input.fold_revision.checked_add(edits) == Some(input.fold_revision)
    }

    /// 有限原片段、真实世代和历史方向共同限定提交；dirty 不作阶段判据，正式 autosave 继续运行。
    fn check_state(&self, surface: &NativeMarkdownWindowSurface, edits: u64) -> Result<(), String> {
        if !self.permits(surface, edits) {
            return Err("长文编辑的原 owner、Input、世代或完整有向选择已失效".into());
        }
        let editor = surface.editor.as_ref().ok_or("长文编辑原正文缺失")?;
        let expected = if edits.is_multiple_of(2) {
            self.original_fragment.as_str()
        } else {
            INSERTED_TEXT
        };
        let expected_units = expected.encode_utf16().count();
        let mut actual = [0_u16; MAX_SELECTED_UNITS];
        let document = editor.document().borrow();
        document
            .copy_utf16_code_units(self.range.start, &mut actual[..expected_units])
            .map_err(|_| "长文编辑有限结果范围不可读")?;
        // 奇数变化态保留可撤销输入；偶数恢复态只有经历过输入后才应允许重做。
        let expected_undo = !edits.is_multiple_of(2);
        let expected_redo = edits != 0 && edits.is_multiple_of(2);
        if !actual[..expected_units]
            .iter()
            .copied()
            .eq(expected.encode_utf16())
            || document.can_undo() != expected_undo
            || document.can_redo() != expected_redo
        {
            return Err("长文Unicode片段或真实单步历史方向与原预期不一致".into());
        }
        Ok(())
    }

    fn require_native_selection(
        &self,
        surface: &NativeMarkdownWindowSurface,
        edits: u64,
    ) -> Result<(), String> {
        self.check_state(surface, edits)?;
        let editor = surface.editor.as_ref().ok_or("长文编辑缺少Native正文")?;
        let paged = editor;
        let selection = self.selection_after(edits).ok_or("长文编辑选择溢出")?;
        let current = || self.check_state(surface, edits).is_ok();
        let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**paged.text_view);
        let actual = native_markdown_table_intent_step(&current, || input.selectedRange())
            .map_err(|_| "长文Native选择读取已撤权")?;
        if actual
            != NSRange::new(
                selection.anchor_utf16.min(selection.head_utf16),
                selection.anchor_utf16.abs_diff(selection.head_utf16),
            )
        {
            return Err("长文Native实际选择与原完整 canonical 选择不一致".into());
        }
        Ok(())
    }

    fn require_body_responder(
        &self,
        surface: &NativeMarkdownWindowSurface,
        edits: u64,
    ) -> Result<(), String> {
        let editor = surface.editor.as_ref().ok_or("长文焦点验证缺少正文")?;
        let paged = editor;
        let current = || self.check_state(surface, edits).is_ok();
        let responder =
            native_markdown_table_intent_step(&current, || surface.panel.firstResponder())
                .map_err(|_| "长文正文焦点读取已撤权")?
                .ok_or("长文编辑没有实际first responder")?;
        let matches = std::ptr::from_ref(&*responder).cast::<c_void>()
            == std::ptr::from_ref(&**paged.text_view).cast::<c_void>();
        drop(responder); // 临时Native claim释放后仍检查原阶段，不把框架焦点保存在验收票中。
        if !current() || !matches {
            return Err("长文输入没有沿真实正文responder执行".into());
        }
        Ok(())
    }

    fn insert(&self, surface: &NativeMarkdownWindowSurface) -> Result<(), String> {
        self.require_native_selection(surface, 0)?;
        let editor = surface.editor.as_ref().ok_or("Unicode输入缺少原正文")?;
        let paged = editor;
        let before = || self.check_state(surface, 0).is_ok();
        let focused = native_markdown_table_intent_step(&before, || {
            surface.panel.makeFirstResponder(Some(&**paged.text_view))
        })
        .map_err(|_| "Unicode正文聚焦期间原票已撤权")?;
        if !focused {
            return Err("Unicode输入无法取得真实正文焦点".into());
        }
        self.require_body_responder(surface, 0)?;
        // 调用原分页视图真实 NSTextInputClient；这里只验协议输入，不宣称系统输入法候选已经实测。
        let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**paged.text_view);
        let value =
            native_markdown_table_intent_step(&before, || NSString::from_str(INSERTED_TEXT))
                .map_err(|_| "Unicode有限输入对象创建期间已撤权")?;
        let transition =
            || self.check_state(surface, 0).is_ok() || self.check_state(surface, 1).is_ok();
        native_markdown_table_intent_step(&transition, || {
            // SAFETY: 同一主线程原正文、原完整合法选择和有限NSString；实际输入由生产NSTextInputClient提交。
            unsafe {
                input.insertText_replacementRange(
                    &value,
                    NSRange::new(self.range.start, self.range.len()),
                );
            }
        })
        .map_err(|_| "Unicode输入没有产生唯一声明的字符后态")?;
        drop(value);
        if let Some(cause) = paged.text_view.take_failure() {
            return Err(cause.to_string());
        }
        self.arm_frame(surface, 1)
    }

    fn history(
        &self,
        surface: &NativeMarkdownWindowSurface,
        before_edits: u64,
        redo: bool,
    ) -> Result<(), String> {
        self.require_native_selection(surface, before_edits)?;
        self.require_body_responder(surface, before_edits)?;
        let editor = surface.editor.as_ref().ok_or("长文历史缺少原正文")?;
        let paged = editor;
        // 原分页正文已经实现公开输入命令协议；历史沿同一个 Input/Doc，不直接改 Core。
        let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**paged.text_view);
        let after_edits = before_edits.checked_add(1).ok_or("长文历史次数溢出")?;
        let transition = || {
            self.check_state(surface, before_edits).is_ok()
                || self.check_state(surface, after_edits).is_ok()
        };
        native_markdown_table_intent_step(&transition, || {
            // SAFETY: 真实正文公开输入命令协议；只调用现有undo:/redo:，不添加按键绑定或直接修改Core。
            unsafe {
                input.doCommandBySelector(if redo { sel!(redo:) } else { sel!(undo:) });
            }
        })
        .map_err(|_| "长文历史没有产生唯一声明的原生后态")?;
        if let Some(cause) = paged.text_view.take_failure() {
            return Err(cause.to_string());
        }
        self.arm_frame(surface, after_edits)
    }

    fn verify_state(
        &self,
        surface: &NativeMarkdownWindowSurface,
        edits: u64,
        expected: Option<omni_markdown_core::MarkdownDocumentRevision>,
    ) -> Result<omni_markdown_core::MarkdownDocumentRevision, String> {
        let frame = self
            .frame
            .borrow()
            .clone()
            .ok_or("长文编辑结果缺少原帧终点")?;
        frame.completed_at(surface)?;
        self.require_native_selection(surface, edits)?;
        self.require_body_responder(surface, edits)?;
        let editor = surface.editor.as_ref().ok_or("长文结果验证缺少原正文")?;
        let revision = editor
            .document()
            .borrow()
            .current_revision()
            .map_err(|error| format!("无法捕获原文档revision：{error}"))?; // 明确验收边界流式hash，不构造全文String。
        if expected.is_some_and(|expected| expected != revision) {
            return Err("长文历史没有恢复预声明的完整原字节摘要".into());
        }
        self.check_state(surface, edits)?;
        Ok(revision)
    }

    fn check_saved_memory(&self, surface: &NativeMarkdownWindowSurface) -> Result<(), String> {
        self.check_state(surface, 4)?;
        let editor = surface.editor.as_ref().ok_or("恢复保存缺少原正文")?;
        let document = editor.document().borrow();
        if document.is_dirty()
            || document.original_revision() != Some(self.original.revision)
            || document
                .current_revision()
                .map_err(|error| format!("无法读取原文档revision：{error}"))?
                != self.original.revision
            || !document.original_file_identity_is_unchanged()
        {
            return Err("正式恢复保存没有建立原revision的真实干净磁盘基线".into());
        }
        Ok(())
    }

    fn verify_saved_disk(
        &self,
        surface: &NativeMarkdownWindowSurface,
        path: &Path,
        expected: DiskStamp,
    ) -> Result<(), String> {
        let frame = self
            .frame
            .borrow()
            .clone()
            .ok_or("长文恢复保存缺少原帧终点")?;
        frame.completed_at(surface)?;
        self.check_saved_memory(surface)?;
        if disk_stamp(path)? != expected {
            return Err("恢复保存后磁盘身份又发生变化".into());
        }
        let file = std::fs::File::open(path).map_err(|_| "无法流式核对恢复保存文件")?;
        if stamp(&file.metadata().map_err(|_| "无法核对恢复保存句柄")?) != expected {
            return Err("恢复保存路径与实际打开的文件句柄身份不同".into());
        }
        let mut comparison = DiskComparison {
            file,
            scratch: [0; 8 * 1_024],
        };
        let editor = surface.editor.as_ref().ok_or("恢复保存字节验证缺少正文")?;
        editor
            .document()
            .borrow()
            .write_to(&mut comparison)
            .map_err(|_| "恢复保存磁盘字节与已恢复的canonical流不一致")?;
        let mut tail = [0_u8; 1];
        if comparison
            .file
            .read(&mut tail)
            .map_err(|_| "恢复保存EOF读取失败")?
            != 0
            || stamp(
                &comparison
                    .file
                    .metadata()
                    .map_err(|_| "恢复保存尾部身份读取失败")?,
            ) != expected
            || disk_stamp(path)? != expected
        {
            return Err("恢复保存存在额外尾字节或流式校验期间文件已变更".into());
        }
        drop(comparison); // 磁盘句柄和固定scratch在结算收据前释放，不保活第二个文档或映射。
        self.check_saved_memory(surface)
    }
}

fn stamp(metadata: &std::fs::Metadata) -> DiskStamp {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
    )
}

fn disk_stamp(path: &Path) -> Result<DiskStamp, String> {
    std::fs::symlink_metadata(path)
        .map_err(|_| "无法核对恢复保存文件元数据".to_owned())
        .and_then(|metadata| {
            if !metadata.is_file() {
                return Err("恢复保存路径不是原普通文件".into());
            }
            Ok(stamp(&metadata))
        })
}

/// canonical每次write仍借原piece；独立磁盘只读入固定8KiB栈缓冲，不创建第二份全文或mmap。
struct DiskComparison {
    file: std::fs::File,
    scratch: [u8; 8 * 1_024],
}

impl Write for DiskComparison {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        for chunk in bytes.chunks(self.scratch.len()) {
            self.file.read_exact(&mut self.scratch[..chunk.len()])?;
            if self.scratch[..chunk.len()] != *chunk {
                // 首个不等片段立即失败；不给部分匹配或相同长度签发完整磁盘等同收据。
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "恢复保存流不一致",
                ));
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(()) // 此writer只读磁盘用于比较，没有待刷出的数据或写权限。
    }
}
