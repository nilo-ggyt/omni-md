//! 真实插表工具栏的一次提交与撤销；仅核有限新表源，不复制原正文或另建表格算法。

use super::*;

mod menu_cancellation;
pub(in crate::native_surface) use menu_cancellation::{
    finish_cancelled_menu, supersede_menu_with_select_all,
};

/// 跨菜单排队事件只保存原 Weak 身份、有限标量及小表候选，不保活 surface、正文 root 或 Native 图。
pub(in crate::native_surface) struct NativeMarkdownToolbarProbe {
    document: WeakMarkdownDocument,
    generation: u64,
    revision: omni_markdown_core::MarkdownDocumentRevision,
    bytes: usize,
    units: usize,
    dirty: bool,
    undo: bool,
    redo: bool,
    saved_selection: MarkdownTextSelection,
    counts: [u64; 4],
    table: String,
    table_units: usize,
    finished: Cell<bool>,
    activation: Option<Rc<NativeMarkdownTableInsertionObservation>>,
    cancelled_menu: Option<menu_cancellation::CancelledMenuSelection>,
}

/// 首 Native 前固定唯一字符前/后态；与产品提交收据分离，只用于观察该合成动作是否完成。
struct ToolbarPermission<'a> {
    surface: &'a NativeMarkdownEditorSurface,
    document: WeakMarkdownDocument,
    generation: u64,
    bytes: usize,
    units: usize,
    added_bytes: usize,
    added_units: usize,
}

/// borrowed 实现检查不能冒充 main loop 已交付；真实菜单场景必须验证正常队列建立的字段终点。
#[derive(Clone, Copy)]
enum ToolbarCompletion {
    Submission,
    ActivatedField,
}

