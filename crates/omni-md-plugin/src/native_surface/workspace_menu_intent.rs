//! 正文菜单只消费点击时的原文档与真实正文或格内选择；排队期间的新输入撤销原动作。

use super::*;
use objc2_foundation::NSUndoManager;

/// 队列只转运 Rust 票据、正文/字段 weak 和标量，不保活 Native 图或另一个正文 root。
pub(super) struct NativeMarkdownWorkspaceEditIntent {
    owner: usize,
    view_identity: usize,
    document: WeakMarkdownDocument,
    source: NativeMarkdownSelectionCapture,
    input_epoch: u64,
    table: Option<NativeMarkdownTableMenuSelection>,
    committed: RefCell<Option<Rc<NativeMarkdownCommittedSelection>>>,
}

/// 格内坐标属于同一实际字段会话；weak 不把 AppKit 的共享 field editor 保活到队列之后。
struct NativeMarkdownTableMenuSelection {
    coordinate: NativeMarkdownTableCellCoordinate,
    source_range: std::ops::Range<usize>,
    authority_identity: usize,
    selection: Option<(Weak<NSTextView>, NSRange)>,
    history: Option<Weak<NSUndoManager>>,
}

impl NativeMarkdownWorkspaceEditIntent {
    /// 必须先于 sender 的第一个 Native getter 捕获；此过程不改变选择或输入代次。
    pub(super) fn capture(controller: &NativeMarkdownController) -> Option<Self> {
        let (mut intent, table) = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
            let surface = active.as_ref()?;
            if !std::ptr::eq(&*surface.controller, controller)
                || !native_markdown_controller_has_foreground_authority(controller)
                || native_markdown_operation_is_busy(
                    surface.pending_note_switch.is_some(),
                    surface.pending_file_mutation.is_some(),
                )
            {
                // 旧窗口、借出或交接中的菜单不能等到新正文出现后再领取编辑权限。
                return None;
            }
            let editor = surface.editor.as_ref()?; // 空态不能把点击时不存在的正文授权给后继文档。
            let view = &editor.backend.text_view;
            if view.ivars().textkit_graph_teardown.get() {
                return None; // accepted 清理后的空壳不再产生排队正文动作。
            }
            let table = view
                .ivars()
                .active_table_cell
                .borrow()
                .as_ref()
                .map(|cell| {
                    (
                        cell.coordinate,
                        cell.source_utf16_range.clone(),
                        cell.authority
                            .as_ref()
                            .map(|authority| Rc::as_ptr(authority).addr()),
                    )
                }); // 只投影坐标和原 authority 身份，不复制单格草稿或跨 Native 持状态借用。
            Some((
                Self {
                    owner: std::ptr::from_ref(controller).addr(),
                    view_identity: std::ptr::from_ref(&**view).addr(),
                    document: Rc::downgrade(editor.document()),
                    source: view.ivars().folds.capture_selection().ok()?, // 失效原 owner 不以新 capture 重试。
                    input_epoch: view.ivars().table_input_intent_epoch.get(),
                    table: None,
                    committed: RefCell::new(None),
                },
                table,
            ))
        })?;
        match table {
            Some((coordinate, source_range, Some(authority_identity))) => {
                // 原纯票先于任何 Native getter；离开 surface 借用后才观察同一现存字段。
                let view = active_document_view().ok()?;
                let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
                let current = || intent.current();
                let selection = (|| {
                    let field = native_markdown_table_intent_step(&current, || {
                        view.existing_table_cell_editor()
                    })
                    .ok()??;
                    let editor =
                        native_markdown_table_intent_step(&current, || field.currentEditor())
                            .ok()??
                            .downcast::<NSTextView>()
                            .ok()?;
                    let selection =
                        native_markdown_table_intent_step(&current, || editor.selectedRange())
                            .ok()?;
                    if selection.location == NSNotFound as NSUInteger
                        || selection.location.checked_add(selection.length).is_none()
                    {
                        return None; // 无真实选区只撤格内格式权限；插表/正文块仍有各自原提交语义。
                    }
                    let weak = native_markdown_table_intent_step(&current, || {
                        Weak::from_retained(&editor)
                    })
                    .ok()?;
                    Some((weak, selection))
                })(); // 不保活 field/editor；全部局部析构后仍核同一原菜单票。
                if !intent.current() {
                    return None;
                }
                let history = if selection.is_some() {
                    // 历史按钮只能使用点击时已有的原字段manager；格式菜单的只读捕获也不创建新历史。
                    let manager = native_markdown_table_intent_step(&current, || {
                        controller
                            .ivars()
                            .undo
                            .cached_manager(&controller.ivars().owned)
                    })
                    .ok()?;
                    match manager {
                        Some(manager) => {
                            let weak = native_markdown_table_intent_step(&current, || {
                                Weak::from_retained(&manager)
                            })
                            .ok()?;
                            drop(manager); // 队列只留Weak，原claim的release也不能跨越已撤销的原票。
                            if !intent.current() {
                                return None;
                            }
                            Some(weak)
                        }
                        None => None, // 尚无字段历史时按钮是原生空操作，不借正文历史替代。
                    }
                } else {
                    None // 原字段已辞任时，没有可授权的字段历史客户端。
                };
                intent.table = Some(NativeMarkdownTableMenuSelection {
                    coordinate,
                    source_range,
                    authority_identity,
                    selection,
                    history,
                });
            }
            Some((_, _, None)) => return None, // 活动格缺少已验证源权限时保留草稿，不误作正文菜单。
            None => {}                         // 原正文菜单不创建或查询任何表格字段。
        }
        Some(intent)
    }

    /// 活动格只消费其原生输入历史；没有活动field editor时才走原正文canonical历史。
    pub(super) fn restore_history(
        &self,
        direction: MarkdownHistoryDirection,
    ) -> Result<(), String> {
        if !self.current() {
            return Ok(()); // 后继正文或输入撤销旧按钮，不重新捕获另一个字段作为目标。
        }
        let view = active_document_view()?;
        let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
        let Some(table) = self
            .table
            .as_ref()
            .filter(|table| table.selection.is_some())
        else {
            if self.current() {
                view.restore_history(direction); // 正文继续原Cell/IME提交、唯一历史与选择收据流程。
            }
            return Ok(());
        };
        let Some(history) = &table.history else {
            return Ok(()); // 捕获时本格没有历史，不能查询后来的manager或改为撤销正文。
        };
        let expired = || "表格输入或其撤销记录已经改变，本次操作已取消".to_owned();
        let current = || self.current() && self.table_input_current(&view);
        let (editor, _) = table.selection.as_ref().expect("上方已限定原实际字段选择");
        let editor = native_markdown_table_intent_step(&current, || editor.load())
            .map_err(|_| expired())?
            .ok_or_else(expired)?;
        let manager = native_markdown_table_intent_step(&current, || history.load())
            .map_err(|_| expired())?
            .ok_or_else(expired)?;
        let current = || {
            current()
                && ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
                    let Some(surface) = active.as_ref() else {
                        return false;
                    };
                    // manager必须仍属于同一controller；原具体field editor的Weak能力也须仍指向它。
                    surface.controller.ivars().undo.preflight().is_ok()
                        && surface
                            .controller
                            .ivars()
                            .undo
                            .owns_observed_manager(&manager)
                        && editor
                            .downcast_ref::<NativeMarkdownSessionFieldEditor>()
                            .is_some_and(|editor| {
                                editor.belongs_to(
                                    &surface.controller.ivars().undo,
                                    &surface.controller.ivars().owned,
                                )
                            })
                })
        };
        let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*editor);
        if native_markdown_table_intent_step(&current, || input.hasMarkedText())
            .map_err(|_| expired())?
        {
            return Err("请先完成当前表格单元格的输入，再撤销或重做".into()); // 不强制确认或取消中文组合输入。
        }
        let actual = native_markdown_table_intent_step(&current, || editor.undoManager())
            .map_err(|_| expired())?
            .ok_or_else(expired)?;
        if !std::ptr::eq(&*actual, &*manager) {
            return Err(expired()); // 已缓存原manager存在才调用getter，拒绝外部或被换掉的实际历史。
        }
        let busy = native_markdown_table_intent_step(&current, || {
            manager.isUndoing() || manager.isRedoing() || manager.groupingLevel() > 1
        })
        .map_err(|_| expired())?;
        if busy {
            return Err("表格单元格的撤销或重做仍在完成中".into()); // 不手工结束嵌套组或插入第二个历史动作。
        }
        let available = native_markdown_table_intent_step(&current, || match direction {
            MarkdownHistoryDirection::Undo => manager.canUndo(),
            MarkdownHistoryDirection::Redo => manager.canRedo(),
        })
        .map_err(|_| expired())?;
        if !available {
            return Ok(()); // 本字段空栈仍保留编辑与焦点，不能降为正文Undo/Redo。
        }
        if !current() {
            return Err(expired()); // canUndo/canRedo会触发checkpoint，执行前仍须核原选区与客户。
        }
        match direction {
            MarkdownHistoryDirection::Undo => manager.undo(),
            MarkdownHistoryDirection::Redo => manager.redo(),
        }
        // 原生动作自行发布真实字段变化及选择；它可推进输入epoch，不以旧before复核误报已完成的操作。
        Ok(())
    }

    pub(super) fn has_table_selection(&self) -> bool {
        self.table.is_some() // 活动格是原菜单目标；选区不可得也不能把行内格式降为正文命令。
    }

    /// 同一单格提交消费原正文 before；不在菜单 getter 或 IME 确认之后重新 capture。
    pub(super) fn source_capture(&self) -> NativeMarkdownSelectionCapture {
        self.source.clone()
    }

    /// 一次 canonical 提交的 pre-notify 收据续接同一菜单；后续 Native 仅检查这份 after。
    pub(super) fn accept_table_commit(&self, receipt: &Rc<NativeMarkdownCommittedSelection>) {
        *self.committed.borrow_mut() = Some(Rc::clone(receipt));
    }

    /// 原生重入可能只移动格内选区而不改文本；每个提交前步骤仍核原实际字段。
    pub(super) fn table_input_current(&self, view: &PieceBackedMarkdownTextView) -> bool {
        if self.committed.borrow().is_some()
            && view
                .ivars()
                .active_table_cell
                .borrow()
                .as_ref()
                .is_none_or(|cell| cell.authority.is_none())
        {
            // 本次字符提交已撤旧格 authority，或字段已正常结束，才不再读取旧格。
            // 同代 body 多选收拢仍保留原 authority，后续步骤继续核真实 field 子选区。
            return self.current();
        }
        let current = || self.current();
        let result = (|| {
            let field =
                native_markdown_table_intent_step(&current, || view.existing_table_cell_editor())
                    .ok()??;
            let actual = native_markdown_table_intent_step(&current, || field.currentEditor())
                .ok()??
                .downcast::<NSTextView>()
                .ok()?;
            self.table_selection_for(view, &actual).ok()?;
            Some(())
        })(); // 所有临时 Native claim 先析构，再决定原动作能否继续。
        result.is_some() && self.current()
    }

    /// 原实际 field editor、源 authority 和子选区必须同时相同；文本相同不能补签选择。
    pub(super) fn table_selection_for(
        &self,
        view: &PieceBackedMarkdownTextView,
        editor: &NSTextView,
    ) -> Result<NSRange, String> {
        let expired = || "表格选区已经改变，草稿将保持打开".to_owned();
        let table = self.table.as_ref().ok_or_else(expired)?;
        let (original_editor, selection) = table.selection.as_ref().ok_or_else(expired)?;
        if !self.current()
            || view
                .ivars()
                .active_table_cell
                .borrow()
                .as_ref()
                .is_none_or(|cell| {
                    cell.coordinate != table.coordinate
                        || cell.source_utf16_range != table.source_range
                        || cell.authority.as_ref().is_none_or(|authority| {
                            Rc::as_ptr(authority).addr() != table.authority_identity
                        })
                })
        {
            return Err(expired()); // 原字段重新激活或 authority 更换后不能复用旧子选区。
        }
        let current = || self.current();
        let original = native_markdown_table_intent_step(&current, || original_editor.load())
            .map_err(|_| expired())?
            .ok_or_else(expired)?;
        let same = std::ptr::eq(&*original, editor);
        drop(original); // Weak claim 的 release 也在原票核验范围内。
        if !same || !self.current() {
            return Err(expired());
        }
        let actual = native_markdown_table_intent_step(&current, || editor.selectedRange())
            .map_err(|_| expired())?;
        if actual != *selection {
            return Err(expired()); // 排队期间仅移动格内选区也撤销旧命令，不套用后来选中的文字。
        }
        Ok(actual)
    }

    /// 同一原票跨菜单 getter 与队列消费复核，拒绝后不 capture 当前选择作为替补。
    pub(super) fn current(&self) -> bool {
        ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
            let Some(surface) = active.as_ref() else {
                return false; // 关闭或暂借 owner 期间没有可消费原菜单动作的当前正文。
            };
            let Some(editor) = surface.editor.as_ref() else {
                return false; // 原文档已经离开，后继空态不能承接旧动作。
            };
            let view = &editor.backend.text_view;
            self.owner == std::ptr::from_ref(&*surface.controller).addr()
                && native_markdown_controller_has_foreground_authority(&surface.controller)
                && !native_markdown_operation_is_busy(
                    surface.pending_note_switch.is_some(),
                    surface.pending_file_mutation.is_some(),
                )
                && self.view_identity == std::ptr::from_ref(&**view).addr()
                && std::rc::Weak::ptr_eq(&self.document, &Rc::downgrade(editor.document()))
                && !view.ivars().textkit_graph_teardown.get()
                && view.ivars().table_input_intent_epoch.get() == self.input_epoch
                && self
                    .committed
                    .borrow()
                    .as_ref()
                    .map_or_else(|| self.source.current(), |receipt| receipt.current())
            // source 同时核原 generation、长度、完整选择、serial、fold、mode 和 owner；
            // 正文选择 A→B→A 也不能复活原票；格内另核真实 field editor 和 selectedRange。
        })
    }
}
