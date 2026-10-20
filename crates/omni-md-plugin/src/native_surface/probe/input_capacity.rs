//! 真实输入容量拒绝的隔离回归；只由显式诊断开关接入已有合成候选场景。

use super::*;

// 与 Native9 创建文档时的实际限制相同；不修改文档配置，也不使用故障注入。
const NATIVE9_DOCUMENT_MAX_BYTES: usize = 2 * 1024 * 1024;

/// 仅复制原事务的标量事实；revision 与原 capture 共同约束正文，不保存原文副本。
#[derive(Debug, Eq, PartialEq)]
struct CoreState {
    generation: u64,
    revision: omni_markdown_core::MarkdownDocumentRevision,
    original_revision: Option<omni_markdown_core::MarkdownDocumentRevision>,
    bytes: usize,
    units: usize,
    dirty: bool,
    undo: bool,
    redo: bool,
    selection: MarkdownTextSelection,
    known: Option<MarkdownTextSelection>,
    reserved: bool,
}

impl CoreState {
    fn read(surface: &NativeMarkdownTextKitEditorSurface) -> Result<Self, String> {
        let document = surface.document().borrow();
        // revision读取失败不能形成CoreState；调用方必须传播错误或撤销原权限。
        Ok(Self {
            generation: document.generation(),
            revision: document
                .current_revision()
                .map_err(|error| format!("无法读取原文档revision：{error}"))?,
            original_revision: document.original_revision(),
            bytes: document.byte_len(),
            units: document.utf16_len(),
            dirty: document.is_dirty(),
            undo: document.can_undo(),
            redo: document.can_redo(),
            selection: document.selection(),
            known: surface.backend.text_view.ivars().folds.known_selection(),
            reserved: document.has_prepared_text_input(),
        })
    }
}

/// 原票只允许此次输入入口推进一代；失败后不领取新 capture 来观察旧动作的后果。
struct Permission<'a> {
    surface: &'a NativeMarkdownTextKitEditorSurface,
    capture: NativeMarkdownSelectionCapture,
    document: WeakMarkdownDocument,
    before: CoreState,
    input_epoch: u64,
    next_input_epoch: u64,
}

impl Permission<'_> {
    fn current(&self) -> bool {
        let view = &self.surface.backend.text_view;
        let storage = &self.surface.backend.text_storage;
        let epoch = storage.ivars().input_transaction_epoch.get();
        self.capture.current()
            && !view.ivars().textkit_graph_teardown.get()
            && view.ivars().folds.owner_is_current()
            && storage.ivars().accepts_edits.get()
            && WeakMarkdownDocument::ptr_eq(&self.document, &Rc::downgrade(self.surface.document()))
            && (epoch == self.input_epoch || epoch == self.next_input_epoch)
            && CoreState::read(self.surface).is_ok_and(|actual| actual == self.before)
    }

    fn step<T>(&self, phase: &'static str, work: impl FnOnce() -> T) -> Result<T, String> {
        native_markdown_table_intent_step(&|| self.current(), work).map_err(|_| {
            format!(
                "容量拒绝的原输入权限已失效：阶段={phase} capture={} epoch={}/{} g={}/{}",
                self.capture.current(),
                self.surface
                    .backend
                    .text_storage
                    .ivars()
                    .input_transaction_epoch
                    .get(),
                self.input_epoch,
                self.surface.document().borrow().generation(),
                self.before.generation,
            )
        })
    }

    /// 必须是实际单范围；多选或未知范围不能由测试替换为伪造的主选择。
    fn ordered_range(&self) -> Result<NSRange, String> {
        let state = self
            .step("ordered_range", || {
                self.surface
                    .backend
                    .text_view
                    .native_ordered_selection_state_for_capture(&self.capture)
            })?
            .map_err(|error| error.to_string())?;
        match state {
            NativeMarkdownOrderedSelection::Single(range) => Ok(range),
            NativeMarkdownOrderedSelection::Multiple => Err("容量拒绝夹具出现实际多重选择".into()),
        }
    }
}