impl ToolbarPermission<'_> {
    fn current(&self) -> bool {
        if self
            .surface
            .backend
            .text_view
            .ivars()
            .textkit_graph_teardown
            .get()
            || !WeakMarkdownDocument::ptr_eq(
                &self.document,
                &Rc::downgrade(self.surface.document()),
            )
        {
            return false; // 同内容的替换文档仍不是原工具栏动作的正文。
        }
        let document = self.surface.document().borrow();
        (document.generation() == self.generation
            && document.byte_len() == self.bytes
            && document.utf16_len() == self.units
            && document.selection() == MarkdownTextSelection::caret(0))
            || (document.generation() == self.generation + 1
                && document.byte_len() == self.bytes + self.added_bytes
                && document.utf16_len() == self.units + self.added_units
                && document.selection() == MarkdownTextSelection::caret(2))
    }

    fn step<T>(&self, work: impl FnOnce() -> T) -> Result<T, String> {
        native_markdown_table_intent_step(&|| self.current(), work)
            .map_err(|_| "插表验收的原唯一字符前/后态已撤销".to_owned())
    }

    /// 实际 superclass count 与 range 同时匹配；Known 不能替代 Native 安装证据。
    fn require_actual_first_cell(&self) -> Result<(), String> {
        let view = &self.surface.backend.text_view;
        let (present, count, raw) = self.step(|| {
            autoreleasepool(|_| {
                let ranges: Option<Retained<NSArray<objc2_foundation::NSValue>>> =
                    self.step(|| {
                        // SAFETY: 同一存活主线程正文的公开 nullable getter；不读取任何正文副本。
                        unsafe { msg_send![super(&**view), selectedRanges] }
                    })?;
                let present = ranges.is_some();
                let count = match &ranges {
                    Some(ranges) => self.step(|| ranges.count())?,
                    None => 0, // nil 不等同合法空选择，也不允许由 semantic getter 合成成功。
                };
                let raw = self.step(|| view.super_selected_range())?;
                drop(ranges); // 原动作后检查覆盖最后 claim 和 pool drain。
                Ok::<_, String>((present, count, raw))
            })
        })??;
        if !present
            || count != 1
            || raw != NSRange::new(2, 0)
            || view.ivars().folds.known_selection() != Some(MarkdownTextSelection::caret(2))
        {
            return Err(format!(
                "插表首格未真实安装：present={present} count={count} raw={raw:?} known={:?}",
                view.ivars().folds.known_selection()
            ));
        }
        Ok(())
    }

    /// 只观察正常队列已完成的原字段；Progress 不由测试补跑 executor 或 nested loop 推进。
    fn activate_and_cancel_first_cell(
        &self,
        target: NativeMarkdownTableCellCoordinate,
    ) -> Result<(), String> {
        let view = &self.surface.backend.text_view;
        let active = view
            .ivars()
            .active_table_cell
            .borrow()
            .as_ref()
            .map(|active| {
                (
                    active.coordinate,
                    active.source_utf16_range.clone(),
                    active.original_source == "列 1",
                )
            });
        if active != Some((target, 2..5, true))
            || !view.ivars().pending_table_cell_activation.is_empty()
            || view.ivars().table_layout_update_scheduled.get()
        {
            return Err(format!(
                "正常插表队列尚未完成原首格终点：active={active:?} pending={:?} scheduled={}",
                view.ivars().pending_table_cell_activation.get(),
                view.ivars().table_layout_update_scheduled.get()
            ));
        }
        self.step(|| {
            autoreleasepool(|_| {
                let field = self
                    .step(|| view.existing_table_cell_editor())?
                    .ok_or("插表首格未创建实际字段")?;
                let hidden = self.step(|| field.isHidden())?;
                let text = self.step(|| field.stringValue())?;
                let input = self
                    .step(|| field.currentEditor())?
                    .ok_or("插表首格没有实际字段编辑关系")?;
                let selection = self.step(|| input.selectedRange())?;
                let text_matches = self.step(|| text.to_string() == "列 1")?;
                if hidden || !text_matches || selection != NSRange::new(0, 3) {
                    return Err(format!(
                        "插表首格实际字段未匹配原目标：hidden={hidden} selection={selection:?}"
                    ));
                }
                drop(input);
                drop(text);
                // 只调用正常取消入口；它负责撤销原请求、辞任字段及清空复用草稿。
                self.step(|| view.cancel_active_table_cell())?;
                let hidden = self.step(|| field.isHidden())?;
                let text = self.step(|| field.stringValue())?;
                let empty = self.step(|| text.length() == 0)?;
                if !hidden
                    || !empty
                    || view.ivars().active_table_cell.borrow().is_some()
                    || !view.ivars().pending_table_cell_activation.is_empty()
                {
                    return Err("插表首格正常取消后仍持草稿或活动请求".into());
                }
                drop(text);
                drop(field); // 原动作的后检查还覆盖字段 claims 析构与 pool drain。
                Ok::<_, String>(())
            })
        })??;
        self.require_actual_first_cell()?;
        Ok(())
    }
}

fn counts(surface: &NativeMarkdownEditorSurface) -> [u64; 4] {
    let state = surface.backend.text_storage.ivars();
    [
        state.character_process_count.get(),
        state.input_commit_count.get(),
        state.input_callback_success_count.get(),
        state.autosave_enqueue_count.get(),
    ]
}

/// 准备阶段先建立合法正文 caret，再把原动作有限前态交给下一次真实菜单事件。
pub(in crate::native_surface) fn prepare(
    surface: &NativeMarkdownEditorSurface,
) -> Result<NativeMarkdownToolbarProbe, String> {
    if surface
        .backend
        .text_view
        .ivars()
        .active_table_cell
        .borrow()
        .is_some()
        || !surface
            .backend
            .text_view
            .ivars()
            .pending_table_cell_activation
            .is_empty()
    {
        return Err("插表准备不能接管尚未完成的单元格输入或原行请求".into());
    }
    let saved = {
        let document = surface.document().borrow();
        let selection = document.selection();
        // 初始实际 Native 选择可能仍是 EOF，不能用未获认证的 head 领取原 full-selection 证据。
        // 这里只保存 canonical 完整选择作为恢复时的新明确目标；两端必须仍是原文档的 scalar 边界。
        document
            .byte_offset_for_utf16(selection.anchor_utf16)
            .map_err(|error| error.to_string())?;
        document
            .byte_offset_for_utf16(selection.head_utf16)
            .map_err(|error| error.to_string())?;
        selection
    };
    // 这个新的显式产品意图自行观察实际 before；不能把上面的纯 Core 目标冒充已验证 Native 方向。
    surface
        .backend
        .text_view
        .set_semantic_native_selection(MarkdownTextSelection::caret(0));
    let initial = surface
        .backend
        .text_view
        .primary_text_selection()
        .map_err(|error| error.to_string())?;
    let actual = surface
        .backend
        .text_view
        .native_ordered_selection_state_for_capture(initial.native_capture())
        .map_err(|error| error.to_string())?;
    if initial.selection != MarkdownTextSelection::caret(0)
        || !initial.current()
        || actual != NativeMarkdownOrderedSelection::Single(NSRange::new(0, 0))
        || surface.backend.text_view.ivars().folds.known_selection()
            != Some(MarkdownTextSelection::caret(0))
    {
        return Err("插表验收未建立原合法正文选择".into());
    }
    let (generation, revision, bytes, units, dirty, undo, redo, suffix) = {
        let document = surface.document().borrow();
        let suffix = document.utf16_len() != 0
            && !document
                .utf16_code_unit_at(0)
                .is_ok_and(|unit| unit == b'\n' as u16 || unit == b'\r' as u16);
        (
            document.generation(),
            document
                .current_revision()
                .map_err(|error| format!("无法读取原文档revision：{error}"))?,
            document.byte_len(),
            document.utf16_len(),
            document.is_dirty(),
            document.can_undo(),
            document.can_redo(),
            suffix,
        )
    };
    drop(initial); // 公开工具栏动作自己捕获原票；测试不把旧 capture 续签成提交后显示权限。
    let table = native_markdown_default_table_source(false, suffix);
    let table_units = table.encode_utf16().count();
    if generation.checked_add(2).is_none()
        || bytes.checked_add(table.len()).is_none()
        || units.checked_add(table_units).is_none()
    {
        return Err("插表验收的有限后态计量溢出".into());
    }
    Ok(NativeMarkdownToolbarProbe {
        document: Rc::downgrade(surface.document()),
        generation,
        revision,
        bytes,
        units,
        dirty,
        undo,
        redo,
        saved_selection: saved,
        counts: counts(surface),
        table,
        table_units,
        finished: Cell::new(false),
        activation: None,
        cancelled_menu: None,
    })
}

/// 菜单执行前只预签观察原字符后果；真实 fold 和 request Rc 在 product 绑定点取得，不发新激活。
pub(in crate::native_surface) fn observe_menu_activation(
    surface: &NativeMarkdownEditorSurface,
    state: &mut NativeMarkdownToolbarProbe,
    callback: fn(NativeMarkdownTableCellOutcome),
) -> Result<(), String> {
    if state.activation.is_some() {
        return Err("菜单插表原观察已签发".into());
    }
    let observation = surface
        .backend
        .text_view
        .probe_prepare_table_insertion_observation(
            surface.document(),
            state.table_units,
            NativeMarkdownTableCellCoordinate {
                table_utf16_start: 0,
                row_index: 0,
                column_index: 0,
            },
            MarkdownTextSelection::caret(2),
            callback,
        )?;
    state.activation = Some(observation);
    Ok(())
}