/// 输入 primitive、最终成功与 autosave 都观察实际入口计数，不能以空 timer 推测未排队。
fn input_counts(surface: &NativeMarkdownTextKitEditorSurface) -> [u64; 4] {
    let state = surface.backend.text_storage.ivars();
    [
        state.character_process_count.get(),
        state.input_commit_count.get(),
        state.input_callback_success_count.get(),
        state.autosave_enqueue_count.get(),
    ]
}

/// 只用于已经建立“中😀文”及相对 1..3 选择的 Native9 小场景；失败不修复测试现场。
pub(super) fn verify(surface: &NativeMarkdownTextKitEditorSurface) -> Result<(), String> {
    // 只有显式隔离场景调用本函数；标量阶段让超时诊断保留真实最后进度，不输出候选。
    eprintln!("omni_input_capacity phase=verify_enter bytes={NATIVE9_DOCUMENT_MAX_BYTES}");
    let capture = surface
        .backend
        .text_view
        .ivars()
        .folds
        .capture_selection()
        .map_err(|error| error.to_string())?;
    let epoch = surface
        .backend
        .text_storage
        .ivars()
        .input_transaction_epoch
        .get();
    let permission = Permission {
        surface,
        capture,
        document: Rc::downgrade(surface.document()),
        before: CoreState::read(surface)?,
        input_epoch: epoch,
        next_input_epoch: epoch.checked_add(1).ok_or("容量拒绝的输入代次耗尽")?,
    };
    if !permission.current()
        || surface
            .backend
            .text_storage
            .ivars()
            .input_transaction
            .borrow()
            .is_some()
        || !surface
            .backend
            .text_storage
            .ivars()
            .composition_active
            .get()
    {
        return Err("容量拒绝测试没有独占原小候选输入边界".into());
    }

    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(surface.backend.text_view());
    let marked = permission.step("marked_range_before", || client.markedRange())?;
    let marked_flag = permission.step("has_marked_before", || client.hasMarkedText())?;
    let native_marked = permission.step("super_marked_before", || {
        surface.backend.text_view.native_marked_state_for_probe()
    })?;
    let ordered = permission.ordered_range()?;
    let relative_start = marked
        .location
        .checked_add(1)
        .ok_or("容量拒绝候选起点溢出")?;
    let relative_end = marked
        .location
        .checked_add(3)
        .ok_or("容量拒绝候选终点溢出")?;
    let expected_selection = MarkdownTextSelection {
        anchor_utf16: relative_start,
        head_utf16: relative_end,
    };
    if !marked_flag
        || marked.length != 4
        || native_marked != (true, marked)
        || ordered != NSRange::new(relative_start, 2)
        || permission.before.selection != expected_selection
        || permission.before.known != Some(expected_selection)
    {
        return Err(format!(
            "容量拒绝测试的小候选假设不成立：marked={marked:?} native={native_marked:?} ordered={ordered:?}"
        ));
    }
    let mut literal = [0_u16; 4];
    surface
        .document()
        .borrow()
        .copy_utf16_code_units(marked.location, &mut literal)
        .map_err(|error| error.to_string())?;
    if literal != [0x4e2d, 0xd83d, 0xde00, 0x6587] {
        return Err("容量拒绝测试的四个原 UTF16 单元不是既有合成候选".into());
    }

    // 已知候选为十字节，2MiB ASCII 替换后仍保留其他正文，必越过真实文档容量。
    let resulting_bytes = permission
        .before
        .bytes
        .checked_sub("中😀文".len())
        .and_then(|remaining| remaining.checked_add(NATIVE9_DOCUMENT_MAX_BYTES))
        .ok_or("容量拒绝测试的结果字节数溢出")?;
    if permission.before.bytes > NATIVE9_DOCUMENT_MAX_BYTES
        || resulting_bytes <= NATIVE9_DOCUMENT_MAX_BYTES
        || NATIVE9_DOCUMENT_MAX_BYTES >= NATIVE_MARKDOWN_INPUT_TRANSACTION_MAX_BYTES
    {
        return Err(format!(
            "容量拒绝测试未构成真实上限反例：before={} after={resulting_bytes}",
            permission.before.bytes
        ));
    }
    let counts = input_counts(surface);
    let attempt = autoreleasepool(|_| {
        // 此副本只含新增合成候选，远低于输入副本限额；不读取、拼接或镜像正文。
        let candidate_text = "x".repeat(NATIVE9_DOCUMENT_MAX_BYTES);
        eprintln!(
            "omni_input_capacity phase=candidate_rust_ready bytes={}",
            candidate_text.len()
        );
        let candidate =
            permission.step("candidate_create", || NSString::from_str(&candidate_text))?;
        eprintln!(
            "omni_input_capacity phase=before_public_set_marked counts={:?}",
            input_counts(surface)
        );
        let result = permission.step("set_marked_capacity", || {
            // SAFETY: ASCII 候选的相对 caret 1 合法；显式范围覆盖原四个完整 UTF16 单元。
            // 只调用当前产品视图的公开输入协议；Core 必须在真实 primitive 内拒绝容量超限。
            unsafe {
                client.setMarkedText_selectedRange_replacementRange(
                    &candidate,
                    NSRange::new(1, 0),
                    marked,
                );
            }
        });
        eprintln!(
            "omni_input_capacity phase=after_public_set_marked accepted_permission={} counts={:?}",
            result.is_ok(),
            input_counts(surface)
        );
        drop(candidate); // Native claim 和 Rust 候选都在此局部池结束前释放。
        drop(candidate_text);
        result
    });
    eprintln!(
        "omni_input_capacity phase=after_candidate_pool counts={:?}",
        input_counts(surface)
    );

    // 先核池 drain 后的原权限，再读取 Native；一旦改根或撤票，不领新票继续检查。
    let after = CoreState::read(surface)?;
    let counts_after = input_counts(surface);
    let original_capture_current = permission.capture.current();
    if !permission.current() {
        return Err(format!(
            "容量拒绝没有保留原 Core 输入：capture={original_capture_current} core_same={} counts={counts:?}/{counts_after:?} before={:?} after={after:?}",
            permission.before == after,
            permission.before,
        ));
    }
    attempt?;
    let marked_after = permission.step("marked_range_after", || client.markedRange())?;
    let marked_flag_after = permission.step("has_marked_after", || client.hasMarkedText())?;
    let native_marked_after = permission.step("super_marked_after", || {
        surface.backend.text_view.native_marked_state_for_probe()
    })?;
    let ordered_after = permission.ordered_range()?;
    let mut literal_after = [0_u16; 4];
    surface
        .document()
        .borrow()
        .copy_utf16_code_units(marked.location, &mut literal_after)
        .map_err(|error| error.to_string())?;
    let slot_empty = surface
        .backend
        .text_storage
        .ivars()
        .input_transaction
        .borrow()
        .is_none();
    let callback_empty = surface
        .backend
        .text_view
        .ivars()
        .input_callback
        .borrow()
        .is_none();
    let composing = surface
        .backend
        .text_storage
        .ivars()
        .composition_active
        .get();
    let actual_epoch = surface
        .backend
        .text_storage
        .ivars()
        .input_transaction_epoch
        .get();
    // getter 的临时 claims 已结束后再取最终计数；前一份快照不能掩盖观察期间的新通知。
    let final_counts = input_counts(surface);
    if after != permission.before
        || counts_after != counts
        || final_counts != counts
        || marked_after != marked
        || !marked_flag_after
        || native_marked_after != native_marked
        || ordered_after != ordered
        || literal_after != literal
        || !slot_empty
        || !callback_empty
        || !composing
        || !original_capture_current
        || actual_epoch != permission.next_input_epoch
        || !permission.current()
    {
        return Err(format!(
            "容量拒绝改变了原组合：core_same={} counts={counts:?}/{counts_after:?}/{final_counts:?} marked={marked:?}/{marked_after:?} super={native_marked:?}/{native_marked_after:?} ordered={ordered:?}/{ordered_after:?} literal_same={} slot_empty={slot_empty} callback_empty={callback_empty} composing={composing} capture={original_capture_current} epoch={actual_epoch}/{}",
            after == permission.before,
            literal_after == literal,
            permission.next_input_epoch,
        ));
    }
    Ok(())
}