/// 原菜单 action 在正常事件中提交后才调用；每份纯状态最多消费一次，不允许失败后重新核发完成权限。
fn finish_with_completion(
    surface: &NativeMarkdownEditorSurface,
    state: &NativeMarkdownToolbarProbe,
    trigger: &'static str,
    completion: ToolbarCompletion,
) -> Result<(), String> {
    if state.finished.replace(true) {
        return Err("原插表验收状态已消费，不能再次完成或撤销".into());
    }
    if matches!(completion, ToolbarCompletion::ActivatedField) {
        let observation = state.activation.as_ref().ok_or("菜单插表缺少原请求观察")?;
        if observation.outcome() != Some(NativeMarkdownTableCellOutcome::Activated)
            || !observation.current_for(&surface.backend.text_view)
        {
            return Err(format!(
                "菜单插表原请求没有真实激活终点：outcome={:?}",
                observation.outcome()
            ));
        }
    }
    let permission = ToolbarPermission {
        surface,
        document: state.document.clone(),
        generation: state.generation,
        bytes: state.bytes,
        units: state.units,
        added_bytes: state.table.len(),
        added_units: state.table_units,
    };
    if !permission.current() {
        return Err("菜单插表完成前原正文身份或唯一字符后态已变更".into());
    }
    permission.require_actual_first_cell()?;
    let before = state.counts;
    let after = counts(surface);
    let inserted = {
        let document = surface.document().borrow();
        document.generation() == state.generation + 1
            && document.byte_len() == state.bytes + state.table.len()
            && document.utf16_len() == state.units + state.table_units
            && document.can_undo()
            && !document.can_redo()
            && !document.has_prepared_text_input()
            && state
                .table
                .encode_utf16()
                .enumerate()
                .all(|(index, expected)| {
                    document
                        .utf16_code_unit_at(index)
                        .is_ok_and(|actual| actual == expected)
                })
    };
    let target = NativeMarkdownTableCellCoordinate {
        table_utf16_start: 0,
        row_index: 0,
        column_index: 0,
    };
    let pending = surface
        .backend
        .text_view
        .ivars()
        .pending_table_cell_activation
        .get()
        == Some(target)
        && surface
            .backend
            .text_view
            .ivars()
            .pending_table_cell_activation
            .request()
            .is_some();
    let activated = surface
        .backend
        .text_view
        .ivars()
        .active_table_cell
        .borrow()
        .as_ref()
        .is_some_and(|active| {
            active.coordinate == target
                && active.source_utf16_range == (2..5)
                && active.original_source == "列 1"
        });
    if !inserted
        || (!pending && !activated)
        || after[0] <= before[0]
        || after[1] != before[1]
        || after[2] != before[2]
        || after[3] != before[3] + 1
    {
        return Err(format!(
            "真实插表未完成一次提交与原首格请求：inserted={inserted} counts={before:?}/{after:?} target={:?}",
            surface
                .backend
                .text_view
                .ivars()
                .pending_table_cell_activation
                .get()
        ));
    }
    let activation = match completion {
        ToolbarCompletion::ActivatedField => {
            // ACTIVE 菜单场景已交还 main loop；字段、草稿、完整选择与正常结束必须全部真实成立。
            permission.activate_and_cancel_first_cell(target)?;
            "actual_field"
        }
        ToolbarCompletion::Submission => {
            // borrowed 实现检查尚未交还事件循环，只证明唯一提交和明确 after；不声称请求已激活。
            // 正常取消入口撤原请求，随后 Undo 才是新的独立意图；不得手清 Ready 或强跑排队任务。
            permission.step(|| surface.backend.text_view.cancel_active_table_cell())?;
            if surface
                .backend
                .text_view
                .ivars()
                .active_table_cell
                .borrow()
                .is_some()
                || !surface
                    .backend
                    .text_view
                    .ivars()
                    .pending_table_cell_activation
                    .is_empty()
            {
                return Err("插表实现检查的原请求未由正常取消入口撤销".into());
            }
            permission.require_actual_first_cell()?;
            "submission_only"
        }
    };
    if counts(surface) != after {
        return Err("插表首格激活或正常取消额外提交了字符或 autosave".into());
    }
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(surface.backend.text_view());
    // SAFETY: 同一产品 undo: 一次恢复刚才的唯一插表；没有直接修改 Core 或手工发 TextKit 通知。
    unsafe {
        client.doCommandBySelector(sel!(undo:));
    }
    let restored = {
        let document = surface.document().borrow();
        WeakMarkdownDocument::ptr_eq(&state.document, &Rc::downgrade(surface.document()))
            && document.generation() == state.generation + 2
            && document
                .current_revision()
                .is_ok_and(|actual| actual == state.revision)
            && document.byte_len() == state.bytes
            && document.utf16_len() == state.units
            && document.is_dirty() == state.dirty
            && document.can_undo() == state.undo
            && document.can_redo()
            && !document.has_prepared_text_input()
    };
    let undo_counts = counts(surface);
    let selected = surface
        .backend
        .text_view
        .primary_text_selection()
        .map_err(|error| error.to_string())?;
    let actual = surface
        .backend
        .text_view
        .native_ordered_selection_state_for_capture(selected.native_capture())
        .map_err(|error| error.to_string())?;
    if !restored
        || selected.selection != MarkdownTextSelection::caret(0)
        || !selected.current()
        || surface.backend.text_view.ivars().folds.known_selection()
            != Some(MarkdownTextSelection::caret(0))
        || actual != NativeMarkdownOrderedSelection::Single(NSRange::new(0, 0))
        || !surface
            .backend
            .text_view
            .ivars()
            .pending_table_cell_activation
            .is_empty()
        || undo_counts[1] != after[1]
        || undo_counts[2] != after[2]
        || undo_counts[3] != after[3] + 1
    {
        return Err(format!(
            "真实插表 Undo 未恢复原 root 或原请求未撤销：restored={restored} counts={after:?}/{undo_counts:?}"
        ));
    }
    drop(selected);
    surface
        .backend
        .text_view
        .set_semantic_native_selection(state.saved_selection);
    let selected = surface
        .backend
        .text_view
        .primary_text_selection()
        .map_err(|error| error.to_string())?;
    let actual = surface
        .backend
        .text_view
        .native_ordered_selection_state_for_capture(selected.native_capture())
        .map_err(|error| error.to_string())?;
    let saved_range = NSRange::new(
        state
            .saved_selection
            .anchor_utf16
            .min(state.saved_selection.head_utf16),
        state
            .saved_selection
            .anchor_utf16
            .abs_diff(state.saved_selection.head_utf16),
    );
    if selected.selection != state.saved_selection
        || !selected.current()
        || actual != NativeMarkdownOrderedSelection::Single(saved_range)
        || surface.backend.text_view.ivars().folds.known_selection() != Some(state.saved_selection)
    {
        return Err("插表验收结束未恢复原合法完整选择".into());
    }
    eprintln!(
        "omni_toolbar_insert restored=true generation_steps=2 history_steps=1 first_cell_utf16=2 activation={activation} executor_count=0 trigger={trigger} counts={before:?}/{after:?}/{undo_counts:?}"
    );
    Ok(())
}

/// 真实 foreground 菜单必须完整通过字段终点；不能以 borrowed 的提交检查替代功能验收。
pub(in crate::native_surface) fn finish(
    surface: &NativeMarkdownEditorSurface,
    state: &NativeMarkdownToolbarProbe,
    trigger: &'static str,
) -> Result<(), String> {
    finish_with_completion(surface, state, trigger, ToolbarCompletion::ActivatedField)
}

/// 独立 Native9 继续覆盖实现入口；正式菜单绑定由真实 foreground case 跨事件调用 prepare/finish。
pub(super) fn verify(surface: &NativeMarkdownEditorSurface) -> Result<(), String> {
    let state = prepare(surface)?;
    let permission = ToolbarPermission {
        surface,
        document: state.document.clone(),
        generation: state.generation,
        bytes: state.bytes,
        units: state.units,
        added_bytes: state.table.len(),
        added_units: state.table_units,
    };
    permission.step(|| surface.backend.text_view.insert_default_markdown_table())?;
    finish_with_completion(
        surface,
        &state,
        "view_implementation",
        ToolbarCompletion::Submission,
    )
}
